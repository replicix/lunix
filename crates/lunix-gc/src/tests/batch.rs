//! Milestone gcbatch: a pass makes one index update for the segments it
//! condemns and one per bounded batch for the segments it deletes, and a
//! crash anywhere in a batch leaves a state the next pass completes.

use std::collections::BTreeSet;

use std::time::Duration;

use lunix_format::{ExtentKind, IdLen, SegId};
use lunix_iface::crypto::ExtentCodec;
use lunix_iface::gc::GcState;
use lunix_iface::index::ChunkIndex;
use lunix_iface::store::SegmentStore;

use super::oracle::check_quiescent;
use super::sim::{data_of, Sim, SimCfg, GRACE};
use crate::condemned::ObjectRef;

const CS: u32 = 16 << 10;

fn sim(chunks: u64, pipeline: bool) -> Sim {
    let mut cfg = SimCfg::new(CS, IdLen::Sixteen);
    cfg.chunk_count = chunks;
    cfg.gc.pipeline = pipeline;
    Sim::new(cfg)
}

fn segs(s: &Sim) -> BTreeSet<SegId> {
    SegmentStore::list(&s.store)
        .unwrap()
        .iter()
        .map(|x| x.seg)
        .collect()
}

/// `n` segments of one chunk each, all overwritten (so dead once the
/// retained window passes the overwrite).
fn id_of(seed: u64) -> lunix_format::ChunkId {
    lunix_testkit::PlainCodec::new().chunk_id(ExtentKind::Data, &data_of(seed, CS))
}

fn make_dead(s: &mut Sim, n: u64) -> BTreeSet<SegId> {
    for i in 0..n {
        s.write(i, 1_000 + i);
        s.commit().unwrap();
    }
    let old = segs(s);
    for i in 0..n {
        s.write(i, 5_000 + i);
    }
    s.commit().unwrap();
    old
}

/// Flags that name a segment with no file: what a delete batch leaves between
/// its unlinks and its index update.
fn flagged_missing(s: &Sim) -> usize {
    let here = segs(s);
    s.index
        .condemned()
        .unwrap()
        .into_iter()
        .filter(|g| !here.contains(g))
        .count()
}

/// The index invariants of a volume at rest: no flag without a file, no entry
/// into a segment without a file.
fn assert_index_consistent(s: &Sim, ctx: &str) {
    let here = segs(s);
    for g in s.index.condemned().unwrap() {
        assert!(here.contains(&g), "{ctx}: flag of missing segment {g:?}");
    }
    let mut bad = Vec::new();
    s.index
        .iter_entries(&mut |e| {
            if !here.contains(&e.hint.seg) {
                bad.push(e.hint.seg);
            }
        })
        .unwrap();
    assert!(
        bad.is_empty(),
        "{ctx}: entries into missing segments {bad:?}"
    );
}

/// Rule 1: the segments a pass condemns are flagged by one index update; the
/// segments it deletes are dropped and unflagged by one more, however many.
#[test]
fn a_pass_condemns_and_retires_its_segments_in_one_index_update_each() {
    for pipeline in [false, true] {
        let mut s = sim(64, pipeline);
        let old = make_dead(&mut s, 12);
        s.quiesce().unwrap();
        assert!(
            old.iter().all(|g| !segs(&s).contains(g)),
            "every dead segment was deleted (pipeline {pipeline})"
        );
        assert_eq!(
            s.index.batch_calls(),
            (1, 1),
            "one condemn update and one retire update (pipeline {pipeline})"
        );
        assert_eq!(s.index.single_calls(), 0, "no per-segment index call");
        assert_index_consistent(&s, "after the quiesce");
    }
}

/// Rule 2: a delete phase's batch is bounded: 600 deletes make three updates
/// (256 + 256 + 88), not 600 and not one unbounded window.
#[test]
fn deletes_are_batched_up_to_a_bound() {
    let mut s = sim(700, false);
    let old = make_dead(&mut s, 600);
    s.quiesce().unwrap();
    assert!(old.iter().all(|g| !segs(&s).contains(g)));
    assert_eq!(s.index.batch_calls(), (1, 3));
    assert_eq!(s.index.single_calls(), 0, "no per-segment index call");
    assert_index_consistent(&s, "after 600 deletes");
}

