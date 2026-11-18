//! The bounded full volume in the engine (ADR-32 T1 and T2; the note
//! `docs/explanation/research/2026-11-18-write-backpressure.md` §2 and §9.2;
//! the p1-bstop review M1-M5): every parked write ends (P1 from its arrival,
//! P6, the stopped engine, the quiesce), discards are never refused for
//! space and are not queued behind parked writes (P4), a refusing volume
//! cuts its discards and turns the retained window whoever cut them (M6),
//! and `JournalReserved` is released per cut. The gate here is the testkit's
//! `MemSpace`; the real gate's deadlines, latch and re-judging are tested in
//! `lunix-space`, the whole path in `lunixd/tests/stack_throttle.rs`.

mod common;

use std::time::Duration;

use common::*;
use lunix_engine::{CommitOutcome, Retry};
use lunix_iface::block::{IoOp, Tag};
use lunix_iface::space::{SpaceEvents, SpaceGate, SpaceKind};
use lunix_testkit::{JournalMode, MemSpaceConfig};

const CS: u32 = 16 << 10;
const EIO: i32 = 5;
const ENOSPC: i32 = 28;
const RING_MIN: Duration = Duration::from_secs(60);
/// The ring bound at quiesce the daemon gives for a 7.5 s `quiesce_timeout`.
const CLOSE_BOUND: Duration = Duration::from_secs(5);

fn pump(r: &mut Running, n: usize) {
    for _ in 0..n {
        r.pump();
    }
}

fn short_timeout(c: &mut lunix_engine::EngineConfig) {
    c.full_timeout = Duration::from_secs(5);
}

/// One acknowledged write (something for a cut), then the ring refuses
/// every reservation; returns the tag of a request parked for the ring.
fn ring_parked(r: &mut Running, op: IoOp) -> Tag {
    assert!(r.write(0, 0, &pattern(1, 4096), false).is_ok());
    r.journal.set_nospace(true);
    let data = pattern(2, 4096);
    let t = match op {
        IoOp::Write => r.submit(0, IoOp::Write, false, 4096, 4096, &data),
        other => r.submit(0, other, false, 4096, 4096, &[]),
    };
    pump(r, 50);
    assert!(!r.is_done(t), "it waits for the ring");
    t
}

#[test]
fn a_ring_wait_with_commits_running_lasts_at_least_60_s_then_fails_eio() {
    // Review m2: a short full_timeout must not turn ordinary journal
    // back-pressure into EIO while commits succeed.
    let mut v = Vol::create(CS, 16, 1, JournalMode::Immediate);
    v.tweak = short_timeout;
    let mut r = v.open().map_err(|e| e.0).expect("open");
    let t = ring_parked(&mut r, IoOp::Write);
    v.clock.advance(Duration::from_secs(30));
    pump(&mut r, 5);
    assert!(!r.is_done(t), "5 s full_timeout, but no commit failed");
    v.clock.advance(RING_MIN - Duration::from_secs(30));
    assert_eq!(r.wait(&[t])[0].0.errno(), Some(EIO));
    // Nothing is left charged for it, and the volume still works.
    let held = r.space.snapshot().by_kind[SpaceKind::JournalReserved.index()];
    assert_eq!(held, r.engine.journal_charged());
    r.journal.set_nospace(false);
    assert!(r.write(0, 8192, &pattern(3, 4096), false).is_ok());
    assert_eq!(r.read(0, 0, 4096).1, pattern(1, 4096));
}

#[test]
fn a_ring_wait_fails_enospc_at_full_timeout_after_a_commit_failed_for_space() {
    // The P6 ENOSPC branch, and review m7: a later failure of another kind
    // keeps the answer ENOSPC until a commit succeeds.
    let mut v = Vol::create(CS, 16, 1, JournalMode::Immediate);
    v.tweak = short_timeout;
    let mut r = v.open().map_err(|e| e.0).expect("open");
    let t = ring_parked(&mut r, IoOp::Write);
    v.clock.advance(Duration::from_secs(1));
    *lock(&v.destage_fault) = Some(Retry::AfterSpace);
    assert!(
        r.engine.commit_now(false).is_err(),
        "the cut fails for space"
    );
    *lock(&v.destage_fault) = Some(Retry::Later);
    assert!(
        r.engine.commit_now(false).is_err(),
        "then for another reason"
    );
    v.clock.advance(Duration::from_millis(3900));
    pump(&mut r, 5);
    assert!(!r.is_done(t), "not before 5 s from its arrival");
    v.clock.advance(Duration::from_millis(100));
    assert_eq!(r.wait(&[t])[0].0.errno(), Some(ENOSPC));
}

