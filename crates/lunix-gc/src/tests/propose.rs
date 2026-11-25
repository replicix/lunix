//! Milestone condemncut: `Collector::propose_dead`. A proposal is a hint with
//! no authority: it lists, flags and starts the grace of what the collector's
//! own rules accept, between passes only, and the recheck mark that began
//! after the grace ended is the only judge of a delete.

use std::collections::BTreeSet;
use std::time::Duration;

use lunix_format::{CommitSeq, IdLen, ObjId, SegId};
use lunix_iface::gc::{Collector, DeadSince, GcState};
use lunix_iface::index::ChunkIndex;
use lunix_iface::store::{GcStateStore, SegmentStore};

use super::sim::{Sim, SimCfg, GRACE};
use crate::condemned::{CondemnedList, ObjectRef};
use crate::config::GcConfig;

const CS: u32 = 16 << 10;

fn cfg() -> SimCfg {
    let mut cfg = SimCfg::new(CS, IdLen::Sixteen);
    cfg.chunk_count = 64;
    cfg.gc = GcConfig {
        pass_interval: Duration::from_secs(30),
        busy_interval: Duration::from_secs(20),
        fast_recheck: Some(Duration::from_secs(1)),
        fast_recheck_always: true,
        tip_refs: true,
        ..GcConfig::daemon()
    };
    cfg
}

/// `n` one-chunk segments (chunks `first..`) overwritten, and the retained
/// window rolled past the overwrite: dead (no root names them, no retained
/// FULL names them).
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

/// The daemon's pressure arm: regular passes far apart, so that only
/// recheck-only passes delete (`fast_recheck_always`).
fn slow_cfg() -> SimCfg {
    let mut c = cfg();
    c.gc.pass_interval = Duration::from_secs(3600);
    c.gc.busy_interval = Duration::from_secs(3600);
    c
}

/// A volume with `n` dead segments and the collector loaded and idle.
fn scene(n: u64) -> Sim {
    scene_with(cfg(), n)
}

fn scene_with(cfg: SimCfg, n: u64) -> Sim {
    let mut s = Sim::new(cfg);
    // A clock well past a grace, so that a grace start of zero is "long ago".
    s.clock.advance(Duration::from_secs(1_000));
    // The collector loads its list at its first step.
    s.step().unwrap();
    make_dead(&mut s, 0, n, 1_000);
    s
}

fn segs(s: &Sim) -> BTreeSet<SegId> {
    SegmentStore::list(&s.store)
        .unwrap()
        .iter()
        .map(|x| x.seg)
        .collect()
}

fn ids(dead: &[DeadSince]) -> BTreeSet<SegId> {
    dead.iter().map(|d| d.seg).collect()
}

fn listed(s: &Sim) -> BTreeSet<SegId> {
    s.gc.condemned()
        .into_iter()
        .filter_map(|o| match o {
            ObjectRef::Segment(x) => Some(x),
            _ => None,
        })
        .collect()
}

fn flagged(s: &Sim) -> BTreeSet<SegId> {
    s.index.condemned().unwrap().into_iter().collect()
}