/// Rule 2b: nothing stays queued between steps (a clean close then leaves no
/// unlinked segment with entries in the index): after every call that does
/// one unit of work, no flagged segment is missing its file.
#[test]
fn a_step_never_leaves_a_deleted_segment_unrecorded() {
    for pipeline in [false, true] {
        let mut s = sim(64, pipeline);
        let old = make_dead(&mut s, 10);
        for _ in 0..4 {
            s.commit().unwrap();
        }
        let mut deleted_in_units = false;
        for _ in 0..400 {
            s.clock.advance(GRACE * 2);
            let before = old.iter().filter(|g| segs(&s).contains(g)).count();
            let p = s.unit().unwrap();
            assert_eq!(
                flagged_missing(&s),
                0,
                "a unit left unlinked segments flagged (pipeline {pipeline})"
            );
            deleted_in_units |= old.iter().filter(|g| segs(&s).contains(g)).count() < before;
            if p.done && deleted_in_units {
                break;
            }
        }
        assert!(deleted_in_units, "deletes happened in unit steps");
        assert_index_consistent(&s, "after the unit steps");
    }
}

/// Rule 3 (crash consistency of the delete batch): a stop after the k-th
/// unlink leaves k segments unlinked and still flagged (their entries are not
/// reachable for dedup); the reopened volume reads every root, and the next
/// passes finish the work with the index consistent.
#[test]
fn a_crash_inside_a_delete_batch_is_completed_by_the_next_passes() {
    for pipeline in [false, true] {
        for k in 1..=6u64 {
            let mut s = sim(64, pipeline);
            let old = make_dead(&mut s, 8);
            let ids: Vec<(SegId, _)> = (0..8)
                .map(|i| {
                    let id = id_of(1_000 + i);
                    (s.index.get(&id).unwrap().expect("indexed").hint.seg, id)
                })
                .collect();
            // Window past the retention; the deletes come in the whole
            // pass itself, or in the pass after the condemning one.
            for _ in 0..4 {
                s.commit().unwrap();
            }
            s.clock.advance(GRACE * 2);
            if pipeline {
                s.pass().unwrap();
                s.clock.advance(GRACE * 2);
            }
            s.hooks.arm("gc.delete.after_unlink", k);
            assert!(
                s.pass().is_err(),
                "the stop at unlink {k} fired (pipeline {pipeline})"
            );
            assert_eq!(
                flagged_missing(&s),
                k as usize,
                "k unlinked segments wait for one index update (pipeline {pipeline})"
            );
            assert_eq!(
                old.iter().filter(|g| !segs(&s).contains(g)).count(),
                k as usize
            );
            // In the window: no dedup hit into an unlinked segment.
            for (g, id) in &ids {
                if !segs(&s).contains(g) {
                    assert!(
                        s.index.get_for_dedup(id).unwrap().is_none(),
                        "a dedup hit into the unlinked segment {g:?}"
                    );
                }
            }
            s.crash_reopen(k).unwrap();
            s.check_roots().unwrap();
            s.clock.advance(GRACE * 2);
            s.commit().unwrap();
            s.pass().unwrap();
            s.check_roots().unwrap();
            s.check_ledger().unwrap();
            check_quiescent(&mut s).unwrap_or_else(|e| panic!("k {k}: {e:?}"));
            assert!(old.iter().all(|g| !segs(&s).contains(g)));
            assert_index_consistent(&s, &format!("k {k} pipeline {pipeline}"));
        }
    }
}

