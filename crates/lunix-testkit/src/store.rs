//! [`MemStore`]: every store trait of a volume directory in memory.
//!
//! One handle (cheap to clone) implements [`SegmentStore`], [`PackStore`],
//! [`CommitStore`], [`CheckpointStore`], [`PinSource`], [`GcStateStore`] and
//! [`VolumeDir`], plus [`IdAllocator`] through [`MemStore::ids`]. It follows the
//! documented durable order: an object that is created is complete when the
//! call returns, create-only objects are refused with `AlreadyExists`, deletes
//! are idempotent, ids are never reused within a process.
//!
//! # What it checks that the real store also checks
//!
//! * `append_extent` parses the fixed prefix of the extent (unencrypted layout:
//!   `CCH1` header, `chunk_id`, `clen`, payload, 16-byte tag), requires the id
//!   to equal the one passed, `clen`/`ulen` to agree with the codec, and
//!   builds the footer (entries sorted by `(chunk_id, off)`, `seg_hash`,
//!   `write_seq` range, dictionary ids) as `FooterBody`;
//! * `read_extent` bounds-checks the hint and compares the stored `clen` with
//!   it (`Corrupt`), only sealed containers are readable;
//! * `delete` of a commit refuses the tip and anything at or above `chain_floor`;
//! * the checkpoint is two slots, the store assigns `slot_seq`;
//! * the sweep refuses a named id above the mark (`Invalid`), deletes unnamed
//!   containers above it, reports the highest id present before it deleted
//!   anything and feeds that id to the allocator;
//! * `leftovers` and the sweep ignore the file of a writer open in this process;
//! * `CheckpointStore::store` refuses a slot of another volume, `load` ignores
//!   one; [`MemStore::load_checkpoint_hint`] applies the tail guard.
//!
//! # Crash model
//!
//! [`MemStore::crash`] returns the durable image: every completed create and
//! delete is in it. A writer that was open at the time leaves, at random, an
//! `*.open` leftover (size at the time) that `leftovers()` reports and the
//! sweep removes. [`MemStore::close`] is a clean shutdown (open writers are
//! abandoned). After a crash or close the old handle is dead (`Io`).
//! The id allocator of an opened image starts at the highest id on disk, not at
//! the old counter, so a caller that forgets `observe` can reuse the id of an
//! abandoned segment; M11's startup order is what prevents it.
//!
//! # Fault points
//!
//! `segment.append`, `segment.seal.{before,after}`, `segment.delete.{before,after}`,
//! `pack.seal.{before,after}`, `pack.delete.{before,after}`,
//! `commit.put.{before,after}`, `commit.delete.{before,after}`,
//! `commit.set_aside.{before,after}`, `checkpoint.store.{before,after}`,
//! `condemned.store.{before,after}`, `evidence.put.{before,after}`,
//! `volume_json.store.{before,after}`, `sweep.before`. See [`Faults`].

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::io::Write;
use std::os::fd::OwnedFd;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use lunix_format::gcfile::Condemned;
use lunix_format::local::{CheckpointSlot, DeviceRecord, LocalPins, VolumeJson};
use lunix_format::node::NodeLoc;
use lunix_format::paths::local as lpaths;
use lunix_format::segment::{
    encode_footer_form0, footer_frame, ContainerKind, FooterBody, FooterEntry, FrameCompression,
    MAX_DATA_ULEN, MAX_NODE_ULEN, TRAILER_LEN,
};
use lunix_format::{
    ChunkId, Codec, CommitSeq, CryptoMode, Hint, ObjId, PackId, SegId, VolumeUuid, WriteSeq,
};
use lunix_iface::journal::WriteSeqRange;
use lunix_iface::store::{
    CheckpointStore, CommitInfo, CommitStore, ExtentLocation, GcStateStore, IdAllocator, Leftover,
    PackInfo, PackRead, PackStore, PackWrite, PinRoot, PinSource, SealedPack, SealedSegment,
    SegmentInfo, SegmentStore, SegmentWriter, SweepReport, SweepRequest, VolumeDir,
};
use lunix_iface::{Error, Result};

use crate::faults::Faults;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// An [`IdAllocator`] that lives in memory. Share one between a [`MemStore`]
/// and a [`MemIndex`](crate::MemIndex) (segments, packs and index runs have one
/// id space).
#[derive(Debug, Default)]
pub struct MemIds {
    high: AtomicU64,
}

impl MemIds {
    /// A counter that has issued nothing.
    pub fn new() -> Self {
        Self::default()
    }
}

