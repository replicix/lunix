//! The cut: jobs, the worker pool, the in-order packer and the map update.

use std::ops::Range;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use lunix_format::WriteSeq;
use lunix_iface::map::{ChunkNo, TxnReport};
use lunix_iface::overlay::OverlayCut;
use lunix_iface::{Error, ErrorKind, Result};

use crate::compress::Pressure;
use crate::deps::DestageDeps;
use crate::outcome::{classify, is_fatal_host_error, DestageError, DestageOutcome, Partial};
use crate::pack::{MapOp, Packer};
use crate::stats::{ns, DestageStats};
use crate::worker::{lock, stopped, Claims, Failure, JobResult, Worker};

struct PoolState {
    results: Vec<Option<JobResult>>,
    /// Jobs the packer has consumed; workers do not run more than
    /// `result_window` jobs ahead of it.
    collected: usize,
    error: Option<Failure>,
}

struct Pool<'a> {
    deps: &'a DestageDeps<'a>,
    chunks: &'a [lunix_iface::overlay::CutChunk],
    cut_seq: WriteSeq,
    pressure: Pressure,
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

    fn fail(&self, f: Failure) {
        let mut g = lock(&self.st);
        if g.error.is_none() {
            g.error = Some(f);
        }
        self.failed.store(true, Ordering::Release);
        self.cv.notify_all();
    }

    fn run_worker(&self) {
        // A panicking worker must not leave the packer waiting for ever.
        struct Guard<'g, 'p>(&'g Pool<'p>);
        impl Drop for Guard<'_, '_> {
            fn drop(&mut self) {
                if thread::panicking() {
                    self.0
                        .fail(Failure::general(Error::invalid("destage worker panicked")));
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
            match worker.job(&self.chunks[range], self.cut_seq, self.pressure, &mut stats) {
                Ok(chunks) => {
                    let mut g = lock(&self.st);
                    g.results[idx] = Some(JobResult { chunks, stats });
                    self.cv.notify_all();
                }
                Err(f) => {
                    lock(&self.err_stats).add(&stats);
                    self.fail(f);
                    return;
                }
            }
        }
    }

    /// The result of job `idx`, waiting for a worker.
    fn wait_job(&self, idx: usize) -> std::result::Result<JobResult, Failure> {
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
                return Err(Failure::general(Error::Closed));
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
/// the commit. On success every segment the cut appended to is **sealed** and
/// the map holds the new entries (see [`DestageOutcome`] for what the
/// coordinator does next). On failure nothing was applied to the map and the
/// segments the cut sealed are deleted (see [`DestageError`]); a crash instead
/// leaves them for the open-time sweep or GC.
///
/// Before any extent is stored the map is probed with a throw-away
/// transaction holding a `set_hole` for every chunk of the cut, so a cut that
/// cannot fit the map's dirty-leaf cap fails `MapFull` having written nothing.
/// The coordinator must bound the number of distinct leaves per cut; see
/// [`ErrorClass::CutTooLarge`](crate::ErrorClass::CutTooLarge).
///
/// # Errors
/// Any storage error of a chunk fails the cut ([`DestageError::class`] says
/// what to do). `ReadOnly` when the space gate is read-only; `Invalid` for a
/// malformed cut (chunks not ascending, beyond the map, pieces outside their
/// chunk or above `cut_seq`).
///
/// # Panics
/// A panic in a worker thread (a bug) propagates to the caller after the pool
/// has stopped, as `std::thread::scope` does. The coordinator decides whether
/// to catch it or abort the process; nothing is applied to the map in either
/// case.
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
///
/// # Panics
/// As for [`destage_cut`].
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
            Ok(DestageOutcome {
                cut_seq: cut.cut_seq,
                segments: packer.finish(),
                map,
                stats,
            })
        }
        Err(f) => {
            if is_fatal_host_error(&f.error) {
                deps.space.fatal_io("destage");
            }
            let class = classify(&f.error, deps.map.dirty_leaf_count() > 0);
            // Nothing durable names the segments this cut sealed: delete them
            // now rather than leave them to the next open (best effort).
            let segments = packer.finish();
            let mut freed_bytes = 0;
            for s in &segments {
                if let Ok(n) = deps.segments.delete(s.seg) {
                    freed_bytes += n;
                }
            }
            stats.wall_ns = ns(started.elapsed());
            Err(DestageError {
                error: f.error,
                class,
                chunk: f.chunk,
                partial: Box::new(Partial {
                    segments,
                    freed_bytes,
                    stats,
                }),
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
    let count = deps.map.chunk_count();
    let mut prev = None;
    for c in &cut.chunks {
        if prev.is_some_and(|p| c.chunk <= p) {
            return Err(Error::invalid("cut chunks not strictly ascending"));
        }
        if c.chunk >= count {
            return Err(Error::invalid("cut chunk beyond the map"));
        }
        prev = Some(c.chunk);
    }
    Ok(())
}

/// Asks the map whether it can take an update for every chunk of the cut,
/// before anything is stored.
fn probe_map(cut: &OverlayCut, deps: &DestageDeps<'_>) -> Result<()> {
    let mut txn = deps.map.begin_update()?;
    for c in &cut.chunks {
        txn.set_hole(ChunkNo(c.chunk))?;
    }
    // Dropped without commit: discarded.
    drop(txn);
    Ok(())
}

fn run(
    cut: &OverlayCut,
    deps: &DestageDeps<'_>,
    cancel: Option<&AtomicBool>,
    packer: &mut Packer<'_>,
    totals: &mut DestageStats,
) -> std::result::Result<TxnReport, Failure> {
    check_cut(cut, deps).map_err(Failure::general)?;
    if cut.chunks.is_empty() {
        return Ok(TxnReport::default());
    }
    probe_map(cut, deps).map_err(Failure::general)?;
    let jobs = plan_jobs(
        cut.chunks.len(),
        deps.config.chunks_per_job(deps.params.chunk_size),
    );
    let workers = deps.config.workers.min(jobs.len());
    let cut_bytes = (cut.chunks.len() as u64).saturating_mul(u64::from(deps.params.chunk_size));
    let pool = Pool {
        deps,
        chunks: &cut.chunks,
        cut_seq: cut.cut_seq,
        pressure: deps.config.pressure_for(cut_bytes),
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

    let packed: std::result::Result<(), Failure> = thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| pool.run_worker());
        }
        let stop = || pool.stop();
        let take = |slot: &crate::worker::SealSlot| slot.take(&stop);
        let mut out = Ok(());
        for idx in 0..pool.jobs.len() {
            let step = pool.wait_job(idx).and_then(|job| {
                totals.add(&job.stats);
                job.chunks.into_iter().try_for_each(|c| {
                    let ci = c.chunk;
                    packer.push(c, &take).map_err(|e| Failure::at(e, ci))
                })
            });
            if let Err(f) = step {
                // A packer that gave up because the cut was stopped reports the
                // error that stopped it, not the stop.
                let cause = lock(&pool.st).error.take();
                // Wake the workers: they stop at their next check.
                pool.fail(Failure::general(Error::Closed));
                out = Err(match cause {
                    Some(c) if f.error.kind() == ErrorKind::Closed => c,
                    _ => f,
                });
                break;
            }
        }
        out
    });
    totals.add(&lock(&pool.err_stats));
    packed?;

    // The last segment is sealed before the map commit, so no map entry ever
    // names a file that is still open (DESIGN section 5 step 7: seal at a
    // commit cut).
    packer.seal_current().map_err(Failure::general)?;

    // Step 8: the map update, all or nothing.
    let t = Instant::now();
    let ops = packer.take_ops();
    let commit = || -> Result<TxnReport> {
        let mut txn = deps.map.begin_update()?;
        for op in ops {
            match op {
                MapOp::Set(ci, e) => txn.set(ci, e)?,
                MapOp::Hole(ci) => txn.set_hole(ci)?,
            }
        }
        txn.commit()
    };
    let report = commit().map_err(Failure::general)?;
    totals.map_ns += ns(t.elapsed());
    Ok(report)
}
