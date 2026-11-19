//! Bounded cuts and the write path's counters (ADR-33 T3 and T4), under a
//! manual clock: the cut follows the drain rate of the last cuts, never less
//! than one ring file of bytes and what frees a ring file of the fullest
//! lane, never more than half the ring; under a light load the cut is
//! everything, as before; the commits follow back to back while a lane is
//! parked or the ring is above the fill trigger; ring waiters and their
//! longest wait are counted.
//!
//! A cut "takes" a time here because the test moves the manual clock when the
//! engine passes a crash point (`TestHooks::advance`). Nothing here depends on
//! the wall clock.

mod common;

use std::time::Duration;

use common::*;
use lunix_engine::CommitOutcome;
use lunix_iface::block::IoOp;
use lunix_iface::journal::Journal;
use lunix_testkit::journal::MIN_FILE_BYTES;
use lunix_testkit::{JournalMode, MemJournalConfig};

const CS: u32 = 16 << 10;
/// Bytes of the journal record of one 4 KiB write.
const REC: u64 = 64 + 4096;
/// Records that make one ring file of journal bytes (the least a cut takes).
const FILE_RECS: u64 = MIN_FILE_BYTES.div_ceil(REC);
const C3: &str = "engine.c3.destaged";
const C9: &str = "engine.c9.reclaimed";

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

/// `n` writes of 4 KiB, one record each, round robin over the queues.
fn writes(r: &mut Running, first: u64, n: u64) {
    let lanes = r.queues.len() as u64;
    for i in first..first + n {
        let off = (i % 256) * 4096;
        assert!(r
            .write((i % lanes) as usize, off, &pattern(i + 1, 4096), false)
            .is_ok());
    }
}

/// The next cut takes `by` of the manual clock (C1 to C9).
fn cut_takes(v: &Vol, by: Duration) {
    *lock(&v.hooks.advance) = vec![(C3, by)];
}

fn cut_seq(out: CommitOutcome) -> u64 {
    match out {
        CommitOutcome::Committed { cut_seq, .. } => cut_seq.0,
        other => panic!("not committed: {other:?}"),
    }
}

/// The half-ring ceiling the engine uses, in record bytes: half the ring's
/// bytes scaled by what the charged records occupy of the files in use.
fn ceiling(r: &Running) -> u64 {
    let f = r.journal.fill();
    let held = r.engine.journal_charged().min(f.used_bytes);
    u64::try_from(u128::from(f.capacity_bytes / 2) * u128::from(held) / u128::from(f.used_bytes))
        .expect("fits")
}

#[test]
fn the_cut_follows_the_drain_rate_of_the_last_cuts() {
    let v = ring(1, 64);
    let mut r = v.open().map_err(|e| e.0).expect("open");
    // No history: the rate bounds nothing, the half ring is far away.
    writes(&mut r, 0, 1200);
    cut_takes(&v, Duration::from_secs(2));
    assert_eq!(
        cut_seq(r.engine.commit_bounded(false).expect("cut 1")),
        1200
    );
    // 1200 records in 2 s: a 1 s target moves 600.
    writes(&mut r, 1200, 2400);
    cut_takes(&v, Duration::from_millis(500));
    assert_eq!(
        cut_seq(r.engine.commit_bounded(false).expect("cut 2")),
        1800
    );
    let st = r.engine.cut_stats();
    assert_eq!((st.last_cut_bytes, st.last_cut_ms), (600 * REC, 500));
    // 1800 records in 2.5 s over the two cuts: 720.
    cut_takes(&v, Duration::from_secs(1));
    assert_eq!(
        cut_seq(r.engine.commit_bounded(false).expect("cut 3")),
        2520
    );
    // An explicit commit takes the rest.
    assert_eq!(cut_seq(r.engine.commit_now(false).expect("rest")), 3600);
    assert!(v.violations().is_empty(), "{:?}", v.violations());
}

#[test]
fn the_coordinators_tick_cuts_the_bound_and_a_light_load_is_cut_whole() {
    let v = ring(1, 64);
    let mut r = v.open().map_err(|e| e.0).expect("open");
    // A light load: T decides and the cut is everything written so far.
    writes(&mut r, 0, 5);
    assert!(matches!(r.engine.tick(), Ok(CommitOutcome::Idle)));
    v.clock.advance(Duration::from_secs(31));
    cut_takes(&v, Duration::from_secs(5));
    assert_eq!(cut_seq(r.engine.tick().expect("T cut")), 5);
    // 5 records in 5 s: the rate allows one; a cut is one ring file at least.
    writes(&mut r, 5, 400);
    assert!(matches!(r.engine.tick(), Ok(CommitOutcome::Idle)));
    v.clock.advance(Duration::from_secs(31));
    cut_takes(&v, Duration::from_secs(1));
    assert_eq!(cut_seq(r.engine.tick().expect("T cut")), 5 + FILE_RECS);
    assert!(v.violations().is_empty(), "{:?}", v.violations());
}

