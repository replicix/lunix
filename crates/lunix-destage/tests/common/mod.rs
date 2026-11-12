//! A destage rig over the testkit fakes and a flat model of the device.
#![allow(dead_code)]

use std::sync::{Arc, Mutex};

use lunix_destage::{
    destage_cut, DestageConfig, DestageDeps, DestageError, DestageOutcome, DestageParams,
};
use lunix_format::journal::RecordKind;
use lunix_format::node::DataEntry;
use lunix_format::volume::CodecPolicy;
use lunix_format::{ChunkId, CryptoMode, ExtentKind, Hint, IdLen, TimeMs, WriteSeq};
use lunix_iface::clock::ManualClock;
use lunix_iface::crypto::{ExtentCodec, OpenExpect, OpenOutput};
use lunix_iface::index::ChunkIndex;
use lunix_iface::journal::{Journal, JournalLane, LaneId, SlotOutcome};
use lunix_iface::map::{entry_hint, BlockMap, ChunkNo};
use lunix_iface::overlay::{BlockRange, Overlay, OverlayCut};
use lunix_iface::store::{PackRead, SegmentStore};
use lunix_iface::wake::WakeFd;
use lunix_testkit::journal::MIN_FILE_BYTES;
use lunix_testkit::{
    FlatMap, FlatOverlay, MemIndex, MemJournal, MemJournalConfig, MemSpace, MemSpaceConfig,
    MemStore, MemStoreConfig, PlainCodec,
};

pub const BLOCK: usize = 4096;

/// Deterministic pseudo-random bytes.
pub fn random_bytes(seed: u64, len: usize) -> Vec<u8> {
    let mut x = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 24) as u8
        })
        .collect()
}

/// Text-like data: words drawn Zipf-ish from a 512-word vocabulary of random
/// lowercase letters, so that it compresses like prose rather than like a
/// repeated phrase.
pub fn text_bytes(seed: u64, len: usize) -> Vec<u8> {
    static VOCAB: std::sync::OnceLock<Vec<Vec<u8>>> = std::sync::OnceLock::new();
    let vocab = VOCAB.get_or_init(|| {
        let r = random_bytes(0x7e47, 512 * 12);
        (0..512)
            .map(|i| {
                let n = 3 + usize::from(r[i * 12]) % 8;
                let mut w: Vec<u8> = r[i * 12 + 1..i * 12 + 1 + n]
                    .iter()
                    .map(|b| b'a' + b % 26)
                    .collect();
                w.push(b' ');
                w
            })
            .collect()
    });
    let r = random_bytes(seed ^ 0x51ed, len / 2 + 16);
    let mut out = Vec::with_capacity(len + 16);
    let mut i = 0;
    while out.len() < len {
        let (a, b) = (
            usize::from(r[i % r.len()]),
            usize::from(r[(i + 1) % r.len()]),
        );
        // min of two draws favours the low indices.
        let w = (a * 2 + (b & 1)).min(b * 2 + (a & 1)) % 512;
        out.extend_from_slice(&vocab[w]);
        i += 2;
    }
    out.truncate(len);
    out
}

/// The extent codecs of the four volume flavours: the testkit's integrity and
/// plain codecs, and the real ones for `integrity` and `encrypted`.
pub fn codecs(chunk_size: u32) -> Vec<(&'static str, Arc<dyn ExtentCodec>)> {
    use lunix_crypto::{Root, VolumeCrypto};
    use lunix_format::keyring::RootKind;
    use lunix_format::volume::VolumeDescriptor;
    use lunix_format::{DekGen, FamilyId, Profile, VolumeUuid};
    let desc = |mode| VolumeDescriptor {
        crypto_mode: mode,
        profile: Profile::Default,
        block_size: 4096,
        chunk_size,
        map_fanout: 512,
        id_len: IdLen::ThirtyTwo,
        codec_policy: CodecPolicy::Auto,
        codec_level: 1,
        volume_uuid: VolumeUuid([7; 16]),
        family_id: FamilyId([9; 16]),
        kmk_fingerprint: [0; 16],
        created_at: TimeMs(0),
        lineage: Vec::new(),
        mac: [0; 32],
    };
    let real = |mode, root: Option<Root>| -> Arc<dyn ExtentCodec> {
        let v = VolumeCrypto::new(&desc(mode), root, DekGen(0)).expect("keys");
        Arc::clone(v.extents())
    };
    vec![
        (
            "testkit integrity",
            Arc::new(PlainCodec::new()) as Arc<dyn ExtentCodec>,
        ),
        ("testkit plain", Arc::new(PlainCodec::unkeyed())),
        (
            "real integrity",
            real(
                CryptoMode::Integrity,
                Some(Root::generate(RootKind::Ik).expect("root")),
            ),
        ),
        (
            "real encrypted",
            real(
                CryptoMode::Encrypted,
                Some(Root::generate(RootKind::Kmk).expect("root")),
            ),
        ),
        ("real plain", real(CryptoMode::Plain, None)),
    ]
}

