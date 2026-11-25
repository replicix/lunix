//! `Liveness::orphan_deps`: a live copy relied on through a hint whose
//! segment is gone. The SIBLING of the condemn-on-cut review's B1: compaction
//! moves the index copy off `S` into `N`, which only the tip FULL names; no
//! segment's `deps` records `N` (the entry hints a gone segment), so `S` was
//! deletable, and after `fsck --repair --truncate-tip` the predecessor could
//! not read the chunk.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use lunix_format::{IdLen, SegId};
use lunix_iface::index::ChunkIndex;
use lunix_iface::map::ChunkNo;
use lunix_iface::store::{SegmentStore, SweepRequest, VolumeDir};
use lunix_testkit::MemIndex;

use super::sim::{high_water, Sim, SimCfg, GRACE};
use crate::Liveness;

const CS: u32 = 16 << 10;

fn segs(s: &Sim) -> BTreeSet<SegId> {
    SegmentStore::list(&s.store)
        .unwrap()
        .iter()
        .map(|x| x.seg)
        .collect()
}

fn hint_of(s: &Sim, ci: u64) -> lunix_format::Hint {
    let e = s.map.get(ChunkNo(ci)).unwrap().expect("populated");
    lunix_iface::map::entry_hint(&e)
}

fn mark(s: &Sim) -> Liveness {
    s.with_view(|v| crate::liveness(v, s.cfg.gc)).unwrap()
}

/// What the script observed.
struct Run {
    /// The predecessor's read-back after the simulated truncate-tip.
    truncated: Result<(), String>,
    /// The segment that held the chunk before compaction moved it.
    s_seg: SegId,
    /// Where compaction moved it.
    n_seg: SegId,
    /// `S` still exists after the delete passes.
    s_alive: bool,
    /// Every `orphan_deps` set a mark returned along the way, and whether
    /// the source `Y` still existed then.
    orphans: Vec<(bool, BTreeSet<SegId>)>,
    /// With `unrelated`: the segment of two chunks no copy depends on, dead
    /// when the delete passes run, and whether it still exists after them.
    u_seg: Option<SegId>,
    u_alive: bool,
    /// `orphan_holds` of a real mark (with the collector's durability facts)
    /// taken once `N` exists and before any further cut.
    holds: BTreeMap<SegId, BTreeSet<SegId>>,
}

/// `--truncate-tip` on the simulated volume: drop the tip FULL, delete every
/// segment and pack above the predecessor's high-water mark, rebuild the
/// index from the segments left, and read every root back.
pub(crate) fn truncate_tip_and_check(s: &mut Sim) -> Result<(), String> {
    let n = s.retained.len();
    assert!(n >= 2, "a predecessor");
    let kept = s.retained[..n - 1].to_vec();
    let hwm = high_water(&kept);
    let mut named_segments = std::collections::HashSet::new();
    let mut named_packs = std::collections::HashSet::new();
    for c in &kept {
        if let lunix_format::commit::Commit::Full(f) = c {
            named_segments.extend(f.new_segments.iter().copied());
            named_packs.extend(f.map_packs.iter().copied());
            named_packs.extend(f.map_root.pack.iter().copied());
        }
    }
    s.store
        .sweep_orphans(&SweepRequest {
            high_water_mark: hwm,
            named_segments: &named_segments,
            named_packs: &named_packs,
        })
        .map_err(|e| e.to_string())?;
    let pred = match &kept[n - 2] {
        lunix_format::commit::Commit::Full(f) => f.clone(),
        _ => panic!("FULL"),
    };
    s.model.remove(&s.tip.common.commit_seq.0);
    s.tip = pred;
    s.retained = kept;
    let rebuilt = MemIndex::new(s.store.mem_ids());
    let all: Vec<SegId> = segs(s).into_iter().collect();
    rebuilt.rebuild(&all, &s.store).map_err(|e| e.to_string())?;
    s.check_roots_with(&rebuilt)
}

fn cfg_with(encrypted: bool) -> SimCfg {
    let mut c = SimCfg::new(CS, IdLen::Sixteen);
    c.encrypted = encrypted;
    c.segment_target = u64::from(CS) * 8;
    c
}

/// Chunks 0..2 (seeds 200..202) share segment `Y` with five more; the five
/// are overwritten and rolled off, so `Y` is compacted into `S`. The move of
/// chunk 1 is dropped with `lose_move` (a restart drops pending moves): its
/// entry keeps hinting `Y`. `Y` is deleted once `S` is durable. Chunks 0 and 2
/// are then overwritten, `S` is compacted into `N`, a cut names `N`, and the
/// delete passes run on `S` before any further cut.
fn script(lose_move: bool) -> Run {
    run_script(lose_move, Ending::TruncateTip, false)
}

/// How the script ends.
#[derive(Clone, Copy)]
enum Ending {
    /// A cut names `N`, the delete passes run, then truncate-tip.
    TruncateTip,
    /// No cut: `N` stays an unnamed output above the high-water mark, the
    /// delete passes run, then the process dies and the volume reopens (the
    /// open-time sweep removes `N`).
    Crash,
}

