//! Milestone gcbatch: a pass makes one index update for the segments it
//! condemns and one per bounded batch for the segments it deletes, and a
//! crash anywhere in a batch leaves a state the next pass completes.

use std::collections::BTreeSet;

use lunix_format::{IdLen, SegId};
use lunix_iface::index::ChunkIndex;
use lunix_iface::store::SegmentStore;

use super::oracle::check_quiescent;
use super::sim::{Sim, SimCfg, GRACE};

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
    assert_index_consistent(&s, "after 600 deletes");
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
            s.crash_reopen(k).unwrap();
            s.check_roots().unwrap();
            // The reopened index still refuses dedup into the missing segments.
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

/// Rule 4 (an abandoned pass loses nothing): an error in the retire update
/// leaves the segments flagged; the next pass redoes the follow-up from the
/// flags and the index ends consistent.
#[test]
fn a_failed_retire_update_is_redone_by_the_next_pass() {
    let mut s = sim(64, false);
    let old = make_dead(&mut s, 6);
    for _ in 0..4 {
        s.commit().unwrap();
    }
    s.clock.advance(GRACE * 2);
    s.arm_fault("index.drop.before", 1, false);
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
