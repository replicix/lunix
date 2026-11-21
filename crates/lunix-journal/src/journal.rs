//! [`FileJournal`]: the `Journal` implementation over ring files.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use lunix_format::local::CheckpointSlot;
use lunix_format::WriteSeq;
use lunix_iface::journal::{
    FlushDone, Journal, JournalFill, JournalLane, JournalLoc, JournalTail, LaneId, ReclaimReport,
    Recovered,
};
use lunix_iface::wake::WakeFd;
use lunix_iface::{Error, Result};

use crate::aligned::AlignedBuf;
use crate::config::JournalConfig;
use crate::coord;
use crate::lane::FileLane;
use crate::ring::{io_err, lock, open_rings, LaneRing, Reinit, RingFile, RingKind, Role};
use crate::scan::{self, LanePlan, Plan};
use crate::shared::{Coord, LaneFlight, MarkDue, Shared, State, NOT_POISONED};
use crate::sys::{self, B, ZERO_LEN};
use crate::window::{Window, WindowStats};

/// The journal of one volume: lanes over preallocated `O_DIRECT` ring files,
/// a flush coordinator thread, recovery and reclaim.
pub struct FileJournal {
    sh: Arc<Shared>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl FileJournal {
    /// Opens (creating the missing ring files of) the journal directory and
    /// starts the flush coordinator. Existing ring files are only read, but
    /// `open` is **not read-only**: it creates missing files, removes
    /// interrupted `*.j.tmp` files, creates the directory and fsyncs it. A
    /// read-only caller (fsck) must not use it on a directory it must not
    /// touch. Call [`Journal::recover`] next.
    ///
    /// # Preconditions on later calls
    /// `recover`'s `floor` must be at least every cut passed to `tail_after`
    /// whose tail was passed to `reclaim`: a lower floor sees a gap where
    /// reclaimed records were, and the first `open_lane` then wipes every later
    /// record, acknowledged ones included. The wipe is irreversible and is
    /// also triggered by `reclaim`; callers that must preserve records beyond
    /// the stop point call neither.
    ///
    /// # Errors
    /// `Invalid` for a bad configuration, `Unsupported` when the directory has
    /// files of a lane beyond `lanes`, `NoSpace` or `Io` while creating files.
    pub fn open(cfg: JournalConfig) -> Result<Self> {
        cfg.validate()?;
        let mut zero = AlignedBuf::zeroed(ZERO_LEN).map_err(Error::Io)?;
        let zero_locked = if cfg.lock_memory {
            zero.lock().map_err(Error::Io)?
        } else {
            0
        };
        let rings = open_rings(&cfg, &zero)?;
        let lanes = usize::from(cfg.lanes);
        let window = Window::new(lanes, cfg.resident_cap());
        let sh = Arc::new(Shared {
            cfg,
            rings,
            counter: AtomicU64::new(0),
            flushed: AtomicU64::new(0),
            witnessed: AtomicU64::new(0),
            marks_written: AtomicU64::new(0),
            marks_skipped: AtomicU64::new(0),
            mark_skipped_at: AtomicU64::new(0),
            mark_ring_full: AtomicBool::new(false),
            last_mark: AtomicU64::new(0),
            locked: AtomicU64::new(zero_locked),
            poison_at: AtomicU64::new(NOT_POISONED),
            flights: (0..lanes).map(|_| Mutex::new(LaneFlight::new())).collect(),
            coord: Coord::new(),
            st: Mutex::new(State {
                plan: None,
                prepared: false,
                lane_open: vec![false; lanes],
                floor: 0,
            }),
            closed: AtomicBool::new(false),
            max_cut_asked: AtomicU64::new(0),
            zero,
            window,
            fail_next: Mutex::new(BTreeSet::new()),
            fail_any: AtomicBool::new(false),
            fail_sync: AtomicBool::new(false),
            fail_write: AtomicBool::new(false),
            wakes: (0..lanes).map(|_| Mutex::new(None)).collect(),
            reclaim_hook: Mutex::new(None),
            free_hook: Mutex::new(None),
            read_gap_hook: Mutex::new(None),
            read_gap_on: AtomicBool::new(false),
            fail_read_submit: AtomicBool::new(false),
            drain_limit_ms: AtomicU64::new(crate::shared::DRAIN_LIMIT_MS),
            drain_hook: Mutex::new(None),
            fail_reclaim_nth: std::sync::atomic::AtomicU32::new(0),
            freed: (0..lanes).map(|_| AtomicU64::new(0)).collect(),
        });
        let worker = Arc::clone(&sh);
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
        let handle = std::thread::Builder::new()
            .name("lunix-journal-flush".into())
            .spawn(move || coord::run(worker, ready_tx))
            .map_err(Error::Io)?;
        let j = Self {
            sh,
            thread: Mutex::new(Some(handle)),
        };
        // The flush thread locks its own memory first (lock_memory).
        match ready_rx.recv() {
            Ok(Ok(())) => Ok(j),
            Ok(Err(e)) => Err(Error::Io(e)),
            Err(_) => Err(Error::Io(std::io::Error::other(
                "the journal flush thread ended at start",
            ))),
        }
    }

    /// Bytes the journal holds locked (`JournalConfig::lock_memory`): the
    /// open lanes' batch buffers, the zero-fill buffer, the flush thread's
    /// stack (locked as touched) and its mark buffer.
    pub fn locked_bytes(&self) -> u64 {
        self.sh.locked.load(Ordering::Acquire)
    }

    /// How many ring files (of the lanes and of the mark ring) were
    /// re-initialised so far by `FALLOC_FL_ZERO_RANGE` and by rewriting
    /// zeros: `(range, rewrite)`.
    pub fn reinit_counts(&self) -> (u64, u64) {
        let r = &self.sh.rings;
        (
            r.zeroed_by_range.load(Ordering::Relaxed),
            r.zeroed_by_rewrite.load(Ordering::Relaxed),
        )
    }

    /// Test hook: `FALLOC_FL_ZERO_RANGE` calls fail with this errno (`None`:
    /// they do not), and how many were made so far.
    #[doc(hidden)]
    pub fn inject_zero_range_errno(&self, errno: Option<i32>) {
        self.sh
            .rings
            .zero_inject_errno
            .store(errno.unwrap_or(0), Ordering::Relaxed);
    }

    /// The errnos `FALLOC_FL_ZERO_RANGE` failed with so far (each once).
    #[doc(hidden)]
    pub fn zero_range_errnos(&self) -> Vec<i32> {
        crate::ring::lock(&self.sh.rings.zero_logged)
            .iter()
            .copied()
            .collect()
    }

    /// `FALLOC_FL_ZERO_RANGE` calls made so far (failed ones included).
    #[doc(hidden)]
    pub fn zero_range_attempts(&self) -> u64 {
        self.sh.rings.zero_attempts.load(Ordering::Relaxed)
    }

    /// Test hook: the next `append` on `lane` fails with an I/O error after
    /// its slot was consumed, as a failed write would.
    #[doc(hidden)]
    pub fn fail_next_append(&self, lane: LaneId) {
        lock(&self.sh.fail_next).insert(lane.0);
        self.sh.fail_any.store(true, Ordering::Relaxed);
    }

    /// Test hook: `f` is called, with the location asked, by a `read_at`
    /// read-around after its window was read and before anything of it is
    /// indexed. It may block the reader there.
    #[doc(hidden)]
    pub fn set_read_hook(&self, f: Option<std::sync::Arc<dyn Fn(JournalLoc) + Send + Sync>>) {
        self.sh.window.set_read_hook(f);
    }

    /// Test hook: puts `bytes` into the window at `(lane, ordinal, offset)`
    /// as a batch copy would be, within the cap.
    ///
    /// # Panics
    /// If the cap has no room.
    #[doc(hidden)]
    pub fn plant_window_entry(&self, lane: LaneId, ordinal: u32, start: u64, bytes: &[u8]) {
        self.sh
            .window
            .plant(lane.0, ordinal, start, std::sync::Arc::from(bytes));
    }

    /// Test hook: the next batch write completes as a failed one (`EIO`), as
    /// a device error would; the journal is poisoned at its first record.
    #[doc(hidden)]
    pub fn fail_next_batch_write(&self) {
        self.sh.fail_write.store(true, Ordering::SeqCst);
    }

    /// Test hook: the next group flush that has files to sync reports a failed
    /// `fdatasync`.
    #[doc(hidden)]
    pub fn fail_next_sync(&self) {
        self.sh.fail_sync.store(true, Ordering::SeqCst);
    }

    /// Test hook: the `n`-th ring file (from 1) the next `reclaim` re-initialises
    /// fails with an I/O error, as a failed write or sync would.
    #[doc(hidden)]
    pub fn fail_nth_reclaim_file(&self, n: u32) {
        self.sh
            .fail_reclaim_nth
            .store(n, std::sync::atomic::Ordering::SeqCst);
    }

    /// Test hook: `f(loc)` is called by a lane's `submit_read` after it found
    /// the file in use and before the read is pinned and queued; reclaim may
    /// run to completion in it.
    #[doc(hidden)]
    pub fn set_read_gap_hook(&self, f: Option<std::sync::Arc<dyn Fn(JournalLoc) + Send + Sync>>) {
        self.sh.read_gap_on.store(f.is_some(), Ordering::Release);
        *lock(&self.sh.read_gap_hook) = f;
    }

    /// Test hook: the next read a lane queues fails its `submit` after the SQE
    /// was pushed, as `EAGAIN` or `EBUSY` would.
    #[doc(hidden)]
    pub fn fail_next_read_submit(&self) {
        self.sh.fail_read_submit.store(true, Ordering::SeqCst);
    }

    /// Test hook: the time one `reclaim` call waits for reads queued on the
    /// files it takes (default 2 s).
    #[doc(hidden)]
    pub fn set_reclaim_drain_limit(&self, limit: Duration) {
        let ms = u64::try_from(limit.as_millis()).unwrap_or(u64::MAX);
        self.sh.drain_limit_ms.store(ms, Ordering::Release);
    }

    /// Test hook: `f` is called by `reclaim` when it starts waiting for reads
    /// queued on a file it is about to zero.
    #[doc(hidden)]
    pub fn set_reclaim_drain_hook(&self, f: Option<std::sync::Arc<dyn Fn() + Send + Sync>>) {
        *lock(&self.sh.drain_hook) = f;
    }

    /// Test hook: `f(lane, freed)` is called by `reclaim` right after a queue
    /// file is on the free list and the ring lock is released (before the
    /// lane is signalled), with the number of files that reclaim has freed so
    /// far; it may block there. A lane can take the file from this moment.
    #[doc(hidden)]
    pub fn set_free_hook(&self, f: Option<std::sync::Arc<dyn Fn(u16, u32) + Send + Sync>>) {
        *lock(&self.sh.free_hook) = f;
    }

    /// Test hook: `f(lane, freed)` is called by `reclaim` after each queue
    /// file it freed (and signalled its lane for), with the number of files
    /// that reclaim has freed so far; it may block there, while the other
    /// files are still pending.
    #[doc(hidden)]
    pub fn set_reclaim_hook(&self, f: Option<std::sync::Arc<dyn Fn(u16, u32) + Send + Sync>>) {
        *lock(&self.sh.reclaim_hook) = f;
    }

    /// The counters of the resident window (ADR-33): bytes resident, hits,
    /// misses, read-arounds.
    pub fn window_stats(&self) -> WindowStats {
        self.sh.window.stats()
    }

    /// Marks written since `open`: witnesses of the flushed watermark the
    /// flush coordinator appended to the mark ring (on-disk-formats §5.3).
    pub fn marks_written(&self) -> u64 {
        self.sh.marks_written.load(Ordering::Relaxed)
    }

    /// Marks skipped since `open` because the mark ring had no free file
    /// (reclaim frees them at the next cut).
    pub fn marks_skipped(&self) -> u64 {
        self.sh.marks_skipped.load(Ordering::Relaxed)
    }

    fn apply_plan(&self, plan: &Plan) {
        let rings = &self.sh.rings;
        let pairs = rings
            .lanes
            .iter()
            .zip(&plan.lanes)
            .chain(std::iter::once((&rings.marks, &plan.marks)));
        for (ring, lp) in pairs {
            let mut inner = lock(&ring.inner);
            inner.free.clear();
            for fp in &lp.files {
                inner.slots[fp.slot].role = fp.role;
                let file = &ring.files[fp.slot];
                file.max_seq.store(fp.max_seq, Ordering::Relaxed);
                file.rec_bytes.store(fp.rec_bytes, Ordering::Relaxed);
                file.hi_off.store(fp.hi_off, Ordering::Relaxed);
                if fp.role == Role::Free {
                    inner.free.push_back(fp.slot);
                }
            }
            match lp.active {
                Some((slot, off)) => {
                    inner.active = Some(slot);
                    inner.resume_off = off;
                }
                None => {
                    inner.active = None;
                    inner.resume_off = B;
                }
            }
        }
    }

    /// Executes the wipe plan: zero what lies beyond the stop point, re-initialise
    /// files with a bad header, make everything that holds records durable.
    /// Runs once per `recover`, before the first lane opens or reclaim runs.
    fn prepare(&self) -> Result<()> {
        let mut st = lock(&self.sh.st);
        if st.prepared {
            return Ok(());
        }
        let Some(plan) = st.plan.clone() else {
            return Err(Error::invalid("recover must run before lanes are opened"));
        };
        let rings = &self.sh.rings;
        let mut renamed = false;
        let pairs: Vec<(&LaneRing, &LanePlan)> = rings
            .lanes
            .iter()
            .zip(&plan.lanes)
            .chain(std::iter::once((&rings.marks, &plan.marks)))
            .collect();
        for (ring, lp) in pairs {
            let mut touched: Vec<usize> = Vec::new();
            for fp in &lp.files {
                let file = &ring.files[fp.slot];
                if fp.bad {
                    let new_ordinal = lock(&ring.inner).take_ordinal();
                    let gen = rings
                        .reinit_slot(
                            &self.sh.cfg,
                            ring,
                            file,
                            Reinit {
                                old_ordinal: fp.ordinal,
                                new_ordinal,
                                zero_to: ring.file_len,
                            },
                            &self.sh.zero,
                        )
                        .map_err(io_err)?;
                    renamed = true;
                    let mut inner = lock(&ring.inner);
                    let s = &mut inner.slots[fp.slot];
                    s.ordinal = new_ordinal;
                    s.gen = gen;
                    s.bad = false;
                    s.reinit = false;
                    drop(inner);
                    file.reset_counters();
                } else if let Some((from, to)) = fp.wipe {
                    sys::zero_range(&file.file, from, to, &self.sh.zero).map_err(io_err)?;
                    touched.push(fp.slot);
                } else if matches!(fp.role, Role::Used | Role::Active) {
                    touched.push(fp.slot);
                }
            }
            // Records that survived are made durable here: a read after a
            // crash may have been served from a volatile cache, and no later
            // group flush would cover them.
            for slot in touched {
                ring.files[slot].file.sync_data().map_err(io_err)?;
            }
        }
        if renamed {
            sys::fsync_dir(&rings.dir).map_err(io_err)?;
        }
        // Every record that survived recovery is durable now (synced above);
        // no lane is open yet, so the counter is the last recovered number.
        let counter = self.sh.counter.load(Ordering::SeqCst);
        self.sh.flushed.fetch_max(counter, Ordering::SeqCst);
        // Witness them too, unless a surviving record already does. Nothing
        // above the floor recovered: a durable commit covers it all, and a
        // mark would only take a number (and make the engine cut nothing).
        if let Some(idle) = self.sh.cfg.mark_idle {
            if counter > st.floor && counter > self.sh.witnessed.load(Ordering::SeqCst) {
                lock(&self.sh.coord.q).mark_due = Some(MarkDue {
                    flushed: counter,
                    counter_at: counter,
                    at: Instant::now() + idle,
                });
                self.sh.coord.cv.notify_all();
            }
        }
        // The wipe may have zeroed bytes a read-around made resident.
        self.sh.window.clear();
        st.prepared = true;
        Ok(())
    }

    fn require_recovered(&self) -> Result<()> {
        if lock(&self.sh.st).plan.is_some() {
            Ok(())
        } else {
            Err(Error::invalid("recover must run first"))
        }
    }
}

impl Drop for FileJournal {
    fn drop(&mut self) {
        self.sh.closed.store(true, Ordering::SeqCst);
        {
            let mut q = lock(&self.sh.coord.q);
            q.shutdown = true;
        }
        self.sh.coord.cv.notify_all();
        self.sh.coord.notify();
        if let Some(h) = lock(&self.thread).take() {
            let _ = h.join();
        }
    }
}

impl Journal for FileJournal {
    fn lanes(&self) -> u16 {
        self.sh.cfg.lanes
    }

