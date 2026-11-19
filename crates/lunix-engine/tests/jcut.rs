//! Bounded cuts and the write path's counters (ADR-33 T3 and T4), under a
//! manual clock: the cut follows the drain rate of the last cuts, never less
//! than what frees a ring file of the fullest lane, never more than half the
//! ring; under a light load the cut is everything, as before; the commits
//! follow back to back above the fill trigger; ring waiters and their longest
//! wait are counted.
//!
//! A cut "takes" a time here because the test moves the manual clock when the
//! engine passes `engine.c3.destaged` (`TestHooks::advance`).

mod common;

use std::time::{Duration, Instant};

use common::*;
use lunix_engine::CommitOutcome;
use lunix_iface::block::IoOp;
use lunix_iface::journal::Journal;
use lunix_testkit::journal::MIN_FILE_BYTES;
use lunix_testkit::{JournalMode, MemJournalConfig};

const CS: u32 = 16 << 10;
/// Bytes of the journal record of one 4 KiB write.
const REC: u64 = 64 + 4096;
const C3: &str = "engine.c3.destaged";

fn ring(max_files: u32) -> Vol {
    let mut v = Vol::create(CS, 64, 1, JournalMode::Immediate);
    v.set_journal(MemJournalConfig {
        lanes: 1,
        file_bytes: MIN_FILE_BYTES,
        max_files,
        pool_depth: 64,
        mode: JournalMode::Immediate,
    });
    v
}

/// `n` writes of 4 KiB, one record each, at distinct blocks of the volume.
fn writes(r: &mut Running, first: u64, n: u64) {
    for i in first..first + n {
        let off = (i % 256) * 4096;
        assert!(r.write(0, off, &pattern(i + 1, 4096), false).is_ok());
    }
}

/// The next cut takes `by` of the manual clock.
fn cut_takes(v: &Vol, by: Duration) {
    *lock(&v.hooks.advance) = Some((C3, by));
}

fn cut_seq(out: CommitOutcome) -> u64 {
    match out {
        CommitOutcome::Committed { cut_seq, .. } => cut_seq.0,
        other => panic!("not committed: {other:?}"),
    }
}

#[test]
fn the_cut_follows_the_drain_rate_of_the_last_cuts() {
    let v = ring(64);
    let mut r = v.open().map_err(|e| e.0).expect("open");
    // No history: the rate bounds nothing, the half ring is far away.
    writes(&mut r, 0, 40);
    cut_takes(&v, Duration::from_secs(2));
    assert_eq!(cut_seq(r.engine.commit_bounded(false).expect("cut 1")), 40);
    // 40 records in 2 s: a 1 s target moves 20 records.
    writes(&mut r, 40, 100);
    cut_takes(&v, Duration::from_millis(500));
    assert_eq!(cut_seq(r.engine.commit_bounded(false).expect("cut 2")), 60);
    let st = r.engine.cut_stats();
    assert_eq!((st.last_cut_bytes, st.last_cut_ms), (20 * REC, 500));
    // 60 records in 2.5 s over the two cuts: 24 records.
    cut_takes(&v, Duration::from_secs(1));
    assert_eq!(cut_seq(r.engine.commit_bounded(false).expect("cut 3")), 84);
    // The window of the last four cuts moves on; an explicit commit takes the rest.
    assert_eq!(cut_seq(r.engine.commit_now(false).expect("rest")), 140);
    assert!(v.violations().is_empty(), "{:?}", v.violations());
}

#[test]
fn the_coordinators_tick_cuts_the_bound_and_a_light_load_is_cut_whole() {
    let v = ring(64);
    let mut r = v.open().map_err(|e| e.0).expect("open");
    // A light load: T decides and the cut is everything written so far.
    writes(&mut r, 0, 5);
    assert!(matches!(r.engine.tick(), Ok(CommitOutcome::Idle)));
    v.clock.advance(Duration::from_secs(31));
    cut_takes(&v, Duration::from_secs(5));
    assert_eq!(cut_seq(r.engine.tick().expect("T cut")), 5);
    // 5 records in 5 s and a 1 s target: one record per cut when the next
    // trigger fires over a backlog.
    writes(&mut r, 5, 50);
    assert!(matches!(r.engine.tick(), Ok(CommitOutcome::Idle)));
    v.clock.advance(Duration::from_secs(31));
    cut_takes(&v, Duration::from_secs(1));
    assert_eq!(cut_seq(r.engine.tick().expect("T cut")), 6);
    assert!(v.violations().is_empty(), "{:?}", v.violations());
}

