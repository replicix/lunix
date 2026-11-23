//! The batch updates of the GC (`mark_condemned_all`, `unmark_condemned_all`,
//! `retire_segments`) are
//! the single-segment calls, only cheaper: after any sequence of operations
//! the condemned set, the dropped entries and every lookup are what the
//! per-segment calls leave, also after a reopen.

mod common;

use std::collections::BTreeMap;
use std::path::Path;

use lunix_format::{ChunkId, Hint, SegId};
use lunix_iface::index::ChunkIndex;
use lunix_index::LsmIndex;
use proptest::prelude::*;

use common::{cid, hint, open, seg, Ids};

const SEGS: u64 = 9;
const CHUNKS: u64 = 60;

#[derive(Clone, Debug)]
enum Op {
    Insert(u64, u64),
    Persist(u64),
    Condemn(Vec<u64>),
    Retire(Vec<u64>),
    Uncondemn(Vec<u64>),
    Reopen,
}

fn segs_of(v: &[u64]) -> Vec<SegId> {
    v.iter().map(|&n| seg(n)).collect()
}

fn op() -> impl Strategy<Value = Op> {
    let set = || prop::collection::vec(1..=SEGS, 0..6);
    prop_oneof![
        3 => (0..CHUNKS, 1..=SEGS).prop_map(|(c, s)| Op::Insert(c, s)),
        1 => (1..=SEGS).prop_map(Op::Persist),
        2 => set().prop_map(Op::Condemn),
        2 => set().prop_map(Op::Retire),
        2 => set().prop_map(Op::Uncondemn),
        1 => Just(Op::Reopen),
    ]
}

/// Everything a caller can observe of the index.
type Seen = (
    Vec<SegId>,
    BTreeMap<ChunkId, (Hint, bool)>,
    Vec<ChunkId>,
    Vec<Option<(Hint, bool)>>,
);

fn observe(ix: &LsmIndex) -> Seen {
    let mut entries = BTreeMap::new();
    ix.iter_entries(&mut |e| {
        entries.insert(e.id, (e.hint, e.condemned));
    })
    .expect("iter");
    let dedup = (0..CHUNKS)
        .filter(|&c| ix.get_for_dedup(&cid(c)).expect("dedup").is_some())
        .map(cid)
        .collect();
    let gets = (0..CHUNKS)
        .map(|c| ix.get(&cid(c)).expect("get").map(|g| (g.hint, g.condemned)))
        .collect::<Vec<_>>();
    (ix.condemned().expect("condemned"), entries, dedup, gets)
}

fn apply(ix: LsmIndex, dir: &Path, ids: &std::sync::Arc<Ids>, o: &Op, batch: bool) -> LsmIndex {
    match o {
        Op::Insert(c, s) => ix
            .insert(cid(*c), hint(*s, (*c as u32) * 8))
            .expect("insert"),
        Op::Persist(s) => {
            ix.persist(seg(*s)).expect("persist");
        }
        Op::Condemn(v) if batch => ix.mark_condemned_all(&segs_of(v)).expect("condemn all"),
        Op::Condemn(v) => {
            for s in segs_of(v) {
                ix.mark_condemned(s).expect("condemn");
            }
        }
        Op::Uncondemn(v) if batch => ix.unmark_condemned_all(&segs_of(v)).expect("uncondemn all"),
        Op::Uncondemn(v) => {
            for s in segs_of(v) {
                ix.unmark_condemned(s).expect("unmark");
            }
        }
        Op::Retire(v) if batch => ix.retire_segments(&segs_of(v)).expect("retire all"),
        Op::Retire(v) => {
            for s in segs_of(v) {
                ix.drop_segment(s).expect("drop");
                ix.unmark_condemned(s).expect("unmark");
            }
        }
        Op::Reopen => {
            drop(ix);
            return open(dir, ids);
        }
    }
    ix
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    #[test]
    fn batch_updates_leave_the_state_the_single_calls_leave(
        ops in prop::collection::vec(op(), 1..40),
    ) {
        let (da, db) = (tempfile::tempdir().expect("tmp"), tempfile::tempdir().expect("tmp"));
        let (ia, ib) = (Ids::new(), Ids::new());
        let mut a = open(da.path(), &ia);
        let mut b = open(db.path(), &ib);
        for o in &ops {
            a = apply(a, da.path(), &ia, o, true);
            b = apply(b, db.path(), &ib, o, false);
            prop_assert_eq!(observe(&a), observe(&b), "after {:?}", o);
        }
        // Both survive a restart the same way.
        drop((a, b));
        prop_assert_eq!(observe(&open(da.path(), &ia)), observe(&open(db.path(), &ib)));
    }
}