impl IdAllocator for MemIds {
    fn allocate(&self) -> Result<ObjId> {
        let refused = || Error::Refused {
            why: "object id counter exhausted",
        };
        loop {
            let cur = self.high.load(Ordering::SeqCst);
            let next = if cur == 0 {
                ObjId::new(0, 1)
            } else {
                ObjId::from_raw(cur).and_then(ObjId::next)
            }
            .map_err(|_| refused())?;
            if self
                .high
                .compare_exchange(cur, next.raw(), Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                return Ok(next);
            }
        }
    }

    fn high_water(&self) -> u64 {
        self.high.load(Ordering::SeqCst)
    }

    fn observe(&self, raw: u64) {
        self.high.fetch_max(raw, Ordering::SeqCst);
    }
}

/// Settings of a [`MemStore`].
#[derive(Clone, Debug)]
pub struct MemStoreConfig {
    /// The volume's uuid.
    pub uuid: VolumeUuid,
    /// The exact `volume` descriptor bytes the store returns.
    pub volume_bytes: Vec<u8>,
    /// A writer reports `is_full` from this many bytes (16 MiB for real).
    pub segment_target: u64,
    /// Total bytes the store may hold; creates beyond it fail `NoSpace`.
    pub capacity_bytes: Option<u64>,
}

impl Default for MemStoreConfig {
    fn default() -> Self {
        Self {
            uuid: VolumeUuid([7; 16]),
            volume_bytes: b"volume-descriptor".to_vec(),
            segment_target: 16 << 20,
            capacity_bytes: None,
        }
    }
}

#[derive(Clone, Debug)]
struct Container {
    extents: Vec<u8>,
    footer: FooterBody,
    size: u64,
}

#[derive(Clone, Debug)]
struct LeftoverFile {
    name: String,
    size: u64,
    id: Option<u64>,
}

#[derive(Clone, Debug)]
struct Durable {
    segments: BTreeMap<u64, Container>,
    packs: BTreeMap<u64, Container>,
    commits: BTreeMap<u64, Vec<u8>>,
    damaged: BTreeMap<u64, Vec<u8>>,
    checkpoint: [Option<CheckpointSlot>; 2],
    condemned: Option<Condemned>,
    evidence: BTreeMap<u64, Vec<u8>>,
    volume_json: VolumeJson,
    device: Option<DeviceRecord>,
    local_pins: Option<LocalPins>,
    leftovers: Vec<LeftoverFile>,
}

/// What survives a crash or a clean close of a [`MemStore`].
#[derive(Clone, Debug)]
pub struct StoreImage {
    cfg: MemStoreConfig,
    durable: Durable,
}

impl StoreImage {
    /// A fresh volume directory.
    pub fn new(cfg: MemStoreConfig) -> Self {
        Self {
            durable: Durable {
                segments: BTreeMap::new(),
                packs: BTreeMap::new(),
                commits: BTreeMap::new(),
                damaged: BTreeMap::new(),
                checkpoint: [None, None],
                condemned: None,
                evidence: BTreeMap::new(),
                volume_json: VolumeJson::new("memvol", "/mem"),
                device: None,
                local_pins: None,
                leftovers: Vec::new(),
            },
            cfg,
        }
    }
}

struct WriterInfo {
    name: String,
    len: u64,
}

struct State {
    d: Durable,
    writers: BTreeMap<u64, WriterInfo>,
    pin_roots: Vec<PinRoot>,
    pins_fail: bool,
    capacity: Option<u64>,
}

struct Shared {
    cfg: MemStoreConfig,
    state: Mutex<State>,
    faults: Faults,
    ids: Arc<MemIds>,
}

/// The in-memory volume directory. Clone it to share it.
#[derive(Clone)]
pub struct MemStore(Arc<Shared>);

impl std::fmt::Debug for MemStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let st = lock(&self.0.state);
        f.debug_struct("MemStore")
            .field("segments", &st.d.segments.len())
            .field("packs", &st.d.packs.len())
            .field("commits", &st.d.commits.len())
            .finish()
    }
}

fn footer_env_len(body: &FooterBody) -> u64 {
    let how = if body.entries.len() < 2000 {
        FrameCompression::Raw
    } else {
        FrameCompression::Zstd(3)
    };
    footer_frame(body, how)
        .and_then(|f| encode_footer_form0(&f, &[0; 32]))
        .map_or(72 + 52 * body.entries.len() as u64, |b| b.len() as u64)
}

fn id_of(name: &str) -> Option<u64> {
    let stem = name.rsplit('/').next()?;
    u64::from_str_radix(stem.split('.').next()?, 16).ok()
}

impl MemStore {
    /// An empty volume directory with default settings.
    pub fn new() -> Self {
        Self::from_image(&StoreImage::new(MemStoreConfig::default()))
    }

    /// An empty volume directory.
    pub fn with_config(cfg: MemStoreConfig) -> Self {
        Self::from_image(&StoreImage::new(cfg))
    }

