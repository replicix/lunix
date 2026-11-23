//! Milestone fastrecheck: the recheck-only pass (`GcConfig::fast_recheck`).
//! Under engine pressure, once a grace has ended, a pass marks, condemns,
//! deletes what its mark rechecks and ends without compaction; it must not
//! delay the regular passes (and their compaction).
//!
//! The timeline of the scenario (seconds on the view's clock, `t0` = start):
//! a regular pass at 1 condemns the dead segments `X` (grace ends at 61); a
//! regular pass at 55 condemns `Z` (grace ends at 115) and ends the regular
//! schedule's last full pass; a low-live segment `L` (a compaction
//! candidate) is made at 58. With `busy_interval` 20 s the next regular pass
//! is due at 75, so between 62 and 75 only a recheck-only pass can delete `X`.

use std::time::Duration;

use lunix_format::{IdLen, SegId};
use lunix_iface::clock::Clock;
use lunix_iface::gc::{Collector, GcState};
use lunix_iface::store::SegmentStore;

use super::sim::{Sim, SimCfg};
use crate::condemned::ObjectRef;
use crate::config::GcConfig;

const CS: u32 = 16 << 10;

fn cfg(fast: Option<Duration>, always: bool) -> SimCfg {
    let mut cfg = SimCfg::new(CS, IdLen::Sixteen);
    cfg.chunk_count = 64;
    cfg.gc = GcConfig {
        pass_interval: Duration::from_secs(30),
        busy_interval: Duration::from_secs(20),
        fast_recheck: fast,
        fast_recheck_always: always,
        ..GcConfig::daemon()
    };
    cfg
}

fn segs(s: &Sim) -> std::collections::BTreeSet<SegId> {
    SegmentStore::list(&s.store)
        .unwrap()
        .iter()
        .map(|x| x.seg)
        .collect()
}

/// `n` segments (one chunk each, chunks `first..`) overwritten, and the
/// retained window rolled past the overwrite: dead at the next mark.
fn make_dead(s: &mut Sim, first: u64, n: u64, seed: u64) {
    for i in 0..n {
        s.write(first + i, seed + i);
        s.commit().unwrap();
    }
    for i in 0..n {
        s.write(first + i, seed + 5_000 + i);
    }
    s.commit().unwrap();
    for _ in 0..4 {
        s.commit().unwrap();
    }
}

/// Steps one pass to its end without moving the clock.
fn pass_now(s: &mut Sim) -> Vec<GcState> {
    let mut states = Vec::new();
    for _ in 0..100_000 {
        let p = s.step().unwrap();
        states.push(p.state);
        if p.done || p.state == GcState::Idle {
            return states;
        }
    }
    panic!("a pass did not finish");
}

/// Moves the clock to `ms` milliseconds after `base`.
fn at(s: &mut Sim, base: Duration, ms: u64) {
    let target = base + Duration::from_millis(ms);
    let now = s.clock.monotonic();
    assert!(now <= target, "the clock is past {ms} ms");
    s.clock.advance(target - now);
}

fn listed_segments(s: &Sim) -> Vec<ObjectRef> {
    s.gc.condemned()
        .into_iter()
        .filter(|o| matches!(o, ObjectRef::Segment(_)))
        .collect()
}

fn dry_compactions(s: &mut Sim) -> u64 {
    let mut gc = std::mem::replace(
        &mut s.gc,
        crate::Gc::new(std::sync::Arc::new(s.store.clone()), s.cfg.gc),
    );
    let n = s.with_view(|v| gc.dry_run(v)).unwrap().segments_compacted;
    s.gc = gc;
    n
}

struct Scene {
    s: Sim,
    base: Duration,
    /// The segments of `X` (condemned at 1) and `Z` (condemned at 55).
    x: Vec<ObjectRef>,
    z: Vec<ObjectRef>,
}