    fn open_lane(&self, lane: LaneId, wake: WakeFd) -> Result<Box<dyn JournalLane>> {
        if lane.0 >= self.sh.cfg.lanes {
            return Err(Error::invalid("no such lane"));
        }
        if self.sh.closed.load(Ordering::SeqCst) {
            return Err(Error::Closed);
        }
        self.require_recovered()?;
        if lock(&self.sh.st).lane_open[usize::from(lane.0)] {
            return Err(Error::invalid("lane already open"));
        }
        self.prepare()?;
        let mut st = lock(&self.sh.st);
        if st.lane_open[usize::from(lane.0)] {
            return Err(Error::invalid("lane already open"));
        }
        let l = FileLane::new(Arc::clone(&self.sh), lane.0, wake.clone())?;
        *lock(&self.sh.wakes[usize::from(lane.0)]) = Some(wake);
        st.lane_open[usize::from(lane.0)] = true;
        Ok(Box::new(l))
    }

    fn last_issued(&self) -> WriteSeq {
        WriteSeq(self.sh.counter.load(Ordering::SeqCst))
    }

    fn submit_group_flush(&self, upto: WriteSeq, done: FlushDone) -> Result<()> {
        if self.sh.closed.load(Ordering::SeqCst) {
            return Err(Error::Closed);
        }
        // A flush never covers a number handed out after its submission: an
        // `upto` above it would let the coordinator raise the watermark over
        // a number not yet written (review m1, 2026-11-14).
        let upto = WriteSeq(upto.0.min(self.sh.counter.load(Ordering::SeqCst)));
        if self.sh.poisoned_at().is_some_and(|p| upto.0 >= p) {
            done(Err(Shared::poisoned_error()));
            return Ok(());
        }
        {
            let mut q = lock(&self.sh.coord.q);
            if q.shutdown {
                return Err(Error::Closed);
            }
            q.queue.push((upto.0, done));
        }
        self.sh.coord.cv.notify_all();
        Ok(())
    }

