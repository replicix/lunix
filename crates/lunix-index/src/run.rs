//! One persisted run (`index/<run:016x>`, on-disk-formats.md section 17).
//!
//! Reading is streaming and bounded. Opening a run reads it once from start
//! to end, checks every block checksum, the block index, the covered list and
//! the BLAKE3 `run_hash`, and keeps only the block index, the covered list
//! and the RAM filter (about 2.4 bytes per entry). A lookup then costs one
//! filter probe, a binary search of the block index and one 4 KiB `pread`.
//!
//! The layout constants and the entry layout come from `lunix_format::index_run`;
//! `lunix_format::index_run::IndexRun` is the reference codec and the tests
//! compare this streaming reader and writer with it byte for byte. The
//! streaming form exists so that building or merging a run never holds all of
//! its entries in memory.

use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Read, Write};
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

use lunix_format::bytes::{crc32c, Reader};
use lunix_format::index_run::{
    IndexEntry, BLOCK_LEN, ENTRIES_PER_BLOCK, ENTRY_LEN, MAGIC, MAX_BLOCKS, MAX_COVERED,
    TRAILER_LEN,
};
use lunix_format::{ChunkId, ObjId, RunId, SegId};
use lunix_iface::{Error, Result};

use crate::bloom::Bloom;

const WHAT: &str = "index run";
const ENTRIES_OFF: usize = 4;
const PAD_OFF: usize = ENTRIES_OFF + ENTRIES_PER_BLOCK * ENTRY_LEN;
const CRC_OFF: usize = BLOCK_LEN - 4;

fn corrupt(detail: &'static str) -> Error {
    Error::Corrupt { what: WHAT, detail }
}

pub(crate) fn run_path(dir: &Path, id: RunId) -> PathBuf {
    dir.join(id.to_string())
}

/// Fsyncs a directory so that a rename in it is durable.
pub(crate) fn sync_dir(dir: &Path) -> Result<()> {
    File::open(dir)?.sync_all()?;
    Ok(())
}

type Block = [u8; BLOCK_LEN];

/// Checks one block's framing and checksum; returns its entry count.
fn check_block(buf: &Block, last: bool) -> Result<usize> {
    let stored = u32::from_le_bytes([
        buf[CRC_OFF],
        buf[CRC_OFF + 1],
        buf[CRC_OFF + 2],
        buf[CRC_OFF + 3],
    ]);
    if stored != crc32c(&buf[..CRC_OFF]) {
        return Err(Error::Corrupt {
            what: "index run block",
            detail: "checksum",
        });
    }
    let count = usize::from(u16::from_le_bytes([buf[0], buf[1]]));
    if count == 0 || count > ENTRIES_PER_BLOCK || (!last && count != ENTRIES_PER_BLOCK) {
        return Err(corrupt("block entry count"));
    }
    let used = ENTRIES_OFF + count * ENTRY_LEN;
    let zero = |s: &[u8]| s.iter().all(|&b| b == 0);
    if !zero(&buf[2..4]) || !zero(&buf[used..CRC_OFF]) {
        return Err(corrupt("reserved bytes"));
    }
    debug_assert!(used <= PAD_OFF);
    Ok(count)
}

fn entry_id(buf: &Block, j: usize) -> &[u8] {
    let o = ENTRIES_OFF + j * ENTRY_LEN;
    &buf[o..o + 32]
}

fn entry_at(buf: &Block, j: usize) -> Result<IndexEntry> {
    let o = ENTRIES_OFF + j * ENTRY_LEN;
    let mut r = Reader::new(&buf[o..o + ENTRY_LEN], WHAT);
    let chunk_id = ChunkId(r.array()?);
    let seg = ObjId::from_raw(r.u64()?)?;
    let off = r.u32()?;
    let clen = r.u32()?;
    Ok(IndexEntry {
        chunk_id,
        seg,
        off,
        clen,
    })
}

/// An opened, fully validated run.
#[derive(Debug)]
pub(crate) struct Run {
    pub id: RunId,
    file: File,
    pub size: u64,
    pub entries: u64,
    firsts: Vec<ChunkId>,
    bloom: Bloom,
    /// Segments with at least one entry here, ascending.
    pub covered: Vec<SegId>,
}