#[test]
fn a_proposal_lists_flags_and_starts_the_grace_and_writes_no_evidence_object() {
    let mut s = scene(6);
    let dead = s.dead_segments();
    assert!(dead.len() >= 6, "the overwritten segments are dead");
    let passes = s.gc.stats().passes;
    let objects = s.store.evidence_passes().unwrap();
    assert!(s.propose(&dead).unwrap(), "an idle collector takes it");
    let want = ids(&dead);
    assert_eq!(listed(&s), want);
    assert_eq!(flagged(&s), want, "the index flags are durable");
    let stored = s
        .store
        .load_condemned()
        .unwrap()
        .expect("the list is stored");
    let stored = CondemnedList::from_file(&stored).expect("decodes");
    assert_eq!(stored.entries.len(), want.len(), "and so is the list");
    assert_eq!(s.store.evidence_passes().unwrap(), objects, "no object");
    assert_eq!(s.gc.stats().passes, passes, "a proposal is not a pass");
    assert_eq!(s.gc.status().state, None, "the collector is still idle");
    assert_eq!(s.gc.proposed().into_iter().collect::<BTreeSet<_>>(), want);
    let r = s.gc.status().reclaim;
    assert_eq!(r.proposals_offered, dead.len() as u64);
    assert_eq!(r.proposal_condemned, dead.len() as u64);
    assert!(r.proposal_condemned_bytes > 0);
    assert_eq!(r.proposal_rejected_early, 0);
    let lines = s.gc.carried_text();
    assert_eq!(lines.lines().count(), dead.len());
    assert!(
        lines.lines().all(|l| l.contains("\"action\":\"condemn\"")
            && l.contains("\"reason\":\"unreferenced\"")
            && l.contains("\"kind\":\"segment\"")),
        "{lines}"
    );
    // Nothing is deleted before a mark that began after the grace ended.
    assert!(want.is_subset(&segs(&s)));
    s.clock.advance(GRACE * 2);
    s.pass().unwrap();
    assert!(
        want.is_disjoint(&segs(&s)),
        "deleted by the pass after the grace"
    );
    let r = s.gc.status().reclaim;
    assert_eq!(r.proposal_deleted, want.len() as u64);
    assert!(r.proposal_deleted_bytes > 0);
    assert_eq!(r.proposal_uncondemned, 0);
    assert!(s.gc.proposed().is_empty());
    // The carried lines went out with the pass's own evidence.
    assert_eq!(s.gc.carried_text(), "");
    assert_eq!(s.store.evidence_passes().unwrap().len(), objects.len() + 1);
    s.check_roots().unwrap();
    s.check_ledger().unwrap();
}

#[test]
fn a_proposal_changes_neither_the_pass_schedule_nor_its_counters() {
    let mut s = scene(4);
    s.clock.advance(Duration::from_secs(40));
    s.pass().unwrap();
    let before = (s.gc.stats(), s.gc.status().last_pass);
    // The pass condemned the dead segments: offering them finds them listed.
    let dead = s.dead_segments();
    assert!(!dead.is_empty() && ids(&dead).is_subset(&listed(&s)));
    // An offer of nothing, of the listed ones, then of a missing id.
    assert!(s.propose(&[]).unwrap());
    assert!(s.propose(&dead).unwrap());
    let missing = DeadSince {
        seg: ObjId::from_raw(0xffff_ffff).unwrap(),
        since: CommitSeq(0),
    };
    assert!(s.propose(&[missing]).unwrap());
    assert_eq!((s.gc.stats(), s.gc.status().last_pass), before);
    let r = s.gc.status().reclaim;
    assert_eq!(r.proposal_condemned, 0);
    assert_eq!(r.proposal_rejected_kept, dead.len() as u64 + 1);
}

#[test]
fn a_busy_collector_refuses_a_proposal_and_takes_it_when_idle() {
    let mut s = scene(3);
    s.clock.advance(Duration::from_secs(40));
    // One unit: the pass is under way (marking).
    let p = s.unit().unwrap();
    assert_ne!(p.state, GcState::Idle, "{:?}", p.state);
    let dead = s.dead_segments();
    assert!(!dead.is_empty());
    assert!(!s.propose(&dead).unwrap(), "refused: offer again");
    assert_eq!(s.gc.status().reclaim.proposals_deferred, 1);
    assert!(listed(&s).is_empty());
    for _ in 0..100_000 {
        let p = s.step().unwrap();
        if p.done || p.state == GcState::Idle {
            break;
        }
    }
    // The pass itself condemned them (it is the same dead set); a second
    // offer finds them listed: consumed, nothing added.
    let before = s.gc.status().reclaim.proposal_condemned;
    assert!(s.propose(&s.dead_segments()).unwrap());
    assert_eq!(s.gc.status().reclaim.proposal_condemned, before);
}

#[test]
fn a_collector_that_has_not_loaded_its_list_refuses() {
    let mut s = Sim::new(cfg());
    make_dead(&mut s, 0, 2, 1_000);
    let dead = s.dead_segments();
    assert!(!s.propose(&dead).unwrap(), "no step has loaded the list");
    s.step().unwrap();
    assert!(s.propose(&dead).unwrap());
}

