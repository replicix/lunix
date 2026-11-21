//! ADR-33 T8: segment seals run beside the packer. The rules: every seal of
//! the cut is done and checked before the map transaction commits; a failed
//! seal fails the cut and leaves nothing behind; seals overlap packing; the
//! seals in flight are bounded; no seal thread outlives the cut.
//!
//! No test here asserts a duration. A seal that must be slow blocks on a
//! [`Gate`] that the test releases when the state it waits for is reached;
//! a helper thread waits for such a state with a limit, so a broken rule
//! fails the test instead of hanging it (a lost wake-up still hangs).

mod common;

use std::collections::HashSet;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use common::*;
use lunix_destage::{
    destage_cut, destage_cut_with_cancel, DestageConfig, DestageError, DestageOutcome,
    MAX_SEALS_IN_FLIGHT,
};
use lunix_format::commit::MapRoot;
use lunix_format::node::DataEntry;
use lunix_format::{ChunkId, ExtentKind, Hint, SegId};
use lunix_iface::crypto::{EncodingDesc, ExtentCodec, OpenExpect, OpenedMeta};
use lunix_iface::journal::WriteSeqRange;
use lunix_iface::map::{
    BlockMap, Cached, ChunkNo, MapTxn, NodeHash, NodeRef, SealResult, TxnReport,
};
use lunix_iface::store::{
    ExtentLocation, PackWrite, SealedSegment, SegmentInfo, SegmentStore, SegmentWriter,
};
use lunix_iface::{Error, ErrorKind, Result};

const CS: u32 = 16 << 10;
/// Chunks per segment with the target below.
const PER_SEG: u64 = 4;
const SEG_TARGET: u64 = 64 << 10;

/// A latch: seals held on it run on `release`.
#[derive(Default)]
struct Gate {
    open: Mutex<bool>,
    cv: Condvar,
}

impl Gate {
    fn wait(&self) {
        let mut g = self.open.lock().expect("gate");
        while !*g {
            g = self.cv.wait(g).expect("gate");
        }
    }
    fn release(&self) {
        *self.open.lock().expect("gate") = true;
        self.cv.notify_all();
    }
}

/// What the store saw.
#[derive(Default)]
struct Seen {
    opened: AtomicUsize,
    started: AtomicUsize,
    sealed: AtomicUsize,
    running: AtomicUsize,
    running_max: AtomicUsize,
    /// The cut is over (the helper stops waiting).
    over: AtomicBool,
}

impl Seen {
    /// Polls `cond` until it holds, the cut is over, or `limit` passes.
    fn wait_for(&self, limit: Duration, cond: impl Fn() -> bool) -> bool {
        let t = Instant::now();
        while !cond() {
            if self.over.load(Ordering::SeqCst) || t.elapsed() > limit {
                return false;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        true
    }
}

const LIMIT: Duration = Duration::from_secs(10);

/// What each segment's seal does, by segment number from 1 in open order.
#[derive(Default)]
struct Plan {
    /// Block until the gate is released.
    hold: Vec<(usize, Arc<Gate>)>,
    /// Sleep this long first (a lower bound only).
    sleep: Vec<(usize, Duration)>,
    /// Fail before sealing (the file is abandoned).
    fail: Vec<usize>,
    /// Seal (rename included), then report a failure: the directory `fsync`.
    fail_after: Vec<usize>,
    /// Panic.
    panic: Vec<usize>,
}

struct SlowStore {
    inner: lunix_testkit::MemStore,
    seen: Arc<Seen>,
    plan: Arc<Plan>,
}

struct SlowWriter {
    inner: Box<dyn SegmentWriter>,
    seen: Arc<Seen>,
    plan: Arc<Plan>,
    n: usize,
}

impl SegmentWriter for SlowWriter {
    fn id(&self) -> SegId {
        self.inner.id()
    }
    fn append_extent(&mut self, c: &ChunkId, d: Option<u32>, e: &[u8]) -> Result<Hint> {
        self.inner.append_extent(c, d, e)
    }
    fn len(&self) -> u64 {
        self.inner.len()
    }
    fn is_full(&self) -> bool {
        self.inner.is_full()
    }
    fn seal(self: Box<Self>, w: Option<WriteSeqRange>) -> Result<SealedSegment> {
        let Self {
            inner,
            seen,
            plan,
            n,
        } = *self;
        seen.started.fetch_add(1, Ordering::SeqCst);
        let now = seen.running.fetch_add(1, Ordering::SeqCst) + 1;
        seen.running_max.fetch_max(now, Ordering::SeqCst);
        let _done = Running(&seen);
        for (k, g) in &plan.hold {
            if *k == n {
                g.wait();
            }
        }
        for (k, d) in &plan.sleep {
            if *k == n {
                std::thread::sleep(*d);
            }
        }
        assert!(!plan.panic.contains(&n), "injected seal panic");
        if plan.fail.contains(&n) {
            return Err(Error::Io(std::io::Error::other("injected seal failure")));
        }
        let r = inner.seal(w);
        seen.sealed.fetch_add(1, Ordering::SeqCst);
        if plan.fail_after.contains(&n) {
            return Err(Error::Io(std::io::Error::other("injected dirsync failure")));
        }
        r
    }
}

/// Counts a seal out, also when it panics.
struct Running<'a>(&'a Seen);

impl Drop for Running<'_> {
    fn drop(&mut self) {
        self.0.running.fetch_sub(1, Ordering::SeqCst);
    }
}