fn run_script(lose_move: bool, ending: Ending, unrelated: bool) -> Run {
    run_script_with(lose_move, ending, unrelated, false)
}

/// A mark with the collector's facts: `named_high` is the highest segment a
/// FULL below the tip names, `outputs` the unnamed compaction outputs.
fn mark_with_facts(s: &Sim, outputs: BTreeSet<SegId>) -> Liveness {
    let below = &s.retained[..s.retained.len() - 1];
    let named_high = below
        .iter()
        .filter_map(|c| match c {
            lunix_format::commit::Commit::Full(f) => f.new_segments.iter().map(|x| x.raw()).max(),
            _ => None,
        })
        .max()
        .unwrap_or(0);
    s.with_view(|v| crate::mark::liveness_with(v, s.cfg.gc, named_high, outputs))
        .unwrap()
}

fn run_script_with(lose_move: bool, ending: Ending, unrelated: bool, encrypted: bool) -> Run {
    let mut s = Sim::new(cfg_with(encrypted));
    for i in 0..8 {
        s.write(i, 200 + i);
    }
    s.commit().unwrap();
    let y = hint_of(&s, 0).seg;
    assert!((0..8).all(|i| hint_of(&s, i).seg == y));
    let h1 = hint_of(&s, 1);
    for i in 3..8 {
        s.write(i, 300 + i);
    }
    s.commit().unwrap();
    for _ in 0..4 {
        s.commit().unwrap();
    }
    let mut orphans = vec![(true, mark(&s).orphan_deps)];
    // Y -> S.
    s.clock.advance(Duration::from_secs(5));
    s.pass().unwrap();
    assert_eq!(s.moved.len(), 3, "three live extents moved");
    let s_seg = s.moved[0].new.seg;
    if lose_move {
        s.moved.retain(|m| m.old != h1);
        assert_eq!(s.moved.len(), 2);
    }
    s.commit().unwrap();
    assert_eq!(hint_of(&s, 1).seg == y, lose_move);
    // Y goes once S is named by a FULL with a successor.
    for _ in 0..12 {
        if !segs(&s).contains(&y) {
            break;
        }
        s.commit().unwrap();
        s.clock.advance(GRACE * 2);
        s.pass().unwrap();
        orphans.push((segs(&s).contains(&y), mark(&s).orphan_deps));
    }
    assert!(!segs(&s).contains(&y), "the source is deleted");
    assert!(segs(&s).contains(&s_seg));
    // With `unrelated`: chunks 20 and 21 get a segment of their own and die
    // with chunks 0 and 2, so that segment is condemned by the same pass that
    // compacts S and is judged by the same delete passes.
    let u_seg = unrelated.then(|| {
        s.write(20, 500);
        s.write(21, 501);
        s.commit().unwrap();
        hint_of(&s, 20).seg
    });
    // Chunks 0 and 2 die; only chunk 1 is live in S (1/3): S -> N.
    s.write(0, 400);
    s.write(2, 402);
    if unrelated {
        s.write(20, 600);
        s.write(21, 601);
    }
    s.commit().unwrap();
    for _ in 0..4 {
        s.commit().unwrap();
    }
    s.clock.advance(Duration::from_secs(5));
    s.pass().unwrap();
    assert!(!s.moved.is_empty(), "S was compacted");
    let n_seg = s.moved[0].new.seg;
    assert_ne!(n_seg, s_seg);
    if matches!(ending, Ending::TruncateTip) {
        // The cut that names N (the tip alone names it).
        s.commit().unwrap();
        assert!(s.tip.new_segments.contains(&n_seg));
    }
    orphans.push((false, mark(&s).orphan_deps));
    let outputs: BTreeSet<SegId> = match ending {
        Ending::Crash => [n_seg].into_iter().collect(),
        Ending::TruncateTip => BTreeSet::new(),
    };
    let real = mark_with_facts(&s, outputs);
    assert_eq!(
        real.hold_ran, lose_move,
        "only a pending orphan copy runs the Hold stage"
    );
    let holds = real.orphan_holds;
    // The delete passes, no cut in between.
    for _ in 0..4 {
        s.clock.advance(GRACE * 2);
        match ending {
            Ending::TruncateTip => s.pass().unwrap(),
            // On the idle volume an ordinary pass is not due again.
            Ending::Crash => {
                s.urgent(u64::MAX).unwrap();
            }
        }
    }
    let s_alive = segs(&s).contains(&s_seg);
    let u_alive = u_seg.is_some_and(|u| segs(&s).contains(&u));
    let truncated = match ending {
        Ending::TruncateTip => truncate_tip_and_check(&mut s),
        Ending::Crash => {
            s.crash_reopen(1).unwrap();
            assert!(!segs(&s).contains(&n_seg), "the sweep removed the output");
            s.check_roots()
        }
    };
    Run {
        truncated,
        s_seg,
        n_seg,
        s_alive,
        orphans,
        u_seg,
        u_alive,
        holds,
    }
}

