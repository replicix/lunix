//! The engine: shared state, `BlockEngine`, the control-plane entry points.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use lunix_format::{CommitSeq, WriteSeq};
use lunix_iface::block::{BlockEngine, QueueConfig, QueueHandler, ResizeHook};
use lunix_iface::clock::Clock;
use lunix_iface::crypto::{CommitCodec, ExtentCodec};
use lunix_iface::gc::{GcControl, GcPassSummary, GcStatusSnapshot};
use lunix_iface::index::ChunkIndex;
use lunix_iface::journal::{Journal, LaneId};
use lunix_iface::map::BlockMap;
use lunix_iface::overlay::Overlay;
use lunix_iface::space::SpaceGate;
use lunix_iface::stats::EngineStats;
use lunix_iface::store::{
    CheckpointStore, CommitStore, GcStateStore, PackStore, PinSource, SegmentStore, VolumeDir,
};
use lunix_iface::wake::WakeFd;
use lunix_iface::{Error, Result};

use crate::cache::ChunkCache;
use crate::config::EngineConfig;
use crate::coord::Coord;
use crate::deps::{EngineDeps, RollbackFloorStore};
use crate::destage::Destager;
use crate::error::OpenError;
use crate::flush::FlushGroups;
use crate::points::Points;
use crate::queue::{Queue, QueueShared};
use crate::read::{ExtentReader, ReadCtx, ReadPool};
use crate::recover::{recover, RecoveryReport};
use crate::stats::Counters;

pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// The background thread's period while a GC pass has work it can do now
/// (`Inner::gc_busy`): each tick runs one budgeted step, so the collector
/// gets most of the coordinator's time while the cuts still come between
/// its steps.
pub(crate) const GC_BUSY_PERIOD: Duration = Duration::from_millis(10);

/// The error of every call after a failure stopped the commits.
pub(crate) fn stopped() -> Error {
    Error::Io(std::io::Error::other(
        "the engine stopped committing after a failure",
    ))
}

/// The collector's last published status, readable without the engine: the
/// daemon keeps one per attached volume for `GcStatus`, so a status call
/// never holds (or becomes the last owner of) an engine.
#[derive(Clone, Debug)]
pub struct GcStatusHandle(Arc<Mutex<GcStatusSnapshot>>);

impl GcStatusHandle {
    /// The status now.
    pub fn get(&self) -> GcStatusSnapshot {
        lock(&self.0).clone()
    }
}

/// What a coordinator run did.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CommitOutcome {
    /// Nothing to cut (an idle volume cuts nothing).
    Idle,
    /// A FULL commit is durable.
    Committed {
        /// Its `commit_seq`.
        commit_seq: CommitSeq,
        /// Its cut.
        cut_seq: WriteSeq,
    },
}