    /// Opens an image (after a crash or a close).
    pub fn from_image(image: &StoreImage) -> Self {
        let d = image.durable.clone();
        let ids = Arc::new(MemIds::new());
        let highest = d
            .segments
            .keys()
            .chain(d.packs.keys())
            .copied()
            .chain(d.leftovers.iter().filter_map(|l| l.id))
            .max()
            .unwrap_or(0);
        ids.observe(highest);
        Self(Arc::new(Shared {
            cfg: image.cfg.clone(),
            state: Mutex::new(State {
                d,
                writers: BTreeMap::new(),
                pin_roots: Vec::new(),
                pins_fail: false,
                capacity: image.cfg.capacity_bytes,
            }),
            faults: Faults::default(),
            ids,
        }))
    }

    /// A clean shutdown: open writers are abandoned (their files removed). The
    /// handle is dead afterwards.
    pub fn close(&self) -> StoreImage {
        self.0.faults.kill();
        let st = lock(&self.0.state);
        StoreImage {
            cfg: self.0.cfg.clone(),
            durable: st.d.clone(),
        }
    }

    /// A crash now. Each open writer leaves, at random (`seed`), an `*.open`
    /// leftover. The handle is dead afterwards.
    pub fn crash(&self, seed: u64) -> StoreImage {
        self.0.faults.kill();
        let st = lock(&self.0.state);
        let mut d = st.d.clone();
        let mut z = seed;
        for (&raw, w) in &st.writers {
            z = z.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut x = z;
            x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            x ^= x >> 27;
            if x.is_multiple_of(2) {
                d.leftovers.push(LeftoverFile {
                    name: w.name.clone(),
                    size: w.len,
                    id: Some(raw),
                });
            }
        }
        StoreImage {
            cfg: self.0.cfg.clone(),
            durable: d,
        }
    }

    /// The fault points.
    pub fn faults(&self) -> &Faults {
        &self.0.faults
    }

    /// The shared id allocator (also hand it to a [`MemIndex`](crate::MemIndex)).
    pub fn mem_ids(&self) -> Arc<MemIds> {
        Arc::clone(&self.0.ids)
    }

    /// Sets the pins `PinSource::roots` returns.
    pub fn set_pins(&self, roots: Vec<PinRoot>) {
        lock(&self.0.state).pin_roots = roots;
    }

    /// Makes `PinSource::roots` fail (an undecodable pin).
    pub fn fail_pins(&self, fail: bool) {
        lock(&self.0.state).pins_fail = fail;
    }

    /// Changes the byte budget (`None` for unlimited).
    pub fn set_capacity(&self, bytes: Option<u64>) {
        lock(&self.0.state).capacity = bytes;
    }

    /// Bytes held by sealed containers, commits and evidence.
    pub fn used_bytes(&self) -> u64 {
        Self::used(&lock(&self.0.state))
    }

    /// Plants a leftover file (a `*.tmp` or `*.open` a crash left).
    pub fn add_leftover(&self, name: &str, size: u64) {
        lock(&self.0.state).d.leftovers.push(LeftoverFile {
            name: name.to_owned(),
            size,
            id: id_of(name),
        });
    }

    /// Overwrites a stored commit's bytes (corruption injection).
    pub fn corrupt_commit(&self, seq: CommitSeq, object: &[u8]) {
        lock(&self.0.state).d.commits.insert(seq.0, object.to_vec());
    }

    /// Damages the newest checkpoint slot (a torn write): `load` then falls
    /// back to the older slot, or `None`.
    pub fn tear_newest_checkpoint(&self) {
        let mut st = lock(&self.0.state);
        let newest =
            st.d.checkpoint
                .iter()
                .enumerate()
                .filter_map(|(i, s)| s.as_ref().map(|s| (s.slot_seq, i)))
                .max();
        if let Some((_, i)) = newest {
            st.d.checkpoint[i] = None;
        }
    }

    fn used(st: &State) -> u64 {
        st.d.segments.values().map(|c| c.size).sum::<u64>()
            + st.d.packs.values().map(|c| c.size).sum::<u64>()
            + st.d.commits.values().map(|c| c.len() as u64).sum::<u64>()
            + st.d.evidence.values().map(|c| c.len() as u64).sum::<u64>()
    }

    fn charge(st: &State, bytes: u64) -> Result<()> {
        match st.capacity {
            Some(cap) if Self::used(st) + bytes > cap => Err(Error::NoSpace),
            _ => Ok(()),
        }
    }

    fn st(&self) -> Result<MutexGuard<'_, State>> {
        self.0.faults.alive()?;
        Ok(lock(&self.0.state))
    }
}