pub struct RigConfig {
    pub chunk_size: u32,
    pub chunks: u64,
    pub codec: Arc<dyn ExtentCodec>,
    pub policy: CodecPolicy,
    pub level: i8,
    pub workers: usize,
    pub segment_target: u64,
    pub id_len: IdLen,
}

impl RigConfig {
    pub fn new(chunk_size: u32, chunks: u64) -> Self {
        Self {
            chunk_size,
            chunks,
            codec: Arc::new(PlainCodec::new()),
            policy: CodecPolicy::Auto,
            level: 1,
            workers: 3,
            segment_target: 16 << 20,
            id_len: IdLen::ThirtyTwo,
        }
    }
}

/// (write_seq, byte offset, data or None for a hole, length).
pub type LogEntry = (u64, u64, Option<Vec<u8>>, u64);

pub struct Rig {
    pub cfg_chunk_size: u32,
    pub chunks: u64,
    pub codec: Arc<dyn ExtentCodec>,
    pub store: MemStore,
    pub index: Mutex<Arc<MemIndex>>,
    pub map: FlatMap,
    pub journal: MemJournal,
    lane: Mutex<Box<dyn JournalLane>>,
    pub overlay: FlatOverlay,
    pub space: Arc<MemSpace>,
    pub params: DestageParams,
    pub config: DestageConfig,
    /// Every record: (write_seq, byte offset, bytes or None for a hole).
    pub log: Mutex<Vec<LogEntry>>,
}

impl Rig {
    pub fn new(c: RigConfig) -> Self {
        let mode = c.codec.mode();
        let store = MemStore::with_config(MemStoreConfig {
            segment_target: c.segment_target,
            mode,
            ..MemStoreConfig::default()
        });
        let index = Arc::new(MemIndex::new(store.mem_ids()));
        let map = FlatMap::new(
            Arc::clone(&c.codec),
            Arc::new(store.clone()) as Arc<dyn PackRead>,
            c.chunks,
            c.id_len,
        );
        let journal = MemJournal::new(MemJournalConfig {
            lanes: 1,
            file_bytes: (8 << 20).max(MIN_FILE_BYTES),
            max_files: 256,
            ..MemJournalConfig::default()
        });
        journal.recover(None, WriteSeq(0)).expect("recover");
        let lane = journal
            .open_lane(LaneId(0), WakeFd::new().expect("eventfd"))
            .expect("lane");
        let space = MemSpace::new(
            MemSpaceConfig::new(1 << 40),
            Arc::new(ManualClock::new(TimeMs(1))),
        );
        let config = DestageConfig {
            workers: c.workers,
            ..DestageConfig::default()
        };
        Self {
            cfg_chunk_size: c.chunk_size,
            chunks: c.chunks,
            codec: c.codec,
            store,
            index: Mutex::new(index),
            map,
            journal,
            lane: Mutex::new(lane),
            overlay: FlatOverlay::new(u64::from(c.chunk_size) / BLOCK as u64),
            space,
            params: DestageParams {
                chunk_size: c.chunk_size,
                id_len: c.id_len,
                policy: c.policy,
                level: c.level,
            },
            config,
            log: Mutex::new(Vec::new()),
        }
    }

    pub fn chunk_bytes(&self) -> usize {
        self.cfg_chunk_size as usize
    }

    pub fn size(&self) -> usize {
        self.chunk_bytes() * self.chunks as usize
    }

    pub fn index(&self) -> Arc<MemIndex> {
        Arc::clone(&self.index.lock().expect("lock"))
    }

    /// Writes `data` at byte offset `off` (4 KiB aligned) through the journal
    /// and the overlay, in records of at most 128 blocks.
    pub fn write(&self, off: usize, data: &[u8]) {
        assert!(off.is_multiple_of(BLOCK) && data.len().is_multiple_of(BLOCK) && !data.is_empty());
        let mut lane = self.lane.lock().expect("lane");
        let mut done = 0;
        while done < data.len() {
            let n = (data.len() - done).min(128 * BLOCK);
            let blocks = (n / BLOCK) as u32;
            let SlotOutcome::Reserved(slot) = lane
                .try_reserve(RecordKind::Write, blocks)
                .expect("reserve")
            else {
                panic!("journal full in a test");
            };
            let lba = ((off + done) / BLOCK) as u64;
            let ap = lane
                .append(slot, lba, &data[done..done + n])
                .expect("append");
            self.overlay.insert_write(
                BlockRange {
                    lba,
                    blocks: u64::from(blocks),
                },
                ap.loc,
                ap.write_seq,
            );
            self.log.lock().expect("log").push((
                ap.write_seq.0,
                (off + done) as u64,
                Some(data[done..done + n].to_vec()),
                n as u64,
            ));
            done += n;
        }
    }