/// State shared by the queues, the coordinator and the helpers.
pub(crate) struct Inner {
    pub(crate) cfg: EngineConfig,
    pub(crate) dir: Arc<dyn VolumeDir>,
    pub(crate) segments: Arc<dyn SegmentStore>,
    pub(crate) packs: Arc<dyn PackStore>,
    pub(crate) commits: Arc<dyn CommitStore>,
    pub(crate) checkpoint: Arc<dyn CheckpointStore>,
    pub(crate) pins: Arc<dyn PinSource>,
    pub(crate) gc_state: Arc<dyn GcStateStore>,
    pub(crate) floor: Arc<dyn RollbackFloorStore>,
    pub(crate) journal: Arc<dyn Journal>,
    pub(crate) overlay: Arc<dyn Overlay>,
    pub(crate) index: Arc<dyn ChunkIndex>,
    pub(crate) extents: Arc<dyn ExtentCodec>,
    pub(crate) commit_codec: Arc<dyn CommitCodec>,
    pub(crate) space: Arc<dyn SpaceGate>,
    pub(crate) clock: Arc<dyn Clock>,
    pub(crate) destager: Arc<dyn Destager>,
    pub(crate) map: Arc<dyn BlockMap>,
    pub(crate) size: AtomicU64,
    pub(crate) stats: Arc<Counters>,
    pub(crate) points: Points,
    pub(crate) queues: Mutex<Vec<Option<Arc<QueueShared>>>>,
    pub(crate) flush: Arc<FlushGroups>,
    pub(crate) pool: ReadPool,
    pub(crate) cache: Arc<ChunkCache>,
    pub(crate) reader: Arc<ExtentReader>,
    /// Signalled when a queue retires an in-flight write (C1 waits on it).
    pub(crate) progress: (Mutex<()>, Condvar),
    /// The lowest `write_seq` whose append failed (`u64::MAX`: none). No
    /// cut ever reaches it: that would skip a missing write.
    pub(crate) poisoned_at: AtomicU64,
    /// A failure inside C3..C6 left the in-memory map ahead of the durable
    /// state: no more commits and no more writes in this process.
    pub(crate) failed: AtomicBool,
    /// The `cut_seq` of the durable tip: every write at or below it is in
    /// the map, so a journal read of such a record may fall back to the map.
    pub(crate) durable_cut: AtomicU64,
    /// A write found the journal ring full (a cut is wanted).
    pub(crate) journal_full: AtomicBool,
    /// `JournalReserved` bytes the engine holds (admissions minus reclaims).
    pub(crate) journal_charged: AtomicU64,
    pub(crate) coord: Mutex<Coord>,
    /// The collector's status after its last call (`GcControl::status` and
    /// [`GcStatusHandle`] read it without the coordinator mutex).
    pub(crate) gc_status: Arc<Mutex<GcStatusSnapshot>>,
    /// `quiesce` was called: the engine is about to be dropped (detach, a
    /// quiesce handover). One-way: after it no collector call starts (a
    /// requested pass ends `Closed`, the ladder's urgent pass frees nothing,
    /// a commit that fails for space runs no urgent pass), also when the
    /// final commit failed and the engine lives on (the device refuses I/O
    /// then). Read under the coordinator mutex by every GC entry; `quiesce`
    /// sets it before it takes that mutex.
    pub(crate) closing: AtomicBool,
    /// What a requested GC pass waits on between steps; `quiesce` notifies it
    /// (under its mutex, after setting `closing`).
    pub(crate) gc_wake: (Mutex<()>, Condvar),
    /// Requested passes being waited for (`GcStatusSnapshot::requests`).
    pub(crate) gc_requests: AtomicU64,
    /// The collector's last step left its pass with work it can do now
    /// (not idle, not in its grace): the background thread ticks again after
    /// [`GC_BUSY_PERIOD`] instead of its period.
    pub(crate) gc_busy: AtomicBool,
    pub(crate) resize_hook: Mutex<Option<Arc<dyn ResizeHook>>>,
    pub(crate) report: RecoveryReport,
}

impl Inner {
    pub(crate) fn size(&self) -> u64 {
        self.size.load(Ordering::Acquire)
    }

    pub(crate) fn is_failed(&self) -> bool {
        self.failed.load(Ordering::Acquire) || self.points.is_dead()
    }

    /// Writes are refused (EIO) once the volume is read-only or failed.
    pub(crate) fn read_only(&self) -> bool {
        self.is_failed()
            || self.space.is_read_only()
            || self.poisoned_at.load(Ordering::Acquire) != u64::MAX
    }

    /// A fatal journal failure at `seq` (DESIGN §3.1; the journal poisons
    /// itself, the volume goes read-only).
    pub(crate) fn poison(&self, seq: u64, what: &str) {
        self.poisoned_at.fetch_min(seq, Ordering::AcqRel);
        self.space.fatal_io(what);
        tracing::error!(
            write_seq = seq,
            what,
            "journal failure: the volume is read-only"
        );
    }

    /// Wakes the coordinator waiting in C1.
    pub(crate) fn notify_progress(&self) {
        let _g = lock(&self.progress.0);
        self.progress.1.notify_all();
    }

    /// Signals every queue (parked writes may fit now).
    pub(crate) fn signal_queues(&self) {
        for q in lock(&self.queues).iter().flatten() {
            q.signal();
        }
    }

    fn all_queues(&self) -> Vec<Arc<QueueShared>> {
        lock(&self.queues).iter().flatten().cloned().collect()
    }

