//! Directed tests the M17.A re-review asked M17.B to add ("Remaining 1"):
//! two guards that the shipped suite did not fail without.
//!
//! * `reconcile` drops the index entries of a flagged segment that is gone
//!   before it clears the flag (the third site of the B1 fix);
//! * the delete-time index invariant looks at **every** segment a dead
//!   segment's stale hints depend on (`deps` is a set), not only the highest.

mod common;

use common::Vol;
use lunix_format::SegId;
use lunix_iface::index::ChunkIndex;
use lunix_iface::store::SegmentStore;

/// A flagged segment that is gone (a crash between the unlink and
/// `drop_segment`), a lost condemned list, and index entries of the process
/// that still point into the segment: the next pass must drop those entries
/// before it clears the flag, or dedup returns a verified hit into a segment
/// that does not exist (silent loss of the next write of that content).
#[test]
fn reconcile_drops_the_entries_of_a_gone_flagged_segment_before_clearing_its_flag() {
    let mut v = Vol::new(4);
    v.write(0, 77);
    v.commit().expect("commit");
    let gone = v.hint(0).seg;
    v.write(0, 78);
    v.commit().expect("commit");
    v.commit().expect("commit");
    v.commit().expect("commit");
    let id = v.id_of(77);
    assert_eq!(
        v.index.get(&id).expect("get").map(|c| c.hint.seg),
        Some(gone),
        "the index still names the old copy"
    );

    // The state a crash after the unlink leaves: the flag is set, the file
    // is gone, the index entries are still there; the list was lost.
    v.index.mark_condemned(gone).expect("flag");
    SegmentStore::delete(&v.store, gone).expect("unlink");
    assert!(!v.segments().contains(&gone));
    assert!(
        v.index.get_for_dedup(&id).expect("dedup").is_none(),
        "a flagged segment is never a dedup target"
    );

    // A restarted collector: no list in memory, none on disk.
    v.restart_collector();
    v.step().expect("a step reconciles the flags with the list");

    assert!(
        !v.index.condemned().expect("flags").contains(&gone),
        "the flag of a gone segment is cleared"
    );
    let hit = v.index.get_for_dedup(&id).expect("dedup");
    assert!(
        hit.is_none(),
        "dedup must not return a copy in the deleted segment: {hit:?}"
    );
    assert!(
        v.index
            .get(&id)
            .expect("get")
            .is_none_or(|c| c.hint.seg != gone),
        "no index entry points into the deleted segment"
    );
    v.passes(2).expect("later passes");
}

/// Segment `S` holds chunks 0 and 1 (and three discarded ones). Compaction
/// moves both to output `T1`; the wiring lags, so `T1` stays unnamed. A
/// later FULL stores another copy of chunk 1's content in `T2 > T1` and the
/// index names it, so the live copies `S`'s stale hints resolve to are in
/// `T1` (unnamed output) and `T2` (named). `S` must not be deleted while `T1`
/// is unnamed: a crash would sweep `T1` and leave chunk 0 without a copy.
/// Checking only the highest dependency (`T2`) misses it.
#[test]
fn a_dead_segment_waits_for_every_unnamed_output_its_ids_moved_to() {
    let mut v = Vol::new(8);
    for ci in 0..5 {
        v.write(ci, 200 + ci);
    }
    v.commit().expect("commit");
    let s = v.hint(0).seg;
    assert!((0..5).all(|ci| v.hint(ci).seg == s), "one segment");
    for ci in 2..5 {
        v.discard(ci);
    }
    // Enough FULLs that the one with all five chunks is no root under any
    // retention rule (GC keeps every FULL from the lowest chain_floor a
    // retained FULL records).
    for _ in 0..3 {
        v.commit().expect("commit");
    }

    // Compaction of S (live fraction 0.4) into T1; the moves are held back.
    v.passes(1).expect("pass");
    assert!(!v.moved.is_empty(), "S was compacted");
    let t1: SegId = v.moved[0].new.seg;
    assert!(v.moved.iter().all(|m| m.old.seg == s && m.new.seg == t1));
    assert!(v.gc.unnamed_outputs().contains(&t1));

    // A second copy of chunk 1's content in T2, named by a FULL; the index
    // now names T2 for it and T1 for chunk 0's.
    v.write(10, 201);
    v.commit_lagging().expect("commit");
    let t2 = v.hint(10).seg;
    assert!(t2 > t1, "T2 {t2:?} is above T1 {t1:?}");
    assert_eq!(
        v.index.get(&v.id_of(201)).expect("get").map(|c| c.hint.seg),
        Some(t2)
    );
    assert_eq!(
        v.index.get(&v.id_of(200)).expect("get").map(|c| c.hint.seg),
        Some(t1)
    );
    // The FULLs that named S leave the retained window; the moves still wait.
    for _ in 0..3 {
        v.commit_lagging().expect("commit");
    }
    assert_eq!(v.hint(0).seg, s, "the map still hints into S");

    v.passes(4).expect("passes");
    assert!(
        v.gc.unnamed_outputs().contains(&t1),
        "T1 is still an unnamed output"
    );
    assert!(
        v.segments().contains(&s),
        "S is deleted only once T1 is named (condemned: {:?})",
        v.gc.condemned()
    );

    // The wiring catches up: T1 is named, the hints move, S goes.
    v.commit().expect("commit");
    assert_eq!(v.hint(0).seg, t1);
    v.commit().expect("commit");
    v.commit().expect("commit");
    v.passes(4).expect("passes");
    assert!(!v.gc.unnamed_outputs().contains(&t1), "T1 is named");
    assert!(!v.segments().contains(&s), "S is deleted once T1 is named");
}