#[test]
fn a_hole_parked_for_the_ring_fails_enospc_when_commits_fail_for_space() {
    let mut v = Vol::create(CS, 16, 1, JournalMode::Immediate);
    v.tweak = short_timeout;
    let mut r = v.open().map_err(|e| e.0).expect("open");
    let t = ring_parked(&mut r, IoOp::Discard);
    v.clock.advance(Duration::from_secs(1));
    *lock(&v.destage_fault) = Some(Retry::AfterSpace);
    assert!(r.engine.commit_now(false).is_err());
    v.clock.advance(Duration::from_secs(4));
    assert_eq!(r.wait(&[t])[0].0.errno(), Some(ENOSPC));
}

#[test]
fn a_ring_wait_follows_a_live_change_of_full_timeout() {
    let mut v = Vol::create(CS, 16, 1, JournalMode::Immediate);
    v.tweak = short_timeout;
    let mut r = v.open().map_err(|e| e.0).expect("open");
    let t = ring_parked(&mut r, IoOp::Write);
    v.clock.advance(Duration::from_millis(10));
    *lock(&v.destage_fault) = Some(Retry::Later);
    assert!(r.engine.commit_now(false).is_err());
    v.clock.advance(Duration::from_secs(2));
    pump(&mut r, 5);
    assert!(!r.is_done(t));
    // Counted from the arrival: 2 s waited, a 1 s timeout is overdue.
    r.engine.set_full_timeout(Duration::from_secs(1));
    assert_eq!(r.wait(&[t])[0].0.errno(), Some(EIO));
}

#[test]
fn begin_close_answers_a_ring_wait_once_a_commit_fails_for_space() {
    // Review M3: the quiesce rule covers writes parked for the ring.
    let v = Vol::create(CS, 16, 1, JournalMode::Immediate);
    let mut r = v.open().map_err(|e| e.0).expect("open");
    let t = ring_parked(&mut r, IoOp::Write);
    *lock(&v.destage_fault) = Some(Retry::AfterSpace);
    assert!(
        r.engine.commit_now(false).is_err(),
        "failed before the close"
    );
    r.engine.begin_close(CLOSE_BOUND);
    pump(&mut r, 5);
    assert!(!r.is_done(t), "a failure before the close does not count");
    *lock(&v.destage_fault) = Some(Retry::AfterSpace);
    assert!(r.engine.commit_now(false).is_err());
    // No clock advance: the failed commit after the close answers it.
    assert_eq!(r.wait(&[t])[0].0.errno(), Some(ENOSPC));
}

#[test]
fn begin_close_answers_a_ring_wait_within_its_bound_when_no_commit_fails() {
    let v = Vol::create(CS, 16, 1, JournalMode::Immediate);
    let mut r = v.open().map_err(|e| e.0).expect("open");
    let t = ring_parked(&mut r, IoOp::Write);
    r.engine.begin_close(CLOSE_BOUND);
    v.clock.advance(Duration::from_millis(4900));
    pump(&mut r, 5);
    assert!(!r.is_done(t), "a healthy ring wait gets a few seconds");
    v.clock.advance(Duration::from_millis(100));
    assert_eq!(r.wait(&[t])[0].0.errno(), Some(EIO));
}

#[test]
fn a_slow_but_healthy_cut_at_quiesce_is_waited_for() {
    // Review N3: the bound comes from the daemon (two thirds of
    // `quiesce_timeout`); a cut that takes 10 s of it answers the parked
    // write with success, not EIO.
    let v = Vol::create(CS, 16, 1, JournalMode::Immediate);
    let mut r = v.open().map_err(|e| e.0).expect("open");
    let t = ring_parked(&mut r, IoOp::Write);
    r.engine.begin_close(Duration::from_secs(20));
    v.clock.advance(Duration::from_secs(10));
    pump(&mut r, 5);
    assert!(!r.is_done(t), "within the bound");
    // The cut lands: the ring takes the write.
    r.journal.set_nospace(false);
    r.engine.commit_now(false).expect("commit");
    assert!(r.wait(&[t])[0].0.is_ok());
}

