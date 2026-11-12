//! Index run (freeze item 13, on-disk-formats.md section 17).
//!
//! `index/<pack:016x>`: an immutable run of `chunk_id -> (seg, off, clen)`
//! in 4096-byte self-checking blocks, a block index, the covered segments and
//! a 64-byte trailer. Runs are checksummed, not authenticated: the trailer's
//! `run_hash` is BLAKE3-256 of everything before the trailer, computed and
//! checked by the caller (this crate performs no hashing).

use crate::bytes::{crc32c, Put, Reader};
use crate::error::{FormatError, Result};
use crate::ids::{ChunkId, ObjId, RunId, SegId};

const WHAT: &str = "index run";

/// Block length.
pub const BLOCK_LEN: usize = 4096;
/// Entries in every block but the last.
pub const ENTRIES_PER_BLOCK: usize = 85;
/// Entry length.
pub const ENTRY_LEN: usize = 48;
/// Trailer length.
pub const TRAILER_LEN: usize = 64;
/// Trailer magic.
pub const MAGIC: &[u8; 4] = b"LXIR";
/// Most blocks.
pub const MAX_BLOCKS: u32 = 1 << 28;
/// Most covered segments.
pub const MAX_COVERED: u32 = 1 << 24;

/// One index entry.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct IndexEntry {
    /// The key.
    pub chunk_id: ChunkId,
    /// Segment holding the current copy.
    pub seg: SegId,
    /// Offset of the extent in the segment.
    pub off: u32,
    /// Payload length of the extent.
    pub clen: u32,
}

/// A decoded run.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct IndexRun {
    /// The id in the object's name.
    pub run_id: RunId,
    /// Entries, strictly ascending by `chunk_id`.
    pub entries: Vec<IndexEntry>,
    /// Segments whose footer entries the run accounts for, ascending, unique.
    pub covered: Vec<SegId>,
    /// The stored `run_hash` (not verified here).
    pub run_hash: [u8; 32],
}

fn prefix_len(nblocks: usize, ncovered: usize) -> Option<usize> {
    nblocks
        .checked_mul(BLOCK_LEN + 32)?
        .checked_add(ncovered.checked_mul(8)?)
}

impl IndexRun {
    /// Validates the lists and builds the run, computing `run_hash` with
    /// `hash` over everything before the trailer.
    pub fn seal(
        run_id: RunId,
        entries: Vec<IndexEntry>,
        covered: Vec<SegId>,
        hash: impl Fn(&[u8]) -> [u8; 32],
    ) -> Result<Self> {
        let mut run = Self {
            run_id,
            entries,
            covered,
            run_hash: [0; 32],
        };
        let mut buf = Vec::new();
        run.encode_prefix(&mut buf)?;
        run.run_hash = hash(&buf);
        Ok(run)
    }

    fn check(&self) -> Result<()> {
        if self.entries.is_empty() {
            return Err(FormatError::invalid(WHAT, "a run has at least one entry"));
        }
        let nblocks = self.entries.len().div_ceil(ENTRIES_PER_BLOCK);
        if nblocks as u64 > u64::from(MAX_BLOCKS) {
            return Err(FormatError::count(
                WHAT,
                "nblocks",
                nblocks as u64,
                u64::from(MAX_BLOCKS),
            ));
        }
        if self.covered.len() as u64 > u64::from(MAX_COVERED) {
            return Err(FormatError::count(
                WHAT,
                "ncovered",
                self.covered.len() as u64,
                u64::from(MAX_COVERED),
            ));
        }
        if self
            .entries
            .windows(2)
            .any(|w| w[0].chunk_id >= w[1].chunk_id)
        {
            return Err(FormatError::not_canonical(
                WHAT,
                "chunk ids must strictly ascend",
            ));
        }
        if self.covered.windows(2).any(|w| w[0] >= w[1]) {
            return Err(FormatError::not_canonical(
                WHAT,
                "covered segments must ascend and be unique",
            ));
        }
        Ok(())
    }

    /// Everything before the trailer: blocks, block index, covered list.
    fn encode_prefix(&self, out: &mut Vec<u8>) -> Result<()> {
        self.check()?;
        let nblocks = self.entries.len().div_ceil(ENTRIES_PER_BLOCK);
        out.reserve(prefix_len(nblocks, self.covered.len()).unwrap_or(0) + TRAILER_LEN);
        for chunk in self.entries.chunks(ENTRIES_PER_BLOCK) {
            let start = out.len();
            out.put_u16(chunk.len() as u16);
            out.put_zeros(2);
            for e in chunk {
                out.extend_from_slice(&e.chunk_id.0);
                out.put_u64(e.seg.raw());
                out.put_u32(e.off);
                out.put_u32(e.clen);
            }
            out.put_zeros((ENTRIES_PER_BLOCK - chunk.len()) * ENTRY_LEN);
            out.put_zeros(8);
            let crc = crc32c(&out[start..]);
            out.put_u32(crc);
        }
        for chunk in self.entries.chunks(ENTRIES_PER_BLOCK) {
            out.extend_from_slice(&chunk[0].chunk_id.0);
        }
        for s in &self.covered {
            out.put_u64(s.raw());
        }
        Ok(())
    }