#[test]
fn compaction_relocate_with_a_gone_hint_keeps_the_old_copy_until_the_new_one_is_durable() {
    let r = script(true);
    r.truncated
        .unwrap_or_else(|e| panic!("predecessor unreadable after truncate-tip: {e}"));
    assert_ne!(r.s_seg, r.n_seg);
    assert!(r.s_alive, "S outlives the copy only the tip names");
    assert!(
        r.orphans.iter().any(|(_, o)| o.contains(&r.n_seg)),
        "a mark saw the copy in N through a gone hint: {:?}",
        r.orphans
    );
}

/// Every re-hint applied: the entry hints `S`, `deps[S]` holds `N` and the
/// old rule already defers. The same read-back passes with or without the
/// orphan rule.
#[test]
fn control_every_rehint_applied_reads_back_after_truncate_tip() {
    let r = script(false);
    r.truncated
        .unwrap_or_else(|e| panic!("predecessor unreadable after truncate-tip: {e}"));
    assert!(
        r.orphans.last().is_some_and(|(_, o)| !o.contains(&r.n_seg)),
        "no entry reaches N through a gone hint: {:?}",
        r.orphans
    );
}

#[test]
fn orphan_deps_is_recorded_only_for_a_gone_hinted_segment() {
    for lose in [true, false] {
        let r = script(lose);
        for (y_exists, o) in &r.orphans {
            if *y_exists {
                assert!(o.is_empty(), "lose={lose}: a hinted segment exists: {o:?}");
            }
        }
    }
    let r = script(true);
    assert!(
        r.orphans.iter().any(|(y, o)| !y && !o.is_empty()),
        "the gone hint is recorded"
    );
}

/// The crash shape (no fsck, no proposal): the only index copy of a chunk is in
/// an unnamed compaction output while the segment it left is deleted; the
/// entries that read it hint a gone segment. A crash drops the output.
#[test]
fn a_crash_with_the_only_index_copy_in_an_unnamed_output_keeps_every_root_readable() {
    let r = run_script(true, Ending::Crash, false);
    r.truncated
        .unwrap_or_else(|e| panic!("a root is unreadable after the crash: {e}"));
    assert!(r.s_alive, "S outlives the unnamed output");
}

#[test]
fn control_a_crash_with_every_rehint_applied_keeps_every_root_readable() {
    let r = run_script(false, Ending::Crash, false);
    r.truncated
        .unwrap_or_else(|e| panic!("a root is unreadable after the crash: {e}"));
}

/// Runs `f` with the orphan hold off on this thread (restored on drop, also
/// when `f` panics).
pub(crate) fn with_hold_off<T>(f: impl FnOnce() -> T) -> T {
    let _off = crate::collector::HoldOff::on();
    f()
}

/// Negative controls of the precise hold: with it switched off the truncate-tip
/// sibling and the crash shape fail, with it on (the tests above) they pass.
#[test]
fn control_with_the_hold_off_the_sibling_and_the_crash_shape_fail() {
    let r = with_hold_off(|| script(true));
    assert!(!r.s_alive, "S is deleted without the hold");
    assert!(r.truncated.is_err(), "the predecessor is unreadable");
    let r = with_hold_off(|| run_script(true, Ending::Crash, false));
    assert!(r.truncated.is_err(), "a root is unreadable after the crash");
}

/// The hold is precise: a segment whose footer holds none of the pending ids
/// is deleted while `S` is held.
#[test]
fn an_unrelated_dead_segment_is_deleted_while_the_orphan_copy_is_pending() {
    for ending in [Ending::TruncateTip, Ending::Crash] {
        let r = run_script(true, ending, true);
        r.truncated
            .unwrap_or_else(|e| panic!("a root is unreadable: {e}"));
        assert!(r.s_alive, "S is still held");
        let u = r.u_seg.expect("the unrelated segment");
        assert_ne!(u, r.s_seg);
        assert!(!r.u_alive, "the unrelated segment {u:?} was not deleted");
    }
}

/// The hold, read from a real mark (not from the judge's own tests): `S`
/// holds the id whose only index copy is in the pending `N`, so `S` is held
/// for `N`; the unrelated dead segment is not a key; with the unrelated
/// segment present in the volume the set of keys is exactly `{S}`.
#[test]
fn a_real_mark_holds_the_segment_that_holds_the_id_and_no_other() {
    for ending in [Ending::TruncateTip, Ending::Crash] {
        let r = run_script(true, ending, true);
        assert_eq!(
            r.holds.keys().copied().collect::<Vec<_>>(),
            vec![r.s_seg],
            "only S is held"
        );
        assert_eq!(r.holds[&r.s_seg], [r.n_seg].into_iter().collect());
        assert!(!r.holds.contains_key(&r.u_seg.expect("U")));
    }
}

/// Review N18: the hold in the integrity-less encrypted sim (footers are
/// authenticated with the codec's keys).
#[test]
fn the_hold_works_with_encrypted_extents() {
    let r = run_script_with(true, Ending::TruncateTip, true, true);
    r.truncated
        .unwrap_or_else(|e| panic!("predecessor unreadable after truncate-tip: {e}"));
    assert!(r.s_alive && !r.u_alive);
    assert_eq!(r.holds.keys().copied().collect::<Vec<_>>(), vec![r.s_seg]);
}