impl SegmentStore for SlowStore {
    fn open_segment(&self) -> Result<Box<dyn SegmentWriter>> {
        let n = self.seen.opened.fetch_add(1, Ordering::SeqCst) + 1;
        Ok(Box::new(SlowWriter {
            inner: self.inner.open_segment()?,
            seen: Arc::clone(&self.seen),
            plan: Arc::clone(&self.plan),
            n,
        }))
    }
    fn read_extent(&self, h: &Hint, out: &mut Vec<u8>) -> Result<()> {
        self.inner.read_extent(h, out)
    }
    fn locate(&self, h: &Hint) -> Result<ExtentLocation> {
        self.inner.locate(h)
    }
    fn footer(&self, s: SegId) -> Result<lunix_format::segment::FooterBody> {
        self.inner.footer(s)
    }
    fn list(&self) -> Result<Vec<SegmentInfo>> {
        self.inner.list()
    }
    fn delete(&self, s: SegId) -> Result<u64> {
        self.inner.delete(s)
    }
}

/// A codec that panics, in a worker, at its `at`-th chunk seal once `ready`
/// says the packer has seals in flight.
struct PanicCodec {
    inner: Arc<dyn ExtentCodec>,
    calls: AtomicUsize,
    at: usize,
    seen: Arc<Seen>,
}

impl ExtentCodec for PanicCodec {
    fn mode(&self) -> lunix_format::CryptoMode {
        self.inner.mode()
    }
    fn chunk_id(&self, k: ExtentKind, d: &[u8]) -> ChunkId {
        self.inner.chunk_id(k, d)
    }
    fn seal(
        &self,
        k: ExtentKind,
        id: &ChunkId,
        desc: &EncodingDesc,
        c: &[u8],
        out: &mut Vec<u8>,
    ) -> Result<()> {
        if self.calls.fetch_add(1, Ordering::SeqCst) + 1 == self.at {
            let s = &self.seen;
            s.wait_for(LIMIT, || s.started.load(Ordering::SeqCst) >= 2);
            panic!("injected worker panic");
        }
        self.inner.seal(k, id, desc, c, out)
    }
    fn open(
        &self,
        k: ExtentKind,
        b: &[u8],
        e: &OpenExpect<'_>,
        out: &mut Vec<u8>,
    ) -> Result<OpenedMeta> {
        self.inner.open(k, b, e, out)
    }
}

/// A map that notes, at the commit of a transaction, how many segments of the
/// cut were opened and how many were sealed.
struct SpyMap<'a> {
    inner: &'a dyn BlockMap,
    seen: Arc<Seen>,
    at_commit: Arc<Mutex<Vec<(usize, usize)>>>,
}

struct SpyTxn<'a> {
    inner: Box<dyn MapTxn + 'a>,
    seen: Arc<Seen>,
    at_commit: Arc<Mutex<Vec<(usize, usize)>>>,
}