/// `unmark_condemned_all` clears the flags of the segments that have one, in
/// one update: duplicates, segments that are not flagged and an empty set are
/// harmless, the entries stay where they are (they are reachable for dedup
/// again), and the result survives a reopen. (Review R1.)
#[test]
fn unmarking_clears_exactly_the_flagged_segments_in_one_update() {
    let dir = tempfile::tempdir().expect("tmp");
    let ids = Ids::new();
    let mut ix = open(dir.path(), &ids);
    for c in 0..30u64 {
        ix.insert(cid(c), hint(1 + c % 3, c as u32 * 8))
            .expect("insert");
    }
    ix.persist(seg(3)).expect("persist");
    ix.mark_condemned_all(&[seg(1), seg(2), seg(3)])
        .expect("condemn");
    ix.unmark_condemned_all(&[]).expect("empty");
    // Segment 4 was never flagged; segment 1 is named twice.
    ix.unmark_condemned_all(&[seg(1), seg(4), seg(3), seg(1)])
        .expect("unmark");
    for reopen in [false, true] {
        if reopen {
            drop(ix);
            ix = open(dir.path(), &ids);
        }
        let (flags, entries, dedup, _) = observe(&ix);
        assert_eq!(flags, vec![seg(2)], "only segment 2 stays flagged");
        assert_eq!(entries.len(), 30, "no entry was touched");
        for c in 0..30u64 {
            let flagged = 1 + c % 3 == 2;
            assert_eq!(
                dedup.contains(&cid(c)),
                !flagged,
                "chunk {c}: dedup follows the flag"
            );
        }
    }
    // Nothing flagged any more: a second call changes nothing.
    ix.unmark_condemned_all(&[seg(2)]).expect("unmark last");
    ix.unmark_condemned_all(&[seg(2)]).expect("unmark again");
    assert!(ix.condemned().expect("flags").is_empty());
}

/// After `retire_segments` no entry of the segment is reachable and its flag
/// is gone, whether its entries sat in the memtable or in a persisted run;
/// both facts survive a reopen.
#[test]
fn retired_segments_lose_entries_and_flags_together() {
    let dir = tempfile::tempdir().expect("tmp");
    let ids = Ids::new();
    let mut ix = open(dir.path(), &ids);
    for c in 0..30u64 {
        ix.insert(cid(c), hint(1 + c % 3, c as u32 * 8))
            .expect("insert");
    }
    ix.persist(seg(2)).expect("persist");
    // Segment 3's entries stay in the memtable (above the persist bound).
    ix.mark_condemned_all(&[seg(1), seg(3)]).expect("condemn");
    ix.retire_segments(&[seg(1), seg(3)]).expect("retire");
    for reopen in [false, true] {
        if reopen {
            drop(ix);
            ix = open(dir.path(), &ids);
        }
        let (flags, entries, _, _) = observe(&ix);
        assert!(flags.is_empty(), "flags cleared: {flags:?}");
        assert!(!entries.is_empty(), "segment 2 keeps its entries");
        for (id, (h, _)) in &entries {
            assert_eq!(h.seg, seg(2), "{id:?} still points into a retired segment");
        }
    }
}

/// Dropping or retiring a segment is a manifest update and a memtable fix:
/// it never reads the runs that cover the segment (it used to scan them to
/// count entries nobody used). The run file is overwritten in place after the
/// open, so any read of its blocks fails its checksum.
#[test]
fn retiring_a_segment_does_not_read_the_runs() {
    let dir = tempfile::tempdir().expect("tmp");
    let ids = Ids::new();
    let ix = open(dir.path(), &ids);
    for c in 0..200u64 {
        ix.insert(cid(c), hint(1 + c % 2, c as u32 * 8))
            .expect("insert");
    }
    let made = ix.persist(seg(2)).expect("persist");
    assert_eq!(made.len(), 1);
    let path = dir.path().join("index").join(made[0].id.to_string());
    let len = std::fs::metadata(&path).expect("meta").len();
    std::fs::write(&path, vec![0xa5u8; len as usize]).expect("overwrite");
    ix.drop_segment(seg(1)).expect("drop reads no run");
    ix.mark_condemned_all(&[seg(2)]).expect("condemn");
    ix.retire_segments(&[seg(2)]).expect("retire reads no run");
    assert!(ix.condemned().expect("flags").is_empty());
}
