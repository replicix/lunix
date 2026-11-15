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
//!
//! # Marks
//!
//! A completed group raises the flushed watermark, which records appended
//! later carry. A record carrying it is durable once a later group with
//! `upto` above the counter at the raise completes. When none does within
//! [`JournalConfig::mark_idle`](crate::JournalConfig::mark_idle), the
//! coordinator itself appends a mark carrying the watermark to the mark ring
//! and syncs it (on-disk-formats §5.3): at most one mark per group that raised
//! the watermark, written only when no flush is queued. The mark takes the next
//! `write_seq`, so it leaves no gap; a mark that fails to be written poisons
//! the journal at its number, as a failed append does.

use std::os::fd::AsRawFd;
use std::os::unix::fs::FileExt;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use io_uring::{opcode, types, IoUring};
use lunix_format::journal::{Record, RecordHeader, RecordKind, BLOCK};
use lunix_format::WriteSeq;
use lunix_iface::journal::FlushDone;
use lunix_iface::Error;

use crate::aligned::AlignedBuf;
use crate::config::TraceEvent;
use crate::ring::{lock, RingFile, Role};
use crate::shared::{MarkDue, Shared};
use crate::sys::{round4k, B};

const WAIT_SLICE: Duration = Duration::from_millis(50);

pub(crate) fn run(sh: Arc<Shared>, ready: std::sync::mpsc::SyncSender<std::io::Result<()>>) {
    let mut ring = IoUring::new(64).ok();
    let mut mark_buf = AlignedBuf::zeroed(BLOCK).ok();
    // Unlocked when this function returns (the C library caches stacks).
    let mut _stack_lock = None;
    if sh.cfg.lock_memory {
        let locked = crate::sys::StackLock::new().and_then(|st| {
            let buf = match mark_buf.as_mut() {
                Some(b) => b.lock()?,
                None => 0,
            };
            let n = st.bytes() + buf;
            _stack_lock = Some(st);
            Ok(n)
        });
        match locked {
            Ok(n) => {
                sh.locked.fetch_add(n, Ordering::AcqRel);
            }
            Err(e) => {
                let _ = ready.send(Err(e));
                return;
            }
        }
    }
    let _ = ready.send(Ok(()));
    loop {
        let flushes = {
            let mut q = lock(&sh.coord.q);
            while q.queue.is_empty() && !q.shutdown {
                let Some(due) = q.mark_due else {
                    q = sh
                        .coord
                        .cv
                        .wait(q)
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    continue;
                };
                let now = Instant::now();
                if now >= due.at {
                    q.mark_due = None;
                    drop(q);
                    let retry = match mark_buf.as_mut() {
                        Some(buf) => write_mark(&sh, due, buf),
                        None => {
                            sh.marks_skipped.fetch_add(1, Ordering::Relaxed);
                            false
                        }
                    };
                    q = lock(&sh.coord.q);
                    if retry && q.mark_due.is_none() {
                        // The ring is full: try again after another idle
                        // delay, or as soon as reclaim frees a mark file.
                        let idle = sh.cfg.mark_idle.unwrap_or(Duration::from_millis(50));
                        q.mark_due = Some(MarkDue {
                            at: Instant::now() + idle,
                            ..due
                        });
                    }
                    continue;
                }
                q = sh
                    .coord
                    .cv
                    .wait_timeout(q, due.at - now)
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .0;
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
    let mut lane_mins: Vec<(u16, u64)> = Vec::new();
    for (lane, flight) in sh.flights.iter().enumerate() {
        let mut fl = lock(flight);
        let lane = lane as u16;
        let dirty = std::mem::take(&mut fl.dirty);
        min_unsynced = min_unsynced.min(fl.unsynced_min);
        lane_mins.push((lane, fl.unsynced_min));
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
        ok = !sh.fail_sync.swap(false, Ordering::SeqCst) && sync_all(ring, &items);
        lunix_iface::crashpoint!("journal.after_fdatasync");
    }
    if !ok {
        // Nothing since the last successful sync is promised any more.
        for it in &items {
            lock(&sh.flights[usize::from(it.lane)])
                .dirty
                .insert(it.slot);
        }
        // The writes of this group are unsynced again: put their minimum back so
        // a later failure poisons no higher than the first unsynced record.
        for &(lane, m) in &lane_mins {
            let mut fl = lock(&sh.flights[usize::from(lane)]);
            fl.unsynced_min = fl.unsynced_min.min(m);
        }
        sh.poison(if min_unsynced == u64::MAX {
            upto
        } else {
            min_unsynced
        });
    }
    sh.cfg.emit(|| TraceEvent::GroupDone { upto, ok });
    let at = sh.poisoned_at();
    // Every record at or below `upto` is durable on every lane now: later
    // records carry it as their flushed watermark (on-disk-formats §5.2).
    if ok && at.is_none_or(|p| upto < p) {
        let before = sh.flushed.fetch_max(upto, Ordering::SeqCst);
        schedule_mark(sh, upto, before);
    }
    for (u, done) in flushes {
        if at.is_some_and(|p| u >= p) {
            done(Err(Shared::poisoned_error()));
        } else {
            done(Ok(()));
        }
    }
}

/// After a group that completed with `upto` (the watermark was `before`):
/// a pending mark whose watermark a record of this group now witnesses is
/// settled, and a mark for the raised watermark is scheduled.
fn schedule_mark(sh: &Shared, upto: u64, before: u64) {
    let Some(idle) = sh.cfg.mark_idle else {
        return;
    };
    let counter = sh.counter.load(Ordering::SeqCst);
    let mut q = lock(&sh.coord.q);
    if let Some(d) = q.mark_due {
        // Every record numbered above `counter_at` was appended after the
        // raise to `d.flushed` and carries at least that; this group made
        // the first of them durable.
        if upto > d.counter_at {
            sh.witnessed.fetch_max(d.flushed, Ordering::SeqCst);
            q.mark_due = None;
        }
    }
    // A group whose only new number is the last mark (an empty FLUSH after
    // it) needs no mark of its own: nothing but the mark lies above `before`.
    let only_a_mark = upto == before + 1 && upto == sh.last_mark.load(Ordering::SeqCst);
    if upto > before && !only_a_mark && upto > sh.witnessed.load(Ordering::SeqCst) {
        q.mark_due = Some(MarkDue {
            flushed: upto,
            counter_at: counter,
            at: Instant::now() + idle,
        });
    }
}

/// Appends one mark carrying the current watermark to the mark ring and syncs
/// it, unless a durable record already witnesses the watermark or a durable
/// cut covers it. Returns true when the ring was full and the mark is still
/// owed.
fn write_mark(sh: &Shared, due: MarkDue, buf: &mut AlignedBuf) -> bool {
    let w = sh.flushed.load(Ordering::SeqCst).max(due.flushed);
    if w <= sh.witnessed.load(Ordering::SeqCst)
        || w <= sh.max_cut_asked.load(Ordering::SeqCst)
        || sh.poisoned_at().is_some()
        || sh.closed.load(Ordering::SeqCst)
    {
        return false;
    }
    let ring = &sh.rings.marks;
    // Place it: the active mark file, or the next free one.
    let placed = {
        let mut inner = lock(&ring.inner);
        let room = inner
            .active
            .is_some_and(|_| inner.resume_off + B <= ring.file_len);
        if !room {
            if let Some(next) = inner.free.pop_front() {
                if let Some(old) = inner.active {
                    inner.slots[old].role = Role::Used;
                }
                inner.slots[next].role = Role::Active;
                inner.active = Some(next);
                inner.resume_off = B;
            }
        }
        match inner.active {
            Some(slot) if inner.resume_off + B <= ring.file_len => Some((
                slot,
                inner.resume_off,
                inner.slots[slot].gen,
                Arc::clone(&ring.files[slot]),
            )),
            _ => None,
        }
    };
    let Some((slot, off, gen, file)) = placed else {
        // The watermark stays unwitnessed until reclaim frees a mark file
        // (the next cut); counted once per watermark, logged once per
        // episode, retried.
        if sh.mark_skipped_at.swap(w, Ordering::SeqCst) != w {
            sh.marks_skipped.fetch_add(1, Ordering::Relaxed);
        }
        if !sh.mark_ring_full.swap(true, Ordering::SeqCst) {
            tracing::warn!(
                flushed = w,
                "journal mark ring full: the last flush stays unwitnessed until a cut frees a mark file"
            );
        }
        return true;
    };
    sh.mark_ring_full.store(false, Ordering::SeqCst);
    // A number of its own: no lane holds it, and no group runs while the
    // coordinator writes it, so no flush can complete over it unwritten.
    let seq = sh.counter.fetch_add(1, Ordering::SeqCst) + 1;
    let mut enc = Vec::with_capacity(64);
    let rec = Record {
        header: RecordHeader {
            kind: RecordKind::Mark,
            write_seq: WriteSeq(seq),
            epoch: 0,
            lba: 0,
            len: 0,
            gen,
            off,
            flushed: WriteSeq(w.min(seq - 1)),
        },
        data: &[],
    };
    if rec.encode(&mut enc).is_err() {
        sh.poison(seq);
        return false;
    }
    let block = buf.as_mut_slice();
    block.fill(0);
    block[..enc.len()].copy_from_slice(&enc);
    file.max_seq.fetch_max(seq, Ordering::AcqRel);
    file.rec_bytes.fetch_add(enc.len() as u64, Ordering::AcqRel);
    file.hi_off
        .fetch_max(round4k(off + enc.len() as u64), Ordering::AcqRel);
    lunix_iface::crashpoint!("journal.mark.before_write");
    let written = file
        .file
        .write_all_at(buf.as_slice(), off)
        .and_then(|()| file.file.sync_data());
    lunix_iface::crashpoint!("journal.mark.after_sync");
    if written.is_err() {
        // The number is taken and will never hold a record: every flush at
        // or above it must fail, as after a failed append.
        sh.poison(seq);
        return false;
    }
    {
        let mut inner = lock(&ring.inner);
        if inner.active == Some(slot) {
            inner.resume_off = off + B;
        }
    }
    sh.witnessed.fetch_max(w, Ordering::SeqCst);
    sh.last_mark.store(seq, Ordering::SeqCst);
    sh.marks_written.fetch_add(1, Ordering::Relaxed);
    sh.cfg.emit(|| TraceEvent::Mark {
        write_seq: seq,
        flushed: w,
    });
    false
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
