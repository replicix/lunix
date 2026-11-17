//! `MemIndex` beyond the contract suite: runs, the manifest, faults, real
//! footers from `MemStore`, and a model of the persist rule.

use std::collections::HashMap;
use std::sync::Arc;

use lunix_format::{ChunkId, Hint, IdLen, IdPrefix, ObjId, SegId};
use lunix_iface::index::{ChunkIndex, Confirm};
use lunix_iface::store::{IdAllocator, SegmentStore};
use lunix_iface::ErrorKind;
use lunix_testkit::{Fault, MemIds, MemIndex, MemStore};
use proptest::prelude::*;

mod common;

fn seg(n: u64) -> SegId {
    ObjId::new(0, n).expect("id")
}

fn cid(n: u8) -> ChunkId {
    ChunkId([n; 32])
}

fn hint(s: u64, off: u32) -> Hint {
    Hint {
        seg: seg(s),
        off,
        clen: 100,
    }
}

fn fresh() -> (Arc<MemIds>, MemIndex) {
    let ids = Arc::new(MemIds::new());
    (ids.clone(), MemIndex::new(ids))
}

fn reopen(ids: &Arc<MemIds>, ix: &MemIndex) -> MemIndex {
    MemIndex::from_image(ids.clone() as Arc<dyn IdAllocator>, &ix.image())
}

#[test]
fn persist_writes_only_dirty_entries_up_to_the_segment() {
    let (ids, ix) = fresh();
    ix.insert(cid(1), hint(1, 0)).unwrap();
    ix.insert(cid(2), hint(2, 0)).unwrap();
    ix.insert(cid(3), hint(3, 0)).unwrap();
    let runs = ix.persist(seg(2)).unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].entries, 2);
    assert_eq!(ix.dirty_entries(), 1, "segment 3 waits for its commit");
    assert!(ix.persist(seg(2)).unwrap().is_empty(), "nothing new");
    let ix2 = reopen(&ids, &ix);
    assert!(ix2.get(&cid(1)).unwrap().is_some());
    assert!(ix2.get(&cid(3)).unwrap().is_none(), "memtable is lost");
}

#[test]
fn run_ids_come_from_the_shared_allocator() {
    let store = MemStore::new();
    let ix = MemIndex::new(store.mem_ids());
    let mut w = store.open_segment().unwrap();
    let id = cid(9);
    w.append_extent(&id, None, &common::data_extent(&id, 200))
        .unwrap();
    let sealed = w.seal(None).unwrap();
    ix.insert(
        id,
        Hint {
            seg: sealed.seg,
            off: 0,
            clen: 200,
        },
    )
    .unwrap();
    let run = ix.persist(sealed.seg).unwrap().remove(0);
    assert!(
        run.id.raw() > sealed.seg.raw(),
        "one id space for every object"
    );
    let next = store.open_segment().unwrap().id();
    assert!(next.raw() > run.id.raw());
}

#[test]
fn a_run_the_manifest_does_not_name_is_listed_but_not_used() {
    let (ids, ix) = fresh();
    ix.insert(cid(1), hint(1, 0)).unwrap();
    ix.faults()
        .arm("index.persist.before_manifest", 1, Fault::Crash);
    assert!(ix.persist(seg(1)).is_err());
    assert!(ix.faults().is_dead());
    assert_eq!(ix.get(&cid(1)).unwrap_err().kind(), ErrorKind::Io, "dead");
    let ix2 = reopen(&ids, &ix);
    assert_eq!(ix2.all_runs().unwrap().len(), 1, "the file exists");
    assert!(ix2.runs().unwrap().is_empty(), "but nothing names it");
    assert!(ix2.get(&cid(1)).unwrap().is_none());
    // The orphan run is deletable, and the allocator moved past it.
    let orphan = ix2.all_runs().unwrap().remove(0);
    assert!(orphan.size_bytes > 0);
    assert_eq!(ix2.delete_run(orphan).unwrap(), orphan.size_bytes);
    assert_eq!(ix2.delete_run(orphan).unwrap(), 0, "idempotent");
    assert!(ids.high_water() >= orphan.id.raw());
}

#[test]
fn a_crash_after_the_manifest_keeps_the_run() {
    let (ids, ix) = fresh();
    ix.insert(cid(1), hint(1, 0)).unwrap();
    ix.faults().arm("index.persist.after", 1, Fault::Crash);
    assert!(ix.persist(seg(1)).is_err());
    let ix2 = reopen(&ids, &ix);
    assert_eq!(ix2.runs().unwrap().len(), 1);
    assert_eq!(
        ix2.get_for_dedup(&cid(1)).unwrap().unwrap().confirm,
        Confirm::Hinted
    );
}