#[test]
fn a_cut_is_at_least_one_ring_file_of_bytes() {
    // A fixed commit cost at or above the target (a target of 0 here, or a
    // cut that spends a second on its fixed part against a 50 ms target)
    // would shrink the cuts to one record without the floor.
    let mut v = ring(1, 64);
    v.tweak = |c| c.cut_target = Duration::from_millis(50);
    let mut r = v.open().map_err(|e| e.0).expect("open");
    writes(&mut r, 0, 500);
    cut_takes(&v, Duration::from_secs(1));
    assert_eq!(cut_seq(r.engine.commit_bounded(false).expect("cut 1")), 500);
    // 500 records in 1 s at 50 ms: 25 records, a file is 253.
    writes(&mut r, 500, 600);
    assert_eq!(
        cut_seq(r.engine.commit_bounded(false).expect("cut 2")),
        500 + FILE_RECS
    );
    let mut z = ring(1, 64);
    z.tweak = |c| c.cut_target = Duration::ZERO;
    let mut r = z.open().map_err(|e| e.0).expect("open");
    writes(&mut r, 0, 100);
    cut_takes(&z, Duration::from_secs(1));
    assert_eq!(cut_seq(r.engine.commit_bounded(false).expect("cut 1")), 100);
    writes(&mut r, 100, 600);
    assert_eq!(
        cut_seq(r.engine.commit_bounded(false).expect("cut 2")),
        100 + FILE_RECS
    );
}

#[test]
fn cuts_without_journal_bytes_do_not_dilute_the_drain_rate() {
    let v = ring(1, 64);
    let mut r = v.open().map_err(|e| e.0).expect("open");
    writes(&mut r, 0, 1200);
    cut_takes(&v, Duration::from_secs(2));
    assert_eq!(
        cut_seq(r.engine.commit_bounded(false).expect("cut 1")),
        1200
    );
    // Three forced FULLs with nothing to cut, each slow: no journal bytes,
    // so they are not in the history.
    cut_takes(&v, Duration::from_secs(10));
    for _ in 0..3 {
        r.engine.commit_bounded(true).expect("forced FULL");
    }
    writes(&mut r, 1200, 2400);
    cut_takes(&v, Duration::from_secs(1));
    // Still 1200 records in 2 s: 600. With the empty cuts in the history it
    // would be 1200 records in 32 s, below one ring file, and the cut would
    // be 253.
    assert_eq!(cut_seq(r.engine.commit_bounded(false).expect("cut")), 1800);
}

#[test]
fn a_cut_is_timed_to_c9_not_to_the_gc_step_after_it() {
    let v = ring(1, 64);
    let mut r = v.open().map_err(|e| e.0).expect("open");
    writes(&mut r, 0, 100);
    *lock(&v.hooks.advance) = vec![
        (C3, Duration::from_secs(1)),
        // After the reclaim: C10 and what follows are not the cut's time.
        (C9, Duration::from_secs(10)),
    ];
    r.engine.commit_bounded(false).expect("cut");
    assert_eq!(r.engine.cut_stats().last_cut_ms, 1000);
}

#[test]
fn a_cut_is_never_less_than_what_frees_a_ring_file() {
    // Four lanes, written round robin: a lane's file ends at a `write_seq`
    // about four files' worth of journal bytes after the last cut, far past
    // the one-file budget. The cut is raised to it and the commit frees it.
    let v = ring(4, 32);
    let mut r = v.open().map_err(|e| e.0).expect("open");
    writes(&mut r, 0, 10);
    cut_takes(&v, Duration::from_secs(100));
    assert_eq!(cut_seq(r.engine.commit_bounded(false).expect("seed")), 10);
    writes(&mut r, 10, 4 * 700);
    let floor = r.journal.cut_freeing_file().expect("a sealed file").0;
    assert!(floor > 10 + FILE_RECS + 100, "{floor}");
    let before = r.journal.fill().used_bytes;
    assert_eq!(cut_seq(r.engine.commit_bounded(false).expect("cut")), floor);
    assert!(
        r.journal.fill().used_bytes < before,
        "the commit under pressure freed a file"
    );
    assert!(v.violations().is_empty(), "{:?}", v.violations());
}

