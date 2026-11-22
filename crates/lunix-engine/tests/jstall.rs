//! The T6 write-stall fixes of milestone jstall, under a manual clock:
//!
//! * the ring pace: writes are admitted at the drain rate of the last cuts
//!   while the ring is nearly full (the gate side is in
//!   `lunix-space/tests/pace.rs`);
//! * the collector yields the device to the cuts while writes wait for the
//!   ring, and still moves;
//! * the ring deadline counts from the last ring progress and every parked
//!   write is judged, on the ring timer's signal too;
//! * the counters: the recent longest wait from arrival, every parked write,
//!   the files freed by lane, the arrival-to-acknowledgement histogram.
//!
//! A cut "takes" a time because the test moves the manual clock when the
//! engine passes a crash point (`TestHooks::advance`). Only the test of the
//! ring timer thread waits on the wall clock, and only for a bound.

mod common;

use std::io::Read;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use common::*;
use lunix_engine::{CommitOutcome, EngineConfig, Retry};
use lunix_iface::block::{IoOp, Tag};
use lunix_iface::clock::{Clock, ManualClock};
use lunix_iface::gc::{
    Collector, GcBudget, GcParams, GcPassSummary, GcProgress, GcState, GcStats, GcView,
};
use lunix_iface::journal::Journal;
use lunix_iface::space::ThrottleState;
use lunix_iface::Result;
use lunix_testkit::journal::MIN_FILE_BYTES;
use lunix_testkit::{JournalMode, MemJournalConfig};

const CS: u32 = 16 << 10;
const EIO: i32 = 5;
const C3: &str = "engine.c3.destaged";
/// Bytes of the journal record of one 4 KiB write.
const REC: u64 = 64 + 4096;
const RING_MIN: Duration = Duration::from_secs(60);

fn ring(lanes: u16, max_files: u32) -> Vol {
    let mut v = Vol::create(CS, 64, lanes, JournalMode::Immediate);
    v.set_journal(MemJournalConfig {
        lanes,
        file_bytes: MIN_FILE_BYTES,
        max_files,
        pool_depth: 64,
        mode: JournalMode::Immediate,
    });
    v
}

fn writes(r: &mut Running, first: u64, n: u64) {
    let lanes = r.queues.len() as u64;
    for i in first..first + n {
        let off = (i % 256) * 4096;
        assert!(r
            .write((i % lanes) as usize, off, &pattern(i + 1, 4096), false)
            .is_ok());
    }
}

fn cut_takes(v: &Vol, by: Duration) {
    *lock(&v.hooks.advance) = vec![(C3, by)];
}

fn pump(r: &mut Running, n: usize) {
    for _ in 0..n {
        r.pump();
    }
}

/// One acknowledged write, then the ring refuses every reservation; returns
/// the tag of a request parked for the ring.
fn ring_parked(r: &mut Running) -> Tag {
    assert!(r.write(0, 0, &pattern(1, 4096), false).is_ok());
    r.journal.set_nospace(true);
    let t = r.submit(0, IoOp::Write, false, 4096, 4096, &pattern(2, 4096));
    pump(r, 20);
    assert!(!r.is_done(t), "it waits for the ring");
    t
}

/// The timer thread's tick, then a poll of the queues (what a woken queue
/// thread does).
fn timer_tick(r: &mut Running) {
    r.engine.ring_timer_tick();
    pump(r, 2);
}

// ------------------------------------------------------------ ring deadline

#[test]
fn a_slow_but_progressing_ring_never_fails_a_write() {
    // Four cuts, each 50 s after the last, each freeing a ring file: the
    // ring moves, slowly. A write parked since the start is 200 s old and has
    // never waited 60 s for nothing, so it must not fail. (Counted from its
    // arrival it fails at the second cut.)
    let mut v = ring(1, 64);
    v.tweak = |c| {
        c.cut_target = Duration::from_millis(1);
        c.cut_min_bytes = Some(1);
    };
    let mut r = v.open().map_err(|e| e.0).expect("open");
    // Six ring files of records, the oldest sealed ones free one per cut.
    writes(&mut r, 0, 6 * 252);
    r.journal.set_nospace(true);
    let t = r.submit(0, IoOp::Write, false, 8192, 4096, &pattern(99, 4096));
    pump(&mut r, 20);
    assert!(!r.is_done(t), "it waits for the ring");
    let arrived = v.clock.monotonic();
    for round in 1..=4u64 {
        v.clock.advance(Duration::from_secs(50));
        timer_tick(&mut r);
        assert!(!r.is_done(t), "round {round}: 50 s without a file is fine");
        let freed = r.engine.write_stats().freed_files_by_lane[0];
        let cut = r.engine.commit_bounded(false).expect("a slow cut");
        assert!(matches!(cut, CommitOutcome::Committed { .. }), "{cut:?}");
        assert!(
            r.engine.write_stats().freed_files_by_lane[0] > freed,
            "the cut freed a ring file"
        );
        timer_tick(&mut r);
        assert!(
            !r.is_done(t),
            "round {round}: {} s after its arrival, a file was just freed",
            (v.clock.monotonic() - arrived).as_secs()
        );
    }
    assert!(v.clock.monotonic() - arrived > 3 * RING_MIN);
    // The ring has room again: the write is admitted, not failed.
    r.journal.set_nospace(false);
    r.engine.commit_now(false).expect("commit");
    assert!(r.wait(&[t])[0].0.is_ok());
}