#[test]
fn a_failed_persist_leaves_the_entries_dirty() {
    let (_, ix) = fresh();
    ix.insert(cid(1), hint(1, 0)).unwrap();
    ix.faults()
        .arm("index.persist.before_run", 1, Fault::NoSpace);
    assert_eq!(ix.persist(seg(1)).unwrap_err().kind(), ErrorKind::NoSpace);
    assert_eq!(ix.dirty_entries(), 1);
    assert_eq!(ix.persist(seg(1)).unwrap().len(), 1, "alive, retry works");
}

#[test]
fn condemned_and_dropped_state_is_durable_at_once() {
    let (ids, ix) = fresh();
    ix.insert(cid(1), hint(1, 0)).unwrap();
    ix.insert(cid(2), hint(2, 0)).unwrap();
    ix.persist(seg(2)).unwrap();
    ix.mark_condemned(seg(1)).unwrap();
    ix.drop_segment(seg(2)).unwrap();
    let ix2 = reopen(&ids, &ix);
    assert_eq!(ix2.condemned().unwrap(), vec![seg(1)]);
    assert!(ix2.get(&cid(1)).unwrap().unwrap().condemned);
    assert!(ix2.get(&cid(2)).unwrap().is_none(), "dropped stays dropped");
}

#[test]
fn the_dedup_hit_follows_the_condemned_flag_not_the_entry() {
    let (_, ix) = fresh();
    ix.insert(cid(1), hint(1, 0)).unwrap();
    ix.mark_condemned(seg(1)).unwrap();
    assert!(ix.get_for_dedup(&cid(1)).unwrap().is_none());
    // A relocation to a live segment makes the chunk dedup-able again.
    assert!(ix.relocate(&cid(1), &hint(1, 0), &hint(9, 0)).unwrap());
    assert!(ix.get_for_dedup(&cid(1)).unwrap().is_some());
    assert!(!ix.get(&cid(1)).unwrap().unwrap().condemned);
}

#[test]
fn id_len_zero_has_no_prefix_lookup() {
    let (_, ix) = fresh();
    let p = IdPrefix::of(&cid(1), IdLen::Zero);
    assert_eq!(
        ix.get_by_prefix(&p).unwrap_err().kind(),
        ErrorKind::Unsupported
    );
}

#[test]
fn catch_up_and_rebuild_read_real_segment_footers() {
    let store = MemStore::new();
    let ix_main = MemIndex::new(store.mem_ids());
    let mut sealed = Vec::new();
    let mut hints = Vec::new();
    for s in 0..3u8 {
        let mut w = store.open_segment().unwrap();
        for n in 0..4u8 {
            let id = cid(s * 10 + n + 1);
            let h = w
                .append_extent(&id, None, &common::data_extent(&id, 100 + usize::from(n)))
                .unwrap();
            hints.push((id, h));
        }
        sealed.push(w.seal(None).unwrap().seg);
    }
    let report = ix_main.catch_up(&sealed[..2], &store).unwrap();
    assert_eq!((report.inserted, report.dropped), (8, 0));
    for (id, h) in &hints[..8] {
        assert_eq!(ix_main.get(id).unwrap().unwrap().hint, *h);
    }
    assert!(ix_main.get(&hints[8].0).unwrap().is_none());
    // A second pass finds nothing new; deleting a segment drops its entries.
    assert_eq!(ix_main.catch_up(&sealed[..2], &store).unwrap().inserted, 0);
    store.delete(sealed[0]).unwrap();
    let report = ix_main.catch_up(&sealed[1..2], &store).unwrap();
    assert_eq!(report.dropped, 4);
    ix_main.rebuild(&sealed[1..], &store).unwrap();
    let mut all = Vec::new();
    ix_main
        .iter_entries(&mut |e| all.push((e.id, e.hint)))
        .unwrap();
    all.sort_by_key(|e| e.0);
    let mut want: Vec<_> = hints[4..].to_vec();
    want.sort_by_key(|e| e.0);
    assert_eq!(all, want);
    assert_eq!(
        ix_main.runs().unwrap().len(),
        1,
        "rebuild replaces every run by one new run, persisted at once"
    );
}