impl Default for MemStore {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------- extents

/// What `append_*` learned from one extent's fixed prefix.
struct Parsed {
    entry: FooterEntry,
    total: usize,
}

fn parse_extent(
    extent: &[u8],
    expect_id: &ChunkId,
    dict_id: Option<u32>,
    max_ulen: u32,
) -> Result<Parsed> {
    let bad = |why: &'static str| Error::Invalid { why };
    let (hdr, id, clen) =
        lunix_format::extent::parse_plain_prefix(extent).map_err(|_| bad("not an extent"))?;
    let ovh = CryptoMode::Plain.extent_ovh() as usize;
    if extent.len() != ovh + clen as usize {
        return Err(bad("extent length does not match its clen"));
    }
    if id != *expect_id {
        return Err(bad("extent id differs from the id passed"));
    }
    let ulen = u32::try_from(hdr.ulen).map_err(|_| bad("ulen"))?;
    if ulen == 0 || ulen > max_ulen {
        return Err(bad("ulen out of range"));
    }
    let size_ok = if hdr.codec == Codec::Raw {
        clen == ulen
    } else {
        (1..=ulen).contains(&clen)
    };
    if !size_ok {
        return Err(bad("clen disagrees with the codec"));
    }
    if dict_id.is_some() != (hdr.codec == Codec::ZstdDict) {
        return Err(bad("dict_id must be given exactly for codec 3"));
    }
    Ok(Parsed {
        entry: FooterEntry {
            chunk_id: id,
            off: 0,
            clen,
            ulen,
            codec: hdr.codec,
        },
        total: extent.len(),
    })
}

struct Builder {
    shared: Arc<Shared>,
    id: ObjId,
    kind: ContainerKind,
    buf: Vec<u8>,
    entries: Vec<FooterEntry>,
    dicts: BTreeSet<u32>,
    done: bool,
}

impl Builder {
    fn new(shared: Arc<Shared>, kind: ContainerKind) -> Result<Self> {
        shared.faults.alive()?;
        let id = shared.ids.allocate()?;
        let name = match kind {
            ContainerKind::Segment => lpaths::segment_open(id),
            ContainerKind::MapPack => lpaths::map_pack_open(id),
        };
        lock(&shared.state)
            .writers
            .insert(id.raw(), WriterInfo { name, len: 0 });
        Ok(Self {
            shared,
            id,
            kind,
            buf: Vec::new(),
            entries: Vec::new(),
            dicts: BTreeSet::new(),
            done: false,
        })
    }

    fn append(
        &mut self,
        id: &ChunkId,
        dict_id: Option<u32>,
        extent: &[u8],
        max_ulen: u32,
    ) -> Result<(u32, u32)> {
        self.shared.faults.point("segment.append")?;
        let mut p = parse_extent(extent, id, dict_id, max_ulen)?;
        let off = u32::try_from(self.buf.len()).map_err(|_| Error::NoSpace)?;
        u32::try_from(self.buf.len() + p.total).map_err(|_| Error::NoSpace)?;
        {
            let st = lock(&self.shared.state);
            MemStore::charge(&st, (self.buf.len() + p.total) as u64)?;
        }
        p.entry.off = off;
        self.buf.extend_from_slice(extent);
        self.entries.push(p.entry);
        if let Some(d) = dict_id {
            self.dicts.insert(d);
        }
        if let Some(w) = lock(&self.shared.state).writers.get_mut(&self.id.raw()) {
            w.len = self.buf.len() as u64;
        }
        Ok((off, p.entry.clen))
    }

    /// Builds and stores the container.
    fn seal(
        &mut self,
        range: Option<WriteSeqRange>,
        before: &'static str,
        after: &'static str,
    ) -> Result<(u64, u32)> {
        if self.entries.is_empty() {
            return Err(Error::invalid("a container needs at least one extent"));
        }
        let (lo, hi) = match range {
            Some(r) if r.first > r.last => return Err(Error::invalid("write_seq range reversed")),
            Some(r) => (r.first, r.last),
            None => (WriteSeq(0), WriteSeq(0)),
        };
        let mut entries = self.entries.clone();
        entries.sort_by_key(|e| (e.chunk_id, e.off));
        let footer = FooterBody {
            container_kind: self.kind,
            object_id: self.id,
            extents_len: self.buf.len() as u64,
            write_seq_lo: lo,
            write_seq_hi: hi,
            seg_hash: *blake3::hash(&self.buf).as_bytes(),
            entries,
            dict_ids: self.dicts.iter().copied().collect(),
        };
        let size = self.buf.len() as u64 + footer_env_len(&footer) + TRAILER_LEN as u64;
        self.shared.faults.point(before)?;
        let n = self.entries.len() as u32;
        {
            let mut st = lock(&self.shared.state);
            MemStore::charge(&st, size)?;
            let c = Container {
                extents: std::mem::take(&mut self.buf),
                footer,
                size,
            };
            match self.kind {
                ContainerKind::Segment => st.d.segments.insert(self.id.raw(), c),
                ContainerKind::MapPack => st.d.packs.insert(self.id.raw(), c),
            };
            st.writers.remove(&self.id.raw());
        }
        self.done = true;
        self.shared.faults.point(after)?;
        Ok((size, n))
    }
}

impl Drop for Builder {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        let mut st = lock(&self.shared.state);
        let Some(w) = st.writers.remove(&self.id.raw()) else {
            return;
        };
        // Abandoned cleanly: the file is removed. After a crash it stays.
        if self.shared.faults.is_dead() {
            st.d.leftovers.push(LeftoverFile {
                name: w.name,
                size: w.len,
                id: Some(self.id.raw()),
            });
        }
    }
}