    fn read_at(&self, loc: JournalLoc, buf: &mut [u8]) -> Result<()> {
        let rings = &self.sh.rings;
        if rings.lanes.get(usize::from(loc.lane.0)).is_none() {
            return Err(Error::invalid("no such lane"));
        }
        let end = loc
            .offset
            .checked_add(buf.len() as u64)
            .ok_or(Error::invalid("journal location overflows"))?;
        if loc.offset < B || end > rings.file_len {
            return Err(Error::invalid("journal location outside the ring file"));
        }
        let (file, gen) = rings
            .find_in_use_gen(loc.lane.0, loc.ordinal)
            .ok_or(Error::not_found("journal data"))?;
        if buf.is_empty() {
            return Ok(());
        }
        let win = &self.sh.window;
        if win.copy_hit(loc, buf) {
            win.count(true);
            return Ok(());
        }
        win.count(false);
        win.read_around(rings, &file, gen, loc, buf)
    }

    fn recover(&self, hint: Option<&CheckpointSlot>, floor: WriteSeq) -> Result<Recovered> {
        let mut st = lock(&self.sh.st);
        if st.lane_open.iter().any(|&o| o) {
            return Err(Error::invalid("recover after a lane was opened"));
        }
        let (rec, plan) = scan::recover(&self.sh.rings, hint, floor)?;
        self.sh.window.clear();
        self.apply_plan(&plan);
        self.sh.counter.store(rec.next_seq.0 - 1, Ordering::SeqCst);
        // Until prepare syncs the recovered records, only the floor (covered
        // by a durable commit) is known durable.
        self.sh.flushed.store(floor.0, Ordering::SeqCst);
        // Only what survives the wipe still witnesses anything: a witness
        // beyond the first gap is zeroed by the first lane open.
        self.sh.witnessed.store(plan.kept_flushed, Ordering::SeqCst);
        lock(&self.sh.coord.q).mark_due = None;
        self.sh.poison_at.store(NOT_POISONED, Ordering::SeqCst);
        for f in &self.sh.flights {
            *lock(f) = LaneFlight::new();
        }
        self.sh.max_cut_asked.store(floor.0, Ordering::SeqCst);
        st.plan = Some(plan);
        st.prepared = false;
        st.floor = floor.0;
        Ok(rec)
    }