impl MapTxn for SpyTxn<'_> {
    fn set(&mut self, ci: ChunkNo, e: DataEntry) -> Result<()> {
        self.inner.set(ci, e)
    }
    fn set_hole(&mut self, ci: ChunkNo) -> Result<()> {
        self.inner.set_hole(ci)
    }
    fn rehint(&mut self, ci: ChunkNo, o: Hint, n: Hint) -> Result<()> {
        self.inner.rehint(ci, o, n)
    }
    fn commit(self: Box<Self>) -> Result<TxnReport> {
        let Self {
            inner,
            seen,
            at_commit,
        } = *self;
        at_commit.lock().expect("lock").push((
            seen.opened.load(Ordering::SeqCst),
            seen.sealed.load(Ordering::SeqCst),
        ));
        inner.commit()
    }
}

impl BlockMap for SpyMap<'_> {
    fn chunk_count(&self) -> u64 {
        self.inner.chunk_count()
    }
    fn get(&self, ci: ChunkNo) -> Result<Option<DataEntry>> {
        self.inner.get(ci)
    }
    fn try_get_cached(&self, ci: ChunkNo) -> Cached {
        self.inner.try_get_cached(ci)
    }
    fn begin_update(&self) -> Result<Box<dyn MapTxn + '_>> {
        Ok(Box::new(SpyTxn {
            inner: self.inner.begin_update()?,
            seen: Arc::clone(&self.seen),
            at_commit: Arc::clone(&self.at_commit),
        }))
    }
    fn seal(&self, p: &mut dyn PackWrite) -> Result<SealResult> {
        self.inner.seal(p)
    }
    fn grow(&self, n: u64) -> Result<()> {
        self.inner.grow(n)
    }
    fn root(&self) -> MapRoot {
        self.inner.root()
    }
    fn dirty_leaf_count(&self) -> u64 {
        self.inner.dirty_leaf_count()
    }
    fn walk_leaves_skipping(
        &self,
        r: &MapRoot,
        s: &HashSet<NodeHash>,
        f: &mut dyn FnMut(ChunkNo, &DataEntry),
    ) -> Result<()> {
        self.inner.walk_leaves_skipping(r, s, f)
    }
    fn walk_nodes(&self, r: &MapRoot, f: &mut dyn FnMut(&NodeRef)) -> Result<()> {
        self.inner.walk_nodes(r, f)
    }
    fn walk_nodes_skipping(
        &self,
        r: &MapRoot,
        s: &HashSet<NodeHash>,
        f: &mut dyn FnMut(&NodeRef),
    ) -> Result<()> {
        self.inner.walk_nodes_skipping(r, s, f)
    }
}

struct Run {
    rig: Rig,
    cut: lunix_iface::overlay::OverlayCut,
    seen: Arc<Seen>,
    at_commit: Arc<Mutex<Vec<(usize, usize)>>>,
}

fn rig_with(segments: u64) -> Run {
    let mut c = RigConfig::new(CS, segments * PER_SEG);
    c.segment_target = SEG_TARGET;
    let rig = Rig::new(c);
    for ci in 0..segments * PER_SEG {
        rig.write_chunk(ci, &random_bytes(ci + 1, CS as usize));
    }
    let cut = rig.cut(rig.last_seq());
    Run {
        rig,
        cut,
        seen: Arc::default(),
        at_commit: Arc::default(),
    }
}

type Outcome = std::result::Result<DestageOutcome, DestageError>;

impl Run {
    /// Destages the cut against the planned store, with a helper thread that
    /// drives the test's schedule (it must release every gate it holds).
    fn destage(
        &self,
        plan: Plan,
        max_in_flight: usize,
        cancel: Option<&AtomicBool>,
        helper: impl FnOnce(&Seen, &Mutex<Vec<(usize, usize)>>) + Send,
    ) -> Outcome {
        let plan = Arc::new(plan);
        let slow = SlowStore {
            inner: self.rig.store.clone(),
            seen: Arc::clone(&self.seen),
            plan: Arc::clone(&plan),
        };
        let spy = SpyMap {
            inner: &self.rig.map,
            seen: Arc::clone(&self.seen),
            at_commit: Arc::clone(&self.at_commit),
        };
        let index = self.rig.index();
        let mut deps = self.rig.deps(&index);
        deps.segments = &slow;
        deps.map = &spy;
        deps.config.max_seals_in_flight = max_in_flight;
        self.run_deps(&deps, &plan, cancel, helper)
    }