struct MemSegmentWriter {
    b: Builder,
}

impl SegmentWriter for MemSegmentWriter {
    fn id(&self) -> SegId {
        self.b.id
    }

    fn append_extent(
        &mut self,
        chunk_id: &ChunkId,
        dict_id: Option<u32>,
        extent: &[u8],
    ) -> Result<Hint> {
        let (off, clen) = self.b.append(chunk_id, dict_id, extent, MAX_DATA_ULEN)?;
        Ok(Hint {
            seg: self.b.id,
            off,
            clen,
        })
    }

    fn len(&self) -> u64 {
        self.b.buf.len() as u64
    }

    fn is_full(&self) -> bool {
        self.b.buf.len() as u64 >= self.b.shared.cfg.segment_target
    }

    fn seal(mut self: Box<Self>, writes: Option<WriteSeqRange>) -> Result<SealedSegment> {
        let (size_bytes, extents) =
            self.b
                .seal(writes, "segment.seal.before", "segment.seal.after")?;
        Ok(SealedSegment {
            seg: self.b.id,
            size_bytes,
            extents,
        })
    }
}

impl SegmentStore for MemStore {
    fn open_segment(&self) -> Result<Box<dyn SegmentWriter>> {
        Ok(Box::new(MemSegmentWriter {
            b: Builder::new(Arc::clone(&self.0), ContainerKind::Segment)?,
        }))
    }

    fn read_extent(&self, hint: &Hint, out: &mut Vec<u8>) -> Result<()> {
        let st = self.st()?;
        let c =
            st.d.segments
                .get(&hint.seg.raw())
                .ok_or(Error::not_found("segment"))?;
        let len = CryptoMode::Plain.extent_ovh() as usize + hint.clen as usize;
        let start = hint.off as usize;
        let bytes = start
            .checked_add(len)
            .and_then(|end| c.extents.get(start..end))
            .ok_or(Error::Corrupt {
                what: "segment",
                detail: "hint outside the extent region",
            })?;
        // The stored clen must be the hint's: a misaligned hint is `Corrupt`.
        let stored = bytes
            .get(48..52)
            .and_then(|b| <[u8; 4]>::try_from(b).ok())
            .map(u32::from_le_bytes);
        if stored != Some(hint.clen) {
            return Err(Error::Corrupt {
                what: "segment",
                detail: "stored clen differs from the hint",
            });
        }
        out.extend_from_slice(bytes);
        Ok(())
    }

    fn locate(&self, hint: &Hint) -> Result<ExtentLocation> {
        let st = self.st()?;
        let c =
            st.d.segments
                .get(&hint.seg.raw())
                .ok_or(Error::not_found("segment"))?;
        let len = CryptoMode::Plain.extent_ovh() + hint.clen;
        if u64::from(hint.off) + u64::from(len) > c.extents.len() as u64 {
            return Err(Error::Corrupt {
                what: "segment",
                detail: "hint outside the extent region",
            });
        }
        // A real file descriptor holding the extent region, for callers that
        // `pread` it. Made on demand; it keeps working after a delete.
        let mut file = tempfile::tempfile()?;
        file.write_all(&c.extents)?;
        file.flush()?;
        Ok(ExtentLocation {
            fd: Arc::new(OwnedFd::from(file)),
            offset: u64::from(hint.off),
            len,
        })
    }

    fn footer(&self, seg: SegId) -> Result<FooterBody> {
        self.st()?
            .d
            .segments
            .get(&seg.raw())
            .map(|c| c.footer.clone())
            .ok_or(Error::not_found("segment"))
    }

    fn list(&self) -> Result<Vec<SegmentInfo>> {
        self.st()?
            .d
            .segments
            .iter()
            .map(|(&raw, c)| {
                Ok(SegmentInfo {
                    seg: ObjId::from_raw(raw)?,
                    size_bytes: c.size,
                })
            })
            .collect()
    }

    fn delete(&self, seg: SegId) -> Result<u64> {
        self.0.faults.point("segment.delete.before")?;
        let freed = lock(&self.0.state)
            .d
            .segments
            .remove(&seg.raw())
            .map_or(0, |c| c.size);
        self.0.faults.point("segment.delete.after")?;
        Ok(freed)
    }
}