#[test]
fn no_proposal_is_taken_while_a_pin_exists() {
    let mut s = scene(4);
    s.pin(0);
    let dead = s.dead_segments();
    assert!(s.propose(&dead).unwrap(), "consumed: rejected for good");
    assert!(listed(&s).is_empty());
    assert!(flagged(&s).is_empty());
    let r = s.gc.status().reclaim;
    assert_eq!(r.proposal_rejected_pins, dead.len() as u64);
    assert_eq!(r.proposal_condemned, 0);
    s.unpin(0);
    assert!(s.propose(&dead).unwrap());
    assert_eq!(listed(&s), ids(&dead));
}

#[test]
fn a_cut_newer_than_the_lowest_chain_floor_is_rejected_as_early() {
    let mut s = scene(4);
    let keep_floor = s.keep_floor();
    let dead: Vec<DeadSince> = s
        .dead_segments()
        .into_iter()
        .map(|d| DeadSince {
            since: CommitSeq(keep_floor + 1),
            ..d
        })
        .collect();
    assert!(s.propose(&dead).unwrap());
    assert!(listed(&s).is_empty());
    assert_eq!(
        s.gc.status().reclaim.proposal_rejected_early,
        dead.len() as u64
    );
    // At the floor itself it passes.
    let dead: Vec<DeadSince> = dead
        .into_iter()
        .map(|d| DeadSince {
            since: CommitSeq(keep_floor),
            ..d
        })
        .collect();
    assert!(s.propose(&dead).unwrap());
    assert_eq!(listed(&s), ids(&dead));
}

#[test]
fn what_a_retained_commit_names_a_listed_object_and_a_missing_one_are_kept() {
    let mut s = scene(3);
    // A fresh segment: the tip FULL names it.
    let before = segs(&s);
    s.write(40, 9_999);
    s.commit().unwrap();
    let named: Vec<DeadSince> = segs(&s)
        .difference(&before)
        .map(|&seg| DeadSince {
            seg,
            since: CommitSeq(0),
        })
        .collect();
    assert_eq!(named.len(), 1);
    assert!(s.propose(&named).unwrap());
    assert!(
        listed(&s).is_empty(),
        "a segment a retained FULL names is not condemned: {:?}",
        listed(&s)
    );
    assert_eq!(s.gc.status().reclaim.proposal_rejected_kept, 1);
    // Listed already: offered twice, condemned once.
    let dead = s.dead_segments();
    assert!(s.propose(&dead).unwrap());
    let once = s.gc.status().reclaim.proposal_condemned;
    assert!(s.propose(&dead).unwrap());
    assert_eq!(s.gc.status().reclaim.proposal_condemned, once);
}

#[test]
fn an_offer_is_capped_by_the_room_of_the_list_and_propose_max() {
    let mut c = cfg();
    c.gc.propose_max = 2;
    let mut s = Sim::new(c);
    s.step().unwrap();
    make_dead(&mut s, 0, 6, 1_000);
    let dead = s.dead_segments();
    assert!(dead.len() > 2);
    assert!(s.propose(&dead).unwrap());
    assert_eq!(listed(&s).len(), 2);
    let r = s.gc.status().reclaim;
    assert_eq!(r.proposal_rejected_room, dead.len() as u64 - 2);

    let mut c = cfg();
    c.gc.propose_max = 0;
    let mut s = Sim::new(c);
    s.step().unwrap();
    make_dead(&mut s, 0, 3, 1_000);
    assert!(s.propose(&s.dead_segments()).unwrap(), "takes none");
    assert!(listed(&s).is_empty());
    assert_eq!(s.gc.status().reclaim.proposals_offered, 0);
}

