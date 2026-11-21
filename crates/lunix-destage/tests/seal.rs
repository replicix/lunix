//! ADR-33 T8: segment seals run beside the packer. The rules: every seal of
//! the cut is done and checked before the map transaction commits; a failed
//! seal fails the cut and leaves nothing behind; seals overlap packing; the
//! seals in flight are bounded.

mod common;

use std::collections::HashSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::*;
use lunix_destage::{destage_cut, DestageOutcome};
use lunix_format::commit::MapRoot;
use lunix_format::node::DataEntry;
use lunix_format::{ChunkId, Hint, SegId};
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

/// What the slow store saw.
#[derive(Default)]
struct Seen {
    opened: AtomicUsize,
    sealed: AtomicUsize,
    running: AtomicUsize,
    running_max: AtomicUsize,
}

/// A segment store whose seals take `delay` (the first one `first_delay`) and
/// the `fail_nth` one (from 1) fails.
struct SlowStore {
    inner: lunix_testkit::MemStore,
    seen: Arc<Seen>,
    delay: Duration,
    first_delay: Duration,
    fail_nth: Option<usize>,
}

struct SlowWriter {
    inner: Box<dyn SegmentWriter>,
    seen: Arc<Seen>,
    delay: Duration,
    fail: bool,
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
            delay,
            fail,
        } = *self;
        let now = seen.running.fetch_add(1, Ordering::SeqCst) + 1;
        seen.running_max.fetch_max(now, Ordering::SeqCst);
        std::thread::sleep(delay);
        seen.running.fetch_sub(1, Ordering::SeqCst);
        if fail {
            return Err(Error::Io(std::io::Error::other("injected seal failure")));
        }
        let r = inner.seal(w);
        seen.sealed.fetch_add(1, Ordering::SeqCst);
        r
    }
}

impl SegmentStore for SlowStore {
    fn open_segment(&self) -> Result<Box<dyn SegmentWriter>> {
        let n = self.seen.opened.fetch_add(1, Ordering::SeqCst) + 1;
        Ok(Box::new(SlowWriter {
            inner: self.inner.open_segment()?,
            seen: Arc::clone(&self.seen),
            delay: if n == 1 { self.first_delay } else { self.delay },
            fail: self.fail_nth == Some(n),
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

impl Run {
    fn destage(
        &self,
        delay: Duration,
        first_delay: Duration,
        fail_nth: Option<usize>,
        max_in_flight: usize,
    ) -> (
        std::result::Result<DestageOutcome, lunix_destage::DestageError>,
        Duration,
    ) {
        let slow = SlowStore {
            inner: self.rig.store.clone(),
            seen: Arc::clone(&self.seen),
            delay,
            first_delay,
            fail_nth,
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
        let t = Instant::now();
        let r = destage_cut(&self.cut, &deps);
        (r, t.elapsed())
    }
}

const NONE: Duration = Duration::ZERO;

#[test]
fn the_map_commits_only_after_every_seal_of_the_cut_is_done() {
    let run = rig_with(4);
    // The first segment's seal outlasts the packing of the other three.
    let (r, _) = run.destage(NONE, Duration::from_millis(400), None, 8);
    let out = r.expect("destage");
    assert_eq!(out.segments.len(), 4);
    let at = run.at_commit.lock().expect("lock").clone();
    // The probe transaction is dropped, not committed: one commit, and at it
    // all four segments were sealed.
    assert_eq!(at, vec![(4, 4)], "(opened, sealed) at the map commit");
    assert!(
        out.stats.seal_wait_ns >= 100_000_000,
        "the cut waited at its end for the slow seal: {} ns",
        out.stats.seal_wait_ns
    );
    run.rig.finish(&run.cut, out);
}

#[test]
fn a_failed_seal_fails_the_cut_and_leaves_nothing_a_retry_cannot_redo() {
    // The second segment's seal fails while the first is still sealing and
    // the packer is still packing; the last one's fails after packing ended,
    // so only the wait at the end can see it.
    for fail in [2, 4] {
        let run = rig_with(4);
        let blocks_before = run.rig.overlay.len_blocks();
        let (r, _) = run.destage(NONE, Duration::from_millis(200), Some(fail), 8);
        let err = r.expect_err("a seal failed");
        assert_eq!(err.kind(), ErrorKind::Io, "failing seal {fail}");
        // Nothing reached the map, no segment of the cut is left, and the
        // overlay still holds every acknowledged write.
        assert!(run.at_commit.lock().expect("lock").is_empty());
        assert_eq!(run.rig.map.dirty_leaf_count(), 0);
        assert!(run.rig.store.list().expect("list").is_empty());
        assert_eq!(run.rig.overlay.len_blocks(), blocks_before);
        // The same cut over the same state succeeds and reads back.
        let out = run.rig.destage(&run.cut).expect("retry");
        run.rig.finish(&run.cut, out);
        let model = run.rig.model_at(run.rig.last_seq());
        run.rig.assert_equals(&model);
    }
}

#[test]
fn seals_overlap_packing_and_each_other() {
    let n = 6u32;
    let delay = Duration::from_millis(150);
    let inline = rig_with(u64::from(n));
    let (r, serial) = inline.destage(delay, delay, None, 0);
    r.expect("inline");
    let overlapped = rig_with(u64::from(n));
    let (r, par) = overlapped.destage(delay, delay, None, 8);
    let out = r.expect("overlapped");
    // Inline: the sum of the seals. Overlapped: about one seal.
    assert!(serial >= delay * n, "inline took {serial:?}");
    assert!(
        par < delay * 3,
        "overlapped took {par:?}, inline {serial:?}"
    );
    assert!(out.stats.seal_in_flight_max >= 2);
    assert_eq!(out.stats.seals, u64::from(n));
    assert!(out.stats.seal_busy_ns >= u64::from(n) * 150_000_000);
}

#[test]
fn the_seals_in_flight_are_bounded() {
    let run = rig_with(6);
    let (r, _) = run.destage(
        Duration::from_millis(60),
        Duration::from_millis(60),
        None,
        2,
    );
    let out = r.expect("destage");
    assert_eq!(out.segments.len(), 6);
    assert_eq!(run.seen.running_max.load(Ordering::SeqCst), 2);
    assert_eq!(out.stats.seal_in_flight_max, 2);
    assert!(out.stats.seal_stall_ns > 0, "the packer waited for a slot");
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
