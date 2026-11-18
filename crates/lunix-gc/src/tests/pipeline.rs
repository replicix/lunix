//! The pipelined schedule (`GcConfig::pipeline`, milestone gcrate): a
//! scheduled pass does not wait out the grace; its mark is the recheck of
//! the listed objects whose grace ended before it began, and a pass starts
//! when a grace ends. A requested pass still runs the whole sequence.

use std::time::Duration;

use lunix_format::{ExtentKind, IdLen, SegId};
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