/// An offer of live segments (the table was wrong: a chunk id in a segment is
/// live elsewhere than the entries hint, or the table has a bug): condemned
/// and flagged for one grace, un-condemned by the recheck, never deleted, and
/// counted.
#[test]
fn a_wrong_proposal_is_uncondemned_by_the_recheck_and_never_deleted() {
    let mut s = scene(3);
    // A live segment with no retained FULL naming it: written long ago.
    s.write(30, 777);
    s.commit().unwrap();
    for _ in 0..4 {
        s.commit().unwrap();
    }
    let dead_before = ids(&s.dead_segments());
    let live: Vec<DeadSince> = s
        .all_segments()
        .into_iter()
        .filter(|d| !dead_before.contains(&d.seg))
        .collect();
    assert!(!live.is_empty());
    assert!(s.propose(&live).unwrap());
    let taken = listed(&s);
    assert!(
        !taken.is_empty(),
        "a live segment no retained FULL names is accepted: the recheck is the judge"
    );
    assert!(taken.is_subset(&flagged(&s)));
    s.clock.advance(GRACE * 2);
    s.pass().unwrap();
    for seg in &taken {
        assert!(segs(&s).contains(seg), "{seg:?} is live and was kept");
    }
    assert!(taken.is_disjoint(&listed(&s)), "un-condemned");
    assert!(taken.is_disjoint(&flagged(&s)), "and unflagged");
    let r = s.gc.status().reclaim;
    assert_eq!(r.proposal_uncondemned, taken.len() as u64);
    assert_eq!(r.proposal_deleted, 0);
    s.check_roots().unwrap();
    s.check_ledger().unwrap();
}

/// N6: an urgent pass in the middle of a recheck pass. The recheck is the
/// only judge of a delete, so a wrong proposal in the list must survive the
/// abandoned pass and the urgent pass's own mark.
#[test]
fn an_urgent_pass_in_the_middle_of_a_recheck_pass_deletes_nothing_live() {
    let mut s = scene_with(slow_cfg(), 3);
    s.write(30, 777);
    s.commit().unwrap();
    for _ in 0..4 {
        s.commit().unwrap();
    }
    // Everything that exists is offered: dead and live.
    let all = s.all_segments();
    assert!(s.propose(&all).unwrap());
    assert!(!listed(&s).is_empty());
    s.clock.advance(GRACE * 2);
    s.clock.advance(Duration::from_secs(2));
    // Under way: units until a pass is in a state past its mark.
    let mut states = Vec::new();
    for _ in 0..40 {
        let p = s.unit().unwrap();
        states.push(p.state);
        if p.state == GcState::Deleting {
            break;
        }
    }
    assert!(
        states.contains(&GcState::Deleting),
        "the pass reached its deletes: {states:?}"
    );
    assert_eq!(s.gc.pass_kind(), "recheck", "{states:?}");
    s.check_roots().unwrap();
    // The urgent pass abandons it and marks again.
    s.urgent(1 << 30).unwrap();
    s.check_roots().unwrap();
    s.check_ledger().unwrap();
    for _ in 0..3 {
        s.clock.advance(GRACE * 2);
        s.commit().unwrap();
        s.pass().unwrap();
        s.check_roots().unwrap();
    }
    s.check_ledger().unwrap();
    let live = s.model_reachable();
    for (k, id) in live {
        if k == 1 {
            assert!(
                segs(&s).contains(&ObjId::from_raw(id).unwrap()),
                "a live segment {id} was deleted"
            );
        }
    }
}

/// Crash images at the two new points (and the existing ones a proposal also
/// passes): the volume reopens, every root reads, and the work converges.
#[test]
fn a_crash_inside_a_proposal_loses_nothing_and_converges() {
    for (point, listed_after) in [
        ("gc.propose.before_list_write", false),
        ("gc.condemn.after_list_write", true),
        ("gc.propose.after_flags", true),
    ] {
        let mut s = scene(4);
        let dead = s.dead_segments();
        s.hooks.arm(point, 1);
        assert!(s.propose(&dead).is_err(), "{point} stops the proposal");
        assert_eq!(s.hooks.crashed(), Some(point));
        s.crash_reopen(7).unwrap();
        s.check_roots().unwrap();
        let on_disk = s
            .store
            .load_condemned()
            .unwrap()
            .and_then(|c| CondemnedList::from_file(&c))
            .map_or(0, |c| c.entries.len());
        assert_eq!(on_disk > 0, listed_after, "{point}");
        // The grace restarts at the reopen (a listed segment is re-flagged):
        // a pass after it deletes the dead ones.
        s.hooks.arm("none", 1);
        s.clock.advance(GRACE * 2);
        s.commit().unwrap();
        s.pass().unwrap();
        s.clock.advance(GRACE * 2);
        s.pass().unwrap();
        s.check_roots().unwrap();
        s.check_ledger().unwrap();
        assert!(
            ids(&dead).is_disjoint(&segs(&s)),
            "{point}: the dead segments converge to deleted"
        );
    }
}

