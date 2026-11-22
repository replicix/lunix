//! The engine's test bench over the `lunix-testkit` fakes: a volume made of
//! crash images (store, index, journal) that opens into a running engine,
//! wrappers that enforce the queue-thread rule and the durable order, and a
//! request driver.

#![allow(dead_code)]

use std::cell::Cell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use lunix_engine::{
    Engine, EngineConfig, EngineDeps, EngineHooks, MapOpener, OpenError, RollbackFloorStore,
    SimpleDestager,
};
use lunix_format::commit::{Commit, Full, MapRoot};
use lunix_format::local::{CheckpointSlot, RollbackFloor};
use lunix_format::node::DataEntry;
use lunix_format::segment::FooterBody;
use lunix_format::{ChunkId, CommitSeq, Hint, IdLen, PackId, SegId, TimeMs, WriteSeq};
use lunix_iface::block::{
    BlockEngine, IoOp, IoRequest, IoStatus, QueueConfig, QueueHandler, QueueId, Tag,
};
use lunix_iface::clock::{Clock, ManualClock};
use lunix_iface::crypto::{CommitCodec, ExtentCodec};
use lunix_iface::index::{CatchUpReport, ChunkIndex, DedupHit, IndexedChunk, RunRef};
use lunix_iface::journal::{
    FlushDone, Journal, JournalFill, JournalLane, JournalLoc, JournalTail, LaneId, ReclaimReport,
    Recovered,
};
use lunix_iface::map::{BlockMap, Cached, ChunkNo, MapTxn, NodeHash, NodeRef, SealResult};
use lunix_iface::store::{
    CommitInfo, CommitStore, ExtentLocation, PackInfo, PackRead, PackStore, PackWrite,
    SegmentFooters, SegmentInfo, SegmentStore, SegmentWriter,
};
use lunix_iface::wake::WakeFd;
use lunix_iface::Result;
use lunix_testkit::{
    CrashModel, FlatMap, FlatOverlay, IndexImage, JournalImage, JournalMode, MemIndex, MemJournal,
    MemJournalConfig, MemSpace, MemSpaceConfig, MemStore, MemStoreConfig, PlainCodec, StoreImage,
    TestCompleter,
};

pub const BLOCK: u64 = 4096;

pub fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

// ------------------------------------------------------------- hooks

thread_local! {
    static ON_QUEUE: Cell<bool> = const { Cell::new(false) };
}

/// Panics when called on a thread inside a queue-handler entry point.
pub fn not_on_queue(what: &str) {
    if ON_QUEUE.with(Cell::get) {
        panic!("blocking call {what} from a queue-thread entry point");
    }
}

#[derive(Default)]
pub struct TestHooks {
    /// The crash point to stop at and the hit number (from 1).
    pub target: Mutex<Option<(String, u64)>>,
    pub hits: Mutex<BTreeMap<&'static str, u64>>,
    pub crashed_at: Mutex<Option<&'static str>>,
    pub seqs: Mutex<Vec<(QueueId, Tag, WriteSeq)>>,
    /// A point to park at, once: the engine thread sends on `reached`, then
    /// waits for `release` (see `park_at`).
    pub parked: Mutex<Option<Park>>,
    /// A point at which the manual clock moves, and by how much, every time
    /// the engine passes it (a cut that takes time, for the bound of a cut).
    pub advance: Mutex<Vec<(&'static str, Duration)>>,
    /// The clock `advance` moves.
    pub clock: Mutex<Option<Arc<ManualClock>>>,
}

/// The two ends of a parked crash point, the engine side.
pub struct Park {
    point: &'static str,
    reached: std::sync::mpsc::Sender<()>,
    release: std::sync::mpsc::Receiver<()>,
}

/// The test side of a parked crash point.
pub struct ParkHandle {
    pub reached: std::sync::mpsc::Receiver<()>,
    pub release: std::sync::mpsc::Sender<()>,
}

impl TestHooks {
    pub fn arm(&self, point: &str, nth: u64) {
        *lock(&self.target) = Some((point.to_owned(), nth.max(1)));
        *lock(&self.crashed_at) = None;
    }

    /// Parks the engine thread the next time it passes `point` (inside
    /// whatever lock it holds there), until the handle's `release` fires.
    pub fn park_at(&self, point: &'static str) -> ParkHandle {
        let (reached, reached_rx) = std::sync::mpsc::channel();
        let (release_tx, release) = std::sync::mpsc::channel();
        *lock(&self.parked) = Some(Park {
            point,
            reached,
            release,
        });
        ParkHandle {
            reached: reached_rx,
            release: release_tx,
        }
    }

    pub fn disarm(&self) {
        *lock(&self.target) = None;
    }

    pub fn crashed(&self) -> Option<&'static str> {
        *lock(&self.crashed_at)
    }

    pub fn take_seqs(&self) -> Vec<(QueueId, Tag, WriteSeq)> {
        std::mem::take(&mut *lock(&self.seqs))
    }
}