    /// Appends the whole run using the stored `run_hash`.
    pub fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        self.encode_prefix(out)?;
        out.put_u64(self.entries.len() as u64);
        out.put_u64(self.run_id.raw());
        out.put_u32(self.entries.len().div_ceil(ENTRIES_PER_BLOCK) as u32);
        out.put_u32(self.covered.len() as u32);
        out.extend_from_slice(&self.run_hash);
        out.put_u8(1);
        out.put_zeros(3);
        out.extend_from_slice(MAGIC);
        Ok(())
    }

    /// The bytes `run_hash` covers: everything before the trailer.
    pub fn hash_input(run: &[u8]) -> Result<&[u8]> {
        run.len()
            .checked_sub(TRAILER_LEN)
            .map(|n| &run[..n])
            .ok_or(FormatError::Truncated {
                what: WHAT,
                need: TRAILER_LEN,
                have: run.len(),
            })
    }

    /// Decodes a whole run.
    pub fn decode(buf: &[u8]) -> Result<Self> {
        let body_len = buf
            .len()
            .checked_sub(TRAILER_LEN)
            .ok_or(FormatError::Truncated {
                what: WHAT,
                need: TRAILER_LEN,
                have: buf.len(),
            })?;
        let mut t = Reader::new(&buf[body_len..], WHAT);
        let entries_n = t.u64()?;
        let run_id = ObjId::from_raw(t.u64()?)?;
        let nblocks = t.u32()?;
        let ncovered = t.u32()?;
        let run_hash: [u8; 32] = t.array()?;
        t.version_u8(1)?;
        t.reserved(3)?;
        t.magic(MAGIC)?;
        t.finish()?;
        if nblocks == 0 || nblocks > MAX_BLOCKS {
            return Err(FormatError::count(
                WHAT,
                "nblocks",
                u64::from(nblocks),
                u64::from(MAX_BLOCKS),
            ));
        }
        if ncovered > MAX_COVERED {
            return Err(FormatError::count(
                WHAT,
                "ncovered",
                u64::from(ncovered),
                u64::from(MAX_COVERED),
            ));
        }
        let (nb, nc) = (nblocks as usize, ncovered as usize);
        if prefix_len(nb, nc) != Some(body_len) {
            return Err(FormatError::invalid(
                WHAT,
                "length does not match the trailer",
            ));
        }
        if entries_n > (nb as u64) * ENTRIES_PER_BLOCK as u64 {
            return Err(FormatError::count(
                WHAT,
                "entries",
                entries_n,
                (nb as u64) * ENTRIES_PER_BLOCK as u64,
            ));
        }
        let mut entries =
            Vec::with_capacity(entries_n.min(body_len as u64 / ENTRY_LEN as u64) as usize);
        let mut firsts = Vec::with_capacity(nb);
        let mut r = Reader::new(&buf[..body_len], WHAT);
        for i in 0..nb {
            let block = r.take(BLOCK_LEN)?;
            let mut b = Reader::new(block, "index run block");
            let count = usize::from(b.u16()?);
            b.reserved(2)?;
            let last = i + 1 == nb;
            if count == 0 || count > ENTRIES_PER_BLOCK || (!last && count != ENTRIES_PER_BLOCK) {
                return Err(FormatError::range("index run block", "count", count as u64));
            }
            for _ in 0..count {
                let chunk_id = ChunkId(b.array()?);
                let seg = ObjId::from_raw(b.u64()?)?;
                let off = b.u32()?;
                let clen = b.u32()?;
                if firsts.len() == i {
                    firsts.push(chunk_id);
                }
                if entries
                    .last()
                    .is_some_and(|p: &IndexEntry| p.chunk_id >= chunk_id)
                {
                    return Err(FormatError::not_canonical(
                        WHAT,
                        "chunk ids must strictly ascend",
                    ));
                }
                entries.push(IndexEntry {
                    chunk_id,
                    seg,
                    off,
                    clen,
                });
            }
            b.reserved((ENTRIES_PER_BLOCK - count) * ENTRY_LEN)?;
            b.reserved(8)?;
            let covered = b.consumed();
            if b.u32()? != crc32c(covered) {
                return Err(FormatError::Checksum {
                    what: "index run block",
                });
            }
            b.finish()?;
        }
        if entries.len() as u64 != entries_n {
            return Err(FormatError::invalid(
                WHAT,
                "entry count differs from the trailer",
            ));
        }
        for f in &firsts {
            if ChunkId(r.array()?) != *f {
                return Err(FormatError::invalid(
                    WHAT,
                    "block index differs from the blocks",
                ));
            }
        }
        let mut covered = Vec::with_capacity(nc);
        for _ in 0..nc {
            let s = ObjId::from_raw(r.u64()?)?;
            if covered.last().is_some_and(|p| *p >= s) {
                return Err(FormatError::not_canonical(
                    WHAT,
                    "covered segments must ascend and be unique",
                ));
            }
            covered.push(s);
        }
        r.finish()?;
        Ok(Self {
            run_id,
            entries,
            covered,
            run_hash,
        })
    }
}