/// The mark report: one call reports the mark that began in it and has not
/// finished, and the mark that finished with whether it began in the call.
#[test]
fn the_mark_report_says_which_marks_began_and_finished_in_a_call() {
    let mut s = scene(3);
    s.clock.advance(Duration::from_secs(40));
    // A unit per call: marks begin in one call and finish in a later one,
    // or begin and finish in the same.
    let (mut open, mut began, mut finished, mut same) = (false, 0, 0, 0);
    for _ in 0..400 {
        let p = s.unit().unwrap();
        if let Some(r) = s.gc.take_mark_report() {
            for f in &r.finished {
                assert_eq!(f.began_in_call, !open, "{r:?}");
                finished += 1;
                same += u32::from(f.began_in_call);
                open = false;
            }
            if r.started.is_some() {
                assert!(!open, "two marks at once");
                open = true;
                began += 1;
            }
        }
        if p.done {
            break;
        }
    }
    assert!(finished >= 1 && began >= 1, "{began} {finished}");
    assert!(same <= finished);
}

// ------------------------------------------------------------- the grace

/// Steps one pass to its end (or until the collector is idle) without moving
/// the clock.
fn pass_now(s: &mut Sim) {
    for _ in 0..100_000 {
        let p = s.step().unwrap();
        if p.done || p.state == GcState::Idle {
            return;
        }
    }
    panic!("a pass did not finish");
}

/// `secs` seconds, one at a time, a pass at each: what a daemon that ticks
/// does while a grace runs.
fn age(s: &mut Sim, secs: u64) {
    for _ in 0..secs {
        s.clock.advance(Duration::from_secs(1));
        pass_now(s);
    }
}

fn all_there(s: &Sim, dead: &BTreeSet<SegId>) {
    assert!(dead.is_subset(&segs(s)), "deleted before the grace ended");
    assert!(dead.is_subset(&listed(s)), "no longer listed");
    assert!(dead.is_subset(&flagged(s)), "no longer flagged");
}

/// The grace of a proposal starts when it is made: with passes every second
/// (recheck-only ones, as soon as a grace ends) nothing goes before it ends
/// and everything after. A grace start of `Duration::ZERO`, or one derived
/// from `since`, fails the first half.
#[test]
fn nothing_proposed_is_deleted_before_its_grace_ends_and_all_of_it_after() {
    let mut s = scene_with(slow_cfg(), 4);
    let dead = s.dead_segments();
    assert!(s.propose(&dead).unwrap());
    let want = ids(&dead);
    age(&mut s, GRACE.as_secs() - 1);
    all_there(&s, &want);
    age(&mut s, 4);
    assert!(want.is_disjoint(&segs(&s)), "still there after the grace");
    assert_eq!(s.gc.status().reclaim.proposal_deleted, want.len() as u64);
}

/// The same across a crash between the flags and the grace: the grace
/// restarts at the load, not at the proposal.
#[test]
fn the_grace_of_a_proposal_restarts_at_the_load_after_a_crash() {
    let mut s = scene_with(slow_cfg(), 4);
    let dead = s.dead_segments();
    let want = ids(&dead);
    s.hooks.arm("gc.propose.after_flags", 1);
    assert!(s.propose(&dead).is_err());
    // Most of a grace passes while the process is down.
    s.clock.advance(GRACE - Duration::from_secs(5));
    s.crash_reopen(3).unwrap();
    s.hooks.arm("none", 1);
    age(&mut s, GRACE.as_secs() - 1);
    assert!(
        want.is_subset(&segs(&s)),
        "deleted before the restarted grace"
    );
    age(&mut s, 5);
    assert!(want.is_disjoint(&segs(&s)), "still there after it");
    s.check_roots().unwrap();
}