/// Rule 4 (an abandoned pass loses nothing): errors in the retire update and
/// in the abandoned pass's retry leave the segments flagged; the next pass redoes the follow-up from the
/// flags and the index ends consistent.
#[test]
fn a_failed_retire_update_is_redone_by_the_next_pass() {
    let mut s = sim(64, false);
    let old = make_dead(&mut s, 6);
    for _ in 0..4 {
        s.commit().unwrap();
    }
    s.clock.advance(GRACE * 2);
    // The update fails, and so does the retry of the abandoned pass.
    s.arm_fault("index.drop.before", 1, false);
    s.arm_fault("index.drop.before", 2, false);
    let failed = s.pass().is_err();
    s.disarm_faults();
    assert!(failed, "the retire update failed once");
    assert_eq!(flagged_missing(&s), 6, "all six wait, flagged");
    for g in &old {
        assert!(
            s.index.condemned().unwrap().contains(g),
            "{g:?} stays flagged"
        );
    }
    for _ in 0..6 {
        s.clock.advance(GRACE * 2);
        s.commit().unwrap();
        s.pass().unwrap();
    }
    assert_index_consistent(&s, "after the retry");
}

/// S1: an error in a delete step (here the unlink of the fourth segment)
/// abandons the pass, but the segments it had already unlinked are recorded
/// by the abort's one try, not left flagged until the next pass.
#[test]
fn an_error_in_a_delete_step_still_records_the_segments_it_unlinked() {
    let mut s = sim(64, false);
    let old = make_dead(&mut s, 8);
    for _ in 0..4 {
        s.commit().unwrap();
    }
    s.clock.advance(GRACE * 2);
    s.arm_fault("segment.delete.before", 4, false);
    let failed = s.pass().is_err();
    s.disarm_faults();
    assert!(failed, "the fourth unlink failed");
    let gone = old.iter().filter(|g| !segs(&s).contains(g)).count();
    assert_eq!(gone, 3, "three segments were unlinked");
    assert_eq!(flagged_missing(&s), 0, "and recorded at the abort");
    assert_index_consistent(&s, "after the failed step");
    for _ in 0..6 {
        s.clock.advance(GRACE * 2);
        s.commit().unwrap();
        s.pass().unwrap();
    }
    assert!(old.iter().all(|g| !segs(&s).contains(g)));
    assert_index_consistent(&s, "after the retries");
}

/// N2: the segments a recheck un-condemns are unflagged by one index update
/// and the list is stored once, however many. Five listed segments get a
/// dedup hit between the mark and the flags (the pipelined order), so the
/// pass after the grace finds them referenced.
#[test]
fn rechecks_uncondemn_their_segments_in_one_update() {
    let mut s = sim(64, true);
    s.cfg.gc.pass_interval = Duration::from_secs(1);
    let old = make_dead(&mut s, 5);
    for _ in 0..4 {
        s.commit().unwrap();
    }
    s.clock.advance(Duration::from_secs(5));
    let listed = |s: &Sim| {
        old.iter()
            .all(|g| s.gc.condemned().contains(&ObjectRef::Segment(*g)))
    };
    for _ in 0..100_000 {
        let p = s.unit().unwrap();
        if p.state == GcState::Condemning && listed(&s) {
            break;
        }
        assert!(!p.done, "the pass ended before the list held the segments");
    }
    assert!(s.index.condemned().unwrap().is_empty(), "flagged already");
    // The same content again, cut: dedup hits on the listed segments.
    for i in 0..5 {
        s.write(20 + i, 1_000 + i);
    }
    s.commit().unwrap();
    for _ in 0..100_000 {
        let p = s.unit().unwrap();
        if p.done || p.state == GcState::Idle {
            break;
        }
    }
    assert_eq!(s.index.condemned().unwrap().len(), 5);
    s.clock.advance(GRACE + Duration::from_secs(1));
    for i in 0..5 {
        s.write(40 + i, 9_000 + i);
    }
    s.commit().unwrap();
    s.pass().unwrap();
    assert!(old.iter().all(|g| segs(&s).contains(g)), "all five kept");
    assert!(s.index.condemned().unwrap().is_empty(), "flags cleared");
    assert_eq!(s.index.uncondemn_calls(), 1, "one update for the five");
    assert_eq!(s.index.single_calls(), 0);
    s.check_roots().unwrap();
    s.check_ledger().unwrap();
}