#[test]
fn a_ring_without_progress_fails_a_write_at_its_deadline_within_one_timer_tick() {
    let v = ring(1, 64);
    let mut r = v.open().map_err(|e| e.0).expect("open");
    let t = ring_parked(&mut r);
    // A millisecond short of the 60 s: alive.
    v.clock.advance(RING_MIN - Duration::from_millis(1));
    timer_tick(&mut r);
    assert!(!r.is_done(t));
    // At the deadline the next tick fails it: EIO, nothing left charged.
    v.clock.advance(Duration::from_millis(1));
    timer_tick(&mut r);
    assert!(r.is_done(t), "failed within one timer tick of the deadline");
    assert_eq!(r.wait(&[t])[0].0.errno(), Some(EIO));
    assert_eq!(r.engine.cut_stats().ring_waiters, 0);
    assert_eq!(r.engine.cut_stats().ring_parked_writes, 0);
}

#[test]
fn a_cut_that_frees_no_file_is_no_progress() {
    // A cut that frees no ring file (an empty FULL here) does not move the
    // ring; it must not keep a parked write alive for ever.
    let v = ring(1, 64);
    let mut r = v.open().map_err(|e| e.0).expect("open");
    let t = ring_parked(&mut r);
    r.engine.commit_now(false).expect("the first write's cut");
    assert!(!r.is_done(t));
    v.clock.advance(Duration::from_secs(50));
    r.engine.commit_now(true).expect("an empty FULL");
    v.clock.advance(Duration::from_secs(11));
    timer_tick(&mut r);
    assert_eq!(
        r.wait(&[t])[0].0.errno(),
        Some(EIO),
        "61 s after the cut that moved the ring"
    );
}

#[test]
fn every_parked_write_is_judged_not_only_the_head() {
    // A bulk write waits in the gate for its pace and stands at the head of
    // the queue; a small FUA write overtakes it and then waits for the ring
    // behind it. Only the head used to be looked at, so the FUA write never
    // saw its deadline.
    let v = Vol::create(CS, 16, 1, JournalMode::Immediate);
    let mut r = v.open().map_err(|e| e.0).expect("open");
    assert!(r.write(0, 0, &pattern(1, 4096), false).is_ok());
    r.space.set_pacing(true);
    let a = r.submit(0, IoOp::Write, false, 4096, 4096, &pattern(2, 4096));
    pump(&mut r, 20);
    assert!(!r.is_done(a), "the bulk write is paced");
    r.journal.set_nospace(true);
    let b = r.submit(0, IoOp::Write, true, 8192, 4096, &pattern(3, 4096));
    pump(&mut r, 20);
    assert!(
        !r.is_done(b),
        "the FUA write overtook and waits for the ring"
    );
    assert_eq!(r.engine.cut_stats().ring_waiters, 1);
    v.clock.advance(RING_MIN);
    timer_tick(&mut r);
    assert_eq!(r.wait(&[b])[0].0.errno(), Some(EIO));
    assert!(!r.is_done(a), "the paced write is the gate's business");
}