#[test]
fn a_cut_is_never_less_than_what_frees_a_ring_file() {
    // A slow drain would cut one record; the oldest sealed file holds a few
    // hundred, and the cut is raised to its last: the commit frees it.
    let v = ring(16);
    let mut r = v.open().map_err(|e| e.0).expect("open");
    writes(&mut r, 0, 10);
    cut_takes(&v, Duration::from_secs(100));
    assert_eq!(cut_seq(r.engine.commit_bounded(false).expect("seed")), 10);
    writes(&mut r, 10, 700);
    let floor = r.journal.cut_freeing_file().expect("a sealed file").0;
    assert!(floor > 11, "the oldest file holds many records: {floor}");
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
    let v = ring(16);
    let mut r = v.open().map_err(|e| e.0).expect("open");
    let half = r.journal.fill().capacity_bytes / 2;
    let records = half.div_ceil(REC);
    // More than half the ring waits; with no history only the half ring bounds.
    writes(&mut r, 0, records + 80);
    cut_takes(&v, Duration::from_micros(1));
    assert_eq!(
        cut_seq(r.engine.commit_bounded(false).expect("cut")),
        records
    );
    // A fast drain (4 GB per microsecond) does not lift it.
    writes(&mut r, records + 80, records + 80);
    assert_eq!(
        cut_seq(r.engine.commit_bounded(false).expect("cut 2")),
        2 * records
    );
}

#[test]
fn a_cut_is_at_least_one_record() {
    let mut v = ring(64);
    v.tweak = |c| c.cut_target = Duration::ZERO;
    let mut r = v.open().map_err(|e| e.0).expect("open");
    writes(&mut r, 0, 10);
    cut_takes(&v, Duration::from_secs(1));
    assert_eq!(cut_seq(r.engine.commit_bounded(false).expect("cut")), 10);
    writes(&mut r, 10, 10);
    assert_eq!(cut_seq(r.engine.commit_bounded(false).expect("cut")), 11);
}

#[test]
fn commits_follow_back_to_back_while_the_ring_is_above_the_fill_trigger() {
    // The background coordinator waits its period between ticks, except while
    // the last tick committed and the ring is above the trigger with writes
    // not cut yet. Cuts here free one file each (a slow drain), so several
    // commits are needed: they come within one period, not one per period.
    fn background(c: &mut lunix_engine::EngineConfig) {
        c.background = true;
        c.background_period = Duration::from_millis(400);
    }
    let mut v = ring(8);
    v.tweak = background;
    let mut r = v.open().map_err(|e| e.0).expect("open");
    // A slow drain in the history: each cut is the floor, one ring file.
    writes(&mut r, 0, 10);
    cut_takes(&v, Duration::from_secs(100));
    assert_eq!(cut_seq(r.engine.commit_now(false).expect("seed")), 10);
    assert_eq!(r.engine.coordinator_wait(), Duration::from_millis(400));
    // Seven of the eight files: three cuts bring the ring to the trigger.
    writes(&mut r, 10, 6 * 252 + 120);
    let seq = |r: &Running| r.engine.tip().0 .0;
    let first = seq(&r);
    let wait_for = |r: &Running, n: u64| {
        let t0 = Instant::now();
        while seq(r) < first + n {
            assert!(t0.elapsed() < Duration::from_secs(10), "no commit chain");
            std::thread::sleep(Duration::from_millis(1));
        }
    };
    wait_for(&r, 1);
    let t1 = Instant::now();
    wait_for(&r, 3);
    assert!(
        t1.elapsed() < Duration::from_millis(300),
        "two more commits took {:?}: a period each",
        t1.elapsed()
    );
    // Below the trigger the thread waits its period again.
    assert_eq!(r.engine.coordinator_wait(), Duration::from_millis(400));
}

#[test]
fn the_wait_is_a_period_when_the_last_tick_did_not_commit() {
    let v = ring(8);
    let r = v.open().map_err(|e| e.0).expect("open");
    assert_eq!(
        r.engine.coordinator_wait(),
        Duration::from_millis(100),
        "an idle volume waits its period"
    );
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
    let v = ring(64);
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
    let v = ring(64);
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
