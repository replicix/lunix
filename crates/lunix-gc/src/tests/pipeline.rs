//! The pipelined schedule (`GcConfig::pipeline`, milestone gcrate): a
//! scheduled pass does not wait out the grace; its mark is the recheck of
//! the listed objects whose grace ended before it began, and a pass starts
//! when a grace ends. A requested pass still runs the whole sequence.

use std::time::Duration;

use lunix_format::{ExtentKind, IdLen, SegId};
use lunix_iface::clock::Clock;
use lunix_iface::gc::{Collector, GcState};
use lunix_iface::index::ChunkIndex;
use lunix_iface::map::{entry_hint, ChunkNo};
use lunix_iface::store::{CommitStore, GcStateStore, PinKind, PinRoot, SegmentStore};

use super::oracle::check_quiescent;
use super::sim::{data_of, Sim, SimCfg, GRACE};
use crate::condemned::ObjectRef;
use crate::config::GcConfig;

fn sim() -> Sim {
    let mut cfg = SimCfg::new(16 << 10, IdLen::Sixteen);
    cfg.gc = GcConfig {
        pass_interval: Duration::from_secs(30),
        busy_interval: Duration::from_secs(2),
        ..GcConfig::daemon()
    };
    Sim::new(cfg)
}

/// Overwrites the same chunks `rounds` times, one commit per round.
fn churn(s: &mut Sim, rounds: u64, base: u64) {
    for r in 0..rounds {
        for i in 0..12 {
            s.write(i, base + r * 100 + i);
        }
        s.commit().unwrap();
    }
}

/// Steps one pass to its end without ever advancing the clock (a pipelined
/// pass has no grace to wait for); returns the states it went through.
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

fn segments_listed(s: &Sim) -> Vec<ObjectRef> {
    s.gc.condemned()
        .into_iter()
        .filter(|o| matches!(o, ObjectRef::Segment(_)))
        .collect()
}

#[test]
fn a_scheduled_pass_condemns_without_waiting_and_a_later_one_deletes_after_the_grace() {
    let mut s = sim();
    churn(&mut s, 6, 0);
    s.clock.advance(Duration::from_secs(1));
    // The first pass: no grace wait, no delete; the dead are listed.
    let states = pass_now(&mut s);
    assert!(!states.contains(&GcState::Grace), "{states:?}");
    let condemned = segments_listed(&s);
    assert!(!condemned.is_empty());
    assert_eq!(s.gc.stats().bytes_freed, 0);
    // Passes before the grace ended delete nothing listed.
    for _ in 0..5 {
        s.clock.advance(Duration::from_secs(5));
        churn(&mut s, 1, 1000);
        pass_now(&mut s);
        for o in &condemned {
            assert!(s.gc.condemned().contains(o), "{o} went before its grace");
        }
    }
    assert_eq!(s.gc.stats().bytes_freed, 0);
    s.check_roots().unwrap();
    // Once the grace is over the next mark is the recheck: deleted.
    s.clock.advance(GRACE);
    churn(&mut s, 1, 2000);
    pass_now(&mut s);
    for o in &condemned {
        assert!(!s.gc.condemned().contains(o), "{o} is still listed");
    }
    assert!(s.gc.stats().bytes_freed > 0);
    s.check_roots().unwrap();
    s.check_ledger().unwrap();
}