struct Trailer {
    entries: u64,
    nblocks: u32,
    ncovered: u32,
    hash: [u8; 32],
}

fn read_trailer(file: &File, len: u64, id: RunId) -> Result<Trailer> {
    let tl = TRAILER_LEN as u64;
    if len < tl {
        return Err(corrupt("shorter than a trailer"));
    }
    let mut t = [0u8; TRAILER_LEN];
    file.read_exact_at(&mut t, len - tl)?;
    let mut r = Reader::new(&t, WHAT);
    let entries = r.u64()?;
    let run_id = r.u64()?;
    let nblocks = r.u32()?;
    let ncovered = r.u32()?;
    let hash: [u8; 32] = r.array()?;
    r.version_u8(1)?;
    r.reserved(3)?;
    r.magic(MAGIC)?;
    r.finish()?;
    if run_id != id.raw() {
        return Err(corrupt("run id differs from the name"));
    }
    if nblocks == 0 || nblocks > MAX_BLOCKS || ncovered > MAX_COVERED {
        return Err(corrupt("trailer counts"));
    }
    let expect = u64::from(nblocks)
        .checked_mul((BLOCK_LEN + 32) as u64)
        .and_then(|v| v.checked_add(u64::from(ncovered) * 8))
        .and_then(|v| v.checked_add(tl));
    if expect != Some(len) {
        return Err(corrupt("length differs from the trailer"));
    }
    if entries > u64::from(nblocks) * ENTRIES_PER_BLOCK as u64 {
        return Err(corrupt("entry count exceeds the blocks"));
    }
    Ok(Trailer {
        entries,
        nblocks,
        ncovered,
        hash,
    })
}

impl Run {
    /// Opens and validates `dir/<id>` end to end.
    pub(crate) fn open(dir: &Path, id: RunId) -> Result<Self> {
        let file = File::open(run_path(dir, id))?;
        let size = file.metadata()?.len();
        let t = read_trailer(&file, size, id)?;
        let nb = t.nblocks as usize;
        let mut rd = BufReader::with_capacity(1 << 20, &file);
        let mut hasher = blake3::Hasher::new();
        let mut bloom = Bloom::with_entries(t.entries);
        let mut firsts: Vec<ChunkId> = Vec::with_capacity(nb);
        let mut prev: Option<ChunkId> = None;
        let mut counted = 0u64;
        let mut buf: Block = [0; BLOCK_LEN];
        for i in 0..nb {
            rd.read_exact(&mut buf)?;
            hasher.update(&buf);
            let count = check_block(&buf, i + 1 == nb)?;
            for j in 0..count {
                let e = entry_at(&buf, j)?;
                if prev.is_some_and(|p| p >= e.chunk_id) {
                    return Err(corrupt("chunk ids must strictly ascend"));
                }
                prev = Some(e.chunk_id);
                bloom.insert(&e.chunk_id);
                if j == 0 {
                    firsts.push(e.chunk_id);
                }
            }
            counted += count as u64;
        }
        if counted != t.entries {
            return Err(corrupt("entry count differs from the trailer"));
        }
        let mut id32 = [0u8; 32];
        for f in &firsts {
            rd.read_exact(&mut id32)?;
            hasher.update(&id32);
            if id32 != f.0 {
                return Err(corrupt("block index differs from the blocks"));
            }
        }
        let mut covered: Vec<SegId> = Vec::with_capacity(t.ncovered as usize);
        let mut w = [0u8; 8];
        for _ in 0..t.ncovered {
            rd.read_exact(&mut w)?;
            hasher.update(&w);
            let s = ObjId::from_raw(u64::from_le_bytes(w))?;
            if covered.last().is_some_and(|p| *p >= s) {
                return Err(corrupt("covered segments must ascend"));
            }
            covered.push(s);
        }
        if hasher.finalize().as_bytes() != &t.hash {
            return Err(Error::Corrupt {
                what: WHAT,
                detail: "run hash",
            });
        }
        drop(rd);
        Ok(Self {
            id,
            file,
            size,
            entries: t.entries,
            firsts,
            bloom,
            covered,
        })
    }