/// An urgent pass judges with the grace floor of its own (30 s here) from the
/// start of the proposal's grace: before it nothing goes.
#[test]
fn an_urgent_pass_less_than_the_grace_floor_after_a_proposal_deletes_none_of_it() {
    let mut s = scene_with(slow_cfg(), 4);
    let dead = s.dead_segments();
    let want = ids(&dead);
    assert!(s.propose(&dead).unwrap());
    s.clock.advance(Duration::from_secs(20));
    s.urgent(1 << 30).unwrap();
    all_there(&s, &want);
    // After the floor (and before the configured grace) the urgent pass may
    // delete them, which shows the first half was not vacuous.
    s.clock.advance(Duration::from_secs(15));
    s.urgent(1 << 30).unwrap();
    assert!(want.is_disjoint(&segs(&s)), "the floor grace was over");
    s.check_roots().unwrap();
}

// ------------------------------------------------------- B1 with a proposal

/// The review's B1: a wrong proposal flags a segment that is live by id
/// through a gone hint. While it is flagged dedup cannot hit it, so the rewrite
/// of the same content makes a second copy `N` that only the tip names; the
/// recheck then finds the first segment `S` dead. The entries that relied on
/// `S` hint a gone segment (a move was lost), so no `deps` held `N`: without
/// the orphan rule `S` is deleted and, after `--truncate-tip` (or a crash),
/// `N` is gone with nothing left to read.
///
/// Script: chunks 0..8 share `Y`; `Y` is compacted into `S`, the move of
/// chunk 1 is lost (a restart drops pending moves), `Y` is deleted once `S` is
/// durable; chunks 0 and 2 are overwritten and the window rolls, so nothing
/// but chunk 1's gone hint reaches `S`. Then (with `propose`) `S` is offered
/// and accepted; chunk 5 gets chunk 1's content (a cut names the new copy
/// `N`, no further cut); the delete passes run; the volume is truncated back
/// to the predecessor and every root is read.
fn b1_script(propose: bool, later_cuts: bool) -> B1 {
    use super::orphandeps::truncate_tip_and_check;
    let mut c = SimCfg::new(CS, IdLen::Sixteen);
    c.segment_target = u64::from(CS) * 8;
    let mut s = Sim::new(c);
    let hint = |s: &Sim, ci: u64| {
        let e = s
            .map
            .get(lunix_iface::map::ChunkNo(ci))
            .unwrap()
            .expect("populated");
        lunix_iface::map::entry_hint(&e)
    };
    for i in 0..8 {
        s.write(i, 200 + i);
    }
    s.commit().unwrap();
    let y = hint(&s, 0).seg;
    let h1 = hint(&s, 1);
    for i in 3..8 {
        s.write(i, 300 + i);
    }
    s.commit().unwrap();
    for _ in 0..4 {
        s.commit().unwrap();
    }
    s.clock.advance(Duration::from_secs(5));
    s.pass().unwrap();
    let s_seg = s.moved[0].new.seg;
    s.moved.retain(|m| m.old != h1);
    s.commit().unwrap();
    assert_eq!(hint(&s, 1).seg, y, "the entry of chunk 1 still hints Y");
    for _ in 0..12 {
        if !segs(&s).contains(&y) {
            break;
        }
        s.commit().unwrap();
        s.clock.advance(GRACE * 2);
        s.pass().unwrap();
    }
    assert!(!segs(&s).contains(&y) && segs(&s).contains(&s_seg));
    s.write(0, 400);
    s.write(2, 402);
    s.commit().unwrap();
    for _ in 0..4 {
        s.commit().unwrap();
    }
    if propose {
        let offer = [DeadSince {
            seg: s_seg,
            since: CommitSeq(0),
        }];
        assert!(s.propose(&offer).unwrap());
        assert!(listed(&s).contains(&s_seg), "the wrong proposal was taken");
    }
    // Chunk 1's content again, at chunk 5.
    s.write(5, 201);
    s.commit().unwrap();
    let n_new = segs(&s).iter().any(|x| x.raw() > s_seg.raw());
    // The mechanism, read right after the rewrite's cut (the passes may
    // compact `S`): where the index keeps the copy of chunk 1's id, and
    // whether only the tip FULL names that segment.
    let id = s.codec.chunk_id(
        lunix_format::ExtentKind::Data,
        &super::sim::data_of(201, CS),
    );
    let idx_seg = s.index.get(&id).unwrap().expect("indexed").hint.seg;
    let tip_only = s.tip.new_segments.contains(&idx_seg)
        && !s.retained[..s.retained.len() - 1].iter().any(|c| {
            matches!(c, lunix_format::commit::Commit::Full(f) if f.new_segments.contains(&idx_seg))
        });
    for _ in 0..4 {
        s.clock.advance(GRACE * 2);
        s.pass().unwrap();
    }
    let alive = segs(&s).contains(&s_seg);
    let mut out = B1 {
        read: Ok(()),
        s_seg,
        s_alive: alive,
        idx_seg,
        tip_only,
        n_new,
        s_alive_after: None,
    };
    if later_cuts {
        // The deferral ends: a cut names the copy and a second gives that
        // FULL a successor.
        for _ in 0..3 {
            s.commit().unwrap();
            s.clock.advance(GRACE * 2);
            s.pass().unwrap();
        }
        for _ in 0..3 {
            s.clock.advance(GRACE * 2);
            s.pass().unwrap();
        }
        out.s_alive_after = Some(segs(&s).contains(&s_seg));
    } else {
        out.read = truncate_tip_and_check(&mut s);
    }
    out
}