#[test]
fn a_pass_starts_when_a_grace_ends_without_a_new_tip() {
    let mut s = sim();
    churn(&mut s, 6, 0);
    s.clock.advance(Duration::from_secs(1));
    pass_now(&mut s);
    assert!(!segments_listed(&s).is_empty());
    // At most one more pass follows the busy one (it changed something);
    // after it, nothing is due until the earliest grace ends.
    s.clock.advance(Duration::from_secs(3));
    pass_now(&mut s);
    let passes = s.gc.stats().passes;
    let evidence = s.store.evidence_passes().unwrap().len();
    for _ in 0..10 {
        s.clock.advance(Duration::from_secs(4));
        let p = s.step().unwrap();
        assert_eq!(p.state, GcState::Idle);
    }
    assert_eq!(s.gc.stats().passes, passes);
    // A pass that only finds graces running writes no evidence object.
    assert_eq!(s.store.evidence_passes().unwrap().len(), evidence);
    // The grace ends (well inside `pass_interval` of the last pass): a pass
    // starts and deletes, although the tip did not change.
    s.clock.advance(GRACE - Duration::from_secs(40));
    pass_now(&mut s);
    assert_eq!(s.gc.stats().passes, passes + 1);
    assert!(s.gc.stats().bytes_freed > 0);
    assert!(segments_listed(&s).is_empty());
    s.check_roots().unwrap();
}

#[test]
fn a_requested_pass_still_waits_out_its_grace_and_deletes() {
    let mut s = sim();
    churn(&mut s, 6, 0);
    s.clock.advance(Duration::from_secs(1));
    assert!(s.gc.request_pass());
    let p = s.step().unwrap();
    assert_eq!(p.state, GcState::Grace);
    s.pass().unwrap();
    assert_eq!(s.gc.stats().passes, 1);
    assert!(s.gc.stats().bytes_freed > 0);
    assert!(segments_listed(&s).is_empty());
    s.check_roots().unwrap();
    s.check_ledger().unwrap();
}

// ------------------------------------------------- the delete boundary (M-1)

/// Steps one unit at a time (budget 0) until `stop` holds; the clock does
/// not move. Returns the last state.
fn units_until(s: &mut Sim, stop: impl Fn(&Sim, &GcState) -> bool) -> GcState {
    for _ in 0..100_000 {
        let p = s.unit().unwrap();
        if stop(s, &p.state) {
            return p.state;
        }
        assert!(!p.done, "the pass ended before the point was reached");
    }
    panic!("the point was never reached");
}

/// One unit at a time to the end of the pass in progress, the clock still.
fn units_to_end(s: &mut Sim) {
    for _ in 0..100_000 {
        let p = s.unit().unwrap();
        if p.done || p.state == GcState::Idle {
            return;
        }
    }
    panic!("a pass did not finish");
}

/// A first pipelined pass at `t0`: dead segments listed, their grace begun.
fn condemned_at_start(s: &mut Sim) -> Vec<ObjectRef> {
    churn(s, 6, 0);
    s.clock.advance(Duration::from_secs(1));
    pass_now(s);
    let c = segments_listed(s);
    assert!(!c.is_empty());
    c
}

fn listed_segment(o: &ObjectRef) -> SegId {
    match o {
        ObjectRef::Segment(seg) => *seg,
        other => panic!("not a segment: {other}"),
    }
}

/// The grace ends after the deleting pass's mark began but before its delete
/// units run: the objects stay for the next mark. A delete judged by the
/// time of the delete unit instead of the mark's start fails here.
#[test]
fn a_grace_that_ends_during_the_mark_waits_for_the_next_mark() {
    let mut s = sim();
    let condemned = condemned_at_start(&mut s);
    // One second before the graces end, a pass starts and its mark pins its
    // roots (the Init unit reads the mark's start).
    s.clock.advance(GRACE - Duration::from_secs(1));
    churn(&mut s, 1, 1000);
    let st = units_until(&mut s, |_, st| *st == GcState::Marking);
    assert_eq!(st, GcState::Marking);
    s.unit().unwrap(); // the mark's first unit: roots, listings, start time
                       // The graces end now, in the middle of the mark.
    s.clock.advance(Duration::from_secs(2));
    units_to_end(&mut s);
    for o in &condemned {
        assert!(
            s.gc.condemned().contains(o),
            "{o} went although its grace ended after the mark began"
        );
        assert!(SegmentStore::list(&s.store)
            .unwrap()
            .iter()
            .any(|x| x.seg == listed_segment(o)));
    }
    s.check_roots().unwrap();
    // The next mark began after the graces: deleted.
    s.clock.advance(Duration::from_secs(3));
    churn(&mut s, 1, 2000);
    pass_now(&mut s);
    for o in &condemned {
        assert!(!s.gc.condemned().contains(o), "{o} is still listed");
    }
    s.check_roots().unwrap();
    s.check_ledger().unwrap();
}