    /// C1: waits until no write with `write_seq <= c` is in flight on any
    /// queue (reserved but not yet in the overlay). New writes are not
    /// stalled. A queue registers a sequence number under its in-flight
    /// lock together with `try_reserve`, so a number `<= c` handed out
    /// before `c` was read is always visible here.
    pub(crate) fn wait_inflight(&self, c: u64) -> Result<()> {
        let start = Instant::now();
        // The check runs under the progress lock and a queue notifies under
        // it after removing a number, so no wake-up is lost; the timeout is
        // only the give-up bound.
        let mut g = lock(&self.progress.0);
        loop {
            let busy = self.all_queues().iter().any(|q| q.has_inflight_upto(c));
            if !busy {
                return Ok(());
            }
            if start.elapsed() > self.cfg.inflight_timeout {
                return Err(Error::Busy {
                    what: "writes in flight at the cut",
                });
            }
            g = self
                .progress
                .1
                .wait_timeout(g, Duration::from_millis(100))
                .unwrap_or_else(|p| p.into_inner())
                .0;
        }
    }
}

/// The stop flag of the background coordinator and its thread.
type Background = (Arc<(Mutex<bool>, Condvar)>, JoinHandle<()>);

/// The block engine of one volume.
pub struct Engine {
    inner: Arc<Inner>,
    bg: Mutex<Option<Background>>,
}

impl std::fmt::Debug for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engine")
            .field("size", &self.inner.size())
            .finish_non_exhaustive()
    }
}

impl Engine {
    /// Opens the volume: verifies the tip and its chain, checks the rollback
    /// floor (R4), sweeps orphans, catches the index up, opens the map,
    /// recovers the journal from the tip's cut and rebuilds the overlay
    /// (DESIGN §4.4). The caller already holds the volume lock (`VolumeDir`),
    /// read the descriptor and ran the attach check (`check_attach`).
    /// Lanes are opened later, by `queue_handler`; the first one wipes the
    /// journal's stale tail, after the damage this returns was logged.
    ///
    /// # Errors
    /// The typed mount errors of [`OpenError`].
    pub fn open(cfg: EngineConfig, mut deps: EngineDeps) -> std::result::Result<Self, OpenError> {
        if !cfg.chunk_size.is_power_of_two() || !(16 << 10..=1 << 20).contains(&cfg.chunk_size) {
            return Err(OpenError::Config("chunk size"));
        }
        let points = Points::new(Arc::clone(&deps.hooks));
        let collector = deps.collector.take();
        let rec = recover(&cfg, &deps, &points)?;
        let stats = Arc::new(Counters::default());
        if let Some(s) = &rec.stats {
            stats.restore(s);
        }
        let reader = Arc::new(ExtentReader {
            segments: Arc::clone(&deps.segments),
            index: Arc::clone(&deps.index),
            codec: Arc::clone(&deps.extents),
            chunk_size: cfg.chunk_size,
            verify_reads: cfg.verify_reads,
            stats: Arc::clone(&stats),
        });
        let cache = Arc::new(ChunkCache::new(cfg.read_cache_max));
        let pool = ReadPool::new(
            Arc::new(ReadCtx {
                map: Arc::clone(&rec.map),
                reader: Arc::clone(&reader),
                cache: Arc::clone(&cache),
            }),
            cfg.read_helpers,
        );
        let lanes = usize::from(deps.journal.lanes());
        let gc_status = collector.as_ref().map(|c| c.status()).unwrap_or_default();
        let coord = Coord::new(&rec, collector, deps.clock.monotonic());
        let inner = Arc::new(Inner {
            size: AtomicU64::new(rec.tip.size),
            map: rec.map,
            dir: deps.dir,
            segments: deps.segments,
            packs: deps.packs,
            commits: deps.commits,
            checkpoint: deps.checkpoint,
            pins: deps.pins,
            gc_state: deps.gc_state,
            floor: deps.floor,
            journal: deps.journal,
            overlay: deps.overlay,
            index: deps.index,
            extents: deps.extents,
            commit_codec: deps.commit_codec,
            space: deps.space,
            clock: deps.clock,
            destager: deps.destager,
            stats,
            points,
            queues: Mutex::new(vec![None; lanes]),
            flush: Arc::new(FlushGroups::default()),
            pool,
            cache,
            reader,
            progress: (Mutex::new(()), Condvar::new()),
            poisoned_at: AtomicU64::new(u64::MAX),
            failed: AtomicBool::new(false),
            journal_full: AtomicBool::new(false),
            durable_cut: AtomicU64::new(rec.tip.cut_seq.0),
            journal_charged: AtomicU64::new(rec.journal_charged),
            coord: Mutex::new(coord),
            gc_status: Arc::new(Mutex::new(gc_status)),
            closing: AtomicBool::new(false),
            gc_wake: (Mutex::new(()), Condvar::new()),
            gc_requests: AtomicU64::new(0),
            gc_busy: AtomicBool::new(false),
            resize_hook: Mutex::new(None),
            report: rec.report,
            cfg,
        });
        let engine = Self {
            inner,
            bg: Mutex::new(None),
        };
        if engine.inner.cfg.background {
            engine.start_background();
        }
        Ok(engine)
    }