#[test]
fn a_cut_is_never_more_than_half_the_ring() {
    let v = ring(1, 16);
    let mut r = v.open().map_err(|e| e.0).expect("open");
    writes(&mut r, 0, 2200);
    // No history: only the half ring bounds.
    let want = ceiling(&r).div_ceil(REC);
    assert!(want < 2200, "the ceiling binds: {want}");
    cut_takes(&v, Duration::from_micros(1));
    assert_eq!(cut_seq(r.engine.commit_bounded(false).expect("cut")), want);
    // A fast drain (4 GB per microsecond) does not lift it.
    writes(&mut r, 2200, 2200);
    let floor = r.journal.cut_freeing_file().map_or(0, |w| w.0);
    let want = (want + ceiling(&r).div_ceil(REC)).max(floor);
    assert_eq!(
        cut_seq(r.engine.commit_bounded(false).expect("cut 2")),
        want
    );
}

#[test]
fn the_first_cut_after_an_attach_over_a_full_ring_is_bounded_by_the_ceiling() {
    // No history after a restart: the ring holds what recovery replays and the
    // first cut is half the ring, not everything. The journal's files hold
    // half as many record bytes as their length (`fill_scale`, as the real
    // journal's batch padding can make it), so half the ring is half of that
    // in record bytes.
    let mut v = ring(1, 16);
    let mut r = v.open().map_err(|e| e.0).expect("open");
    writes(&mut r, 0, 3800);
    assert!(r.flush(0).is_ok());
    let p = r.stop();
    v.absorb(p, 1);
    v.checks
        .fill_scale
        .store(2, std::sync::atomic::Ordering::SeqCst);
    let r = v.open().map_err(|e| e.0).expect("reopen");
    let held = r.engine.journal_charged();
    assert!(held >= 3700 * REC, "the records were recovered: {held}");
    let f = r.journal.fill();
    let want_bytes =
        u128::from(f.capacity_bytes / 2) * u128::from(held) / u128::from(2 * f.used_bytes);
    let want = u64::try_from(want_bytes).expect("fits").div_ceil(REC);
    assert!(want < 1200, "half of the unscaled ceiling: {want}");
    assert_eq!(
        cut_seq(r.engine.commit_bounded(false).expect("first cut")),
        want
    );
}

#[test]
fn commits_follow_back_to_back_while_the_ring_is_above_the_fill_trigger() {
    // The background thread waits its period between ticks, except while the
    // last tick committed and the ring is above the trigger with writes not
    // cut yet. A slow drain: each cut is about one ring file, so several
    // commits are needed. The thread's loop is run here by hand.
    let v = ring(1, 8);
    let mut r = v.open().map_err(|e| e.0).expect("open");
    writes(&mut r, 0, 10);
    cut_takes(&v, Duration::from_secs(100));
    assert_eq!(cut_seq(r.engine.commit_now(false).expect("seed")), 10);
    assert_eq!(r.engine.coordinator_wait(), Duration::from_millis(100));
    // Seven of the eight files.
    writes(&mut r, 10, 6 * 252 + 120);
    let first = r.engine.tip().0 .0;
    r.engine.background_tick();
    let mut more = 0;
    while r.engine.coordinator_wait().is_zero() {
        r.engine.background_tick();
        more += 1;
        assert!(more < 20, "the chain does not end");
    }
    assert_eq!(more, 2, "three commits, the last two without a wait");
    assert_eq!(r.engine.tip().0 .0, first + 3);
    // Below the trigger the thread waits its period again.
    assert_eq!(r.engine.coordinator_wait(), Duration::from_millis(100));
}

#[test]
fn one_parked_lane_among_several_gets_back_to_back_cuts_below_the_fill_trigger() {
    // Four queues, one writing: its lane is full for the ring while the whole
    // ring is nearly empty (here the fake refuses the reservation). The cut
    // is bounded and leaves a backlog; the next one follows at once.
    let v = ring(4, 64);
    let mut r = v.open().map_err(|e| e.0).expect("open");
    writes(&mut r, 0, 8);
    cut_takes(&v, Duration::from_secs(100));
    assert_eq!(cut_seq(r.engine.commit_now(false).expect("seed")), 8);
    for i in 0..400 {
        assert!(r
            .write(0, (i % 256) * 4096, &pattern(i, 4096), false)
            .is_ok());
    }
    r.journal.set_nospace(true);
    let data = pattern(7, 4096);
    let t = r.submit(0, IoOp::Write, false, 4096, 4096, &data);
    for _ in 0..20 {
        r.pump();
    }
    assert!(!r.is_done(t));
    assert!(r.journal.fill().ratio() < 0.5, "the ring is nearly empty");
    r.engine.background_tick();
    assert!(
        r.engine.coordinator_wait().is_zero(),
        "a parked write and a backlog: the next cut follows at once"
    );
}