    /// Heap bytes held for the filter and the directory.
    pub(crate) fn ram_bytes(&self) -> usize {
        self.bloom.heap_bytes() + self.firsts.len() * 32 + self.covered.len() * 8
    }

    fn read_block(&self, i: usize, buf: &mut Block) -> Result<usize> {
        let off = (i as u64) * BLOCK_LEN as u64;
        self.file.read_exact_at(buf, off)?;
        check_block(buf, i + 1 == self.firsts.len())
    }

    /// The entry for `id`, if the run has one. A damaged block is an error.
    pub(crate) fn lookup(&self, id: &ChunkId) -> Result<Option<IndexEntry>> {
        if !self.bloom.maybe_contains(id) {
            return Ok(None);
        }
        let b = self.firsts.partition_point(|f| f <= id);
        let Some(block) = b.checked_sub(1) else {
            return Ok(None);
        };
        let mut buf: Block = [0; BLOCK_LEN];
        let count = self.read_block(block, &mut buf)?;
        let (mut lo, mut hi) = (0usize, count);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            match entry_id(&buf, mid).cmp(&id.0[..]) {
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
                std::cmp::Ordering::Equal => return entry_at(&buf, mid).map(Some),
            }
        }
        Ok(None)
    }

    /// Entries in ascending order starting at the first id `>= start`.
    pub(crate) fn iter_from(self: &std::sync::Arc<Self>, start: Option<&ChunkId>) -> RunIter {
        let block = start
            .map(|s| self.firsts.partition_point(|f| f <= s).saturating_sub(1))
            .unwrap_or(0);
        RunIter {
            run: self.clone(),
            next_block: block,
            buf: Box::new([0; BLOCK_LEN]),
            count: 0,
            idx: 0,
            start: start.copied(),
        }
    }
}

/// Ascending iterator over a run, one block at a time.
pub(crate) struct RunIter {
    run: std::sync::Arc<Run>,
    next_block: usize,
    buf: Box<Block>,
    count: usize,
    idx: usize,
    start: Option<ChunkId>,
}

impl Iterator for RunIter {
    type Item = Result<IndexEntry>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if self.idx >= self.count {
                if self.next_block >= self.run.firsts.len() {
                    return None;
                }
                match self.run.read_block(self.next_block, &mut self.buf) {
                    Ok(c) => {
                        self.count = c;
                        self.idx = 0;
                        self.next_block += 1;
                    }
                    Err(e) => {
                        self.next_block = usize::MAX;
                        self.count = 0;
                        return Some(Err(e));
                    }
                }
            }
            let j = self.idx;
            self.idx += 1;
            if let Some(s) = &self.start {
                if entry_id(&self.buf, j) < &s.0[..] {
                    continue;
                }
                self.start = None;
            }
            return Some(entry_at(&self.buf, j));
        }
    }
}

/// Streams a run to `<dir>/<id>.tmp` and renames it into place when finished.
pub(crate) struct RunWriter {
    dir: PathBuf,
    id: RunId,
    tmp: PathBuf,
    out: BufWriter<File>,
    hasher: blake3::Hasher,
    block: Vec<u8>,
    count: usize,
    entries: u64,
    written: u64,
    firsts: Vec<ChunkId>,
    last: Option<ChunkId>,
    bloom: Bloom,
}

impl RunWriter {
    /// `expected` sizes the RAM filter (an upper bound is fine).
    pub(crate) fn create(dir: &Path, id: RunId, expected: u64) -> Result<Self> {
        let tmp = dir.join(format!("{id}.tmp"));
        let file = File::create(&tmp)?;
        Ok(Self {
            dir: dir.to_owned(),
            id,
            tmp,
            out: BufWriter::with_capacity(1 << 20, file),
            hasher: blake3::Hasher::new(),
            block: Vec::with_capacity(BLOCK_LEN),
            count: 0,
            entries: 0,
            written: 0,
            firsts: Vec::new(),
            last: None,
            bloom: Bloom::with_entries(expected),
        })
    }

    fn emit(&mut self, bytes: &[u8]) -> Result<()> {
        self.hasher.update(bytes);
        self.out.write_all(bytes)?;
        self.written += bytes.len() as u64;
        Ok(())
    }