    fn run_deps(
        &self,
        deps: &lunix_destage::DestageDeps<'_>,
        plan: &Plan,
        cancel: Option<&AtomicBool>,
        helper: impl FnOnce(&Seen, &Mutex<Vec<(usize, usize)>>) + Send,
    ) -> Outcome {
        std::thread::scope(|s| {
            s.spawn(|| {
                helper(&self.seen, &self.at_commit);
                // Whatever the helper did, nothing stays held.
                for (_, g) in &plan.hold {
                    g.release();
                }
            });
            let r = match cancel {
                Some(c) => destage_cut_with_cancel(&self.cut, deps, Some(c)),
                None => destage_cut(&self.cut, deps),
            };
            self.seen.over.store(true, Ordering::SeqCst);
            r
        })
    }

    /// Nothing of the failed cut is left in the store and no seal runs.
    fn assert_clean(&self) {
        assert_eq!(self.seen.running.load(Ordering::SeqCst), 0, "a seal runs");
        assert!(
            self.rig.store.list().expect("list").is_empty(),
            "a segment is left"
        );
        assert!(
            self.at_commit.lock().expect("lock").is_empty(),
            "the map committed"
        );
        assert_eq!(self.rig.map.dirty_leaf_count(), 0);
    }

    /// The same cut over the same state succeeds and reads back.
    fn assert_retry_reads_back(&self) {
        let out = self.rig.destage(&self.cut).expect("retry");
        self.rig.finish(&self.cut, out);
        let model = self.rig.model_at(self.rig.last_seq());
        self.rig.assert_equals(&model);
    }
}

fn gate() -> Arc<Gate> {
    Arc::default()
}

#[test]
fn the_map_commits_only_after_every_seal_of_the_cut_is_done() {
    let run = rig_with(4);
    let g = gate();
    let plan = Plan {
        hold: vec![(1, g.clone())],
        ..Plan::default()
    };
    // The first seal is held while the other three finish and the packer
    // ends; only then is it released (after the map had its chance to commit).
    let (r, seen2) = (
        run.destage(plan, 8, None, |seen, at| {
            seen.wait_for(LIMIT, || {
                seen.opened.load(Ordering::SeqCst) == 4 && seen.sealed.load(Ordering::SeqCst) == 3
            });
            seen.wait_for(Duration::from_millis(300), || {
                !at.lock().expect("lock").is_empty()
            });
            g.release();
        }),
        Arc::clone(&run.seen),
    );
    let out = r.expect("destage");
    assert_eq!(out.segments.len(), 4);
    let at = run.at_commit.lock().expect("lock").clone();
    // One commit (the probe transaction is dropped), at which all four
    // segments were sealed.
    assert_eq!(at, vec![(4, 4)], "(opened, sealed) at the map commit");
    assert_eq!(seen2.sealed.load(Ordering::SeqCst), 4);
    run.rig.finish(&run.cut, out);
}

#[test]
fn seals_overlap_packing_and_each_other() {
    let run = rig_with(4);
    let g = gate();
    let plan = Plan {
        hold: vec![(1, g.clone())],
        ..Plan::default()
    };
    let reached = Mutex::new(false);
    let r = run.destage(plan, 8, None, |seen, _| {
        // With the first seal held, the packer must still pack the other
        // three segments and their seals must complete: overlap.
        let ok = seen.wait_for(Duration::from_secs(3), || {
            seen.opened.load(Ordering::SeqCst) == 4 && seen.sealed.load(Ordering::SeqCst) == 3
        });
        *reached.lock().expect("lock") = ok;
        g.release();
    });
    let out = r.expect("destage");
    assert!(
        *reached.lock().expect("lock"),
        "packing waited for a held seal"
    );
    assert!(out.stats.seal_in_flight_max >= 2);
    assert_eq!(out.stats.seals, 4);
}

#[test]
fn the_seals_in_flight_are_bounded() {
    let run = rig_with(6);
    let g = gate();
    let plan = Plan {
        hold: vec![(1, g.clone()), (2, g.clone())],
        ..Plan::default()
    };
    let started_while_full = Mutex::new(0);
    let r = run.destage(plan, 2, None, |seen, _| {
        // Two seals held, the packer on its third segment: the third seal
        // must not start while both slots are taken.
        seen.wait_for(LIMIT, || {
            seen.running.load(Ordering::SeqCst) == 2 && seen.opened.load(Ordering::SeqCst) == 3
        });
        std::thread::sleep(Duration::from_millis(100));
        *started_while_full.lock().expect("lock") = seen.started.load(Ordering::SeqCst);
        g.release();
    });
    let out = r.expect("destage");
    assert_eq!(*started_while_full.lock().expect("lock"), 2);
    assert_eq!(run.seen.running_max.load(Ordering::SeqCst), 2);
    assert_eq!(out.stats.seal_in_flight_max, 2);
    assert!(out.stats.seal_stall_ns > 0, "the packer waited for a slot");
}