// ------------------------------------------------------------------ packs

struct MemPackWriter {
    b: Builder,
}

impl PackWrite for MemPackWriter {
    fn id(&self) -> PackId {
        self.b.id
    }

    fn append_node(&mut self, hash: &[u8; 32], extent: &[u8]) -> Result<NodeLoc> {
        let id = ChunkId(*hash);
        let (off, _) = self.b.append(&id, None, extent, MAX_NODE_ULEN)?;
        Ok(NodeLoc {
            pack: self.b.id,
            off,
            len: u32::try_from(extent.len()).map_err(|_| Error::invalid("node too long"))?,
        })
    }

    fn len(&self) -> u64 {
        self.b.buf.len() as u64
    }

    fn seal(mut self: Box<Self>) -> Result<SealedPack> {
        let (size_bytes, nodes) = self.b.seal(None, "pack.seal.before", "pack.seal.after")?;
        Ok(SealedPack {
            pack: self.b.id,
            size_bytes,
            nodes,
        })
    }
}

impl PackRead for MemStore {
    fn read_node(&self, loc: NodeLoc, out: &mut Vec<u8>) -> Result<()> {
        let st = self.st()?;
        let c =
            st.d.packs
                .get(&loc.pack.raw())
                .ok_or(Error::not_found("pack"))?;
        let start = loc.off as usize;
        let bytes = start
            .checked_add(loc.len as usize)
            .and_then(|end| c.extents.get(start..end))
            .ok_or(Error::Corrupt {
                what: "pack",
                detail: "range outside the pack",
            })?;
        out.extend_from_slice(bytes);
        Ok(())
    }

    fn pack_footer(&self, pack: PackId) -> Result<FooterBody> {
        self.st()?
            .d
            .packs
            .get(&pack.raw())
            .map(|c| c.footer.clone())
            .ok_or(Error::not_found("pack"))
    }

    fn list_packs(&self) -> Result<Vec<PackInfo>> {
        self.st()?
            .d
            .packs
            .iter()
            .map(|(&raw, c)| {
                Ok(PackInfo {
                    pack: ObjId::from_raw(raw)?,
                    size_bytes: c.size,
                })
            })
            .collect()
    }
}

impl PackStore for MemStore {
    fn open_pack(&self) -> Result<Box<dyn PackWrite>> {
        Ok(Box::new(MemPackWriter {
            b: Builder::new(Arc::clone(&self.0), ContainerKind::MapPack)?,
        }))
    }

    fn delete_pack(&self, pack: PackId) -> Result<u64> {
        self.0.faults.point("pack.delete.before")?;
        let freed = lock(&self.0.state)
            .d
            .packs
            .remove(&pack.raw())
            .map_or(0, |c| c.size);
        self.0.faults.point("pack.delete.after")?;
        Ok(freed)
    }
}

// ---------------------------------------------------------------- commits

impl CommitStore for MemStore {
    fn put(&self, seq: CommitSeq, object: &[u8]) -> Result<()> {
        self.0.faults.point("commit.put.before")?;
        {
            let mut st = lock(&self.0.state);
            if st.d.commits.contains_key(&seq.0) {
                return Err(Error::AlreadyExists { what: "commit" });
            }
            Self::charge(&st, object.len() as u64)?;
            st.d.commits.insert(seq.0, object.to_vec());
        }
        self.0.faults.point("commit.put.after")
    }

    fn get(&self, seq: CommitSeq) -> Result<Vec<u8>> {
        self.st()?
            .d
            .commits
            .get(&seq.0)
            .cloned()
            .ok_or(Error::not_found("commit"))
    }

    fn list(&self) -> Result<Vec<CommitInfo>> {
        Ok(self
            .st()?
            .d
            .commits
            .iter()
            .map(|(&k, v)| CommitInfo {
                seq: CommitSeq(k),
                size_bytes: v.len() as u64,
            })
            .collect())
    }

    fn tip(&self) -> Result<Option<CommitSeq>> {
        Ok(self
            .st()?
            .d
            .commits
            .keys()
            .next_back()
            .map(|&k| CommitSeq(k)))
    }

    fn delete(&self, seq: CommitSeq, chain_floor: CommitSeq) -> Result<u64> {
        self.0.faults.point("commit.delete.before")?;
        let freed = {
            let mut st = lock(&self.0.state);
            if seq >= chain_floor || st.d.commits.keys().next_back() == Some(&seq.0) {
                return Err(Error::Refused {
                    why: "commit is the tip or at or above chain_floor",
                });
            }
            st.d.commits.remove(&seq.0).map_or(0, |v| v.len() as u64)
        };
        self.0.faults.point("commit.delete.after")?;
        Ok(freed)
    }