    /// DISCARD / WRITE_ZEROES of `len` bytes at `off`.
    pub fn discard(&self, off: usize, len: usize) {
        assert!(off.is_multiple_of(BLOCK) && len.is_multiple_of(BLOCK) && len > 0);
        let mut lane = self.lane.lock().expect("lane");
        let mut done = 0;
        while done < len {
            let n = (len - done).min(128 * BLOCK);
            let blocks = (n / BLOCK) as u32;
            let SlotOutcome::Reserved(slot) =
                lane.try_reserve(RecordKind::Hole, blocks).expect("reserve")
            else {
                panic!("journal full in a test");
            };
            let lba = ((off + done) / BLOCK) as u64;
            let ap = lane.append(slot, lba, &[]).expect("append");
            self.overlay.insert_hole(
                BlockRange {
                    lba,
                    blocks: u64::from(blocks),
                },
                ap.write_seq,
            );
            self.log.lock().expect("log").push((
                ap.write_seq.0,
                (off + done) as u64,
                None,
                n as u64,
            ));
            done += n;
        }
    }

    pub fn last_seq(&self) -> WriteSeq {
        self.journal.last_issued()
    }

    /// The device as of write_seq `c`, from the log.
    pub fn model_at(&self, c: WriteSeq) -> Vec<u8> {
        let mut m = vec![0u8; self.size()];
        for (seq, off, data, len) in self.log.lock().expect("log").iter() {
            if *seq > c.0 {
                continue;
            }
            let off = *off as usize;
            match data {
                Some(d) => m[off..off + d.len()].copy_from_slice(d),
                None => m[off..off + *len as usize].fill(0),
            }
        }
        m
    }

    pub fn deps<'a>(&'a self, index: &'a MemIndex) -> DestageDeps<'a> {
        DestageDeps {
            journal: &self.journal,
            map: &self.map,
            index,
            segments: &self.store,
            codec: &*self.codec,
            space: &*self.space,
            params: self.params,
            config: self.config,
        }
    }

    pub fn cut(&self, c: WriteSeq) -> OverlayCut {
        self.overlay.cut(c)
    }

    /// Destage without finishing (no seal, no index insert, no release).
    pub fn destage(&self, cut: &OverlayCut) -> Result<DestageOutcome, DestageError> {
        let index = self.index();
        destage_cut(cut, &self.deps(&index))
    }

    /// The caller's steps after a successful destage: seal (C4), index inserts,
    /// release (C9).
    pub fn finish(&self, cut: &OverlayCut, mut out: DestageOutcome) -> DestageOutcome {
        out.seal_open().expect("seal");
        out.insert_into_index(&*self.index()).expect("index");
        self.overlay.release(cut);
        out
    }

    pub fn run_cut(&self, c: WriteSeq) -> DestageOutcome {
        let cut = self.cut(c);
        let out = self.destage(&cut).expect("destage");
        self.finish(&cut, out)
    }

    /// Reads chunk `ci` back through the map and the store.
    pub fn read_chunk(&self, ci: u64) -> Vec<u8> {
        match self.map.get(ChunkNo(ci)).expect("map get") {
            None => vec![0; self.chunk_bytes()],
            Some(e) => self.read_entry(&e),
        }
    }

    pub fn read_entry(&self, e: &DataEntry) -> Vec<u8> {
        let mut raw = Vec::new();
        self.store
            .read_extent(&entry_hint(e), &mut raw)
            .expect("read extent");
        let expect = OpenExpect {
            chunk_size: self.cfg_chunk_size,
            chunk_id: None,
            id_prefix: Some(&e.prefix),
            verify_id: true,
            output: OpenOutput::Plaintext,
        };
        let mut out = Vec::new();
        self.codec
            .open(ExtentKind::Data, &raw, &expect, &mut out)
            .expect("open");
        out
    }

    /// Every chunk equals `model`, and a chunk is a map hole exactly when it is all zero.
    pub fn assert_equals(&self, model: &[u8]) {
        let cs = self.chunk_bytes();
        for ci in 0..self.chunks {
            let want = &model[ci as usize * cs..(ci as usize + 1) * cs];
            let entry = self.map.get(ChunkNo(ci)).expect("get");
            let zero = want.iter().all(|&b| b == 0);
            assert_eq!(entry.is_none(), zero, "chunk {ci}: hole iff zero");
            if let Some(e) = entry {
                assert_eq!(self.read_entry(&e), want, "chunk {ci} content");
            }
        }
    }

