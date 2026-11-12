//! The cut: jobs, the worker pool, the in-order packer and the map update.

use std::ops::Range;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use lunix_iface::overlay::OverlayCut;
use lunix_iface::{Error, ErrorKind, Result};

use crate::compress::Pressure;
use crate::deps::DestageDeps;
use crate::outcome::{DestageError, DestageOutcome, Partial};
use crate::pack::{MapOp, Packer};
use crate::stats::{ns, DestageStats};
use crate::worker::{lock, stopped, Claims, JobResult, Worker};

/// Linux `EIO`: a host I/O error makes the volume read-only (DESIGN section 4.3).
const EIO: i32 = 5;

struct PoolState {
    results: Vec<Option<JobResult>>,
    /// Jobs the packer has consumed; workers do not run more than
    /// `result_window` jobs ahead of it.
    collected: usize,
    error: Option<Error>,
}

struct Pool<'a> {
    deps: &'a DestageDeps<'a>,
    chunks: &'a [lunix_iface::overlay::CutChunk],
    jobs: Vec<Range<usize>>,
    next: AtomicUsize,
    failed: AtomicBool,
    external: Option<&'a AtomicBool>,
    st: Mutex<PoolState>,
    cv: Condvar,
    claims: Claims,
    /// Counters of the job that failed.
    err_stats: Mutex<DestageStats>,
}

impl Pool<'_> {
    fn stop(&self) -> bool {
        stopped(&self.failed, self.external)
    }

    fn fail(&self, e: Error) {
        let mut g = lock(&self.st);
        if g.error.is_none() {
            g.error = Some(e);
        }
        self.failed.store(true, Ordering::Release);
        self.cv.notify_all();
    }

    /// The pressure for job `idx`: jobs that have not started yet are the
    /// queue's depth. Depending on the index alone makes the decision the same
    /// on every run.
    fn pressure(&self, idx: usize) -> Pressure {
        let depth = self.jobs.len() - idx - 1;
        let c = &self.deps.config;
        if c.raw_depth.is_some_and(|d| depth > d) {
            Pressure::Raw
        } else if c.lz4_depth.is_some_and(|d| depth > d) {
            Pressure::Lz4
        } else {
            Pressure::None
        }
    }

    fn run_worker(&self) {
        // A panicking worker must not leave the packer waiting for ever.
        struct Guard<'g, 'p>(&'g Pool<'p>);
        impl Drop for Guard<'_, '_> {
            fn drop(&mut self) {
                if thread::panicking() {
                    self.0.fail(Error::invalid("destage worker panicked"));
                }
            }
        }
        let _guard = Guard(self);
        let stop = || self.stop();
        let mut worker = Worker::new(self.deps, &self.claims, &stop);
        let window = self.deps.config.result_window;
        loop {
            if self.stop() {
                return;
            }
            let idx = self.next.fetch_add(1, Ordering::Relaxed);
            if idx >= self.jobs.len() {
                return;
            }
            {
                let mut g = lock(&self.st);
                while idx >= g.collected.saturating_add(window) && !self.stop() {
                    g = self
                        .cv
                        .wait_timeout(g, Duration::from_millis(20))
                        .unwrap_or_else(|p| p.into_inner())
                        .0;
                }
            }
            if self.stop() {
                return;
            }
            let range = self.jobs[idx].clone();
            let mut stats = DestageStats::default();
            match worker.job(&self.chunks[range], self.pressure(idx), &mut stats) {
                Ok(chunks) => {
                    let mut g = lock(&self.st);
                    g.results[idx] = Some(JobResult { chunks, stats });
                    self.cv.notify_all();
                }
                Err(e) => {
                    lock(&self.err_stats).add(&stats);
                    self.fail(e);
                    return;
                }
            }
        }
    }

    /// The result of job `idx`, waiting for a worker.
    fn wait_job(&self, idx: usize) -> Result<JobResult> {
        let mut g = lock(&self.st);
        loop {
            if let Some(e) = g.error.take() {
                return Err(e);
            }
            if let Some(r) = g.results[idx].take() {
                g.collected = idx + 1;
                self.cv.notify_all();
                return Ok(r);
            }
            if self.stop() {
                return Err(Error::Closed);
            }
            g = self
                .cv
                .wait_timeout(g, Duration::from_millis(20))
                .unwrap_or_else(|p| p.into_inner())
                .0;
        }
    }
}

fn plan_jobs(n: usize, per_job: usize) -> Vec<Range<usize>> {
    (0..n)
        .step_by(per_job)
        .map(|s| s..(s + per_job).min(n))
        .collect()
}