impl EngineHooks for TestHooks {
    fn crash_at(&self, point: &'static str) -> bool {
        let by: Duration = lock(&self.advance)
            .iter()
            .filter(|(p, _)| *p == point)
            .map(|a| a.1)
            .sum();
        if let (false, Some(clock)) = (by.is_zero(), lock(&self.clock).as_ref()) {
            clock.advance(by);
        }
        let park = {
            let mut p = lock(&self.parked);
            if p.as_ref().is_some_and(|k| k.point == point) {
                p.take()
            } else {
                None
            }
        };
        if let Some(k) = park {
            let _ = k.reached.send(());
            let _ = k.release.recv();
        }
        *lock(&self.hits).entry(point).or_insert(0) += 1;
        let mut t = lock(&self.target);
        if let Some((name, nth)) = t.as_mut() {
            if name == point {
                *nth -= 1;
                if *nth == 0 {
                    *t = None;
                    *lock(&self.crashed_at) = Some(point);
                    return true;
                }
            }
        }
        false
    }

    fn sequenced(&self, queue: QueueId, tag: Tag, write_seq: WriteSeq) {
        lock(&self.seqs).push((queue, tag, write_seq));
    }

    fn queue_thread(&self, entering: bool) {
        ON_QUEUE.with(|c| c.set(entering));
    }
}

// --------------------------------------------------------- the checks

/// Durable-order checks shared by the wrappers (plan M11 gate, order tests).
#[derive(Default)]
pub struct Checks {
    pub store: Mutex<Option<MemStore>>,
    pub index: Mutex<Option<Arc<MemIndex>>>,
    /// The highest cut handed to `tail_after`.
    pub max_tail_cut: AtomicU64,
    /// Journal reads to fail: at `submit_read` (an `Err`) and at their
    /// completion (an error status).
    pub fail_jread_submit: AtomicU64,
    pub fail_jread_completion: AtomicU64,
    /// Group flushes submitted, and the most outstanding at once.
    pub flush_submitted: AtomicU64,
    pub flush_outstanding: Arc<AtomicU64>,
    pub flush_outstanding_max: AtomicU64,
    pub violations: Mutex<Vec<String>>,
    /// The journal's `fill().used_bytes` is multiplied by this (0 or 1: as
    /// is): rings whose files hold fewer record bytes than their length
    /// (batch padding in the real journal).
    pub fill_scale: AtomicU64,
}

impl Checks {
    fn violate(&self, s: String) {
        lock(&self.violations).push(s);
    }

    /// The cut of the durable tip.
    fn durable_cut(&self) -> Option<u64> {
        let store = lock(&self.store).clone()?;
        let tip = CommitStore::tip(&store).ok()??;
        let bytes = CommitStore::get(&store, tip).ok()?;
        match PlainCodec::new().open_commit(tip, &bytes).ok()? {
            Commit::Full(f) => Some(f.cut_seq.0),
            _ => None,
        }
    }

    /// A commit about to be put names only durable objects (C6 after C4/C5,
    /// D3 for index runs).
    fn check_commit(&self, seq: CommitSeq, bytes: &[u8]) {
        let Some(store) = lock(&self.store).clone() else {
            return;
        };
        let Ok(Commit::Full(f)) = PlainCodec::new().open_commit(seq, bytes) else {
            return;
        };
        let segs: HashSet<SegId> = SegmentStore::list(&store)
            .unwrap_or_default()
            .iter()
            .map(|s| s.seg)
            .collect();
        let packs: HashSet<PackId> = store
            .list_packs()
            .unwrap_or_default()
            .iter()
            .map(|p| p.pack)
            .collect();
        for s in &f.new_segments {
            if !segs.contains(s) {
                self.violate(format!("commit {seq} names segment {s} that is not sealed"));
            }
        }
        for p in f.map_packs.iter().chain(f.map_root.pack.iter()) {
            if !packs.contains(p) {
                self.violate(format!("commit {seq} names pack {p} that is not sealed"));
            }
        }
        if let Some(index) = lock(&self.index).clone() {
            let runs: HashSet<u64> = index
                .runs()
                .unwrap_or_default()
                .iter()
                .map(|r| r.id.raw())
                .collect();
            for r in &f.index_packs {
                if !runs.contains(&r.raw()) {
                    self.violate(format!(
                        "commit {seq} names index run {r} that is not durable"
                    ));
                }
            }
        }
    }
}

/// The rollback floor of `state/pins`, kept across reopens. Fails the test
/// if a floor is ever above the durable tip or lowered.
#[derive(Default)]
pub struct MemFloor {
    pub floor: Mutex<Option<RollbackFloor>>,
    pub checks: Arc<Checks>,
    pub raises: AtomicU64,
}

impl RollbackFloorStore for MemFloor {
    fn load(&self) -> Result<Option<RollbackFloor>> {
        Ok(*lock(&self.floor))
    }