#[test]
fn the_ring_timer_wakes_a_parked_queue_while_a_cut_holds_the_coordinator() {
    // The coordinator thread sits inside a cut (parked at C3 here) for as long
    // as the cut lasts, which is exactly when writes are parked; the ring
    // timer thread signals the queue on its own, and the queue fails the write
    // at its deadline. The test plays the queue thread: it sleeps on the
    // queue's wake fd and polls when it is signalled.
    let mut v = Vol::create(CS, 16, 1, JournalMode::Immediate);
    v.tweak = |c: &mut EngineConfig| {
        c.background = true;
        c.background_period = Duration::from_millis(10);
    };
    let mut r = v.open().map_err(|e| e.0).expect("open");
    let t = ring_parked(&mut r);
    let park = v.hooks.park_at(C3);
    // The queue's wake fd, dup'ed: the description (and its non-blocking flag)
    // is shared, so a read drains it or says it was not signalled.
    let mut wake = std::fs::File::from(
        r.queues[0]
            .wake_fd()
            .try_clone_to_owned()
            .expect("dup the wake fd"),
    );
    let Running {
        engine,
        queues,
        done,
        ..
    } = &mut r;
    let engine = &*engine;
    let mut result = None;
    std::thread::scope(|s| {
        let cut = s.spawn(|| engine.commit_now(false));
        park.reached
            .recv_timeout(Duration::from_secs(20))
            .expect("the cut holds the coordinator");
        v.clock.advance(RING_MIN + Duration::from_secs(1));
        let t0 = std::time::Instant::now();
        // A bound, not a measure; on expiry the cut is released first, so the
        // failure is reported instead of the scope waiting for it.
        while result.is_none() && t0.elapsed() < Duration::from_secs(20) {
            let mut b = [0u8; 8];
            match wake.read(&mut b) {
                Ok(8) => {}
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(2));
                    continue;
                }
                other => panic!("wake fd: {other:?}"),
            }
            for q in queues.iter_mut() {
                q.poll(done);
            }
            result = done
                .take_completed()
                .into_iter()
                .find(|(tag, _)| *tag == t)
                .map(|(_, st)| st);
        }
        park.release.send(()).expect("release");
        let _ = cut.join().expect("cut thread");
    });
    let st = result.expect("the queue was never woken for the deadline");
    assert_eq!(st.errno(), Some(EIO));
}

#[test]
fn a_lane_that_gets_no_file_fails_its_parked_writes_whatever_the_other_lanes_do() {
    // Lane 1 can never get a file; lane 0 keeps cutting and freeing its own.
    // Ring progress is the lane's: the write parked on lane 1 fails at its
    // deadline although every cut moves the ring for lane 0.
    let mut v = ring(2, 64);
    v.tweak = |c| {
        c.cut_target = Duration::from_millis(1);
        c.cut_min_bytes = Some(1);
    };
    let mut r = v.open().map_err(|e| e.0).expect("open");
    for i in 0..6 * 252u64 {
        assert!(r
            .write(0, (i % 256) * 4096, &pattern(i + 1, 4096), false)
            .is_ok());
    }
    r.journal.set_nospace_lane(1, true);
    let t = r.submit(1, IoOp::Write, false, 4096, 4096, &pattern(7, 4096));
    pump(&mut r, 20);
    assert!(!r.is_done(t), "lane 1 has no room");
    let mut cuts = 0;
    for _ in 0..3 {
        v.clock.advance(Duration::from_secs(19));
        timer_tick(&mut r);
        assert!(!r.is_done(t), "not yet at 57 s");
        let freed = r.engine.write_stats().freed_files_by_lane;
        r.engine.commit_bounded(false).expect("lane 0 cuts");
        let now = r.engine.write_stats().freed_files_by_lane;
        assert!(now[0] > freed[0], "lane 0 got a file: {freed:?} {now:?}");
        assert_eq!(now[1], 0);
        cuts += 1;
    }
    assert_eq!(cuts, 3);
    // 57 s + 4 s: past the deadline of the write, with a file just freed
    // on the other lane.
    v.clock.advance(Duration::from_secs(4));
    timer_tick(&mut r);
    assert_eq!(r.wait(&[t])[0].0.errno(), Some(EIO));
}

#[test]
fn a_lane_the_cuts_have_not_reached_yet_is_not_failed_on_a_slow_ring() {
    // Lane 1 is full; its records stand behind lane 0's backlog in write_seq,
    // so the cuts (one file of lane 0 each, 25 s apart) have not reached them.
    // A merely slow lane is not a stuck one: every cut that released bytes
    // while lane 1 still holds records above it is progress for lane 1, and a
    // tick between two cuts does not fail its write, though it is 75 s old.
    let mut v = ring(2, 64);
    v.tweak = |c| {
        c.cut_target = Duration::from_millis(1);
        c.cut_min_bytes = Some(1);
    };
    let mut r = v.open().map_err(|e| e.0).expect("open");
    for i in 0..6 * 252u64 {
        assert!(r
            .write(0, (i % 256) * 4096, &pattern(i + 1, 4096), false)
            .is_ok());
    }
    for i in 0..20u64 {
        assert!(r
            .write(1, (i % 256) * 4096, &pattern(i + 9, 4096), false)
            .is_ok());
    }
    r.journal.set_nospace_lane(1, true);
    let t = r.submit(1, IoOp::Write, false, 4096, 4096, &pattern(7, 4096));
    pump(&mut r, 20);
    assert!(!r.is_done(t), "lane 1 has no room");
    for round in 1..=3 {
        v.clock.advance(Duration::from_secs(25));
        let freed = r.engine.write_stats().freed_files_by_lane;
        r.engine.commit_bounded(false).expect("a cut");
        let now = r.engine.write_stats().freed_files_by_lane;
        assert!(now[0] > freed[0] && now[1] == 0, "{freed:?} {now:?}");
        timer_tick(&mut r);
        assert!(
            !r.is_done(t),
            "round {round}: {} s old, the cuts are on their way to lane 1",
            25 * round
        );
    }
    r.journal.set_nospace_lane(1, false);
    r.engine.commit_bounded(false).expect("the fourth cut");
    assert!(r.wait(&[t])[0].0.is_ok());
}