/// A dedup hit between the mark and the flag, in the pipelined order (the
/// list is written, the flags not yet): the segment is live again when the
/// next mark pins its roots, so that pass un-condemns it instead of deleting.
#[test]
fn a_dedup_hit_between_the_mark_and_the_flag_is_uncondemned() {
    let mut s = sim();
    churn(&mut s, 6, 0);
    s.clock.advance(Duration::from_secs(1));
    // Round 0's chunk 3 (seed 3) is dead; find its segment.
    let cs = s.cfg.chunk_size;
    let id = s.codec.chunk_id(ExtentKind::Data, &data_of(3, cs));
    let seg = s.index.get(&id).unwrap().expect("indexed").hint.seg;
    // Step until the list holds `seg` and its flag is not set yet.
    units_until(&mut s, |s, st| {
        *st == GcState::Condemning && s.gc.condemned().contains(&ObjectRef::Segment(seg))
    });
    assert!(
        !s.index.condemned().unwrap().contains(&seg),
        "flagged already"
    );
    // The same content written again and cut: a dedup hit on `seg`.
    s.write(20, 3);
    s.commit().unwrap();
    let e = s.map.get(ChunkNo(20)).unwrap().expect("mapped");
    assert_eq!(
        entry_hint(&e).seg,
        seg,
        "the write was not a dedup hit on the listed segment"
    );
    units_to_end(&mut s);
    assert!(s.index.condemned().unwrap().contains(&seg));
    // After the grace the next mark finds it referenced.
    s.clock.advance(GRACE + Duration::from_secs(1));
    churn(&mut s, 1, 1000);
    pass_now(&mut s);
    assert!(!s.gc.condemned().contains(&ObjectRef::Segment(seg)));
    assert!(!s.index.condemned().unwrap().contains(&seg));
    assert!(SegmentStore::list(&s.store)
        .unwrap()
        .iter()
        .any(|x| x.seg == seg));
    s.check_roots().unwrap();
    s.check_ledger().unwrap();
}

/// An urgent pass while a pipelined pass is in its condemn units (list
/// written, flags partly set) or its deletes.
#[test]
fn an_urgent_pass_in_the_middle_of_a_pipelined_pass() {
    for at in [GcState::Condemning, GcState::Deleting] {
        let mut s = sim();
        condemned_at_start(&mut s);
        s.clock.advance(GRACE + Duration::from_secs(1));
        churn(&mut s, 2, 1000);
        units_until(&mut s, |_, st| *st == at);
        s.urgent(1 << 20).unwrap();
        assert_eq!(s.gc.state(), GcState::Idle);
        s.check_roots()
            .unwrap_or_else(|e| panic!("urgent in {at:?}: {e}"));
        s.check_ledger()
            .unwrap_or_else(|e| panic!("urgent in {at:?}: {e}"));
        check_quiescent(&mut s).unwrap_or_else(|e| panic!("urgent in {at:?}: {e}"));
    }
}

/// A request during a pipelined pass: the pass in progress ends as it is,
/// the next one is the whole sequence (it waits out a grace).
#[test]
fn a_request_during_a_pipelined_pass_makes_the_next_pass_whole() {
    let mut s = sim();
    condemned_at_start(&mut s);
    s.clock.advance(GRACE + Duration::from_secs(1));
    churn(&mut s, 2, 1000);
    units_until(&mut s, |_, st| *st == GcState::Deleting);
    assert!(s.gc.request_pass());
    let states = pass_now(&mut s);
    assert!(!states.contains(&GcState::Grace), "{states:?}");
    let passes = s.gc.stats().passes;
    churn(&mut s, 2, 2000);
    let mut grace = false;
    for _ in 0..10_000 {
        let p = s.step().unwrap();
        if p.state == GcState::Grace {
            grace = true;
            break;
        }
        assert!(!p.done, "the requested pass ended without a grace");
    }
    assert!(grace);
    s.pass().unwrap();
    assert_eq!(s.gc.stats().passes, passes + 1);
    s.check_roots().unwrap();
    s.check_ledger().unwrap();
}