/// The timeline above, up to second 58 (`mark_secs`: how long the mark of
/// the pass at 55 takes on the view's clock).
fn scene(fast: Option<Duration>, always: bool, mark_secs: u64) -> Scene {
    let mut s = Sim::new(cfg(fast, always));
    let base = s.clock.monotonic();
    make_dead(&mut s, 0, 6, 1_000);
    at(&mut s, base, 1_000);
    pass_now(&mut s);
    let x = listed_segments(&s);
    assert!(!x.is_empty(), "the first pass condemned X");
    at(&mut s, base, 54_000);
    make_dead(&mut s, 20, 3, 2_000);
    at(&mut s, base, 55_000);
    if mark_secs > 0 {
        s.hooks
            .advance_at("gc.mark.end", Duration::from_secs(mark_secs));
    }
    pass_now(&mut s);
    s.hooks.advance_at("gc.mark.end", Duration::ZERO);
    let z: Vec<ObjectRef> = listed_segments(&s)
        .into_iter()
        .filter(|o| !x.contains(o))
        .collect();
    assert!(!z.is_empty(), "the pass at 55 condemned Z");
    assert_eq!(s.gc.stats().passes, 2);
    assert_eq!(s.gc.stats().bytes_freed, 0, "nothing deleted yet");
    at(&mut s, base, 58_000);
    // L: four chunks in one segment, three overwritten, the window rolled.
    for i in 0..4 {
        s.write(40 + i, 3_000 + i);
    }
    s.commit().unwrap();
    for i in 1..4 {
        s.write(40 + i, 4_000 + i);
    }
    s.commit().unwrap();
    for _ in 0..4 {
        s.commit().unwrap();
    }
    Scene { s, base, x, z }
}

fn reclaim(s: &Sim) -> lunix_iface::gc::GcReclaimStats {
    *s.gc.status().reclaim
}

/// The headline: after its grace a condemned segment is deleted by a pass
/// that does no compaction, while a regular pass is not due.
#[test]
fn a_recheck_pass_deletes_a_condemned_segment_after_its_grace_without_compacting() {
    let mut sc = scene(Some(Duration::from_secs(1)), false, 0);
    sc.s.gc.set_pressure(true);
    assert!(
        dry_compactions(&mut sc.s) > 0,
        "L is a compaction candidate"
    );
    at(&mut sc.s, sc.base, 62_000);
    pass_now(&mut sc.s);
    let s = &mut sc.s;
    assert_eq!(s.gc.stats().passes, 3);
    assert_eq!(reclaim(s).fast_passes, 1);
    assert_eq!(reclaim(s).regular_passes, 2);
    let listed = s.gc.condemned();
    for o in &sc.x {
        assert!(!listed.contains(o), "{o} was deleted after its grace");
        if let ObjectRef::Segment(g) = o {
            assert!(!segs(s).contains(g));
        }
    }
    for o in &sc.z {
        assert!(listed.contains(o), "{o} is still waiting out its grace");
    }
    assert!(s.gc.stats().bytes_freed > 0);
    assert!(reclaim(s).fast_segments_deleted > 0);
    assert_eq!(reclaim(s).fast_bytes_freed, s.gc.stats().bytes_freed);
    // No compaction of any kind.
    assert_eq!(s.gc.stats().compaction_read, 0);
    assert_eq!(s.gc.stats().compaction_written, 0);
    assert_eq!(s.gc.stats().segments_compacted, 0);
    let last = s.gc.status().last_pass.expect("a pass finished");
    assert_eq!(last.segments_compacted, 0);
    assert_eq!(last.bytes_moved, 0);
    assert!(s.moved.is_empty());
    assert!(s.gc.unnamed_outputs().is_empty());
    assert!(dry_compactions(s) > 0, "L still waits for a regular pass");
    // The lag figures cover the deletes: X ended at 61, deleted at 62.
    let r = reclaim(s);
    assert!(r.recheck_lag_count > 0);
    assert_eq!(r.recheck_lag_ms_max, 1_000);
    s.check_roots().unwrap();
    s.check_ledger().unwrap();

    // The next regular pass compacts, on its own schedule (75: 20 s after
    // the regular pass at 55).
    at(s, sc.base, 80_000);
    pass_now(s);
    assert_eq!(reclaim(s).regular_passes, 3);
    assert!(s.gc.stats().segments_compacted > 0, "compaction ran");
    s.check_roots().unwrap();
    s.check_ledger().unwrap();
}