    fn raise(&self, f: &RollbackFloor) -> Result<()> {
        let mut cur = lock(&self.floor);
        if let Some(c) = *cur {
            if f.max_commit_seq < c.max_commit_seq {
                self.checks.violate(format!(
                    "floor lowered from {} to {}",
                    c.max_commit_seq, f.max_commit_seq
                ));
            }
        }
        if let Some(store) = lock(&self.checks.store).clone() {
            match CommitStore::get(&store, f.max_commit_seq) {
                Ok(bytes) => {
                    if PlainCodec::new().object_hash(&bytes) != f.tip_hash {
                        self.checks
                            .violate("floor hash is not the stored commit's".into());
                    }
                }
                Err(_) => self
                    .checks
                    .violate(format!("floor {} above the durable tip", f.max_commit_seq)),
            }
        }
        *cur = Some(*f);
        self.raises.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

// --------------------------------------------------------- wrappers

/// A parked read (`ReadFaults::hold`).
pub struct ReadHold {
    pub hint: Hint,
    pub entered: std::sync::mpsc::Sender<()>,
    pub release: std::sync::mpsc::Receiver<()>,
}

/// Read-path tampering for the R1 gate and a slow store.
#[derive(Default)]
pub struct ReadFaults {
    /// Microseconds every `read_extent` sleeps.
    pub delay_us: AtomicU64,
    /// A read of the key hint returns the bytes stored at the value hint.
    pub swap: Mutex<HashMap<Hint, Hint>>,
    /// A read of these hints returns the bytes with the last byte flipped.
    pub flip: Mutex<HashSet<Hint>>,
    pub reads: AtomicU64,
    /// The next read of the hint signals `entered` and then blocks until
    /// `release` gets a message (once: the entry is taken).
    pub hold: Mutex<Option<ReadHold>>,
    /// Every segment seal moves this manual clock by this much (and sleeps a
    /// few real milliseconds, so a thread that watches sees the steps): a cut
    /// that takes long and keeps sealing.
    pub seal_advance: Mutex<Option<(Arc<ManualClock>, Duration)>>,
}

/// A segment writer whose seal takes manual time (`ReadFaults::seal_advance`).
struct GWriter {
    inner: Box<dyn SegmentWriter>,
    faults: Arc<ReadFaults>,
}

impl SegmentWriter for GWriter {
    fn id(&self) -> SegId {
        self.inner.id()
    }
    fn append_extent(
        &mut self,
        chunk_id: &ChunkId,
        dict_id: Option<u32>,
        extent: &[u8],
    ) -> Result<Hint> {
        self.inner.append_extent(chunk_id, dict_id, extent)
    }
    fn len(&self) -> u64 {
        self.inner.len()
    }
    fn is_full(&self) -> bool {
        self.inner.is_full()
    }
    fn seal(
        self: Box<Self>,
        writes: Option<lunix_iface::journal::WriteSeqRange>,
    ) -> Result<lunix_iface::store::SealedSegment> {
        let Self { inner, faults } = *self;
        let r = inner.seal(writes);
        if let Some((clock, by)) = lock(&faults.seal_advance).clone() {
            clock.advance(by);
            std::thread::sleep(Duration::from_millis(5));
        }
        r
    }
}

/// The store behind every store trait, with the queue-thread rule and the
/// commit-order check.
#[derive(Clone)]
pub struct GStore {
    pub inner: MemStore,
    pub checks: Arc<Checks>,
    pub faults: Arc<ReadFaults>,
}

impl SegmentStore for GStore {
    fn open_segment(&self) -> Result<Box<dyn SegmentWriter>> {
        not_on_queue("open_segment");
        let inner = self.inner.open_segment()?;
        Ok(Box::new(GWriter {
            inner,
            faults: Arc::clone(&self.faults),
        }))
    }
    fn read_extent(&self, hint: &Hint, out: &mut Vec<u8>) -> Result<()> {
        not_on_queue("read_extent");
        self.faults.reads.fetch_add(1, Ordering::SeqCst);
        let held = {
            let mut g = lock(&self.faults.hold);
            if g.as_ref().is_some_and(|h| h.hint == *hint) {
                g.take()
            } else {
                None
            }
        };
        if let Some(h) = held {
            let _ = h.entered.send(());
            let _ = h.release.recv();
        }
        let d = self.faults.delay_us.load(Ordering::SeqCst);
        if d > 0 {
            std::thread::sleep(Duration::from_micros(d));
        }
        let target = lock(&self.faults.swap).get(hint).copied();
        match target {
            Some(t) => {
                // The other extent's bytes, presented under the asked hint.
                let mut b = Vec::new();
                self.inner.read_extent(&t, &mut b)?;
                out.extend_from_slice(&b);
            }
            None => self.inner.read_extent(hint, out)?,
        }
        if lock(&self.faults.flip).contains(hint) {
            if let Some(last) = out.last_mut() {
                *last ^= 1;
            }
        }
        Ok(())
    }
    fn locate(&self, hint: &Hint) -> Result<ExtentLocation> {
        self.inner.locate(hint)
    }
    fn footer(&self, seg: SegId) -> Result<FooterBody> {
        not_on_queue("footer");
        self.inner.footer(seg)
    }
    fn list(&self) -> Result<Vec<SegmentInfo>> {
        not_on_queue("segments.list");
        SegmentStore::list(&self.inner)
    }
    fn delete(&self, seg: SegId) -> Result<u64> {
        not_on_queue("segments.delete");
        SegmentStore::delete(&self.inner, seg)
    }
}

impl PackRead for GStore {
    fn read_node(&self, loc: lunix_format::node::NodeLoc, out: &mut Vec<u8>) -> Result<()> {
        not_on_queue("read_node");
        self.inner.read_node(loc, out)
    }
    fn pack_footer(&self, pack: PackId) -> Result<FooterBody> {
        self.inner.pack_footer(pack)
    }
    fn list_packs(&self) -> Result<Vec<PackInfo>> {
        self.inner.list_packs()
    }
}

impl PackStore for GStore {
    fn open_pack(&self) -> Result<Box<dyn PackWrite>> {
        not_on_queue("open_pack");
        self.inner.open_pack()
    }
    fn delete_pack(&self, pack: PackId) -> Result<u64> {
        self.inner.delete_pack(pack)
    }
}

impl CommitStore for GStore {
    fn put(&self, seq: CommitSeq, object: &[u8]) -> Result<()> {
        not_on_queue("commit put");
        self.checks.check_commit(seq, object);
        CommitStore::put(&self.inner, seq, object)
    }
    fn get(&self, seq: CommitSeq) -> Result<Vec<u8>> {
        CommitStore::get(&self.inner, seq)
    }
    fn list(&self) -> Result<Vec<CommitInfo>> {
        CommitStore::list(&self.inner)
    }
    fn tip(&self) -> Result<Option<CommitSeq>> {
        CommitStore::tip(&self.inner)
    }
    fn delete(&self, seq: CommitSeq, chain_floor: CommitSeq) -> Result<u64> {
        CommitStore::delete(&self.inner, seq, chain_floor)
    }
    fn set_aside(&self, seq: CommitSeq) -> Result<()> {
        self.inner.set_aside(seq)
    }
}

/// The journal with the queue-thread rule on its blocking methods and the
/// check that nothing is reclaimed before the commit covering it is durable.
pub struct GJournal {
    pub inner: Arc<MemJournal>,
    pub checks: Arc<Checks>,
}

impl Journal for GJournal {
    fn lanes(&self) -> u16 {
        self.inner.lanes()
    }
    fn open_lane(&self, lane: LaneId, wake: WakeFd) -> Result<Box<dyn JournalLane>> {
        Ok(Box::new(GLane {
            inner: self.inner.open_lane(lane, wake)?,
            checks: Arc::clone(&self.checks),
        }))
    }
    fn last_issued(&self) -> WriteSeq {
        self.inner.last_issued()
    }
    fn submit_group_flush(&self, upto: WriteSeq, done: FlushDone) -> Result<()> {
        self.checks.flush_submitted.fetch_add(1, Ordering::SeqCst);
        let out = Arc::clone(&self.checks.flush_outstanding);
        let n = out.fetch_add(1, Ordering::SeqCst) + 1;
        self.checks
            .flush_outstanding_max
            .fetch_max(n, Ordering::SeqCst);
        self.inner.submit_group_flush(
            upto,
            Box::new(move |r| {
                out.fetch_sub(1, Ordering::SeqCst);
                done(r);
            }),
        )
    }
    fn read_at(&self, loc: JournalLoc, buf: &mut [u8]) -> Result<()> {
        not_on_queue("journal read_at");
        self.inner.read_at(loc, buf)
    }
    fn recover(&self, hint: Option<&CheckpointSlot>, floor: WriteSeq) -> Result<Recovered> {
        not_on_queue("journal recover");
        self.inner.recover(hint, floor)
    }
    fn tail_after(&self, cut: WriteSeq) -> Result<JournalTail> {
        not_on_queue("journal tail_after");
        self.checks.max_tail_cut.fetch_max(cut.0, Ordering::SeqCst);
        self.inner.tail_after(cut)
    }
    fn reclaim(&self, upto: &JournalTail) -> Result<ReclaimReport> {
        not_on_queue("journal reclaim");
        let want = self.checks.max_tail_cut.load(Ordering::SeqCst);
        if let Some(durable) = self.checks.durable_cut() {
            if durable < want {
                self.checks.violate(format!(
                    "reclaim up to cut {want} while the durable tip covers only {durable}"
                ));
            }
        }
        self.inner.reclaim(upto)
    }
    fn fill(&self) -> JournalFill {
        let mut f = self.inner.fill();
        f.used_bytes = f
            .used_bytes
            .saturating_mul(self.checks.fill_scale.load(Ordering::SeqCst).max(1));
        f
    }
    fn cut_freeing_file(&self) -> Option<WriteSeq> {
        self.inner.cut_freeing_file()
    }
    fn freed_files_by_lane(&self) -> Vec<u64> {
        self.inner.freed_files_by_lane()
    }
}

fn take_one(a: &AtomicU64) -> bool {
    a.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
        .is_ok()
}

/// A lane whose reads can be made to fail.
pub struct GLane {
    inner: Box<dyn JournalLane>,
    checks: Arc<Checks>,
}

impl JournalLane for GLane {
    fn lane(&self) -> LaneId {
        self.inner.lane()
    }
    fn try_reserve(
        &mut self,
        kind: lunix_format::journal::RecordKind,
        blocks: u32,
    ) -> Result<lunix_iface::journal::SlotOutcome> {
        self.inner.try_reserve(kind, blocks)
    }
    fn append(
        &mut self,
        slot: lunix_iface::journal::Slot,
        lba: u64,
        data: &[u8],
    ) -> Result<lunix_iface::journal::Appended> {
        self.inner.append(slot, lba, data)
    }
    fn submit_read(
        &mut self,
        loc: JournalLoc,
        buf: Vec<u8>,
        token: lunix_iface::journal::Token,
    ) -> Result<()> {
        if take_one(&self.checks.fail_jread_submit) {
            return Err(lunix_iface::Error::Io(std::io::Error::other(
                "injected read error",
            )));
        }
        self.inner.submit_read(loc, buf, token)
    }
    fn poll(&mut self, out: &mut Vec<lunix_iface::journal::JournalCompletion>) {
        let start = out.len();
        self.inner.poll(out);
        for c in &mut out[start..] {
            if let lunix_iface::journal::JournalCompletion::Read { status, .. } = c {
                if take_one(&self.checks.fail_jread_completion) {
                    *status = IoStatus::err(5);
                }
            }
        }
    }
    fn completed_watermark(&self) -> WriteSeq {
        self.inner.completed_watermark()
    }
}

/// A destager that fails one call with a chosen class.
pub struct FaultyDestager {
    pub fail: Arc<Mutex<Option<lunix_engine::Retry>>>,
}

impl lunix_engine::Destager for FaultyDestager {
    fn destage(
        &self,
        cut: &lunix_iface::overlay::OverlayCut,
        ctx: &lunix_engine::DestageCtx<'_>,
    ) -> std::result::Result<lunix_engine::DestageOutcome, lunix_engine::DestageFailure> {
        if let Some(retry) = lock(&self.fail).take() {
            // `AfterSpace` is what a full host file system gives (a
            // `NoSpace` error); every other class an I/O error.
            let error = if retry == lunix_engine::Retry::AfterSpace {
                lunix_iface::Error::NoSpace
            } else {
                lunix_iface::Error::Io(std::io::Error::other("injected destage failure"))
            };
            return Err(lunix_engine::DestageFailure {
                error,
                retry,
                chunk: cut.chunks.first().map(|c| c.chunk),
            });
        }
        SimpleDestager.destage(cut, ctx)
    }
}

/// The index with the queue-thread rule; `persist` fails while the counter
/// is above zero (one failure per unit).
pub struct GIndex(pub Arc<MemIndex>, pub Arc<AtomicU64>);

impl ChunkIndex for GIndex {
    fn get_for_dedup(&self, id: &ChunkId) -> Result<Option<DedupHit>> {
        not_on_queue("index get_for_dedup");
        self.0.get_for_dedup(id)
    }
    fn get(&self, id: &ChunkId) -> Result<Option<IndexedChunk>> {
        not_on_queue("index get");
        self.0.get(id)
    }
    fn get_by_prefix(&self, prefix: &lunix_format::IdPrefix) -> Result<Vec<IndexedChunk>> {
        not_on_queue("index get_by_prefix");
        self.0.get_by_prefix(prefix)
    }
    fn insert(&self, id: ChunkId, hint: Hint) -> Result<()> {
        not_on_queue("index insert");
        self.0.insert(id, hint)
    }
    fn relocate(&self, id: &ChunkId, old: &Hint, new: &Hint) -> Result<bool> {
        self.0.relocate(id, old, new)
    }
    fn mark_condemned(&self, seg: SegId) -> Result<()> {
        self.0.mark_condemned(seg)
    }
    fn unmark_condemned(&self, seg: SegId) -> Result<()> {
        self.0.unmark_condemned(seg)
    }
    fn condemned(&self) -> Result<Vec<SegId>> {
        self.0.condemned()
    }
    fn drop_segment(&self, seg: SegId) -> Result<()> {
        self.0.drop_segment(seg)
    }
    fn persist(&self, upto_seg: SegId) -> Result<Vec<RunRef>> {
        not_on_queue("index persist");
        if take_one(&self.1) {
            return Err(lunix_iface::Error::Io(std::io::Error::other(
                "injected persist failure",
            )));
        }
        self.0.persist(upto_seg)
    }
    fn runs(&self) -> Result<Vec<RunRef>> {
        self.0.runs()
    }
    fn all_runs(&self) -> Result<Vec<RunRef>> {
        self.0.all_runs()
    }
    fn catch_up(&self, segments: &[SegId], footers: &dyn SegmentFooters) -> Result<CatchUpReport> {
        not_on_queue("index catch_up");
        self.0.catch_up(segments, footers)
    }
    fn rebuild(&self, segments: &[SegId], footers: &dyn SegmentFooters) -> Result<()> {
        self.0.rebuild(segments, footers)
    }
    fn delete_run(&self, run: RunRef) -> Result<u64> {
        self.0.delete_run(run)
    }
    fn iter_entries(&self, f: &mut dyn FnMut(&IndexedChunk)) -> Result<()> {
        self.0.iter_entries(f)
    }
}

/// The map with the queue-thread rule (only `try_get_cached` is allowed)
/// and injectable walk failures.
pub struct GMap(pub FlatMap, pub Arc<AtomicU64>);

impl BlockMap for GMap {
    fn chunk_count(&self) -> u64 {
        self.0.chunk_count()
    }
    fn get(&self, ci: ChunkNo) -> Result<Option<DataEntry>> {
        not_on_queue("map get");
        self.0.get(ci)
    }
    fn try_get_cached(&self, ci: ChunkNo) -> Cached {
        self.0.try_get_cached(ci)
    }
    fn begin_update(&self) -> Result<Box<dyn MapTxn + '_>> {
        not_on_queue("map begin_update");
        self.0.begin_update()
    }
    fn seal(&self, packs: &mut dyn PackWrite) -> Result<SealResult> {
        not_on_queue("map seal");
        self.0.seal(packs)
    }
    fn grow(&self, n: u64) -> Result<()> {
        self.0.grow(n)
    }
    fn root(&self) -> MapRoot {
        self.0.root()
    }
    fn dirty_leaf_count(&self) -> u64 {
        self.0.dirty_leaf_count()
    }
    fn walk_leaves_skipping(
        &self,
        root: &MapRoot,
        skip: &HashSet<NodeHash>,
        f: &mut dyn FnMut(ChunkNo, &DataEntry),
    ) -> Result<()> {
        not_on_queue("map walk");
        if take_one(&self.1) {
            return Err(lunix_iface::Error::Io(std::io::Error::other(
                "injected walk failure",
            )));
        }
        self.0.walk_leaves_skipping(root, skip, f)
    }
    fn walk_nodes(&self, root: &MapRoot, f: &mut dyn FnMut(&NodeRef)) -> Result<()> {
        self.0.walk_nodes(root, f)
    }
    fn walk_nodes_skipping(
        &self,
        root: &MapRoot,
        skip: &HashSet<NodeHash>,
        f: &mut dyn FnMut(&NodeRef),
    ) -> Result<()> {
        self.0.walk_nodes_skipping(root, skip, f)
    }
}

pub struct FlatOpener {
    pub codec: Arc<dyn ExtentCodec>,
    pub packs: Arc<dyn PackRead>,
    pub id_len: IdLen,
    pub dirty_cap: Option<u64>,
    pub walk_faults: Arc<AtomicU64>,
}

impl MapOpener for FlatOpener {
    fn open(&self, root: &MapRoot, chunk_count: u64) -> Result<Arc<dyn BlockMap>> {
        let mut m = FlatMap::open(
            Arc::clone(&self.codec),
            Arc::clone(&self.packs),
            root,
            chunk_count,
            self.id_len,
        )?;
        if let Some(cap) = self.dirty_cap {
            m = m.with_dirty_leaf_cap(cap);
        }
        Ok(Arc::new(GMap(m, Arc::clone(&self.walk_faults))))
    }
}

// --------------------------------------------------------- the volume

/// A volume at rest: what survives a crash, plus host-side state.
pub struct Vol {
    pub cs: u32,
    pub id_len: IdLen,
    pub lanes: u16,
    pub codec: PlainCodec,
    pub store: StoreImage,
    pub index: IndexImage,
    pub journal: JournalImage,
    pub jcfg: MemJournalConfig,
    pub floor: Arc<MemFloor>,
    pub checks: Arc<Checks>,
    pub clock: Arc<ManualClock>,
    pub hooks: Arc<TestHooks>,
    pub space_capacity: u64,
    pub dirty_cap: Option<u64>,
    pub tweak: fn(&mut EngineConfig),
    pub space_cfg: Option<MemSpaceConfig>,
    pub read_faults: Arc<ReadFaults>,
    pub collector: Mutex<Option<Box<dyn lunix_iface::gc::Collector>>>,
    pub destage_fault: Arc<Mutex<Option<lunix_engine::Retry>>>,
    pub walk_faults: Arc<AtomicU64>,
    /// `persist` calls of the index that fail.
    pub persist_faults: Arc<AtomicU64>,
    /// The journal writes a mark after every group flush that raises the
    /// flushed watermark (`MemJournal::set_marks`).
    pub marks: bool,
}

pub fn default_tweak(_: &mut EngineConfig) {}

impl Vol {
    /// A new volume of `chunks` chunks with its genesis commit.
    pub fn create(cs: u32, chunks: u64, lanes: u16, mode: JournalMode) -> Self {
        Self::create_with(cs, chunks, lanes, mode, PlainCodec::new(), IdLen::Sixteen)
    }