/// What `b1_script` observed.
struct B1 {
    read: Result<(), String>,
    s_seg: SegId,
    s_alive: bool,
    /// The segment the index keeps chunk 1's id in after the rewrite.
    idx_seg: SegId,
    /// Only the tip FULL names that segment.
    tip_only: bool,
    n_new: bool,
    /// With `later_cuts`: `S` exists after the cuts that end the deferral.
    s_alive_after: Option<bool>,
}

#[test]
fn a_wrong_proposal_cannot_delete_the_only_copy_a_gone_hint_relies_on() {
    let r = b1_script(true, false);
    r.read
        .unwrap_or_else(|e| panic!("the predecessor is unreadable after truncate-tip: {e}"));
    assert!(r.s_alive, "S outlives the copy only the tip names");
    // The mechanism, not just the outcome: the flagged `S` was skipped by
    // dedup, so the index copy is in a new segment only the tip names.
    assert!(r.n_new);
    assert_ne!(r.idx_seg, r.s_seg, "the index copy left S");
    assert!(r.tip_only, "only the tip names the copy {:?}", r.idx_seg);
}

/// Negative control on this code (review S10): with the orphan hold off the
/// proposal script loses the predecessor's chunk after truncate-tip.
#[test]
fn control_with_the_hold_off_the_b1_proposal_script_fails() {
    let r = super::orphandeps::with_hold_off(|| b1_script(true, false));
    assert!(!r.s_alive, "S is deleted without the hold");
    assert!(
        r.read.is_err(),
        "the predecessor reads back without the hold"
    );
}

/// The same script without the proposal: dedup hits `S`, nothing is lost.
#[test]
fn control_without_the_proposal_the_predecessor_reads_back() {
    let r = b1_script(false, false);
    r.read
        .unwrap_or_else(|e| panic!("the predecessor is unreadable after truncate-tip: {e}"));
    assert_eq!(r.idx_seg, r.s_seg, "the index copy is still in S");
}

/// The deferral ends: after the cuts that make the copy durable `S` goes.
#[test]
fn the_deferral_of_the_b1_script_ends_with_two_more_cuts() {
    let r = b1_script(true, true);
    assert!(r.s_alive, "held before the cuts");
    assert_eq!(r.s_alive_after, Some(false), "S is deleted once durable");
}

// -------------------------------------------------------------- the witness