/// Without pressure, with the option off, or without `pipeline`, no
/// recheck-only pass ever runs.
#[test]
fn no_recheck_pass_without_pressure_or_with_the_option_off() {
    // Pressure off.
    let mut sc = scene(Some(Duration::from_secs(1)), false, 0);
    at(&mut sc.s, sc.base, 62_000);
    pass_now(&mut sc.s);
    assert_eq!(sc.s.gc.stats().passes, 2, "no pass without pressure");
    assert!(sc.s.gc.condemned().contains(&sc.x[0]));
    // Pressure on, the option off.
    let mut sc = scene(None, false, 0);
    sc.s.gc.set_pressure(true);
    at(&mut sc.s, sc.base, 62_000);
    pass_now(&mut sc.s);
    assert_eq!(sc.s.gc.stats().passes, 2, "the option is off");
    // Pressure on, then off again: no pass either.
    let mut sc = scene(Some(Duration::from_secs(1)), false, 0);
    sc.s.gc.set_pressure(true);
    sc.s.gc.set_pressure(false);
    at(&mut sc.s, sc.base, 62_000);
    pass_now(&mut sc.s);
    assert_eq!(sc.s.gc.stats().passes, 2);
    // `fast_recheck_always` needs no pressure.
    let mut sc = scene(Some(Duration::from_secs(1)), true, 0);
    at(&mut sc.s, sc.base, 62_000);
    pass_now(&mut sc.s);
    assert_eq!(sc.s.gc.stats().passes, 3);
    assert_eq!(reclaim(&sc.s).fast_passes, 1);
}

/// The delay: a grace that ended less than `delay` ago waits; and a pass
/// follows the last one at the earliest after three times its mark.
#[test]
fn a_recheck_pass_waits_for_its_delay_and_for_the_duty_cap() {
    let mut sc = scene(Some(Duration::from_secs(1)), false, 0);
    sc.s.gc.set_pressure(true);
    // The grace of X ended at 61: not yet 61 + 1.
    at(&mut sc.s, sc.base, 61_900);
    pass_now(&mut sc.s);
    assert_eq!(sc.s.gc.stats().passes, 2, "the delay is not over");
    at(&mut sc.s, sc.base, 62_000);
    pass_now(&mut sc.s);
    assert_eq!(sc.s.gc.stats().passes, 3);

    // The pass at 55 took 2 s of mark: its end (57) plus three times that.
    let mut sc = scene(Some(Duration::from_secs(1)), false, 2);
    sc.s.gc.set_pressure(true);
    at(&mut sc.s, sc.base, 62_900);
    pass_now(&mut sc.s);
    assert_eq!(sc.s.gc.stats().passes, 2, "within 3 x the last mark");
    at(&mut sc.s, sc.base, 63_000);
    pass_now(&mut sc.s);
    assert_eq!(sc.s.gc.stats().passes, 3);
    assert_eq!(reclaim(&sc.s).fast_passes, 1);
}

/// When both are due, the regular pass runs (it contains the recheck).
#[test]
fn a_regular_pass_wins_when_both_are_due() {
    let mut sc = scene(Some(Duration::from_secs(1)), false, 0);
    sc.s.gc.set_pressure(true);
    at(&mut sc.s, sc.base, 80_000);
    pass_now(&mut sc.s);
    let s = &mut sc.s;
    assert_eq!(reclaim(s).fast_passes, 0);
    assert_eq!(reclaim(s).regular_passes, 3);
    assert!(
        s.gc.stats().segments_compacted > 0,
        "a regular pass compacts"
    );
    for o in &sc.x {
        assert!(!s.gc.condemned().contains(o));
    }
    s.check_roots().unwrap();
}

/// A recheck pass deletes only what its own mark rechecks: `Z`'s grace had
/// not ended, so `Z` stays; and an abandoned pass switches the fast path off
/// until a regular pass succeeded.
#[test]
fn an_abandoned_pass_switches_recheck_passes_off() {
    let mut sc = scene(Some(Duration::from_secs(1)), false, 0);
    sc.s.gc.set_pressure(true);
    at(&mut sc.s, sc.base, 62_000);
    sc.s.arm_fault("segment.delete.before", 1, false);
    let failed = (0..100_000).any(|_| sc.s.step().is_err());
    sc.s.disarm_faults();
    assert!(failed, "the delete of the recheck pass failed");
    // The abandoned pass left the segments listed; fast passes stay off.
    let passes = sc.s.gc.stats().passes;
    at(&mut sc.s, sc.base, 66_000);
    pass_now(&mut sc.s);
    assert_eq!(sc.s.gc.stats().passes, passes, "nothing due after an abort");
    assert_eq!(reclaim(&sc.s).fast_passes, 0);
    // The regular pass after `pass_interval` finishes the work.
    at(&mut sc.s, sc.base, 100_000);
    pass_now(&mut sc.s);
    assert!(reclaim(&sc.s).regular_passes >= 3);
    for o in &sc.x {
        assert!(!sc.s.gc.condemned().contains(o));
    }
    sc.s.check_roots().unwrap();
    sc.s.check_ledger().unwrap();
}