    fn tail_after(&self, cut: WriteSeq) -> Result<JournalTail> {
        self.require_recovered()?;
        self.sh.max_cut_asked.fetch_max(cut.0, Ordering::SeqCst);
        let mut lane_ordinals = Vec::with_capacity(self.sh.rings.lanes.len());
        for ring in &self.sh.rings.lanes {
            let inner = lock(&ring.inner);
            let lowest = inner
                .slots
                .iter()
                .enumerate()
                .filter(|(i, s)| {
                    matches!(s.role, Role::Used | Role::Active)
                        && ring.files[*i].max_seq.load(Ordering::Acquire) > cut.0
                })
                .map(|(_, s)| s.ordinal)
                .min();
            let ordinal = lowest
                .or_else(|| inner.active.map(|a| inner.slots[a].ordinal))
                .or_else(|| inner.free.front().map(|&f| inner.slots[f].ordinal))
                .unwrap_or(1);
            lane_ordinals.push(ordinal);
        }
        Ok(JournalTail { lane_ordinals })
    }

    fn reclaim(&self, upto: &JournalTail) -> Result<ReclaimReport> {
        if upto.lane_ordinals.len() != self.sh.rings.lanes.len() {
            return Err(Error::invalid("tail has the wrong lane count"));
        }
        self.require_recovered()?;
        self.prepare()?;
        let rings = &self.sh.rings;
        let cut_limit = self.sh.max_cut_asked.load(Ordering::SeqCst);
        let mut report = ReclaimReport::default();
        let mut freed_mark_file = false;
        // One deadline for the whole call: reads queued on files this call
        // takes are waited for at most this long in total.
        let deadline =
            Instant::now() + Duration::from_millis(self.sh.drain_limit_ms.load(Ordering::Acquire));
        let mut drain_timeout = false;
        // The mark ring has no tail: every mark at or below the cut is
        // covered, so any of its used files may go (`u32::MAX`).
        let tails = upto
            .lane_ordinals
            .iter()
            .copied()
            .chain(std::iter::once(u32::MAX));
        for (ring, tail) in rings.all().zip(tails) {
            struct Job {
                slot: usize,
                old: u32,
                new: u32,
                hi: u64,
                bytes: u64,
            }
            let mut jobs = Vec::new();
            {
                let mut inner = lock(&ring.inner);
                let mut cands: Vec<usize> = (0..inner.slots.len())
                    .filter(|&i| {
                        let s = &inner.slots[i];
                        let f = &ring.files[i];
                        s.role == Role::Used
                            && s.ordinal < tail
                            && f.inflight.load(Ordering::Acquire) == 0
                            && f.max_seq.load(Ordering::Acquire) <= cut_limit
                    })
                    .collect();
                cands.sort_by_key(|&i| inner.slots[i].ordinal);
                for slot in cands {
                    inner.slots[slot].role = Role::Recycling;
                    let new = inner.take_ordinal();
                    let f = &ring.files[slot];
                    jobs.push(Job {
                        slot,
                        old: inner.slots[slot].ordinal,
                        new,
                        hi: f.hi_off.load(Ordering::Acquire),
                        bytes: f.rec_bytes.load(Ordering::Acquire),
                    });
                }
            }
            if ring.kind == RingKind::Queue {
                // Out of use (`Recycling`) before its entries go, so a
                // read-around cannot make any resident again.
                for job in &jobs {
                    self.sh.window.release_file(ring.lane, job.old);
                }
            }
            let mut jobs = jobs.into_iter();
            while let Some(job) = jobs.next() {
                let file = &ring.files[job.slot];
                lunix_iface::crashpoint!("journal.before_reclaim_delete");
                // The file is out of use (`Recycling`): no read can be pinned
                // on it any more. Reads already queued finish first; zeroing
                // under them would hand the guest zeros (or the next
                // incarnation's bytes) with a success status.
                if !self.wait_reads_drained(file, deadline) {
                    // A read on this file is not reaped in time (a stuck
                    // queue thread). This file and the rest of this ring go
                    // back to `Used` untouched (the wait precedes any
                    // zeroing); the other rings go on. A later call retries.
                    let mut inner = lock(&ring.inner);
                    inner.slots[job.slot].role = Role::Used;
                    for rest in jobs.by_ref() {
                        inner.slots[rest.slot].role = Role::Used;
                    }
                    drain_timeout = true;
                    break;
                }
                let done = if self.sh.inject_reclaim_failure() {
                    Err(std::io::Error::other("injected reclaim failure"))
                } else {
                    rings
                        .reinit_slot(
                            &self.sh.cfg,
                            ring,
                            file,
                            Reinit {
                                old_ordinal: job.old,
                                new_ordinal: job.new,
                                zero_to: job.hi,
                            },
                            &self.sh.zero,
                        )
                        .and_then(|g| sys::fsync_dir(&rings.dir).map(|()| g))
                };
                let mut inner = lock(&ring.inner);
                match done {
                    Ok(gen) => {
                        let s = &mut inner.slots[job.slot];
                        s.ordinal = job.new;
                        s.gen = gen;
                        s.role = Role::Free;
                        s.dirty = false;
                        // The counters go before the slot is on the free
                        // list, under the ring lock: a lane that takes the
                        // file (the early wake of ADR-33 makes that quick)
                        // appends after the reset, never before it.
                        file.reset_counters();
                        inner.free.push_back(job.slot);
                        drop(inner);
                        // Marks are the journal's own: nothing was charged
                        // for them, so nothing is released.
                        if ring.kind == RingKind::Queue {
                            report.files += 1;
                            report.bytes += job.bytes;
                            // Counted before the lane can see the file.
                            if let Some(n) = self.sh.freed.get(usize::from(ring.lane)) {
                                n.fetch_add(1, Ordering::Release);
                            }
                            let hook = lock(&self.sh.free_hook).clone();
                            if let Some(h) = hook {
                                h(ring.lane, report.files);
                            }
                            // ADR-33: the lane is woken as soon as this file
                            // is free (zero rewrite done), not after the
                            // files of every lane.
                            self.sh.wake_lane(ring.lane);
                            let hook = lock(&self.sh.reclaim_hook).clone();
                            if let Some(h) = hook {
                                h(ring.lane, report.files);
                            }
                        } else {
                            freed_mark_file = true;
                        }
                        lunix_iface::crashpoint!("journal.after_reclaim_delete");
                    }
                    Err(e) => {
                        // This file and every candidate not yet processed go
                        // back to `Used`: left `Recycling` they would never be
                        // a candidate again (a later reclaim retries them).
                        // The zeroing may have begun: no guest read is
                        // pinned on it until a reclaim has redone it.
                        inner.slots[job.slot].role = Role::Used;
                        inner.slots[job.slot].dirty = true;
                        for rest in jobs {
                            inner.slots[rest.slot].role = Role::Used;
                        }
                        return Err(io_err(e));
                    }
                }
            }
        }
        if freed_mark_file {
            // A mark owed while the ring was full can be written now.
            let mut q = lock(&self.sh.coord.q);
            if let Some(d) = q.mark_due.as_mut() {
                d.at = Instant::now();
            }
            drop(q);
            self.sh.coord.cv.notify_all();
        }
        if drain_timeout {
            return Err(Error::Io(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "guest reads still in flight on a ring file; retried at the next cut",
            )));
        }
        Ok(report)
    }