/// Destages `cut`: the pipeline of DESIGN section 5, one map transaction at the
/// end.
///
/// `cut` is borrowed because the caller keeps it for `Overlay::release` after
/// the commit. See [`DestageOutcome`] for what the caller does next (seal the
/// open segment, insert into the index) and [`DestageError`] for failure.
///
/// # Errors
/// Any storage error of a chunk fails the cut; the map is untouched.
/// `ReadOnly` when the space gate is read-only; `Invalid` for a malformed cut;
/// `MapFull` from the map transaction (seal the map and cut again).
pub fn destage_cut(
    cut: &OverlayCut,
    deps: &DestageDeps<'_>,
) -> Result<DestageOutcome, DestageError> {
    destage_cut_with_cancel(cut, deps, None)
}

/// Like [`destage_cut`], and a set `cancel` flag ends the cut early with
/// `Closed` (nothing applied), as a shutdown needs.
///
/// # Errors
/// As for [`destage_cut`], plus `Closed` on cancellation.
pub fn destage_cut_with_cancel(
    cut: &OverlayCut,
    deps: &DestageDeps<'_>,
    cancel: Option<&AtomicBool>,
) -> Result<DestageOutcome, DestageError> {
    let started = Instant::now();
    let mut packer = Packer::new(deps.segments, deps.params.id_len);
    let mut job_stats = DestageStats::default();
    let run = run(cut, deps, cancel, &mut packer, &mut job_stats);
    let mut stats = std::mem::take(&mut packer.stats);
    stats.add(&job_stats);
    match run {
        Ok(map) => {
            stats.wall_ns = ns(started.elapsed());
            let (segments, open) = packer.finish();
            Ok(DestageOutcome {
                cut_seq: cut.cut_seq,
                segments,
                open,
                map,
                stats,
            })
        }
        Err(error) => {
            if matches!(&error, Error::Io(e) if e.raw_os_error() == Some(EIO)) {
                deps.space.fatal_io("destage");
            }
            let (segments, open) = packer.finish();
            // The open writer is abandoned; the store removes the file.
            drop(open);
            stats.wall_ns = ns(started.elapsed());
            Err(DestageError {
                error,
                partial: Box::new(Partial { segments, stats }),
            })
        }
    }
}

fn check_cut(cut: &OverlayCut, deps: &DestageDeps<'_>) -> Result<()> {
    deps.params.validate()?;
    deps.config.validate()?;
    if deps.space.is_read_only() {
        return Err(Error::ReadOnly);
    }
    let mut prev = None;
    for c in &cut.chunks {
        if prev.is_some_and(|p| c.chunk <= p) {
            return Err(Error::invalid("cut chunks not strictly ascending"));
        }
        prev = Some(c.chunk);
    }
    Ok(())
}

fn run(
    cut: &OverlayCut,
    deps: &DestageDeps<'_>,
    cancel: Option<&AtomicBool>,
    packer: &mut Packer<'_>,
    totals: &mut DestageStats,
) -> Result<lunix_iface::map::TxnReport> {
    check_cut(cut, deps)?;
    if cut.chunks.is_empty() {
        return Ok(lunix_iface::map::TxnReport::default());
    }
    let jobs = plan_jobs(
        cut.chunks.len(),
        deps.config.chunks_per_job(deps.params.chunk_size),
    );
    let workers = deps.config.workers.min(jobs.len());
    let pool = Pool {
        deps,
        chunks: &cut.chunks,
        jobs,
        next: AtomicUsize::new(0),
        failed: AtomicBool::new(false),
        external: cancel,
        st: Mutex::new(PoolState {
            results: Vec::new(),
            collected: 0,
            error: None,
        }),
        cv: Condvar::new(),
        claims: Claims::new(),
        err_stats: Mutex::new(DestageStats::default()),
    };
    lock(&pool.st).results.resize_with(pool.jobs.len(), || None);

    let packed: Result<()> = thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| pool.run_worker());
        }
        let stop = || pool.stop();
        let take = |slot: &crate::worker::SealSlot| slot.take(&stop);
        let mut out = Ok(());
        for idx in 0..pool.jobs.len() {
            let step = pool.wait_job(idx).and_then(|job| {
                totals.add(&job.stats);
                job.chunks
                    .into_iter()
                    .try_for_each(|c| packer.push(c, &take))
            });
            if let Err(e) = step {
                // A packer that gave up because the cut was stopped reports the
                // error that stopped it, not the stop.
                let cause = lock(&pool.st).error.take();
                // Wake the workers: they stop at their next check.
                pool.fail(Error::Closed);
                out = Err(match cause {
                    Some(c) if e.kind() == ErrorKind::Closed => c,
                    _ => e,
                });
                break;
            }
        }
        out
    });
    totals.add(&lock(&pool.err_stats));
    packed?;

    // Step 8: the map update, all or nothing.
    let t = Instant::now();
    let ops = packer.take_ops();
    let mut txn = deps.map.begin_update()?;
    for op in ops {
        match op {
            MapOp::Set(ci, e) => txn.set(ci, e)?,
            MapOp::Hole(ci) => txn.set_hole(ci)?,
        }
    }
    let report = txn.commit()?;
    totals.map_ns += ns(t.elapsed());
    Ok(report)
}
