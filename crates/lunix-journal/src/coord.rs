//! The flush coordinator: one thread, one `io_uring` of its own, one flush
//! group in flight at a time.
//!
//! A group forms from every queued flush. Its `upto` is the highest `upto`
//! among the flushes that the poison state has not already failed. The
//! coordinator waits until no lane has a reserved or appending `write_seq <=
//! upto` (a reserved slot counts: the write behind it will be acknowledged
//! and must be covered), then `fdatasync`s every ring file with reaped but
//! unsynced writes, all in parallel, and completes the group when all of them
//! have finished.

use std::os::fd::AsRawFd;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use io_uring::{opcode, types, IoUring};
use lunix_iface::journal::FlushDone;
use lunix_iface::Error;

use crate::config::TraceEvent;
use crate::ring::{lock, RingFile};
use crate::shared::Shared;

const WAIT_SLICE: Duration = Duration::from_millis(50);

pub(crate) fn run(sh: Arc<Shared>) {
    let mut ring = IoUring::new(64).ok();
    loop {
        let flushes = {
            let mut q = lock(&sh.coord.q);
            while q.queue.is_empty() && !q.shutdown {
                q = sh
                    .coord
                    .cv
                    .wait(q)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
            if q.shutdown {
                let rest = std::mem::take(&mut q.queue);
                drop(q);
                for (_, done) in rest {
                    done(Err(Error::Closed));
                }
                return;
            }
            std::mem::take(&mut q.queue)
        };
        group(&sh, ring.as_mut(), flushes);
    }
}

/// Waits for the lanes; returns the flushes that are still alive and their
/// `upto`, or `None` when shutdown or poison finished them all.
fn wait_for_lanes(
    sh: &Shared,
    mut flushes: Vec<(u64, FlushDone)>,
) -> Option<(u64, Vec<(u64, FlushDone)>)> {
    loop {
        sh.coord.waiting.store(true, Ordering::SeqCst);
        let epoch = sh.coord.epoch.load(Ordering::SeqCst);
        if let Some(at) = sh.poisoned_at() {
            let (bad, good): (Vec<_>, Vec<_>) = flushes.into_iter().partition(|f| f.0 >= at);
            for (_, done) in bad {
                done(Err(Shared::poisoned_error()));
            }
            flushes = good;
        }
        if flushes.is_empty() {
            sh.coord.waiting.store(false, Ordering::SeqCst);
            return None;
        }
        let upto = flushes.iter().map(|f| f.0).max().unwrap_or(0);
        if sh.lanes_clear(upto) {
            sh.coord.waiting.store(false, Ordering::SeqCst);
            return Some((upto, flushes));
        }
        let q = lock(&sh.coord.q);
        if q.shutdown {
            drop(q);
            sh.coord.waiting.store(false, Ordering::SeqCst);
            for (_, done) in flushes {
                done(Err(Error::Closed));
            }
            return None;
        }
        if sh.coord.epoch.load(Ordering::SeqCst) == epoch {
            let _ = sh.coord.cv.wait_timeout(q, WAIT_SLICE);
        }
    }
}

struct SyncItem {
    lane: u16,
    slot: usize,
    file: Arc<RingFile>,
}

fn group(sh: &Shared, ring: Option<&mut IoUring>, flushes: Vec<(u64, FlushDone)>) {
    let Some((upto, flushes)) = wait_for_lanes(sh, flushes) else {
        return;
    };
    lunix_iface::crashpoint!("journal.group_flush.after_watermark_wait");
    sh.cfg.emit(|| TraceEvent::GroupStart { upto });

    // Take the dirty sets. Writes reaped after this point belong to the next
    // group; every write at or below `upto` was reaped before the wait ended.
    let mut items = Vec::new();
    let mut min_unsynced = u64::MAX;
    for (lane, flight) in sh.flights.iter().enumerate() {
        let mut fl = lock(flight);
        let lane = lane as u16;
        let dirty = std::mem::take(&mut fl.dirty);
        min_unsynced = min_unsynced.min(fl.unsynced_min);
        fl.unsynced_min = u64::MAX;
        let pending_min = fl.pending.first().copied();
        drop(fl);
        for slot in dirty {
            let file = Arc::clone(&sh.rings.lanes[usize::from(lane)].files[slot]);
            sh.cfg.emit(|| TraceEvent::Fdatasync {
                lane,
                fd: file.file.as_raw_fd(),
                upto,
                lane_pending_min: pending_min,
            });
            items.push(SyncItem { lane, slot, file });
        }
    }

    let mut ok = true;
    if !items.is_empty() {
        lunix_iface::crashpoint!("journal.before_fdatasync");
        ok = sync_all(ring, &items);
        lunix_iface::crashpoint!("journal.after_fdatasync");
    }
    if !ok {
        // Nothing since the last successful sync is promised any more.
        for it in &items {
            lock(&sh.flights[usize::from(it.lane)]).dirty.insert(it.slot);
        }
        sh.poison(min_unsynced.min(upto));
    }
    sh.cfg.emit(|| TraceEvent::GroupDone { upto, ok });
    let at = sh.poisoned_at();
    for (u, done) in flushes {
        if at.is_some_and(|p| u >= p) {
            done(Err(Shared::poisoned_error()));
        } else {
            done(Ok(()));
        }
    }
}

/// `fdatasync`s every item in parallel; true when all succeeded.
fn sync_all(ring: Option<&mut IoUring>, items: &[SyncItem]) -> bool {
    let Some(ring) = ring else {
        return items.iter().all(|it| it.file.file.sync_data().is_ok());
    };
    let mut queued = 0usize;
    for (i, it) in items.iter().enumerate() {
        let sqe = opcode::Fsync::new(types::Fd(it.file.file.as_raw_fd()))
            .flags(types::FsyncFlags::DATASYNC)
            .build()
            .user_data(i as u64);
        // SAFETY: an Fsync entry references no memory; the descriptor is kept
        // open by `items` until the completions below are reaped.
        if unsafe { ring.submission().push(&sqe) }.is_err() {
            if ring.submit().is_err() {
                return false;
            }
            // SAFETY: as above.
            if unsafe { ring.submission().push(&sqe) }.is_err() {
                return false;
            }
        }
        queued += 1;
    }
    if ring.submit_and_wait(queued).is_err() {
        return false;
    }
    let mut ok = true;
    let mut seen = 0usize;
    while seen < queued {
        for cqe in ring.completion() {
            seen += 1;
            if cqe.result() < 0 {
                ok = false;
            }
        }
        if seen < queued && ring.submit_and_wait(1).is_err() {
            return false;
        }
    }
    ok
}