#[test]
fn a_stopped_engine_fails_the_writes_parked_for_the_ring_at_once() {
    let v = Vol::create(CS, 16, 1, JournalMode::Immediate);
    let mut r = v.open().map_err(|e| e.0).expect("open");
    let t = ring_parked(&mut r, IoOp::Write);
    *lock(&v.destage_fault) = Some(Retry::Never);
    assert!(r.engine.commit_now(false).is_err());
    assert!(r.engine.is_stopped());
    // No clock advance: the stop alone ends the wait.
    assert_eq!(r.wait(&[t])[0].0.errno(), Some(EIO));
}

#[test]
fn a_stopped_engine_fails_the_writes_parked_for_space_at_once() {
    let mut v = Vol::create(CS, 16, 1, JournalMode::Immediate);
    v.space_cfg = Some(MemSpaceConfig::new(64 << 10).wait(None));
    let mut r = v.open().map_err(|e| e.0).expect("open");
    assert!(r.write(0, 0, &pattern(1, 4096), false).is_ok());
    let t = r.submit(0, IoOp::Write, false, 0, 65536, &pattern(2, 65536));
    pump(&mut r, 20);
    assert!(!r.is_done(t), "the write waits for space");
    assert_eq!(r.space.snapshot().waiters, 1);
    *lock(&v.destage_fault) = Some(Retry::Never);
    assert!(r.engine.commit_now(false).is_err());
    assert_eq!(r.wait(&[t])[0].0.errno(), Some(EIO));
    // Its waiter was withdrawn.
    assert_eq!(r.space.snapshot().waiters, 0);
}

#[test]
fn a_read_only_volume_fails_its_parked_writes_with_eio() {
    let mut v = Vol::create(CS, 16, 1, JournalMode::Immediate);
    v.space_cfg = Some(MemSpaceConfig::new(64 << 10).wait(None));
    let mut r = v.open().map_err(|e| e.0).expect("open");
    let t = r.submit(0, IoOp::Write, false, 0, 65536, &pattern(2, 65536));
    pump(&mut r, 20);
    assert!(!r.is_done(t));
    r.space.fatal_io("test");
    assert_eq!(r.wait(&[t])[0].0.errno(), Some(EIO));
}

#[test]
fn a_waiter_expired_for_space_is_enospc() {
    let mut v = Vol::create(CS, 16, 1, JournalMode::Immediate);
    v.space_cfg = Some(MemSpaceConfig::new(64 << 10).wait(Some(Duration::from_secs(1))));
    let mut r = v.open().map_err(|e| e.0).expect("open");
    let t = r.submit(0, IoOp::Write, false, 0, 65536, &pattern(2, 65536));
    pump(&mut r, 20);
    v.clock.advance(Duration::from_secs(1));
    assert_eq!(r.space.tick(), 1);
    assert_eq!(r.wait(&[t])[0].0.errno(), Some(ENOSPC));
}

#[test]
fn writes_queued_behind_a_parked_one_wait_at_most_full_timeout_from_their_arrival() {
    // Review M1: three writes on one queue; the later two reach the gate
    // only when the one before them leaves it, but their waits count from
    // their arrival.
    const W: usize = 48 << 10;
    let mut v = Vol::create(CS, 256, 1, JournalMode::Immediate);
    v.space_cfg = Some(MemSpaceConfig::new(66_500).wait(Some(Duration::from_secs(5))));
    let mut r = v.open().map_err(|e| e.0).expect("open");
    // A zero write fills the ledger and stores nothing once cut.
    assert!(r.write(0, 0, &vec![0u8; 64 << 10], false).is_ok());
    let a = r.submit(0, IoOp::Write, false, 0, W as u32, &pattern(1, W));
    v.clock.advance(Duration::from_secs(1));
    let b = r.submit(0, IoOp::Write, false, 1 << 20, W as u32, &pattern(2, W));
    v.clock.advance(Duration::from_secs(1));
    let c = r.submit(0, IoOp::Write, false, 2 << 20, W as u32, &pattern(3, W));
    pump(&mut r, 20);
    assert_eq!(
        r.space.snapshot().waiters,
        1,
        "only the head is in the gate"
    );
    // t = 4 s: the cut gives the zero write's journal bytes back; `a` fits,
    // `b` does not and reaches the gate 3 s after its arrival.
    v.clock.advance(Duration::from_secs(2));
    r.engine.commit_now(false).expect("commit");
    assert!(r.wait(&[a])[0].0.is_ok());
    pump(&mut r, 20);
    assert!(!r.is_done(b) && !r.is_done(c));
    assert_eq!(r.space.snapshot().waiters, 1);
    // t = 6 s: `b`'s deadline (arrival 1 s + 5 s), not 4 s + 5 s.
    v.clock.advance(Duration::from_secs(2));
    assert_eq!(r.space.tick(), 1);
    assert_eq!(r.wait(&[b])[0].0.errno(), Some(ENOSPC));
    pump(&mut r, 20);
    assert!(!r.is_done(c));
    // t = 7 s: `c`'s (arrival 2 s + 5 s).
    v.clock.advance(Duration::from_secs(1));
    assert_eq!(r.space.tick(), 1);
    assert_eq!(r.wait(&[c])[0].0.errno(), Some(ENOSPC));
}