    fn flush_block(&mut self) -> Result<()> {
        if self.count == 0 {
            return Ok(());
        }
        if self.firsts.len() as u64 >= u64::from(MAX_BLOCKS) {
            return Err(Error::invalid("index run exceeds the block limit"));
        }
        let mut b = std::mem::take(&mut self.block);
        b[..2].copy_from_slice(&(self.count as u16).to_le_bytes());
        b.resize(CRC_OFF, 0);
        let crc = crc32c(&b);
        b.extend_from_slice(&crc.to_le_bytes());
        debug_assert_eq!(b.len(), BLOCK_LEN);
        self.emit(&b)?;
        b.clear();
        self.block = b;
        self.count = 0;
        Ok(())
    }

    /// Appends an entry; ids must strictly ascend.
    pub(crate) fn push(&mut self, e: &IndexEntry) -> Result<()> {
        if self.last.is_some_and(|p| p >= e.chunk_id) {
            return Err(Error::invalid("run entries must strictly ascend"));
        }
        self.last = Some(e.chunk_id);
        if self.count == 0 {
            self.block.clear();
            self.block.extend_from_slice(&[0; ENTRIES_OFF]);
            self.firsts.push(e.chunk_id);
        }
        self.block.extend_from_slice(&e.chunk_id.0);
        self.block.extend_from_slice(&e.seg.raw().to_le_bytes());
        self.block.extend_from_slice(&e.off.to_le_bytes());
        self.block.extend_from_slice(&e.clen.to_le_bytes());
        self.bloom.insert(&e.chunk_id);
        self.count += 1;
        self.entries += 1;
        if self.count == ENTRIES_PER_BLOCK {
            self.flush_block()?;
        }
        Ok(())
    }

    /// Completes the run (durably) and opens it. `None` when nothing was
    /// pushed: no file is left behind.
    pub(crate) fn finish(mut self, covered: &[SegId]) -> Result<Option<Run>> {
        if self.entries == 0 {
            drop(self.out);
            let _ = fs::remove_file(&self.tmp);
            return Ok(None);
        }
        if covered.len() as u64 > u64::from(MAX_COVERED) {
            return Err(Error::invalid("too many covered segments"));
        }
        if covered.windows(2).any(|w| w[0] >= w[1]) {
            return Err(Error::invalid("covered segments must ascend"));
        }
        self.flush_block()?;
        let firsts = std::mem::take(&mut self.firsts);
        for f in &firsts {
            self.emit(&f.0)?;
        }
        for s in covered {
            self.emit(&s.raw().to_le_bytes())?;
        }
        let hash = *self.hasher.finalize().as_bytes();
        let mut t = Vec::with_capacity(TRAILER_LEN);
        t.extend_from_slice(&self.entries.to_le_bytes());
        t.extend_from_slice(&self.id.raw().to_le_bytes());
        t.extend_from_slice(&(firsts.len() as u32).to_le_bytes());
        t.extend_from_slice(&(covered.len() as u32).to_le_bytes());
        t.extend_from_slice(&hash);
        t.extend_from_slice(&[1, 0, 0, 0]);
        t.extend_from_slice(MAGIC);
        self.out.write_all(&t)?;
        self.written += t.len() as u64;
        let file = self.out.into_inner().map_err(|e| e.into_error())?;
        file.sync_all()?;
        drop(file);
        fs::rename(&self.tmp, run_path(&self.dir, self.id))?;
        sync_dir(&self.dir)?;
        Ok(Some(Run {
            id: self.id,
            file: File::open(run_path(&self.dir, self.id))?,
            size: self.written,
            entries: self.entries,
            firsts,
            bloom: self.bloom,
            covered: covered.to_vec(),
        }))
    }
}

/// Reads only the trailer of an unopened run file for listings: `(size, entries)`.
/// A file that does not parse reports 0 entries.
pub(crate) fn peek(dir: &Path, id: RunId) -> Result<(u64, u64)> {
    let file = File::open(run_path(dir, id))?;
    let size = file.metadata()?.len();
    let entries = read_trailer(&file, size, id).map_or(0, |t| t.entries);
    Ok((size, entries))
}