    fn start_background(&self) {
        let stop = Arc::new((Mutex::new(false), Condvar::new()));
        let weak = Arc::downgrade(&self.inner);
        let period = self.inner.cfg.background_period;
        let s = Arc::clone(&stop);
        let handle = std::thread::Builder::new()
            .name("lunix-coord".into())
            .spawn(move || loop {
                // A pass with work left steps again soon: at one 50 ms step
                // per period the collector would get a third of its budget
                // and fall behind a sustained overwrite (milestone gcrate).
                let wait = weak.upgrade().map_or(period, |inner| {
                    if inner.gc_busy.load(Ordering::Acquire) {
                        period.min(GC_BUSY_PERIOD)
                    } else {
                        period
                    }
                });
                {
                    let g = lock(&s.0);
                    let (g, _) = s.1.wait_timeout(g, wait).unwrap_or_else(|p| p.into_inner());
                    if *g {
                        return;
                    }
                }
                let Some(inner) = weak.upgrade() else {
                    return;
                };
                if inner.is_failed() {
                    continue;
                }
                inner.background_tick();
            })
            .expect("spawn the coordinator thread");
        *lock(&self.bg) = Some((stop, handle));
    }

    /// What startup recovery found.
    pub fn recovery_report(&self) -> &RecoveryReport {
        &self.inner.report
    }

    /// Cuts and commits now (an explicit request). With `force` a FULL
    /// commit is written even when nothing changed. Blocks; never call it
    /// from a queue thread.
    ///
    /// # Errors
    /// Whatever the cut or commit reports.
    pub fn commit_now(&self, force: bool) -> Result<CommitOutcome> {
        let mut co = lock(&self.inner.coord);
        self.inner.commit(&mut co, force, None)
    }

    /// Checks the commit triggers (`T`, `B`, journal fill, a full ring,
    /// pending `moved` records) and commits when one fires. What the
    /// background thread runs; tests with a manual clock call it directly.
    ///
    /// # Errors
    /// Whatever the cut or commit reports.
    pub fn tick(&self) -> Result<CommitOutcome> {
        self.inner.tick()
    }

    /// The verified tip: `commit_seq`, its cut and the rollback floor the
    /// engine last wrote.
    pub fn tip(&self) -> (CommitSeq, WriteSeq) {
        let co = lock(&self.inner.coord);
        (co.tip.common.commit_seq, co.tip.cut_seq)
    }

    /// The tip's `created_at` (the writer clamp of D17), for status displays.
    pub fn tip_created_at(&self) -> lunix_format::TimeMs {
        lock(&self.inner.coord).tip.common.created_at
    }

    /// True once a test hook stopped the engine at a crash point, or a
    /// failure inside a cut made it give up.
    pub fn is_stopped(&self) -> bool {
        self.inner.is_failed()
    }

    /// The collector's status, readable after the engine is gone (see
    /// [`GcStatusHandle`]).
    pub fn gc_status_handle(&self) -> GcStatusHandle {
        GcStatusHandle(Arc::clone(&self.inner.gc_status))
    }

    /// The `JournalReserved` bytes the engine believes it holds.
    pub fn journal_charged(&self) -> u64 {
        self.inner.journal_charged.load(Ordering::Acquire)
    }

    /// Empties the decoded-chunk read cache (bounded by `read_cache_max`) and
    /// returns the bytes it held. For an idle volume: the daemon calls it
    /// before it trims the heap, so a volume that ran a workload comes back
    /// to its fresh resident size (M12a memory gate); the next reads refill
    /// the cache from the store. Never blocks on I/O.
    pub fn release_read_cache(&self) -> u64 {
        self.inner.cache.clear()
    }