/// A volume whose writes no longer fit: 1 MiB of writes (`headroom` more of
/// reserved region), three 256 KiB blocks written.
fn full_volume(on_full_wait: bool, headroom: u64) -> (Vol, Running) {
    let mut v = Vol::create(CS, 256, 1, JournalMode::Immediate);
    let cfg = MemSpaceConfig::new((1 << 20) + headroom).headroom(headroom);
    v.space_cfg = Some(if on_full_wait { cfg.wait(None) } else { cfg });
    let mut r = v.open().map_err(|e| e.0).expect("open");
    for i in 0..3u64 {
        let d = pattern(10 + i, 256 << 10);
        assert!(r.write(0, i * (256 << 10), &d, false).is_ok(), "fill {i}");
    }
    (v, r)
}

#[test]
fn discards_and_write_zeroes_overtake_a_write_parked_for_space() {
    let (_v, mut r) = full_volume(true, 1 << 20);
    let w = r.submit(0, IoOp::Write, false, 0, 512 << 10, &pattern(20, 512 << 10));
    pump(&mut r, 20);
    assert!(!r.is_done(w), "the write waits for space");
    // Same queue: neither waits behind the parked write.
    let d = r.submit(0, IoOp::Discard, false, 0, 64 << 10, &[]);
    let z = r.submit(0, IoOp::WriteZeroes, false, 256 << 10, 64 << 10, &[]);
    pump(&mut r, 20);
    assert!(r.is_done(d) && r.is_done(z), "the holes completed");
    let st = r.wait(&[d, z]);
    assert!(st[0].0.is_ok() && st[1].0.is_ok(), "{st:?}");
    assert!(!r.is_done(w), "the write still waits");
    assert_eq!(r.read(0, 0, 4096).1, vec![0u8; 4096]);
    assert_eq!(r.read(0, 256 << 10, 4096).1, vec![0u8; 4096]);
    assert_eq!(
        r.space.snapshot().waiters,
        1,
        "the write is the gate's to grant"
    );
}

#[test]
fn a_discard_is_admitted_with_no_reserved_region_left() {
    // Review M5: a hole record needs no new disk space; it is charged
    // without a limit check, even with nothing left of the region.
    let (_v, mut r) = full_volume(false, 0);
    assert_eq!(
        r.write(0, 0, &pattern(30, 512 << 10), false).errno(),
        Some(ENOSPC)
    );
    assert!(r
        .space
        .reserve_headroom(SpaceKind::Commits, 1 << 20)
        .is_err());
    assert!(r.hole(0, IoOp::Discard, 0, 256 << 10).is_ok());
    assert!(r.hole(0, IoOp::WriteZeroes, 256 << 10, 256 << 10).is_ok());
    assert_eq!(r.read(0, 0, 4096).1, vec![0u8; 4096]);
}