#[test]
fn a_failed_seal_fails_the_cut_and_leaves_nothing_a_retry_cannot_redo() {
    // The first seal and the last one fail, after the other segments are
    // packed: the wait at the end sees the error.
    for fail in [1, 4] {
        let run = rig_with(4);
        let g = gate();
        let plan = Plan {
            hold: vec![(fail, g.clone())],
            fail: vec![fail],
            ..Plan::default()
        };
        let blocks = run.rig.overlay.len_blocks();
        let err = run
            .destage(plan, 8, None, |seen, _| {
                seen.wait_for(LIMIT, || seen.opened.load(Ordering::SeqCst) == 4);
                std::thread::sleep(Duration::from_millis(50));
                g.release();
            })
            .expect_err("a seal failed");
        assert_eq!(err.kind(), ErrorKind::Io, "failing seal {fail}");
        assert!(err.partial.stats.seals >= 1, "the seal figures are kept");
        run.assert_clean();
        assert_eq!(run.rig.overlay.len_blocks(), blocks, "no write is lost");
        run.assert_retry_reads_back();
    }
}

#[test]
fn a_seal_that_fails_while_the_packer_waits_for_a_slot_fails_the_cut() {
    let run = rig_with(4);
    let g = gate();
    let plan = Plan {
        hold: vec![(1, g.clone())],
        fail: vec![1],
        ..Plan::default()
    };
    // One slot: the packer stalls on the second segment's seal until the
    // first one ends, here by failing.
    let err = run
        .destage(plan, 1, None, |seen, _| {
            seen.wait_for(LIMIT, || seen.opened.load(Ordering::SeqCst) == 2);
            std::thread::sleep(Duration::from_millis(100));
            g.release();
        })
        .expect_err("the seal failed");
    assert_eq!(err.kind(), ErrorKind::Io);
    run.assert_clean();
    run.assert_retry_reads_back();
}

#[test]
fn a_seal_that_fails_after_the_rename_leaves_no_sealed_file() {
    let run = rig_with(4);
    // The directory `fsync` of the second segment fails: the file has its
    // sealed name when the seal reports the error.
    let plan = Plan {
        fail_after: vec![2],
        ..Plan::default()
    };
    let err = run
        .destage(plan, 8, None, |_, _| {})
        .expect_err("the seal failed");
    assert_eq!(err.kind(), ErrorKind::Io);
    assert!(run.seen.sealed.load(Ordering::SeqCst) >= 2);
    run.assert_clean();
    run.assert_retry_reads_back();
}

#[test]
fn several_failed_seals_are_all_reported() {
    let run = rig_with(4);
    let g = gate();
    let plan = Plan {
        hold: vec![(1, g.clone()), (2, g.clone())],
        fail: vec![1, 2],
        ..Plan::default()
    };
    let err = run
        .destage(plan, 8, None, |seen, _| {
            seen.wait_for(LIMIT, || seen.opened.load(Ordering::SeqCst) == 4);
            std::thread::sleep(Duration::from_millis(50));
            g.release();
        })
        .expect_err("two seals failed");
    assert_eq!(err.kind(), ErrorKind::Io);
    assert_eq!(err.partial.more_seal_errors.len(), 1);
    assert_eq!(err.partial.stats.seals, 4);
    run.assert_clean();
}

#[test]
fn a_panicking_seal_fails_the_cut_with_invalid_and_the_next_cut_works() {
    let run = rig_with(4);
    let plan = Plan {
        panic: vec![2],
        ..Plan::default()
    };
    let err = run
        .destage(plan, 8, None, |_, _| {})
        .expect_err("a seal panicked");
    assert_eq!(err.kind(), ErrorKind::Invalid);
    run.assert_clean();
    run.assert_retry_reads_back();
}