/// A request that finds a recheck pass in progress is answered by a whole
/// pass: the next pass is the forced one (the engine's `run_pass` counts two).
#[test]
fn a_request_during_a_recheck_pass_runs_the_whole_sequence_next() {
    let mut sc = scene(Some(Duration::from_secs(1)), true, 0);
    at(&mut sc.s, sc.base, 62_000);
    let s = &mut sc.s;
    // One unit at a time into the recheck pass's mark.
    let mut state = GcState::Idle;
    for _ in 0..1_000 {
        state = s.unit().unwrap().state;
        if state == GcState::Marking {
            break;
        }
    }
    assert_eq!(state, GcState::Marking);
    assert!(s.gc.request_pass());
    // The pass in progress ends as a recheck-only pass.
    loop {
        let p = s.unit().unwrap();
        assert_ne!(p.state, GcState::Grace, "the pass in progress has no grace");
        if p.done {
            break;
        }
    }
    assert_eq!(reclaim(s).fast_passes, 1);
    // The request is served by the whole sequence: it waits out a grace.
    let mut grace = false;
    for _ in 0..1_000 {
        let p = s.unit().unwrap();
        grace |= p.state == GcState::Grace;
        if grace || p.done {
            break;
        }
    }
    assert!(grace, "the requested pass runs condemn, grace, recheck");
}

/// Sustained pressure with graces ending all the time: recheck passes run
/// between the regular ones but do not starve them, and the compaction of a
/// low-live segment still happens.
#[test]
fn compaction_still_runs_on_schedule_under_sustained_pressure() {
    let mut s = Sim::new(cfg(Some(Duration::from_secs(1)), false));
    s.gc.set_pressure(true);
    // L: a low-live segment that stays a candidate.
    for i in 0..4 {
        s.write(40 + i, 3_000 + i);
    }
    s.commit().unwrap();
    for i in 1..4 {
        s.write(40 + i, 4_000 + i);
    }
    s.commit().unwrap();
    let mut seed = 10_000;
    let mut compacted_at = None;
    for sec in 0..180u64 {
        // Garbage every second: dead segments, so graces end all the time.
        for i in 0..3 {
            seed += 1;
            s.write(i, seed);
        }
        s.commit().unwrap();
        s.clock.advance(Duration::from_secs(1));
        for _ in 0..100_000 {
            let p = s.step().unwrap();
            if p.done || p.state == GcState::Idle {
                break;
            }
        }
        if compacted_at.is_none() && s.gc.stats().segments_compacted > 0 {
            compacted_at = Some(sec);
        }
    }
    let r = reclaim(&s);
    assert!(r.fast_passes >= 1, "recheck passes ran: {r:?}");
    // 180 s at one regular pass per 20 s of busy interval (plus its time):
    // at least five, despite the recheck passes.
    assert!(r.regular_passes >= 5, "regular passes kept going: {r:?}");
    let at = compacted_at.expect("the low-live segment was compacted");
    assert!(at <= 60, "compaction ran early enough: second {at}");
    assert!(r.recheck_lag_count > 0);
    s.check_roots().unwrap();
    s.check_ledger().unwrap();
}

/// A recheck pass deletes the same objects a pipelined pass would at that
/// point (same verdict rule): replaying the scene with the regular schedule
/// leaves the same files once both have run their deletes.
#[test]
fn a_recheck_pass_and_a_regular_pass_delete_the_same_objects() {
    let mut a = scene(Some(Duration::from_secs(1)), false, 0);
    a.s.gc.set_pressure(true);
    at(&mut a.s, a.base, 62_000);
    pass_now(&mut a.s);
    let mut b = scene(None, false, 0);
    at(&mut b.s, b.base, 80_000);
    pass_now(&mut b.s);
    // The same segments of X are gone in both; Z stays listed in both.
    for o in &a.x {
        assert!(!a.s.gc.condemned().contains(o));
        assert!(!b.s.gc.condemned().contains(o));
    }
    for o in &a.z {
        assert!(a.s.gc.condemned().contains(o));
        assert!(b.s.gc.condemned().contains(o));
    }
}