#[test]
fn the_window_turns_inside_the_latch_band() {
    // Review N1: latched, GC frees a little (more than 1 MiB, less than the
    // resume mark), the guest discards: the gate parks nothing, but the
    // engine sees the latch, cuts the discard and turns the window; once the
    // extents are gone the latch opens.
    let mut v = Vol::create(CS, 512, 1, JournalMode::Immediate);
    v.space_cfg = Some(
        MemSpaceConfig::new(5 << 20)
            .headroom(1 << 20)
            .wait(Some(Duration::from_secs(1)))
            .latch(3 << 20),
    );
    let mut r = v.open().map_err(|e| e.0).expect("open");
    for i in 0..14u64 {
        let d = pattern(40 + i, 256 << 10);
        assert!(r.write(0, i * (256 << 10), &d, false).is_ok(), "fill {i}");
    }
    r.engine.commit_now(false).expect("commit the fill");
    let w = r.submit(0, IoOp::Write, false, 0, 1 << 20, &pattern(50, 1 << 20));
    pump(&mut r, 20);
    v.clock.advance(Duration::from_secs(1));
    assert_eq!(r.space.tick(), 1);
    assert_eq!(r.wait(&[w])[0].0.errno(), Some(ENOSPC));
    assert!(r.space.snapshot().full, "latched");
    // Take the turn the open owes (a second has passed: the first FULL is
    // due now), then "GC frees" 1.5 MiB of segments.
    let _ = r.engine.tick().expect("tick");
    let _ = turn(&v, &mut r);
    r.space.release(SpaceKind::Segments, 3 << 19);
    let snap = r.space.snapshot();
    let headroom = snap.limit - snap.used();
    assert!(headroom > (1 << 20) && headroom < (3 << 20), "{snap:?}");
    assert!(
        snap.full && snap.waiters == 0,
        "the band: latched, nothing parked"
    );
    // The guest discards; one second later the cut, then the turn.
    let start = tip(&r);
    assert!(r.hole(0, IoOp::Discard, 0, 1 << 20).is_ok());
    v.clock.advance(Duration::from_secs(1));
    assert!(matches!(
        r.engine.tick().expect("tick"),
        CommitOutcome::Committed { .. }
    ));
    assert_eq!(r.engine.window_turn_left(), 3);
    assert_eq!(turn(&v, &mut r), 3);
    assert_eq!(tip(&r), start + 4);
    // GC deletes the discarded extents (1.25 MiB of segments here): the
    // headroom passes the mark, the latch opens and a write goes in.
    r.space.release(SpaceKind::Segments, 5 << 18);
    assert!(!r.space.snapshot().full);
    assert!(r.write(0, 0, &pattern(51, 64 << 10), false).is_ok());
}

fn tip(r: &Running) -> u64 {
    r.engine.tip().0 .0
}

/// Ticks once a second until the window turn is over; returns the FULLs it
/// wrote.
fn turn(v: &Vol, r: &mut Running) -> u64 {
    let start = tip(r);
    for _ in 0..10 {
        assert_eq!(
            r.engine.tick().expect("tick"),
            CommitOutcome::Idle,
            "spacing"
        );
        v.clock.advance(Duration::from_secs(1));
        if r.engine.tick().expect("tick") == CommitOutcome::Idle {
            break;
        }
    }
    tip(r) - start
}

#[test]
fn a_full_volume_cuts_its_discards_and_turns_the_retained_window() {
    let (v, mut r) = full_volume(false, 1 << 20);
    r.engine.commit_now(false).expect("commit the fill");
    // Refused for space: the gate is in its stop or full state.
    assert_eq!(
        r.write(0, 0, &pattern(30, 1 << 20), false).errno(),
        Some(ENOSPC)
    );
    let start = tip(&r);
    // At open the last commit with a hole is taken to be the tip (the
    // conservative reading): one commit followed it, two FULLs are owed.
    assert_eq!(r.engine.window_turn_left(), 2);
    assert_eq!(turn(&v, &mut r), 2);
    assert_eq!(r.engine.window_turn_left(), 0);
    // A second has passed since the last FULL: the discard is cut at once.
    assert!(r.hole(0, IoOp::Discard, 0, 256 << 10).is_ok());
    assert!(matches!(
        r.engine.tick().expect("tick"),
        CommitOutcome::Committed { .. }
    ));
    assert_eq!(tip(&r), start + 3, "the cut that takes the discard");
    // A second discard right after it waits for the spacing.
    assert!(r.hole(0, IoOp::Discard, 256 << 10, 4096).is_ok());
    assert_eq!(
        r.engine.tick().expect("tick"),
        CommitOutcome::Idle,
        "once a second"
    );
    v.clock.advance(Duration::from_secs(1));
    assert!(matches!(
        r.engine.tick().expect("tick"),
        CommitOutcome::Committed { .. }
    ));
    assert_eq!(tip(&r), start + 4);
    // retain_last = 2: three more FULLs, one a second, empty on this idle
    // volume, so the discarded extent leaves every retained root.
    assert_eq!(r.engine.window_turn_left(), 3);
    assert_eq!(turn(&v, &mut r), 3);
    assert_eq!(r.engine.window_turn_left(), 0);
    v.clock.advance(Duration::from_secs(1));
    assert_eq!(r.engine.tick().expect("tick"), CommitOutcome::Idle);
    assert_eq!(tip(&r), start + 7);
    assert_eq!(r.read(0, 0, 4096).1, vec![0u8; 4096]);
}