#[test]
fn a_cancel_with_seals_in_flight_ends_the_cut_after_they_ended() {
    // Many jobs and a window of three: the packer is still taking results
    // when the flag is set.
    let run = rig_with(128);
    let g = gate();
    let plan = Plan {
        hold: vec![(1, g.clone())],
        sleep: vec![(2, Duration::from_millis(400))],
        ..Plan::default()
    };
    let cancel = AtomicBool::new(false);
    let r = {
        let plan = Arc::new(plan);
        let slow = SlowStore {
            inner: run.rig.store.clone(),
            seen: Arc::clone(&run.seen),
            plan: Arc::clone(&plan),
        };
        let index = run.rig.index();
        let mut deps = run.rig.deps(&index);
        deps.segments = &slow;
        deps.config.max_seals_in_flight = 2;
        deps.config.result_window = deps.config.workers;
        run.run_deps(&deps, &plan, Some(&cancel), |seen, _| {
            // Seals 1 (held) and 2 (slow) fill both slots; the packer is on
            // its third segment.
            seen.wait_for(LIMIT, || {
                seen.running.load(Ordering::SeqCst) == 2 && seen.opened.load(Ordering::SeqCst) == 3
            });
            cancel.store(true, Ordering::SeqCst);
            g.release();
        })
    };
    let err = r.expect_err("cancelled");
    assert_eq!(err.kind(), ErrorKind::Closed);
    // The seal of segment 2 was still sleeping when the packer gave up: the
    // cut waited for it, and deleted what it sealed.
    run.assert_clean();
}

#[test]
fn a_worker_panic_unwinding_out_of_the_cut_leaves_no_seal_thread_and_no_sealed_file() {
    let mut c = RigConfig::new(CS, 32 * PER_SEG);
    c.segment_target = SEG_TARGET;
    let rig = Rig::new(c);
    for ci in 0..32 * PER_SEG {
        rig.write_chunk(ci, &random_bytes(ci + 1, CS as usize));
    }
    let run = Run {
        cut: rig.cut(rig.last_seq()),
        rig,
        seen: Arc::default(),
        at_commit: Arc::default(),
    };
    let g = gate();
    let plan = Arc::new(Plan {
        hold: vec![(1, g.clone()), (2, g.clone())],
        sleep: vec![(3, Duration::from_millis(200))],
        ..Plan::default()
    });
    let slow = SlowStore {
        inner: run.rig.store.clone(),
        seen: Arc::clone(&run.seen),
        plan: Arc::clone(&plan),
    };
    let codec = PanicCodec {
        inner: Arc::clone(&run.rig.codec),
        calls: AtomicUsize::new(0),
        // Past the first three jobs: the packer has taken job 0, so seals
        // are in flight.
        at: 60,
        seen: Arc::clone(&run.seen),
    };
    let index = run.rig.index();
    let mut deps = run.rig.deps(&index);
    deps.segments = &slow;
    deps.codec = &codec;
    deps.config.max_seals_in_flight = 8;
    deps.config.result_window = deps.config.workers;
    let unwound = std::panic::catch_unwind(AssertUnwindSafe(|| {
        run.run_deps(&deps, &plan, None, |seen, _| {
            // Release the held seals a moment after the panic is under way.
            seen.wait_for(LIMIT, || seen.started.load(Ordering::SeqCst) >= 2);
            std::thread::sleep(Duration::from_millis(300));
        })
    }));
    assert!(unwound.is_err(), "the worker panic unwinds out of the cut");
    // On return no seal thread runs and nothing sealed is left.
    run.assert_clean();
}

#[test]
fn the_seals_in_flight_have_an_upper_bound() {
    let ok = DestageConfig {
        max_seals_in_flight: MAX_SEALS_IN_FLIGHT,
        ..DestageConfig::default()
    };
    ok.validate().expect("the bound itself is allowed");
    let too_many = DestageConfig {
        max_seals_in_flight: MAX_SEALS_IN_FLIGHT + 1,
        ..DestageConfig::default()
    };
    too_many.validate().expect_err("above the bound");
    let none = DestageConfig {
        max_seals_in_flight: 0,
        ..DestageConfig::default()
    };
    none.validate().expect("0 seals inline");
}

#[cfg(feature = "crashpoints")]
#[test]
fn the_seal_crash_points_are_registered() {
    let names = lunix_iface::crashpoint::registered();
    for n in [
        "destage.seal_submitted",
        "destage.before_seals_wait",
        "destage.after_seals_wait",
        "destage.after_segment_seal",
    ] {
        assert!(names.contains(&n), "{n} missing");
    }
}