    fn set_aside(&self, seq: CommitSeq) -> Result<()> {
        self.0.faults.point("commit.set_aside.before")?;
        {
            let mut st = lock(&self.0.state);
            match st.d.commits.keys().next_back().copied() {
                None => return Err(Error::not_found("commit")),
                Some(t) if t != seq.0 => {
                    return Err(Error::Refused {
                        why: "only the tip can be set aside",
                    })
                }
                Some(t) => {
                    let bytes = st.d.commits.remove(&t).unwrap_or_default();
                    st.d.damaged.insert(t, bytes);
                }
            }
        }
        self.0.faults.point("commit.set_aside.after")
    }
}

// ------------------------------------------------------------- checkpoint

impl CheckpointStore for MemStore {
    fn load(&self) -> Result<Option<CheckpointSlot>> {
        // A slot written for another volume is ignored, like a torn one.
        Ok(self
            .st()?
            .d
            .checkpoint
            .iter()
            .flatten()
            .max_by_key(|s| s.slot_seq)
            .filter(|s| s.volume_uuid == self.0.cfg.uuid)
            .cloned())
    }

    fn store(&self, slot: &CheckpointSlot) -> Result<()> {
        if slot.volume_uuid != self.0.cfg.uuid {
            return Err(Error::invalid("checkpoint is for another volume"));
        }
        self.0.faults.point("checkpoint.store.before")?;
        {
            let mut st = lock(&self.0.state);
            let newest = st.d.checkpoint.iter().flatten().map(|s| s.slot_seq).max();
            let older = match (&st.d.checkpoint[0], &st.d.checkpoint[1]) {
                (None, _) => 0,
                (_, None) => 1,
                (Some(a), Some(b)) => usize::from(a.slot_seq > b.slot_seq),
            };
            let mut s = slot.clone();
            s.slot_seq = newest.unwrap_or(0) + 1;
            st.d.checkpoint[older] = Some(s);
        }
        self.0.faults.point("checkpoint.store.after")
    }

    fn zero(&self) -> Result<()> {
        self.st()?.d.checkpoint = [None, None];
        Ok(())
    }
}

/// A checkpoint slot plus the verdict of the tail guard (as `lunix-store`'s
/// `CheckpointHint`).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct CheckpointHint {
    /// The newest valid slot; its `lane_tail_ordinals` are all zero when
    /// `tails_usable` is false ("skip no journal file").
    pub slot: CheckpointSlot,
    /// True when the slot was written for a commit at or below the tip.
    pub tails_usable: bool,
}

/// The tail guard: lane tails are usable only if the checkpoint was written
/// for a commit at or below the tip.
pub fn tails_usable(slot: &CheckpointSlot, tip: Option<CommitSeq>) -> bool {
    tip.is_some_and(|t| slot.last_commit_seq <= t.0)
}

impl MemStore {
    /// The checkpoint with the tail guard applied against the commit tip. The
    /// plain [`CheckpointStore::load`] returns the slot as written. An inherent
    /// method on the real `LocalVolume` too, not part of the traits.
    ///
    /// # Errors
    /// `Io` when the store is dead.
    pub fn load_checkpoint_hint(&self) -> Result<Option<CheckpointHint>> {
        let Some(mut slot) = CheckpointStore::load(self)? else {
            return Ok(None);
        };
        let usable = tails_usable(&slot, CommitStore::tip(self)?);
        if !usable {
            slot.lane_tail_ordinals.iter_mut().for_each(|o| *o = 0);
        }
        Ok(Some(CheckpointHint {
            slot,
            tails_usable: usable,
        }))
    }
}

// --------------------------------------------------------------- GC state

impl PinSource for MemStore {
    fn roots(&self) -> Result<Vec<PinRoot>> {
        let st = self.st()?;
        if st.pins_fail {
            return Err(Error::Corrupt {
                what: "pin",
                detail: "undecodable pin",
            });
        }
        Ok(st.pin_roots.clone())
    }
}

impl GcStateStore for MemStore {
    fn load_condemned(&self) -> Result<Option<Condemned>> {
        Ok(self.st()?.d.condemned.clone())
    }

    fn store_condemned(&self, list: &Condemned) -> Result<()> {
        self.0.faults.point("condemned.store.before")?;
        lock(&self.0.state).d.condemned = Some(list.clone());
        self.0.faults.point("condemned.store.after")
    }

    fn put_evidence(&self, pass: u64, lines: &[u8]) -> Result<()> {
        if pass == 0 {
            return Err(Error::invalid("gc pass numbers start at 1"));
        }
        self.0.faults.point("evidence.put.before")?;
        {
            let mut st = lock(&self.0.state);
            if st.d.evidence.contains_key(&pass) {
                return Err(Error::AlreadyExists {
                    what: "gc evidence",
                });
            }
            Self::charge(&st, lines.len() as u64)?;
            st.d.evidence.insert(pass, lines.to_vec());
        }
        self.0.faults.point("evidence.put.after")
    }