    pub fn create_with(
        cs: u32,
        chunks: u64,
        lanes: u16,
        mode: JournalMode,
        codec: PlainCodec,
        id_len: IdLen,
    ) -> Self {
        let clock = Arc::new(ManualClock::new(TimeMs(1_700_000_000_000)));
        let store = MemStore::with_config(MemStoreConfig {
            mode: codec.mode(),
            segment_target: 256 << 10,
            ..MemStoreConfig::default()
        });
        let packs: Arc<dyn PackRead> = Arc::new(store.clone());
        let ext: Arc<dyn ExtentCodec> = Arc::new(codec);
        let empty = FlatMap::new(ext, packs, chunks, id_len).root();
        let vol_bytes = lunix_iface::store::VolumeDir::volume_bytes(&store).expect("volume");
        let g = Full::genesis(
            codec.object_hash(&vol_bytes),
            chunks * u64::from(cs),
            empty.hash,
            clock.now(),
        );
        let sealed = codec.seal_commit(&Commit::Full(g)).expect("seal genesis");
        CommitStore::put(&store, CommitSeq(0), &sealed.bytes).expect("put genesis");
        let jcfg = MemJournalConfig {
            lanes,
            file_bytes: lunix_testkit::journal::MIN_FILE_BYTES,
            max_files: 64,
            pool_depth: 64,
            mode,
        };
        let checks = Arc::new(Checks::default());
        Self {
            cs,
            id_len,
            lanes,
            codec,
            store: store.close(),
            index: IndexImage::default(),
            journal: MemJournal::new(jcfg).crash_with(0, CrashModel::keep_all()),
            jcfg,
            floor: Arc::new(MemFloor {
                checks: Arc::clone(&checks),
                ..MemFloor::default()
            }),
            checks,
            clock: Arc::clone(&clock),
            hooks: {
                let h = TestHooks::default();
                *lock(&h.clock) = Some(Arc::clone(&clock));
                Arc::new(h)
            },
            space_capacity: 1 << 40,
            dirty_cap: None,
            tweak: default_tweak,
            space_cfg: None,
            read_faults: Arc::new(ReadFaults::default()),
            collector: Mutex::new(None),
            destage_fault: Arc::new(Mutex::new(None)),
            walk_faults: Arc::new(AtomicU64::new(0)),
            persist_faults: Arc::new(AtomicU64::new(0)),
            marks: false,
        }
    }