#[test]
fn catch_up_does_not_undo_a_relocation() {
    let store = MemStore::new();
    let ix = MemIndex::new(store.mem_ids());
    let id = cid(5);
    let mut sealed = Vec::new();
    for _ in 0..2 {
        let mut w = store.open_segment().unwrap();
        let h = w
            .append_extent(&id, None, &common::data_extent(&id, 100))
            .unwrap();
        sealed.push((w.seal(None).unwrap().seg, h));
    }
    let (old, new) = (sealed[0].1, sealed[1].1);
    ix.insert(id, old).unwrap();
    assert!(ix.relocate(&id, &old, &new).unwrap());
    let segs = [sealed[0].0, sealed[1].0];
    let report = ix.catch_up(&segs, &store).unwrap();
    assert_eq!(report.inserted, 0, "the id is already indexed");
    assert_eq!(ix.get(&id).unwrap().unwrap().hint, new, "newest wins");
}

#[test]
fn catch_up_restores_an_entry_whose_relocation_target_vanished() {
    let store = MemStore::new();
    let ix = MemIndex::new(store.mem_ids());
    let id = cid(5);
    let mut w = store.open_segment().unwrap();
    let h = w
        .append_extent(&id, None, &common::data_extent(&id, 100))
        .unwrap();
    let s1 = w.seal(None).unwrap().seg;
    ix.insert(id, h).unwrap();
    let moved = Hint {
        seg: seg(99),
        off: 0,
        clen: 100,
    };
    assert!(ix.relocate(&id, &h, &moved).unwrap());
    // The relocation target was never sealed (crash): its entry is dropped and
    // the surviving copy in segment 1 is found again from the footer.
    let report = ix.catch_up(&[s1], &store).unwrap();
    assert_eq!((report.dropped, report.inserted), (1, 1));
    assert_eq!(ix.get(&id).unwrap().unwrap().hint, h);
}

#[derive(Clone, Debug)]
enum Op {
    Insert(u8, u8),
    Relocate(u8, u8),
    Persist(u8),
    Reopen,
}

fn ops() -> impl Strategy<Value = Vec<Op>> {
    proptest::collection::vec(
        prop_oneof![
            (0u8..8, 1u8..6).prop_map(|(i, s)| Op::Insert(i, s)),
            (0u8..8, 1u8..9).prop_map(|(i, s)| Op::Relocate(i, s)),
            (1u8..9).prop_map(Op::Persist),
            Just(Op::Reopen),
        ],
        1..60,
    )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(300))]

    /// The live table equals a model of "memtable over runs", through persists
    /// and crashes.
    #[test]
    fn matches_a_model_of_memtable_over_runs(ops in ops()) {
        let (ids, mut ix) = fresh();
        let mut mem: HashMap<u8, (Hint, bool)> = HashMap::new();
        let mut durable: HashMap<u8, Hint> = HashMap::new();
        for op in ops {
            match op {
                Op::Insert(i, s) => {
                    let h = hint(u64::from(s), u32::from(i));
                    ix.insert(cid(i), h).unwrap();
                    mem.insert(i, (h, true));
                }
                Op::Relocate(i, s) => {
                    let new = hint(u64::from(s), 1000 + u32::from(i));
                    let old = mem.get(&i).map(|e| e.0).unwrap_or_else(|| hint(77, 0));
                    let did = ix.relocate(&cid(i), &old, &new).unwrap();
                    prop_assert_eq!(did, mem.contains_key(&i));
                    if did {
                        mem.insert(i, (new, true));
                    }
                }
                Op::Persist(upto) => {
                    ix.persist(seg(u64::from(upto))).unwrap();
                    for (i, (h, dirty)) in mem.iter_mut() {
                        if *dirty && h.seg <= seg(u64::from(upto)) {
                            durable.insert(*i, *h);
                            *dirty = false;
                        }
                    }
                }
                Op::Reopen => {
                    ix = reopen(&ids, &ix);
                    mem = durable.iter().map(|(i, h)| (*i, (*h, false))).collect();
                }
            }
            for i in 0u8..8 {
                let got = ix.get(&cid(i)).unwrap().map(|e| e.hint);
                prop_assert_eq!(got, mem.get(&i).map(|e| e.0), "entry {}", i);
            }
        }
    }
}

#[test]
fn a_planted_hinted_entry_is_hinted_and_not_in_any_run() {
    let (ids, ix) = fresh();
    ix.insert_hinted(cid(1), hint(1, 0));
    let hit = ix.get_for_dedup(&cid(1)).unwrap().unwrap();
    assert_eq!(hit.confirm, Confirm::Hinted);
    assert_eq!(ix.dirty_entries(), 0);
    assert!(ix.persist(seg(9)).unwrap().is_empty());
    assert!(reopen(&ids, &ix).get(&cid(1)).unwrap().is_none());
}