/// A pin on a condemned commit object between its condemn and the pass that
/// would delete it: the commit and everything its root reaches stay.
#[test]
fn a_pin_between_condemn_and_delete_keeps_the_commit_and_its_root() {
    let mut s = sim();
    churn(&mut s, 6, 0);
    s.clock.advance(Duration::from_secs(1));
    pass_now(&mut s);
    let seq =
        s.gc.condemned()
            .into_iter()
            .filter_map(|o| match o {
                ObjectRef::Commit(c) => Some(c),
                _ => None,
            })
            .max()
            .expect("a condemned commit object");
    s.pins.push(PinRoot {
        kind: PinKind::Snapshot,
        name: "gcrate-pin".into(),
        commit_seq: seq,
        deadline: None,
    });
    s.store.set_pins(s.pins.clone());
    s.clock.advance(GRACE + Duration::from_secs(1));
    churn(&mut s, 1, 1000);
    pass_now(&mut s);
    assert!(s.commit_exists(seq.0), "the pinned commit was deleted");
    assert!(!s.gc.condemned().contains(&ObjectRef::Commit(seq)));
    assert!(CommitStore::list(&s.store)
        .unwrap()
        .iter()
        .any(|c| c.seq == seq));
    // Everything the pinned root reaches is still there (the model keeps no
    // content for a commit below the retained window, so this is checked
    // against a mark of the current roots, the pin among them).
    let l = s.with_view(|v| crate::liveness(v, s.cfg.gc)).expect("mark");
    assert!(
        l.roots.iter().any(|r| r.seq == seq),
        "the pin is not a root"
    );
    assert_eq!(l.unresolved, 0, "a root reaches a deleted extent");
    let segs: Vec<SegId> = SegmentStore::list(&s.store)
        .unwrap()
        .iter()
        .map(|x| x.seg)
        .collect();
    for seg in l.live.keys() {
        assert!(segs.contains(seg), "{seg:?} is live and gone");
    }
    s.check_ledger().unwrap();
}

// ---------------------------------------------- pacing (re-review M-2)

/// Steps once; true when a pass started (the step left the idle state, or
/// a pass ended within it).
fn starts(s: &mut Sim) -> bool {
    let before = s.gc.stats().passes;
    let p = s.unit().unwrap();
    p.state != GcState::Idle || s.gc.stats().passes != before
}

/// A pass that condemns only commit objects and map packs is not busy: the
/// next pass waits `pass_interval`, not `busy_interval`.
#[test]
fn a_pass_that_condemns_only_commits_and_packs_does_not_hurry_the_next() {
    let mut s = sim();
    churn(&mut s, 1, 0);
    s.clock.advance(Duration::from_secs(1));
    pass_now(&mut s);
    // Cuts without new data: old commit objects and packs die, no segment.
    for _ in 0..4 {
        s.commit().unwrap();
    }
    s.clock.advance(Duration::from_secs(31));
    pass_now(&mut s);
    let last = s.gc.status().last_pass.unwrap();
    assert_eq!(last.segments_condemned, 0, "{last:?}");
    assert!(
        s.gc.condemned()
            .iter()
            .any(|o| matches!(o, ObjectRef::Commit(_))),
        "the pass condemned no commit object"
    );
    // A new tip after the busy interval does not start a pass...
    s.clock.advance(Duration::from_secs(3));
    s.commit().unwrap();
    assert!(!starts(&mut s), "a pass followed after the busy interval");
    // ...the pass interval does.
    s.clock.advance(Duration::from_secs(28));
    assert!(starts(&mut s), "no pass after the pass interval");
}