    pub fn config(&self) -> EngineConfig {
        let mut cfg = EngineConfig::new(self.cs);
        cfg.id_len = self.id_len;
        cfg.background = false;
        cfg.read_helpers = 2;
        cfg.inflight_timeout = Duration::from_secs(20);
        cfg.read_cache_max = 4 << 20;
        (self.tweak)(&mut cfg);
        cfg
    }

    /// Opens the volume.
    pub fn open(&self) -> std::result::Result<Running, (OpenError, Pieces)> {
        let store = MemStore::from_image(&self.store);
        let index = Arc::new(MemIndex::from_image(store.mem_ids(), &self.index));
        let journal = Arc::new(MemJournal::from_image(&self.journal));
        journal.set_completion_mode(self.jcfg.mode);
        journal.set_marks(self.marks);
        *lock(&self.checks.store) = Some(store.clone());
        *lock(&self.checks.index) = Some(Arc::clone(&index));
        let space = MemSpace::new(
            self.space_cfg
                .unwrap_or_else(|| MemSpaceConfig::new(self.space_capacity).headroom(256 << 20)),
            self.clock.clone(),
        );
        let g = GStore {
            inner: store.clone(),
            checks: Arc::clone(&self.checks),
            faults: Arc::clone(&self.read_faults),
        };
        let ext: Arc<dyn ExtentCodec> = Arc::new(self.codec);
        let deps = EngineDeps {
            dir: Arc::new(store.clone()),
            segments: Arc::new(g.clone()),
            packs: Arc::new(g.clone()),
            commits: Arc::new(g.clone()),
            checkpoint: Arc::new(store.clone()),
            pins: Arc::new(store.clone()),
            gc_state: Arc::new(store.clone()),
            floor: self.floor.clone(),
            journal: Arc::new(GJournal {
                inner: Arc::clone(&journal),
                checks: Arc::clone(&self.checks),
            }),
            overlay: Arc::new(FlatOverlay::new(u64::from(self.cs) / BLOCK)),
            index: Arc::new(GIndex(Arc::clone(&index), Arc::clone(&self.persist_faults))),
            extents: Arc::clone(&ext),
            commit_codec: Arc::new(self.codec),
            space: space.clone(),
            clock: self.clock.clone(),
            maps: Arc::new(FlatOpener {
                codec: ext,
                packs: Arc::new(g),
                id_len: self.id_len,
                dirty_cap: self.dirty_cap,
                walk_faults: Arc::clone(&self.walk_faults),
            }),
            destager: Arc::new(FaultyDestager {
                fail: Arc::clone(&self.destage_fault),
            }),
            collector: lock(&self.collector).take(),
            hooks: self.hooks.clone(),
        };
        match Engine::open(self.config(), deps) {
            Ok(engine) => {
                let mut r = Running {
                    engine,
                    store,
                    journal,
                    index,
                    space,
                    queues: Vec::new(),
                    done: TestCompleter::new(4 << 20),
                    next_tag: 0,
                    read_len: HashMap::new(),
                    completed: HashMap::new(),
                };
                for q in 0..self.lanes {
                    let h = r
                        .engine
                        .queue_handler(&QueueConfig {
                            queue: QueueId(q),
                            nr_queues: self.lanes,
                            depth: 128,
                            max_io_buf_bytes: 4 << 20,
                        })
                        .expect("queue handler");
                    r.queues.push(h);
                }
                Ok(r)
            }
            Err(e) => Err((
                e,
                Pieces {
                    store,
                    index,
                    journal,
                },
            )),
        }
    }