    /// Persists the index and reopens it from its image: every entry is then
    /// `Hinted`, as after a restart.
    pub fn reopen_index_hinted(&self) {
        let segs = self.store.list().expect("list");
        let top = segs.last().map(|s| s.seg);
        let cur = self.index();
        if let Some(top) = top {
            cur.persist(top).expect("persist");
        }
        let fresh = MemIndex::from_image(self.store.mem_ids(), &cur.image());
        *self.index.lock().expect("lock") = Arc::new(fresh);
    }

    pub fn id_of(&self, data: &[u8]) -> ChunkId {
        self.codec.chunk_id(ExtentKind::Data, data)
    }

    pub fn segment_count(&self) -> usize {
        self.store.list().expect("list").len()
    }

    /// Extents in all sealed segments.
    pub fn extent_count(&self) -> usize {
        self.store
            .list()
            .expect("list")
            .iter()
            .map(|s| self.store.footer(s.seg).expect("footer").entries.len())
            .sum()
    }
}

pub fn hint_of(e: &DataEntry) -> Hint {
    entry_hint(e)
}

pub fn mode_of(codec: &dyn ExtentCodec) -> CryptoMode {
    codec.mode()
}

/// A segment store that can damage or count reads, delegating to a `MemStore`.
pub struct TamperStore {
    pub inner: MemStore,
    /// Flip one bit at this offset (from the start of the extent bytes) in
    /// every read of this segment.
    pub flip: Mutex<Option<(lunix_format::SegId, usize)>>,
    pub reads: std::sync::atomic::AtomicU64,
    /// Fail every read with a host `EIO`.
    pub eio: std::sync::atomic::AtomicBool,
}

impl TamperStore {
    pub fn new(inner: MemStore) -> Self {
        Self {
            inner,
            flip: Mutex::new(None),
            reads: std::sync::atomic::AtomicU64::new(0),
            eio: std::sync::atomic::AtomicBool::new(false),
        }
    }

    pub fn reads(&self) -> u64 {
        self.reads.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl SegmentStore for TamperStore {
    fn open_segment(&self) -> lunix_iface::Result<Box<dyn lunix_iface::store::SegmentWriter>> {
        self.inner.open_segment()
    }

    fn read_extent(&self, hint: &Hint, out: &mut Vec<u8>) -> lunix_iface::Result<()> {
        self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if self.eio.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(lunix_iface::Error::Io(std::io::Error::from_raw_os_error(5)));
        }
        let before = out.len();
        self.inner.read_extent(hint, out)?;
        if let Some((seg, at)) = *self.flip.lock().expect("flip") {
            if seg == hint.seg && before + at < out.len() {
                out[before + at] ^= 1;
            }
        }
        Ok(())
    }

    fn locate(&self, hint: &Hint) -> lunix_iface::Result<lunix_iface::store::ExtentLocation> {
        self.inner.locate(hint)
    }

    fn footer(
        &self,
        seg: lunix_format::SegId,
    ) -> lunix_iface::Result<lunix_format::segment::FooterBody> {
        self.inner.footer(seg)
    }

    fn list(&self) -> lunix_iface::Result<Vec<lunix_iface::store::SegmentInfo>> {
        self.inner.list()
    }

    fn delete(&self, seg: lunix_format::SegId) -> lunix_iface::Result<u64> {
        self.inner.delete(seg)
    }
}

impl Rig {
    /// Like `destage` with another index.
    pub fn destage_with_index(
        &self,
        cut: &OverlayCut,
        index: &MemIndex,
    ) -> Result<DestageOutcome, DestageError> {
        destage_cut(cut, &self.deps(index))
    }

    /// Like `destage` but reading extents through `store`.
    pub fn destage_via(
        &self,
        cut: &OverlayCut,
        store: &dyn SegmentStore,
    ) -> Result<DestageOutcome, DestageError> {
        let index = self.index();
        let mut deps = self.deps(&index);
        deps.segments = store;
        destage_cut(cut, &deps)
    }

    /// Map content as `(chunk, entry)`.
    pub fn snapshot(&self) -> std::collections::BTreeMap<u64, DataEntry> {
        self.map.snapshot()
    }

    /// Writes a whole chunk `ci` with `data` (chunk-sized).
    pub fn write_chunk(&self, ci: u64, data: &[u8]) {
        assert_eq!(data.len(), self.chunk_bytes());
        self.write(ci as usize * self.chunk_bytes(), data);
    }
}