#[test]
fn the_wait_is_a_period_when_the_last_tick_did_not_commit() {
    let v = ring(1, 8);
    let r = v.open().map_err(|e| e.0).expect("open");
    assert_eq!(
        r.engine.coordinator_wait(),
        Duration::from_millis(100),
        "an idle volume waits its period"
    );
}

#[test]
fn a_thread_waiting_for_the_coordinator_is_counted() {
    // The hot loop leaves the mutex to such a thread (`coordinator_wait`);
    // here only that the explicit calls are seen while the mutex is held.
    let v = ring(1, 64);
    let mut r = v.open().map_err(|e| e.0).expect("open");
    writes(&mut r, 0, 10);
    let park = v.hooks.park_at(C3);
    let engine = &r.engine;
    std::thread::scope(|s| {
        let a = s.spawn(|| engine.commit_now(false));
        park.reached
            .recv_timeout(Duration::from_secs(20))
            .expect("the first commit holds the coordinator");
        assert_eq!(engine.coordinator_waiters(), 0);
        let b = s.spawn(|| engine.commit_now(true));
        let t0 = std::time::Instant::now();
        while engine.coordinator_waiters() == 0 {
            assert!(t0.elapsed() < Duration::from_secs(20), "never counted");
            std::thread::yield_now();
        }
        park.release.send(()).expect("release");
        assert!(a.join().expect("a").is_ok());
        assert!(b.join().expect("b").is_ok());
    });
    assert_eq!(r.engine.coordinator_waiters(), 0);
}

/// One acknowledged write, then the ring refuses every reservation; returns
/// the tag of a request parked for the ring.
fn ring_parked(r: &mut Running) -> lunix_iface::block::Tag {
    assert!(r.write(0, 0, &pattern(1, 4096), false).is_ok());
    r.journal.set_nospace(true);
    let data = pattern(2, 4096);
    let t = r.submit(0, IoOp::Write, false, 4096, 4096, &data);
    for _ in 0..20 {
        r.pump();
    }
    assert!(!r.is_done(t), "it waits for the ring");
    t
}

#[test]
fn ring_waiters_and_the_longest_wait_are_counted() {
    let v = ring(1, 64);
    let mut r = v.open().map_err(|e| e.0).expect("open");
    assert_eq!(r.engine.cut_stats(), lunix_engine::CutStats::default());
    let t = ring_parked(&mut r);
    assert_eq!(r.engine.cut_stats().ring_waiters, 1);
    v.clock.advance(Duration::from_secs(3));
    for _ in 0..20 {
        r.pump();
    }
    let st = r.engine.cut_stats();
    assert_eq!(st.ring_waiters, 1);
    assert!(st.ring_wait_longest_ms >= 3000, "{st:?}");
    // The ring frees: the write is admitted, the waiter count goes, the
    // longest wait stays.
    r.journal.set_nospace(false);
    r.engine.commit_now(false).expect("commit");
    assert!(r.wait(&[t])[0].0.is_ok());
    let st = r.engine.cut_stats();
    assert_eq!(st.ring_waiters, 0);
    assert!(st.ring_wait_longest_ms >= 3000, "{st:?}");
    assert!(st.last_cut_bytes > 0);
}

#[test]
fn a_failed_ring_wait_leaves_the_waiter_count() {
    let v = ring(1, 64);
    let mut r = v.open().map_err(|e| e.0).expect("open");
    let t = ring_parked(&mut r);
    assert_eq!(r.engine.cut_stats().ring_waiters, 1);
    // The deadline: 60 s while commits run.
    v.clock.advance(Duration::from_secs(61));
    assert_eq!(r.wait(&[t])[0].0.errno(), Some(5));
    let st = r.engine.cut_stats();
    assert_eq!(st.ring_waiters, 0);
    assert!(st.ring_wait_longest_ms >= 60_000, "{st:?}");
}