#[test]
fn a_cut_that_ran_long_and_then_failed_for_space_is_no_progress() {
    // The ADR-32 rule: a commit that failed for space after the write arrived
    // shortens its limit to `full_timeout`; a cut that ran for 30 s and failed
    // is no progress once it ended, so the 30 s old write fails (ENOSPC) at
    // the next tick.
    let mut v = Vol::create(CS, 16, 1, JournalMode::Immediate);
    v.tweak = |c| c.full_timeout = Duration::from_secs(5);
    let mut r = v.open().map_err(|e| e.0).expect("open");
    let t = ring_parked(&mut r);
    *lock(&v.hooks.advance) = vec![("engine.c2.cut", Duration::from_secs(30))];
    *lock(&v.destage_fault) = Some(Retry::AfterSpace);
    assert!(
        r.engine.commit_now(false).is_err(),
        "the cut fails for space"
    );
    *lock(&v.hooks.advance) = Vec::new();
    timer_tick(&mut r);
    assert_eq!(r.wait(&[t])[0].0.errno(), Some(28));
}

#[test]
fn a_cut_longer_than_the_deadline_that_keeps_sealing_does_not_fail_a_parked_write() {
    // One cut seals segments for 100 s of the manual clock: more than the
    // 60 s deadline of a write that parked before it. The cut is alive the
    // whole time (and frees files at its end), so the write is not failed
    // mid-cut. A thread polls the queue on the timer's signal while it runs.
    let v = ring(1, 64);
    let mut r = v.open().map_err(|e| e.0).expect("open");
    writes(&mut r, 0, 6 * 252);
    r.journal.set_nospace(true);
    let t = r.submit(0, IoOp::Write, false, 8192, 4096, &pattern(99, 4096));
    pump(&mut r, 20);
    assert!(!r.is_done(t));
    *lock(&v.read_faults.seal_advance) = Some((Arc::clone(&v.clock), Duration::from_secs(25)));
    let started = v.clock.monotonic();
    let journal = Arc::clone(&r.journal);
    let Running {
        engine,
        queues,
        done,
        ..
    } = &mut r;
    let engine = &*engine;
    let mut failed = None;
    std::thread::scope(|s| {
        let cut = s.spawn(|| engine.commit_now(false));
        let t0 = std::time::Instant::now();
        while !cut.is_finished() && t0.elapsed() < Duration::from_secs(30) {
            engine.ring_timer_tick();
            for q in queues.iter_mut() {
                q.poll(done);
            }
            if let Some((_, st)) = done.take_completed().into_iter().find(|(tag, _)| *tag == t) {
                failed = Some(st);
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        // The cut ends whatever the check saw.
        let out = cut.join().expect("cut thread");
        assert!(out.is_ok(), "{out:?}");
    });
    assert!(
        v.clock.monotonic() - started > RING_MIN,
        "the cut lasted past the deadline"
    );
    assert!(failed.is_none(), "failed mid-cut: {failed:?}");
    journal.set_nospace(false);
    assert!(
        r.wait(&[t])[0].0.is_ok(),
        "freed files at the end of the cut"
    );
}

// ------------------------------------------------------------------ counters

#[test]
fn the_recent_longest_wait_counts_from_the_first_ring_refusal_and_ends_after_60_s() {
    let v = Vol::create(CS, 16, 1, JournalMode::Immediate);
    let mut r = v.open().map_err(|e| e.0).expect("open");
    assert!(r.write(0, 0, &pattern(1, 4096), false).is_ok());
    // A waits 10 s in the gate for its pace; only then does it meet the full
    // ring. The 10 s in the gate are the space throttle's, not a ring wait.
    r.space.set_pacing(true);
    let a = r.submit(0, IoOp::Write, false, 4096, 4096, &pattern(2, 4096));
    pump(&mut r, 20);
    v.clock.advance(Duration::from_secs(10));
    r.journal.set_nospace(true);
    r.space.set_pacing(false);
    pump(&mut r, 20);
    assert!(!r.is_done(a));
    let st = r.engine.cut_stats();
    assert_eq!(st.ring_waiters, 1);
    assert!(st.ring_wait_max_60s_ms < 1000, "{st:?}");
    assert!(st.ring_wait_max_since_attach_ms < 1000, "{st:?}");
    // 7 s at the ring.
    v.clock.advance(Duration::from_secs(7));
    pump(&mut r, 20);
    let st = r.engine.cut_stats();
    assert!((7000..10_000).contains(&st.ring_wait_max_60s_ms), "{st:?}");
    // Admitted: the figures stay.
    r.journal.set_nospace(false);
    r.engine.commit_now(false).expect("commit");
    assert!(r.wait(&[a])[0].0.is_ok());
    assert!(r.engine.cut_stats().ring_wait_max_60s_ms >= 7000);
    // A minute later the window has forgotten it, the lifetime figure has not.
    v.clock.advance(Duration::from_secs(61));
    let st = r.engine.cut_stats();
    assert_eq!(st.ring_wait_max_60s_ms, 0, "{st:?}");
    assert!(st.ring_wait_max_since_attach_ms >= 7000, "{st:?}");
    // And a shorter, later wait shows alone.
    let t = ring_parked_again(&mut r);
    v.clock.advance(Duration::from_secs(3));
    pump(&mut r, 20);
    let st = r.engine.cut_stats();
    assert!((3000..7000).contains(&st.ring_wait_max_60s_ms), "{st:?}");
    r.journal.set_nospace(false);
    r.engine.commit_now(false).expect("commit");
    assert!(r.wait(&[t])[0].0.is_ok());
}

/// A write parked for the ring on a volume that already holds data.
fn ring_parked_again(r: &mut Running) -> Tag {
    r.journal.set_nospace(true);
    let t = r.submit(0, IoOp::Write, false, 12288, 4096, &pattern(5, 4096));
    pump(r, 20);
    assert!(!r.is_done(t));
    t
}

#[test]
fn every_parked_write_is_counted_not_only_the_waiter_at_the_head() {
    let v = Vol::create(CS, 16, 1, JournalMode::Immediate);
    let mut r = v.open().map_err(|e| e.0).expect("open");
    let head = ring_parked(&mut r);
    let mut tags = vec![head];
    for i in 0..3u64 {
        tags.push(r.submit(
            0,
            IoOp::Write,
            false,
            8192 + 4096 * i,
            4096,
            &pattern(i, 4096),
        ));
    }
    pump(&mut r, 20);
    let st = r.engine.cut_stats();
    assert_eq!(st.ring_waiters, 1, "one write was refused a ring slot");
    assert_eq!(st.ring_parked_writes, 4, "four wait for it: {st:?}");
    r.journal.set_nospace(false);
    r.engine.commit_now(false).expect("commit");
    for st in r.wait(&tags) {
        assert!(st.0.is_ok());
    }
    let st = r.engine.cut_stats();
    assert_eq!((st.ring_waiters, st.ring_parked_writes), (0, 0));
}

#[test]
fn freed_ring_files_are_counted_by_lane() {
    let v = ring(2, 64);
    let mut r = v.open().map_err(|e| e.0).expect("open");
    // Only queue 1 writes: its lane seals files, the other lane's stay empty.
    for i in 0..600u64 {
        assert!(r
            .write(1, (i % 256) * 4096, &pattern(i + 1, 4096), false)
            .is_ok());
    }
    assert_eq!(r.engine.write_stats().freed_files_by_lane, vec![0, 0]);
    r.engine.commit_now(false).expect("commit");
    let freed = r.engine.write_stats().freed_files_by_lane;
    assert_eq!(freed.len(), 2);
    assert_eq!(freed[0], 0, "{freed:?}");
    assert!(freed[1] >= 1, "{freed:?}");
}

#[test]
fn the_write_latency_histogram_counts_writes_from_arrival_to_acknowledgement() {
    let v = Vol::create(CS, 16, 1, JournalMode::Immediate);
    let mut r = v.open().map_err(|e| e.0).expect("open");
    // The manual clock stands still: the daemon took 0 us.
    writes(&mut r, 0, 5);
    // A FUA write is acknowledged after its flush: counted once too. Reads and
    // flushes are not writes.
    assert!(r.write(0, 4096, &pattern(9, 4096), true).is_ok());
    assert!(r.flush(0).is_ok());
    assert_eq!(r.read(0, 0, 4096).1.len(), 4096);
    let h = r.engine.write_stats().write_latency_log2_us;
    assert_eq!(h[0], 6, "{h:?}");
    assert_eq!(h.iter().sum::<u64>(), 6);
    // A write that waits 3 s for the ring takes 3 000 000 us: the bucket with
    // 2^21 <= us < 2^22.
    let t = ring_parked(&mut r);
    v.clock.advance(Duration::from_secs(3));
    r.journal.set_nospace(false);
    r.engine.commit_now(false).expect("commit");
    assert!(r.wait(&[t])[0].0.is_ok());
    let h = r.engine.write_stats().write_latency_log2_us;
    assert_eq!(h[22], 1, "{h:?}");
    assert_eq!(h[0], 7, "the first write of ring_parked and the six before");
}

// ------------------------------------------------------------------ ring pace

#[test]
fn writes_are_paced_to_the_drain_rate_while_the_ring_is_nearly_full() {
    let v = ring(1, 8);
    let mut r = v.open().map_err(|e| e.0).expect("open");
    writes(&mut r, 0, 10);
    cut_takes(&v, Duration::from_secs(1));
    r.engine.commit_now(false).expect("seed");
    assert_eq!(r.space.ring_pace(), None, "an empty ring is not paced");
    // Seven of the eight files in use.
    writes(&mut r, 10, 6 * 252 + 120);
    assert!(r.journal.fill().ratio() >= 0.75, "{:?}", r.journal.fill());
    // One bounded cut of one ring file in one second: its journal bytes per
    // second are the drain rate, and the ring is still at three quarters.
    // Half a second between the two cuts (the collector's share, the tick):
    // the pace is the sustained rate, over the time from cut end to cut end.
    v.clock.advance(Duration::from_millis(500));
    r.engine.commit_bounded(false).expect("cut");
    let st = r.engine.cut_stats();
    assert!(st.last_cut_bytes > 0);
    assert!(r.journal.fill().ratio() >= 0.75, "{:?}", r.journal.fill());
    let seed = 10 * REC;
    // 1 s + 1.5 s of interval for the two cuts, which took 1 s each.
    let rate = (seed + st.last_cut_bytes) * 2 / 5;
    assert_eq!(r.space.ring_pace(), Some(rate), "{st:?}");
    assert_eq!(st.ring_pace_rate, rate);
    // Draining the ring ends the pace below half, and not before: between
    // the marks it keeps its state.
    let mut on = true;
    let mut cuts = 0;
    while let Ok(CommitOutcome::Committed { .. }) = r.engine.commit_bounded(false) {
        cuts += 1;
        assert!(cuts < 20);
        let ratio = r.journal.fill().ratio();
        if ratio < 0.5 {
            on = false;
        } else if ratio >= 0.75 {
            on = true;
        }
        assert_eq!(r.space.ring_pace().is_some(), on, "ratio {ratio}");
    }
    assert!(cuts >= 2);
    assert_eq!(r.space.ring_pace(), None, "a drained ring is not paced");
    assert_eq!(r.engine.cut_stats().ring_pace_rate, 0);
}

#[test]
fn a_write_that_waited_for_the_ring_keeps_the_pace_on_for_a_few_seconds() {
    // One lane is full while the ring as a whole is nearly empty (the load
    // maps to some queues only): the ring pace follows the waiting write, not
    // the fill ratio.
    let v = ring(4, 64);
    let mut r = v.open().map_err(|e| e.0).expect("open");
    writes(&mut r, 0, 8);
    cut_takes(&v, Duration::from_secs(1));
    r.engine.commit_now(false).expect("seed");
    let t = ring_parked_again(&mut r);
    assert!(r.journal.fill().ratio() < 0.5);
    r.engine.tick().expect("tick");
    assert!(r.space.ring_pace().is_some(), "a write waits for the ring");
    // The ring has room: the write is admitted. The pace stays for 5 s.
    r.journal.set_nospace(false);
    assert!(r.wait(&[t])[0].0.is_ok());
    // (The tick cuts the admitted write, and the cut takes a second.)
    v.clock.advance(Duration::from_secs(3));
    r.engine.tick().ok();
    assert!(r.space.ring_pace().is_some(), "4 s after the last wait");
    v.clock.advance(Duration::from_secs(3));
    r.engine.tick().ok();
    assert_eq!(r.space.ring_pace(), None, "7 s after the last wait");
}

// ----------------------------------------------------------------- store pace

#[test]
fn the_engine_tells_the_gate_the_store_factor_and_the_cut_in_flight() {
    let v = ring(1, 64);
    let mut r = v.open().map_err(|e| e.0).expect("open");
    assert_eq!(r.space.store_pace(), (1000, 0), "one before any cut");
    writes(&mut r, 0, 20);
    r.engine.commit_now(false).expect("first cut");
    let st = r.engine.cut_stats();
    assert!(st.last_cut_bytes > 0);
    // The first cut had no history: it predicted one.
    assert_eq!(st.store_pred_milli, 1000);
    assert!(st.store_real_milli > 0, "{st:?}");
    // The cut said its journal bytes while it ran, and none after it.
    assert_eq!(r.space.store_in_flight_max(), st.last_cut_bytes);
    let (a, in_flight) = r.space.store_pace();
    assert_eq!(in_flight, 0);
    assert_eq!(a, st.store_real_milli.clamp(1000, 8000), "{st:?}");
    // The next cut predicts what the first measured.
    writes(&mut r, 20, 20);
    r.engine.commit_now(false).expect("second cut");
    assert_eq!(r.engine.cut_stats().store_pred_milli, a);
}

// ------------------------------------------------------------------- GC yield

/// What the scripted collector saw and does.
#[derive(Default)]
struct Script {
    steps: AtomicU64,
    /// The compaction rate of the last step.
    rate: AtomicU64,
    /// The smallest rate any step saw (u64::MAX: none yet).
    min_rate: AtomicU64,
    /// Clock advance of the next step only (ms), then cleared.
    next_ms: AtomicU64,
    /// Clock advance of every other step (ms).
    each_ms: AtomicU64,
    /// Ask for a cut at every step (a cut with nothing to take).
    cut_wanted: AtomicBool,
}

struct Scripted {
    script: Arc<Script>,
    clock: Arc<ManualClock>,
}

impl Collector for Scripted {
    fn step(&mut self, view: &GcView<'_>, _b: GcBudget) -> Result<GcProgress> {
        let s = &self.script;
        let n = s.steps.fetch_add(1, Ordering::SeqCst) + 1;
        assert!(n < 2000, "runaway share: {n} collector steps");
        let rate = view.params.rate_bytes_per_s;
        s.rate.store(rate, Ordering::SeqCst);
        s.min_rate.fetch_min(rate, Ordering::SeqCst);
        let once = s.next_ms.swap(0, Ordering::SeqCst);
        let ms = if once > 0 {
            once
        } else {
            s.each_ms.load(Ordering::SeqCst)
        };
        self.clock.advance(Duration::from_millis(ms));
        Ok(GcProgress {
            moved: Vec::new(),
            freed: 0,
            state: GcState::Deleting,
            done: false,
            cut_wanted: s.cut_wanted.load(Ordering::SeqCst),
        })
    }

    fn urgent(&mut self, _v: &GcView<'_>, _want: u64) -> Result<GcProgress> {
        Ok(GcProgress {
            moved: Vec::new(),
            freed: 0,
            state: GcState::Idle,
            done: true,
            cut_wanted: false,
        })
    }

    fn dry_run(&mut self, _v: &GcView<'_>) -> Result<GcPassSummary> {
        Ok(GcPassSummary::default())
    }

    fn stats(&self) -> GcStats {
        GcStats::default()
    }
}

const GC_RATE: u64 = 64 << 20;

fn gc_on(cfg: &mut EngineConfig) {
    cfg.gc_budget = Some(GcBudget {
        max_duration: Duration::from_millis(50),
        max_bytes: 1 << 20,
    });
    cfg.gc_params = Some(GcParams {
        retain_last: 2,
        grace: Duration::from_secs(60),
        read_timeout: Duration::from_secs(10),
        rate_bytes_per_s: GC_RATE,
        live_threshold: 0.5,
        chunk_size: CS,
        id_len: lunix_format::IdLen::Sixteen,
    });
    cfg.commit_interval = Duration::from_secs(30);
}

fn gc_setup() -> (Vol, Running, Arc<Script>) {
    let mut v = Vol::create(CS, 64, 1, JournalMode::Immediate);
    v.tweak = gc_on;
    let script = Arc::new(Script {
        min_rate: AtomicU64::new(u64::MAX),
        ..Script::default()
    });
    *lock(&v.collector) = Some(Box::new(Scripted {
        script: Arc::clone(&script),
        clock: Arc::clone(&v.clock),
    }));
    let mut r = v.open().map_err(|e| e.0).expect("open");
    assert!(r.write(0, 0, &pattern(1, CS as usize), false).is_ok());
    r.engine.commit_now(false).expect("commit");
    (v, r, script)
}

/// Ticks the coordinator `n` times with the collector asking for a cut at
/// every step, each cut taking 3 s and each collector step 100 ms; returns the
/// steps of each tick.
fn tick_steps(r: &Running, script: &Script, n: usize) -> Vec<u64> {
    script.cut_wanted.store(true, Ordering::SeqCst);
    script.each_ms.store(100, Ordering::SeqCst);
    // An idle volume still steps the collector: its first step asks for the
    // cut the next tick makes.
    assert_eq!(r.engine.tick().expect("prime"), CommitOutcome::Idle);
    let mut out = Vec::new();
    for _ in 0..n {
        let before = script.steps.load(Ordering::SeqCst);
        script.next_ms.store(3000, Ordering::SeqCst);
        let o = r.engine.tick().expect("tick");
        assert!(matches!(o, CommitOutcome::Committed { .. }), "{o:?}");
        out.push(script.steps.load(Ordering::SeqCst) - before);
    }
    out
}

/// Parks a write for the ring and leaves it unpolled, so the waiter stays
/// counted while the test drives the coordinator.
fn park_one(r: &mut Running) {
    r.journal.set_nospace(true);
    let t = r.submit(0, IoOp::Write, false, CS.into(), 4096, &pattern(7, 4096));
    assert!(!r.is_done(t));
    assert_eq!(r.engine.cut_stats().ring_waiters, 1);
}

#[test]
fn the_collector_yields_to_the_cuts_while_a_write_waits_for_the_ring() {
    // Free: after each cut the share is a third of its time, 1 s, in steps of
    // 100 ms, and the compaction rate is the volume's.
    let (_v, r, script) = gc_setup();
    let free = tick_steps(&r, &script, 3);
    assert!(free.iter().all(|&n| n >= 5), "full shares: {free:?}");
    assert_eq!(script.rate.load(Ordering::SeqCst), GC_RATE);
    // A write waits for the ring: the cut's own step and one step of share,
    // at an eighth of the rate.
    let (_v, mut r, script) = gc_setup();
    park_one(&mut r);
    let yielded = tick_steps(&r, &script, 3);
    assert_eq!(yielded, vec![2, 2, 2], "one share step after each cut");
    assert_eq!(script.rate.load(Ordering::SeqCst), GC_RATE / 8);
}

#[test]
fn the_collector_keeps_yielding_for_two_seconds_after_the_last_waiter() {
    // No flip-flop of the compaction rate between two steps: the switch holds
    // for 2 s after the write that waited was admitted.
    let (v, mut r, script) = gc_setup();
    park_one(&mut r);
    r.engine.tick().expect("tick");
    assert_eq!(script.rate.load(Ordering::SeqCst), GC_RATE / 8);
    r.journal.set_nospace(false);
    pump(&mut r, 10);
    assert_eq!(r.engine.cut_stats().ring_waiters, 0, "the write went on");
    v.clock.advance(Duration::from_secs(1));
    r.engine.tick().expect("tick");
    assert_eq!(
        script.rate.load(Ordering::SeqCst),
        GC_RATE / 8,
        "1 s after the last waiter"
    );
    v.clock.advance(Duration::from_secs(2));
    r.engine.tick().expect("tick");
    assert_eq!(script.rate.load(Ordering::SeqCst), GC_RATE, "3 s after it");
}

#[test]
fn the_collector_keeps_moving_under_sustained_parking() {
    let (_v, mut r, script) = gc_setup();
    park_one(&mut r);
    let before = script.steps.load(Ordering::SeqCst);
    let per_tick = tick_steps(&r, &script, 25);
    // The cut's own step and one step of the share after every cut, however
    // long the parking lasts.
    assert!(
        per_tick.iter().all(|&n| n >= 2),
        "never starved: {per_tick:?}"
    );
    assert!(script.steps.load(Ordering::SeqCst) - before >= 50);
    // Slowed, never stopped, and never below the floor.
    assert!(script.min_rate.load(Ordering::SeqCst) >= 4 << 20);
    assert!(script.min_rate.load(Ordering::SeqCst) > 0);
}

#[test]
fn space_pressure_keeps_the_collector_whole_while_a_write_waits_for_the_ring() {
    // ADR-32: with the gate slowing writes the volume needs the collector
    // more than the ring does: the raised figures and the whole share stay.
    let (_v, mut r, script) = gc_setup();
    park_one(&mut r);
    r.space.set_throttle_state(ThrottleState::Slowdown);
    let steps = tick_steps(&r, &script, 3);
    assert!(steps.iter().all(|&n| n >= 5), "full shares: {steps:?}");
    // Raised: 4 x the rate, not an eighth.
    assert_eq!(script.rate.load(Ordering::SeqCst), 4 * GC_RATE);
}