    /// Takes the durable images of a stopped or crashed volume.
    pub fn absorb(&mut self, p: Pieces, seed: u64) {
        self.store = p.store.crash(seed);
        self.index = p.index.image();
        self.journal = p.journal.crash_with(seed, CrashModel::default());
    }

    /// Replaces the journal with an empty one of `cfg` (before the first open).
    pub fn set_journal(&mut self, cfg: MemJournalConfig) {
        self.jcfg = cfg;
        self.journal = MemJournal::new(cfg).crash_with(0, CrashModel::keep_all());
    }

    pub fn violations(&self) -> Vec<String> {
        lock(&self.checks.violations).clone()
    }
}

/// The fakes of a running or failed open, for imaging.
pub struct Pieces {
    pub store: MemStore,
    pub index: Arc<MemIndex>,
    pub journal: Arc<MemJournal>,
}

pub struct Running {
    pub engine: Engine,
    pub store: MemStore,
    pub journal: Arc<MemJournal>,
    pub index: Arc<MemIndex>,
    pub space: Arc<MemSpace>,
    pub queues: Vec<Box<dyn QueueHandler>>,
    pub done: TestCompleter,
    next_tag: u16,
    read_len: HashMap<Tag, usize>,
    completed: HashMap<Tag, (IoStatus, Vec<u8>)>,
}

impl Running {
    /// Stops the engine and returns the fakes (a crash right now).
    pub fn stop(self) -> Pieces {
        let Running {
            engine,
            store,
            journal,
            index,
            queues,
            ..
        } = self;
        let p = Pieces {
            store,
            index,
            journal,
        };
        drop(queues);
        drop(engine);
        p
    }