    fn freed_files_by_lane(&self) -> Vec<u64> {
        self.sh
            .freed
            .iter()
            .map(|n| n.load(Ordering::Acquire))
            .collect()
    }

    fn cut_freeing_file(&self) -> Option<WriteSeq> {
        // (free files, highest seq of the oldest sealed file) per lane that
        // has a sealed file; the fewest free files, then the lowest seq.
        self.sh
            .rings
            .lanes
            .iter()
            .filter_map(|ring| {
                let inner = lock(&ring.inner);
                let oldest = inner
                    .slots
                    .iter()
                    .enumerate()
                    .filter(|(_, s)| s.role == Role::Used)
                    .min_by_key(|(_, s)| s.ordinal)?
                    .0;
                let seq = ring.files[oldest].max_seq.load(Ordering::Acquire);
                Some((inner.free.len(), seq))
            })
            .min()
            .map(|(_, seq)| WriteSeq(seq))
    }

    fn fill(&self) -> JournalFill {
        let len = self.sh.rings.file_len;
        let mut used = 0u64;
        let mut total = 0u64;
        for ring in &self.sh.rings.lanes {
            let inner = lock(&ring.inner);
            for (i, s) in inner.slots.iter().enumerate() {
                total += len;
                let holds = match s.role {
                    Role::Used | Role::Recycling => true,
                    Role::Active => ring.files[i].hi_off.load(Ordering::Acquire) > B,
                    Role::Free => false,
                };
                if holds {
                    used += len;
                }
            }
        }
        JournalFill {
            used_bytes: used,
            capacity_bytes: total,
            file_bytes: len,
        }
    }
}

impl FileJournal {
    /// Waits until no guest read is queued on `file`, or `deadline`: `false`
    /// on the deadline.
    fn wait_reads_drained(&self, file: &RingFile, deadline: Instant) -> bool {
        if file.reads.load(Ordering::Acquire) == 0 {
            return true;
        }
        let hook = lock(&self.sh.drain_hook).clone();
        if let Some(h) = hook {
            h();
        }
        let mut spins = 0u32;
        while file.reads.load(Ordering::Acquire) != 0 {
            if Instant::now() >= deadline {
                return false;
            }
            spins += 1;
            if spins < 64 {
                std::thread::yield_now();
            } else {
                std::thread::sleep(Duration::from_micros(100));
            }
        }
        true
    }
}