#[test]
fn the_window_turns_when_a_normal_commit_cut_the_discard_before_the_stop() {
    // Review M2, cases 1-3: the holes were cut while writes were admitted
    // (by `commit_now`, as a control request or a `T` commit would), then
    // the volume refuses (`error`) or parks (`wait`): the debt counts from
    // that commit.
    for wait in [false, true] {
        let (v, mut r) = full_volume(wait, 1 << 20);
        assert!(r.hole(0, IoOp::Discard, 0, 256 << 10).is_ok());
        r.engine.commit_now(false).expect("the discard is cut");
        let hole_commit = tip(&r);
        assert_eq!(r.engine.window_turn_left(), 0, "not refusing: owes nothing");
        let w = r.submit(0, IoOp::Write, false, 0, 1 << 20, &pattern(31, 1 << 20));
        pump(&mut r, 20);
        if wait {
            assert!(!r.is_done(w));
        } else {
            assert_eq!(r.wait(&[w])[0].0.errno(), Some(ENOSPC));
        }
        assert_eq!(r.engine.window_turn_left(), 3, "wait={wait}");
        assert_eq!(turn(&v, &mut r), 3, "wait={wait}");
        assert_eq!(tip(&r), hole_commit + 3);
    }
}

#[test]
fn the_window_turns_after_a_restart() {
    // Review M2 case 4: the replayed hole records are cut by the first
    // commit after the restart, which then owes the turn.
    let (mut v, mut r) = full_volume(false, 1 << 20);
    assert!(r.hole(0, IoOp::Discard, 0, 256 << 10).is_ok());
    assert!(r.flush(0).is_ok());
    let p = r.stop();
    v.absorb(p, 7);
    let mut r = v.open().map_err(|e| e.0).expect("reopen");
    r.engine
        .commit_now(false)
        .expect("commit the replayed records");
    assert_eq!(
        r.write(0, 0, &pattern(30, 1 << 20), false).errno(),
        Some(ENOSPC)
    );
    assert_eq!(r.engine.window_turn_left(), 3);
    assert_eq!(turn(&v, &mut r), 3);
}

#[test]
fn a_volume_that_admits_writes_does_not_cut_for_a_discard() {
    let v = Vol::create(CS, 64, 1, JournalMode::Immediate);
    let mut r = v.open().map_err(|e| e.0).expect("open");
    assert!(r.write(0, 0, &pattern(1, 4096), false).is_ok());
    r.engine.commit_now(false).expect("commit");
    let start = tip(&r);
    assert!(r.hole(0, IoOp::Discard, 0, 4096).is_ok());
    v.clock.advance(Duration::from_secs(2));
    // Only the normal triggers (`T` is 30 s): no cut, no window turn.
    assert_eq!(r.engine.tick().expect("tick"), CommitOutcome::Idle);
    assert_eq!(tip(&r), start);
    assert_eq!(r.engine.window_turn_left(), 0);
}

#[test]
fn journal_charges_are_released_per_cut() {
    // Review M4: after a cut nothing at or below it stays charged, whatever
    // ring files the journal recycled.
    let v = Vol::create(CS, 64, 2, JournalMode::Immediate);
    let mut r = v.open().map_err(|e| e.0).expect("open");
    for i in 0..8u64 {
        assert!(r
            .write(i as usize, i * 4096, &pattern(i, 4096), false)
            .is_ok());
    }
    assert!(r.hole(1, IoOp::Discard, 0, 4096).is_ok());
    let held = |r: &Running| r.space.snapshot().by_kind[SpaceKind::JournalReserved.index()];
    assert!(held(&r) > 0);
    r.engine.commit_now(false).expect("commit");
    assert_eq!(held(&r), 0, "every record is at or below the cut");
    assert_eq!(r.engine.journal_charged(), 0);
    assert!(r.write(0, 0, &pattern(9, 4096), false).is_ok());
    assert_eq!(held(&r), r.engine.journal_charged());
    assert!(held(&r) > 0);
}