/// The lag-rule witness of mode `on` (`proposal_named`): a proposed segment
/// that a retained root other than the tip still names. The collector's own
/// `since` filter keeps it out; with the filter off (test configuration) the
/// mark that follows counts it, and the recheck un-condemns it.
#[test]
fn a_proposal_a_retained_older_root_still_names_is_counted_and_never_deleted() {
    let run = |unfiltered: bool| {
        let mut c = cfg();
        c.gc.propose_unfiltered = unfiltered;
        let mut s = Sim::new(c);
        s.step().unwrap();
        s.write(5, 7_001);
        s.commit().unwrap();
        // Its creating commit leaves the retained window; the chunk stays
        // in every retained root.
        for _ in 0..4 {
            s.commit().unwrap();
        }
        let before = segs(&s);
        s.write(5, 7_002);
        s.commit().unwrap();
        let old: Vec<DeadSince> = before
            .iter()
            .map(|&seg| DeadSince {
                seg,
                since: s.tip.common.commit_seq,
            })
            .collect();
        (s, old)
    };
    // Filtered: the cut is newer than the lowest chain_floor.
    let (mut s, old) = run(false);
    assert!(s.propose(&old).unwrap());
    assert!(listed(&s).is_empty());
    assert!(s.gc.status().reclaim.proposal_rejected_early > 0);
    // Unfiltered: condemned, then named by the older root at the next mark.
    let (mut s, old) = run(true);
    assert!(s.propose(&old).unwrap());
    let taken = listed(&s);
    assert!(!taken.is_empty(), "the filter is off");
    s.clock.advance(GRACE * 2);
    s.pass().unwrap();
    let r = s.gc.status().reclaim;
    assert!(r.proposal_named > 0, "{r:?}");
    assert!(taken.iter().all(|x| segs(&s).contains(x)), "kept");
    s.check_roots().unwrap();
}

// ------------------------------------------------------------ equivalence

fn next(z: &mut u64) -> u64 {
    *z = z.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut x = *z;
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}

/// One seeded workload (unique data, so nothing dedups; no compaction), with
/// or without correct proposals, run to quiescence. Returns the segments that
/// exist then (their number and their bytes: the ids of the later ones depend
/// on how many packs and runs the passes' timing made) and the segments the
/// proposals condemned.
fn converge(seed: u64, propose: bool) -> ((usize, u64), u64) {
    let mut c = cfg();
    c.gc.compact_per_pass = 0;
    c.gc.tip_refs = false;
    let mut s = Sim::new(c);
    let mut z = seed;
    let mut unique = 1_000_000u64;
    for _ in 0..300 {
        match next(&mut z) % 100 {
            0..=44 => {
                unique += 1;
                s.write(next(&mut z) % 64, unique);
            }
            45..=49 => s.discard(next(&mut z) % 64),
            50..=64 => s.commit().expect("commit"),
            65..=79 => {
                s.step().expect("step");
                s.clock.advance(Duration::from_secs(1 + next(&mut z) % 40));
            }
            80..=86 => s.clock.advance(Duration::from_secs(20 + next(&mut z) % 80)),
            87..=89 => s.set_retain(1 + (next(&mut z) % 4) as u32),
            90..=93 => {
                s.urgent(next(&mut z) % (512 << 10)).expect("urgent");
            }
            _ => {
                // No draw in either run: the two streams stay the same.
                if propose {
                    let dead = s.dead_segments();
                    s.propose(&dead).expect("propose");
                }
            }
        }
    }
    s.quiesce().expect("quiesce");
    s.check_roots().expect("every root reads");
    s.check_ledger().expect("the ledger equals the files");
    let condemned = s.gc.status().reclaim.proposal_condemned;
    let segs = SegmentStore::list(&s.store).expect("list");
    (
        (segs.len(), segs.iter().map(|x| x.size_bytes).sum()),
        condemned,
    )
}

/// Correct proposals change when segments are condemned and deleted, never
/// which segments the volume ends with (the number and the bytes of the ones
/// left; the quiescent oracle says they are exactly the reachable ones).
#[test]
fn correct_proposals_converge_to_the_same_objects_as_none() {
    let mut proposed = 0;
    for seed in 0..10u64 {
        let (with, n) = converge(0x7072_6f70_6f73_0000 + seed, true);
        let (without, none) = converge(0x7072_6f70_6f73_0000 + seed, false);
        assert_eq!(none, 0);
        assert_eq!(with, without, "seed {seed}: the segments left differ");
        proposed += n;
    }
    assert!(proposed > 0, "no proposal condemned anything");
}