    fn evidence_passes(&self) -> Result<Vec<u64>> {
        Ok(self.st()?.d.evidence.keys().copied().collect())
    }
}

// ----------------------------------------------------------- volume dir

impl VolumeDir for MemStore {
    fn uuid(&self) -> VolumeUuid {
        self.0.cfg.uuid
    }

    fn dir(&self) -> &Path {
        // The path is a label (`/mem/<uuid>`); nothing exists on disk.
        Path::new("/mem")
    }

    fn volume_bytes(&self) -> Result<Vec<u8>> {
        self.0.faults.alive()?;
        Ok(self.0.cfg.volume_bytes.clone())
    }

    fn volume_json(&self) -> Result<VolumeJson> {
        Ok(self.st()?.d.volume_json.clone())
    }

    fn store_volume_json(&self, json: &VolumeJson) -> Result<()> {
        self.0.faults.point("volume_json.store.before")?;
        lock(&self.0.state).d.volume_json = json.clone();
        self.0.faults.point("volume_json.store.after")
    }

    fn device_record(&self) -> Result<Option<DeviceRecord>> {
        Ok(self.st()?.d.device.clone())
    }

    fn store_device_record(&self, rec: &DeviceRecord) -> Result<()> {
        self.st()?.d.device = Some(rec.clone());
        Ok(())
    }

    fn clear_device_record(&self) -> Result<()> {
        self.st()?.d.device = None;
        Ok(())
    }

    fn local_pins(&self) -> Result<Option<LocalPins>> {
        Ok(self.st()?.d.local_pins.clone())
    }

    fn store_local_pins(&self, pins: &LocalPins) -> Result<()> {
        self.st()?.d.local_pins = Some(pins.clone());
        Ok(())
    }

    fn ids(&self) -> Arc<dyn IdAllocator> {
        self.mem_ids()
    }

    fn leftovers(&self) -> Result<Vec<Leftover>> {
        // A writer open in this process is not a leftover: its file is in use.
        let st = self.st()?;
        let mut v: Vec<Leftover> =
            st.d.leftovers
                .iter()
                .map(|l| Leftover {
                    name: l.name.clone(),
                    size_bytes: l.size,
                })
                .collect();
        v.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(v)
    }

    fn remove_leftovers(&self) -> Result<u32> {
        let mut st = self.st()?;
        let n = st.d.leftovers.len() as u32;
        st.d.leftovers.clear();
        Ok(n)
    }

    fn sweep_orphans(&self, req: &SweepRequest<'_>) -> Result<SweepReport> {
        let bad = |id: &ObjId| id.raw() > req.high_water_mark;
        if req.named_segments.iter().any(bad) || req.named_packs.iter().any(bad) {
            return Err(Error::invalid(
                "a named id is above high_water_mark: the mark was computed wrongly",
            ));
        }
        self.0.faults.point("sweep.before")?;
        let mut st = lock(&self.0.state);
        let highest_id_before =
            st.d.segments
                .keys()
                .chain(st.d.packs.keys())
                .copied()
                .chain(st.d.leftovers.iter().filter_map(|l| l.id))
                .max()
                .unwrap_or(0);
        // Ids on disk count for the allocator before anything is deleted.
        self.0.ids.observe(highest_id_before);
        let mut report = SweepReport {
            highest_id_before,
            ..SweepReport::default()
        };
        report.deleted_temp = st.d.leftovers.len() as u32;
        st.d.leftovers.clear();
        let mark = req.high_water_mark;
        let named_segs: &HashSet<SegId> = req.named_segments;
        let named_packs: &HashSet<PackId> = req.named_packs;
        let mut gone_segs = Vec::new();
        for &raw in st.d.segments.keys() {
            let id = ObjId::from_raw(raw)?;
            if named_segs.contains(&id) {
                continue;
            }
            if raw > mark {
                gone_segs.push(id);
            } else {
                report.kept_unreferenced.push(id);
            }
        }
        let mut gone_packs = Vec::new();
        for &raw in st.d.packs.keys() {
            let id = ObjId::from_raw(raw)?;
            if named_packs.contains(&id) {
                continue;
            }
            if raw > mark {
                gone_packs.push(id);
            } else {
                report.kept_unreferenced.push(id);
            }
        }
        for id in &gone_segs {
            st.d.segments.remove(&id.raw());
        }
        for id in &gone_packs {
            st.d.packs.remove(&id.raw());
        }
        report.deleted_segments = gone_segs;
        report.deleted_packs = gone_packs;
        report.kept_unreferenced.sort_unstable();
        Ok(report)
    }
}