    pub fn tag(&mut self) -> Tag {
        loop {
            self.next_tag = self.next_tag.wrapping_add(1);
            let t = Tag(self.next_tag);
            if !self.read_len.contains_key(&t) && !self.completed.contains_key(&t) {
                return t;
            }
        }
    }

    fn collect(&mut self, inline: Option<(Tag, &[u8])>) {
        for (t, s) in self.done.take_completed() {
            let data = match self.read_len.remove(&t) {
                Some(len) if s.is_ok() => match inline {
                    Some((it, b)) if it == t => b.to_vec(),
                    _ => self.done.buf_of(t, len),
                },
                _ => Vec::new(),
            };
            assert!(
                self.completed.insert(t, (s, data)).is_none(),
                "tag {t:?} completed twice"
            );
        }
    }

    /// Hands a request to queue `q`; returns its tag.
    pub fn submit(
        &mut self,
        q: usize,
        op: IoOp,
        fua: bool,
        off: u64,
        len: u32,
        data: &[u8],
    ) -> Tag {
        let tag = self.tag();
        self.submit_as(tag, q, op, fua, off, len, data)
    }

    /// Hands a request with a tag from [`Running::tag`] to queue `q`.
    #[allow(clippy::too_many_arguments)]
    pub fn submit_as(
        &mut self,
        tag: Tag,
        q: usize,
        op: IoOp,
        fua: bool,
        off: u64,
        len: u32,
        data: &[u8],
    ) -> Tag {
        let req = IoRequest {
            tag,
            op,
            fua,
            offset_bytes: off,
            len,
        };
        let mut buf = match op {
            IoOp::Write => data.to_vec(),
            IoOp::Read => vec![0u8; len as usize],
            _ => Vec::new(),
        };
        if op == IoOp::Read {
            self.read_len.insert(tag, len as usize);
        }
        let qi = q % self.queues.len();
        self.queues[qi].on_request(req, &mut buf, &mut self.done);
        self.collect(Some((tag, &buf)));
        tag
    }

