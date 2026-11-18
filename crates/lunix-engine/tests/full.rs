//! The bounded full volume in the engine (ADR-32 T1 and T2; the note
//! `docs/explanation/research/2026-11-18-write-backpressure.md` §2 and §9.2):
//! every parked write ends (P1, P6, the stopped engine), discards are not
//! queued behind parked writes and are admitted on a full volume (P4), and a
//! full volume cuts its discards and turns the retained window (M6). The gate
//! here is the testkit's `MemSpace`; the real gate's deadlines, latch and
//! re-judging are tested in `lunix-space`, the whole path in
//! `lunixd/tests/stack_throttle.rs`.

mod common;

use std::time::Duration;

use common::*;
use lunix_engine::{CommitOutcome, Retry};
use lunix_iface::block::IoOp;
use lunix_iface::space::{SpaceEvents, SpaceGate, SpaceKind};
use lunix_testkit::{JournalMode, MemSpaceConfig};

const CS: u32 = 16 << 10;
const EIO: i32 = 5;
const ENOSPC: i32 = 28;

fn pump(r: &mut Running, n: usize) {
    for _ in 0..n {
        r.pump();
    }
}

fn short_timeout(c: &mut lunix_engine::EngineConfig) {
    c.full_timeout = Duration::from_secs(5);
}

#[test]
fn a_write_parked_for_a_full_ring_fails_at_its_full_timeout() {
    let mut v = Vol::create(CS, 16, 1, JournalMode::Immediate);
    v.tweak = short_timeout;
    let mut r = v.open().map_err(|e| e.0).expect("open");
    assert!(r.write(0, 0, &pattern(1, 4096), false).is_ok());
    // The ring refuses every reservation; no cut runs (manual engine).
    r.journal.set_nospace(true);
    let t = r.submit(0, IoOp::Write, false, 4096, 4096, &pattern(2, 4096));
    pump(&mut r, 50);
    assert!(!r.is_done(t), "the write waits for the ring");
    v.clock.advance(Duration::from_millis(4900));
    pump(&mut r, 5);
    assert!(!r.is_done(t), "not before its deadline");
    v.clock.advance(Duration::from_millis(100));
    // The last commit did not fail for space: EIO, not ENOSPC.
    assert_eq!(r.wait(&[t])[0].0.errno(), Some(EIO));
    // Nothing is left charged for it, and the volume still works.
    let held = r.space.snapshot().by_kind[SpaceKind::JournalReserved.index()];
    assert_eq!(held, r.engine.journal_charged());
    r.journal.set_nospace(false);
    assert!(r.write(0, 8192, &pattern(3, 4096), false).is_ok());
    assert_eq!(r.read(0, 0, 4096).1, pattern(1, 4096));
}

#[test]
fn a_ring_wait_follows_a_live_change_of_full_timeout() {
    let mut v = Vol::create(CS, 16, 1, JournalMode::Immediate);
    v.tweak = short_timeout;
    let mut r = v.open().map_err(|e| e.0).expect("open");
    r.journal.set_nospace(true);
    let t = r.submit(0, IoOp::Write, false, 0, 4096, &pattern(2, 4096));
    v.clock.advance(Duration::from_secs(2));
    pump(&mut r, 5);
    assert!(!r.is_done(t));
    // Counted from the arrival: 2 s waited, a 1 s timeout is overdue.
    r.engine.set_full_timeout(Duration::from_secs(1));
    assert_eq!(r.wait(&[t])[0].0.errno(), Some(EIO));
}

#[test]
fn a_stopped_engine_fails_the_writes_parked_for_the_ring_at_once() {
    let v = Vol::create(CS, 16, 1, JournalMode::Immediate);
    let mut r = v.open().map_err(|e| e.0).expect("open");
    assert!(r.write(0, 0, &pattern(1, 4096), false).is_ok());
    r.journal.set_nospace(true);
    let t = r.submit(0, IoOp::Write, false, 4096, 4096, &pattern(2, 4096));
    pump(&mut r, 20);
    assert!(!r.is_done(t));
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

/// A volume whose writes no longer fit: `limit` 1 MiB of writes, 1 MiB of
/// reserved region the hole records may take.
fn full_volume(on_full_wait: bool) -> (Vol, Running) {
    let mut v = Vol::create(CS, 256, 1, JournalMode::Immediate);
    let cfg = MemSpaceConfig::new(2 << 20).headroom(1 << 20);
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
    let (_v, mut r) = full_volume(true);
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
fn a_discard_is_admitted_from_the_reserve_on_a_refusing_volume() {
    let (_v, mut r) = full_volume(false);
    assert_eq!(
        r.write(0, 0, &pattern(30, 512 << 10), false).errno(),
        Some(ENOSPC)
    );
    let snap = r.space.snapshot();
    assert!(snap.used() + (512 << 10) > snap.limit, "{snap:?}");
    assert!(r.hole(0, IoOp::Discard, 0, 256 << 10).is_ok());
}

fn tip(r: &Running) -> u64 {
    r.engine.tip().0 .0
}

#[test]
fn a_full_volume_cuts_its_discards_and_turns_the_retained_window() {
    let (v, mut r) = full_volume(false);
    r.engine.commit_now(false).expect("commit the fill");
    // Refused for space: the gate is in its stop or full state.
    assert_eq!(
        r.write(0, 0, &pattern(30, 1 << 20), false).errno(),
        Some(ENOSPC)
    );
    let start = tip(&r);
    assert_eq!(r.engine.tick().expect("tick"), CommitOutcome::Idle);
    assert!(r.hole(0, IoOp::Discard, 0, 256 << 10).is_ok());
    // At most once a second.
    assert_eq!(r.engine.tick().expect("tick"), CommitOutcome::Idle);
    v.clock.advance(Duration::from_secs(1));
    assert!(matches!(
        r.engine.tick().expect("tick"),
        CommitOutcome::Committed { .. }
    ));
    assert_eq!(tip(&r), start + 1, "the cut that takes the discard");
    // retain_last = 2: three more FULLs, one a second, empty on this idle
    // volume, so the discarded extent leaves every retained root.
    assert_eq!(r.engine.window_turn_left(), 3);
    for n in 1..=3u64 {
        assert_eq!(r.engine.tick().expect("tick"), CommitOutcome::Idle);
        v.clock.advance(Duration::from_secs(1));
        assert!(matches!(
            r.engine.tick().expect("tick"),
            CommitOutcome::Committed { .. }
        ));
        assert_eq!(tip(&r), start + 1 + n);
    }
    assert_eq!(r.engine.window_turn_left(), 0);
    v.clock.advance(Duration::from_secs(1));
    assert_eq!(r.engine.tick().expect("tick"), CommitOutcome::Idle);
    assert_eq!(tip(&r), start + 4, "the turn is over");
    assert_eq!(r.read(0, 0, 4096).1, vec![0u8; 4096]);
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