    fn stop_threads(&self) {
        if let Some((stop, h)) = lock(&self.bg).take() {
            *lock(&stop.0) = true;
            stop.1.notify_all();
            let _ = h.join();
        }
        self.inner.pool.shutdown();
    }
}

/// The daemon's way to the collector (the control socket's `Gc` and
/// `GcStatus`, the space ladder's compact step); see the `gc` module.
impl GcControl for Engine {
    /// Blocks until the pass ends (it waits out the grace) or, for a dry run,
    /// until the mark is done. Never call it from a queue thread.
    fn run_pass(&self, dry_run: bool) -> Result<GcPassSummary> {
        self.inner.run_pass(dry_run)
    }

    /// Blocks for the urgent pass. Never call it from a queue thread.
    fn run_urgent(&self, want_bytes: u64) -> Result<u64> {
        self.inner.run_urgent(want_bytes)
    }

    fn status(&self) -> GcStatusSnapshot {
        lock(&self.inner.gc_status).clone()
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.stop_threads();
    }
}

impl BlockEngine for Engine {
    fn size_bytes(&self) -> u64 {
        self.inner.size()
    }

    fn queue_handler(&self, cfg: &QueueConfig) -> Result<Box<dyn QueueHandler>> {
        let lanes = self.inner.journal.lanes();
        if cfg.nr_queues != lanes || cfg.queue.0 >= lanes {
            return Err(Error::invalid(
                "queue number does not match the journal lanes",
            ));
        }
        let idx = usize::from(cfg.queue.0);
        if lock(&self.inner.queues)
            .get(idx)
            .is_some_and(Option::is_some)
        {
            return Err(Error::invalid("a handler for this queue exists"));
        }
        let wake = WakeFd::new()?;
        let lane = self
            .inner
            .journal
            .open_lane(LaneId(cfg.queue.0), wake.clone())?;
        let shared = Arc::new(QueueShared::new(cfg.queue, wake));
        if let Some(slot) = lock(&self.inner.queues).get_mut(idx) {
            *slot = Some(Arc::clone(&shared));
        }
        let max_buf = cfg.max_io_buf_bytes.min(self.inner.cfg.max_io_buf_bytes);
        Ok(Box::new(Queue::new(
            Arc::clone(&self.inner),
            shared,
            lane,
            max_buf,
        )))
    }

    fn grow(&self, new_size_bytes: u64) -> Result<()> {
        let cs = u64::from(self.inner.cfg.chunk_size);
        let cur = self.inner.size();
        if new_size_bytes < cur {
            return Err(Error::Unsupported { what: "shrink" });
        }
        if !new_size_bytes.is_multiple_of(cs) {
            return Err(Error::invalid("size is not a multiple of the chunk size"));
        }
        if new_size_bytes == cur {
            return Ok(());
        }
        {
            let mut co = lock(&self.inner.coord);
            self.inner.commit(&mut co, true, Some(new_size_bytes))?;
        }
        let hook = lock(&self.inner.resize_hook).clone();
        match hook {
            Some(h) => h.resized(new_size_bytes),
            None => Ok(()),
        }
    }

    fn set_on_resize(&self, hook: Arc<dyn ResizeHook>) {
        *lock(&self.inner.resize_hook) = Some(hook);
    }

    /// Also ends GC on this engine for good (see `Inner::closing`): a
    /// requested pass waiting between its steps wakes and ends `Closed`, and
    /// no collector call starts after the final commit.
    fn quiesce(&self) -> Result<()> {
        // Before the coordinator mutex: every GC entry checks the flag under
        // that mutex, so none starts after the final commit below.
        self.inner.closing.store(true, Ordering::Release);
        {
            let _g = lock(&self.inner.gc_wake.0);
            self.inner.gc_wake.1.notify_all();
        }
        let mut co = lock(&self.inner.coord);
        self.inner.commit(&mut co, false, None).map(|_| ())
    }

    fn stats(&self) -> EngineStats {
        let mut s = self.inner.stats.snapshot();
        s.dirty_leaves = self.inner.map.dirty_leaf_count();
        s
    }
}