    /// Polls every queue once.
    pub fn pump(&mut self) {
        for q in &mut self.queues {
            q.poll(&mut self.done);
        }
        self.collect(None);
    }

    /// Is `tag` complete?
    pub fn is_done(&self, tag: Tag) -> bool {
        self.completed.contains_key(&tag)
    }

    /// Polls until every tag completed; a stuck volume (writes parked on a
    /// full ring) gets a coordinator tick.
    pub fn wait(&mut self, tags: &[Tag]) -> Vec<(IoStatus, Vec<u8>)> {
        let start = Instant::now();
        let mut idle = 0u32;
        loop {
            if tags.iter().all(|t| self.completed.contains_key(t)) {
                return tags
                    .iter()
                    .map(|t| self.completed.remove(t).expect("checked"))
                    .collect();
            }
            let n = self.completed.len();
            self.pump();
            if self.completed.len() == n {
                idle += 1;
                if idle.is_multiple_of(64) {
                    std::thread::sleep(Duration::from_micros(200));
                } else {
                    std::thread::yield_now();
                }
                if idle.is_multiple_of(2000) {
                    let _ = self.engine.tick();
                }
            } else {
                idle = 0;
            }
            assert!(
                start.elapsed() < Duration::from_secs(60),
                "requests {tags:?} never completed"
            );
        }
    }

    pub fn write(&mut self, q: usize, off: u64, data: &[u8], fua: bool) -> IoStatus {
        let len = u32::try_from(data.len()).expect("len");
        let t = self.submit(q, IoOp::Write, fua, off, len, data);
        self.wait(&[t])[0].0
    }

    pub fn hole(&mut self, q: usize, op: IoOp, off: u64, len: u32) -> IoStatus {
        let t = self.submit(q, op, false, off, len, &[]);
        self.wait(&[t])[0].0
    }

    pub fn read(&mut self, q: usize, off: u64, len: u32) -> (IoStatus, Vec<u8>) {
        let t = self.submit(q, IoOp::Read, false, off, len, &[]);
        self.wait(&[t]).remove(0)
    }

    pub fn flush(&mut self, q: usize) -> IoStatus {
        let t = self.submit(q, IoOp::Flush, false, 0, 0, &[]);
        self.wait(&[t])[0].0
    }

    /// The whole device, read through the engine in 64 KiB requests.
    pub fn read_all(&mut self) -> Vec<u8> {
        let size = self.engine.size_bytes();
        let mut out = Vec::with_capacity(size as usize);
        let mut off = 0;
        while off < size {
            let len = (size - off).min(64 << 10);
            let (st, data) = self.read(0, off, len as u32);
            assert!(st.is_ok(), "read at {off} failed: {st:?}");
            out.extend_from_slice(&data);
            off += len;
        }
        out
    }
}

/// Deterministic bytes.
pub fn pattern(seed: u64, len: usize) -> Vec<u8> {
    lunix_testkit::strategies::pattern(seed, len)
}

/// Every byte of `n` chunks written with distinct patterns, for tests that
/// want data in the map.
pub fn fill(r: &mut Running, cs: u64, chunks: u64, seed: u64) -> Vec<u8> {
    let mut all = Vec::new();
    for c in 0..chunks {
        let d = pattern(seed + c, cs as usize);
        assert!(r.write(c as usize, c * cs, &d, false).is_ok());
        all.extend_from_slice(&d);
    }
    all
}

pub fn elapsed_ok(start: Instant, limit: Duration) -> bool {
    start.elapsed() < limit
}