/// The duty cap: the next pipelined pass waits at least three times the
/// last pass's mark time, also when graces end and the tip moves.
#[test]
fn the_next_pass_waits_three_times_the_last_mark() {
    let mut s = sim();
    condemned_at_start(&mut s);
    assert!(s.gc.min_gap() < Duration::from_secs(3));
    // As if the mark had taken 20 s on the view's clock.
    s.gc.last_mark = Duration::from_secs(20);
    assert_eq!(s.gc.min_gap(), Duration::from_secs(60));
    s.clock.advance(GRACE + Duration::from_secs(1));
    churn(&mut s, 1, 1000);
    assert!(
        starts(&mut s),
        "61 s after the pass, past the 60 s gap: a pass is due"
    );
    pass_now(&mut s);
    s.gc.last_mark = Duration::from_secs(20);
    s.clock.advance(Duration::from_secs(30));
    churn(&mut s, 1, 2000);
    assert!(!starts(&mut s), "a pass inside the duty gap");
    s.clock.advance(Duration::from_secs(31));
    assert!(starts(&mut s), "no pass after the duty gap");
}

/// An abandoned pipelined pass: the next one waits `pass_interval`, also for
/// a grace that has ended since the abandoned pass's mark began (the
/// `last_aborted` term of `due`: without it the grace trigger would retry at
/// the busy pace, every time a grace ends; gcrate final review F-2).
#[test]
fn after_an_abandoned_pass_the_next_waits_the_pass_interval() {
    let mut s = sim();
    // A is listed at about t = 1 s (its grace ends at about 61 s).
    condemned_at_start(&mut s);
    // B is listed at about t = 31 s (its grace ends at about 91 s).
    s.clock.advance(Duration::from_secs(30));
    churn(&mut s, 1, 500);
    pass_now(&mut s);
    // t = 62 s: A's grace has ended, B's has not. The next pass's mark
    // begins now and takes 35 s on the view's clock, so B's grace ends
    // after the mark began and before the pass is abandoned.
    s.clock.advance(Duration::from_secs(31));
    churn(&mut s, 1, 1000);
    s.hooks.advance_at("gc.mark.end", Duration::from_secs(35));
    s.arm_fault("index.drop.before", 1, false);
    let mut failed = false;
    for _ in 0..10_000 {
        match s.unit() {
            Ok(p) if p.done || p.state == GcState::Idle => break,
            Ok(_) => {}
            Err(_) => {
                failed = true;
                break;
            }
        }
    }
    assert!(failed, "the armed delete fault was not hit");
    s.disarm_faults();
    s.hooks.advance_at("gc.mark.end", Duration::ZERO);
    assert_eq!(s.gc.state(), GcState::Idle);
    // B's grace has ended since the abandoned mark began: only the abandoned
    // pass's pace holds the next pass back.
    s.clock.advance(Duration::from_secs(5));
    churn(&mut s, 1, 2000);
    assert!(!starts(&mut s), "a pass 5 s after an abandoned one");
    s.clock.advance(Duration::from_secs(26));
    assert!(starts(&mut s), "no retry after the pass interval");
    units_to_end(&mut s);
    s.check_roots().unwrap();
}

/// The duty cap's measure comes from the pass itself (gcrate final review
/// F-1): the time its mark units took on the view's clock, and nothing of the
/// delete units. A mark that takes 7 s makes the next gap 21 s; a pass whose
/// time goes into deletes leaves it at the busy interval.
#[test]
fn a_pass_measures_its_mark_time_on_the_view_clock_and_not_its_deletes() {
    let mut s = sim();
    churn(&mut s, 6, 0);
    s.clock.advance(Duration::from_secs(1));
    s.hooks.advance_at("gc.mark.end", Duration::from_secs(7));
    pass_now(&mut s);
    assert_eq!(s.gc.last_mark, Duration::from_secs(7));
    assert_eq!(s.gc.min_gap(), Duration::from_secs(21));
    // A pass whose mark takes no time and whose deletes take 40 s each.
    s.hooks.advance_at("gc.mark.end", Duration::ZERO);
    s.hooks
        .advance_at("gc.delete.before_unlink", Duration::from_secs(40));
    s.clock.advance(GRACE + Duration::from_secs(1));
    churn(&mut s, 1, 1000);
    pass_now(&mut s);
    assert!(s.gc.stats().bytes_freed > 0, "the pass deleted nothing");
    assert_eq!(
        s.gc.last_mark,
        Duration::ZERO,
        "deletes counted as mark time"
    );
    assert_eq!(s.gc.min_gap(), Duration::from_secs(2));
}

/// A grace shortened to the floor between condemn and verdict (what the
/// engine does under write pressure; the note, §5.6) is compared with the
/// mark's start on the same monotonic clock: nothing is deleted before the
/// shortened grace ran, whatever the clock does during the pass.
#[test]
fn a_grace_shortened_between_condemn_and_verdict_deletes_nothing_early() {
    let floor = Duration::from_secs(30);
    let mut s = sim();
    // Condemned at about t = 1 s under the full grace (60 s).
    let condemned = condemned_at_start(&mut s);
    s.grace = floor;
    // A pass whose mark begins at t = 29 s, 2 s before the shortened grace
    // ends, and runs 12 s on the view's clock: its deletes judge the mark's
    // start, not the clock they run at.
    s.clock.advance(Duration::from_secs(28));
    churn(&mut s, 1, 1000);
    s.hooks.advance_at("gc.mark.end", Duration::from_secs(12));
    pass_now(&mut s);
    s.hooks.advance_at("gc.mark.end", Duration::ZERO);
    for o in &condemned {
        assert!(s.gc.condemned().contains(o), "{o} went before its grace");
    }
    assert_eq!(s.gc.stats().bytes_freed, 0);
    s.check_roots().unwrap();
    // The next mark begins at about 45 s: after the shortened grace (31 s)
    // and before the configured one (61 s), so only the floor can have
    // deleted. The duty gap of the 12 s mark is cleared by hand so that the
    // pass is due.
    s.gc.last_mark = Duration::ZERO;
    let now = s.clock.monotonic();
    assert!(now >= Duration::from_secs(41), "{now:?}");
    s.clock.advance(Duration::from_secs(45).saturating_sub(now));
    churn(&mut s, 1, 2000);
    pass_now(&mut s);
    for o in &condemned {
        assert!(!s.gc.condemned().contains(o), "{o} is still listed");
    }
    assert!(s.gc.stats().bytes_freed > 0);
    // Not later than the configured grace would have allowed: nothing else
    // than the floor deleted these.
    assert!(s.clock.monotonic() < Duration::from_secs(61));
    s.check_roots().unwrap();
    s.check_ledger().unwrap();
}

/// An idle check after a busy pass clears "busy": a tip that moves later
/// waits for `pass_interval`.
#[test]
fn an_idle_check_after_a_busy_pass_clears_busy() {
    let mut s = sim();
    condemned_at_start(&mut s);
    // The graces end without a new tip: a pass deletes everything listed.
    s.clock.advance(GRACE + Duration::from_secs(1));
    pass_now(&mut s);
    assert!(s.gc.condemned().is_empty(), "{:?}", s.gc.condemned());
    let last = s.gc.status().last_pass.unwrap();
    assert!(last.segments_deleted > 0, "{last:?}");
    // Busy interval over, nothing changed: the idle check.
    s.clock.advance(Duration::from_secs(3));
    assert!(!starts(&mut s));
    // A new tip now waits for the pass interval.
    s.clock.advance(Duration::from_secs(3));
    churn(&mut s, 1, 1000);
    assert!(!starts(&mut s), "the idle check did not clear busy");
    // The idle check looked at +3 s: the next look is 30 s after it.
    s.clock.advance(Duration::from_secs(28));
    assert!(starts(&mut s));
}
