//! Self-consistency of `FlatMap`: random update sequences against a
//! `BTreeMap` model, with seals, reopens and grows in between.

use std::collections::BTreeMap;
use std::sync::Arc;

use lunix_format::commit::MapRoot;
use lunix_format::node::DataEntry;
use lunix_format::IdLen;
use lunix_iface::crypto::ExtentCodec;
use lunix_iface::map::{entry_hint, BlockMap, Cached, ChunkNo};
use lunix_iface::store::{PackRead, PackStore};
use lunix_testkit::strategies::{data_entry, map_ops, MapOp, TxnOp};
use lunix_testkit::{FlatMap, MemPacks, PlainCodec};
use proptest::prelude::*;

const ID_LEN: IdLen = IdLen::Sixteen;
const CHUNKS: u64 = 2000;

fn codec() -> Arc<dyn ExtentCodec> {
    Arc::new(PlainCodec::new())
}

fn new_map(packs: &MemPacks, chunks: u64) -> FlatMap {
    FlatMap::new(codec(), Arc::new(packs.clone()), chunks, ID_LEN)
}

fn seal(map: &FlatMap, packs: &MemPacks) -> Result<MapRoot, TestCaseError> {
    let mut pack = packs.open_pack().expect("pack");
    let res = map.seal(&mut *pack).expect("seal");
    prop_assert_eq!(map.root(), res.root);
    prop_assert_eq!(map.dirty_leaf_count(), 0);
    prop_assert!(res.new_packs.len() <= 1);
    prop_assert_eq!(res.new_packs.is_empty(), pack.is_empty());
    prop_assert_eq!(res.bytes, pack.len());
    if !pack.is_empty() {
        pack.seal().expect("seal pack");
    }
    Ok(res.root)
}

fn leaves(map: &FlatMap, root: &MapRoot) -> BTreeMap<u64, DataEntry> {
    let mut out = BTreeMap::new();
    map.walk_leaves(root, &mut |ci, e| {
        assert!(out.insert(ci.0, *e).is_none(), "each chunk once");
    })
    .expect("walk");
    out
}

/// What a fresh map holding exactly `content` seals to.
fn rebuilt_root(content: &BTreeMap<u64, DataEntry>, count: u64) -> MapRoot {
    let packs = MemPacks::new();
    let map = new_map(&packs, count);
    let mut txn = map.begin_update().expect("begin");
    for (ci, e) in content {
        txn.set(ChunkNo(*ci), *e).expect("set");
    }
    txn.commit().expect("commit");
    seal(&map, &packs).expect("seal")
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Outcome {
    Set,
    RehintApplied,
    RehintSkipped,
}

fn apply(model: &mut BTreeMap<u64, DataEntry>, op: TxnOp) -> Outcome {
    match op {
        TxnOp::Set { ci, version } => {
            model.insert(ci, data_entry(ci, version, ID_LEN));
            Outcome::Set
        }
        TxnOp::Hole { ci } => {
            model.remove(&ci);
            Outcome::Set
        }
        TxnOp::Rehint { ci, from, to } => {
            let old = entry_hint(&data_entry(ci, from, ID_LEN));
            let new = entry_hint(&data_entry(ci, to, ID_LEN));
            match model.get_mut(&ci).filter(|e| entry_hint(e) == old) {
                Some(e) => {
                    e.seg = new.seg;
                    e.off = new.off;
                    e.clen = new.clen;
                    Outcome::RehintApplied
                }
                None => Outcome::RehintSkipped,
            }
        }
    }
}

fn run_txn<'a>(map: &'a FlatMap, ops: &[TxnOp]) -> Box<dyn lunix_iface::map::MapTxn + 'a> {
    let mut txn = map.begin_update().expect("begin");
    for op in ops {
        match *op {
            TxnOp::Set { ci, version } => txn
                .set(ChunkNo(ci), data_entry(ci, version, ID_LEN))
                .expect("set"),
            TxnOp::Hole { ci } => txn.set_hole(ChunkNo(ci)).expect("hole"),
            TxnOp::Rehint { ci, from, to } => txn
                .rehint(
                    ChunkNo(ci),
                    entry_hint(&data_entry(ci, from, ID_LEN)),
                    entry_hint(&data_entry(ci, to, ID_LEN)),
                )
                .expect("rehint"),
        }
    }
    txn
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1000))]

    #[test]
    fn flat_map_matches_the_model(ops in map_ops(CHUNKS, 100)) {
        let packs = MemPacks::new();
        let mut map = new_map(&packs, CHUNKS);
        let mut model: BTreeMap<u64, DataEntry> = BTreeMap::new();
        let mut count = CHUNKS;
        let mut history: Vec<(MapRoot, BTreeMap<u64, DataEntry>)> = Vec::new();

        for op in ops {
            match op {
                MapOp::Commit(txn_ops) => {
                    let mut want = model.clone();
                    let mut counts = [0u64; 3];
                    for &t in &txn_ops {
                        counts[apply(&mut want, t) as usize] += 1;
                    }
                    let report = run_txn(&map, &txn_ops).commit().expect("commit");
                    prop_assert_eq!(
                        (report.sets, report.rehints_applied, report.rehints_skipped),
                        (counts[0], counts[1], counts[2])
                    );
                    model = want;
                }
                MapOp::Abort(txn_ops) => {
                    drop(run_txn(&map, &txn_ops));
                    prop_assert_eq!(&map.snapshot(), &model);
                    prop_assert!(map.begin_update().is_ok(), "the aborted transaction released the map");
                }
                MapOp::Get { ci } => {
                    prop_assert_eq!(map.get(ChunkNo(ci)).expect("get"), model.get(&ci).copied());
                    match map.try_get_cached(ChunkNo(ci)) {
                        Cached::Hit(e) => prop_assert_eq!(Some(e), model.get(&ci).copied()),
                        Cached::Hole => prop_assert!(!model.contains_key(&ci)),
                        Cached::Miss => {}
                    }
                }
                MapOp::Seal => {
                    let root = seal(&map, &packs)?;
                    prop_assert_eq!(&leaves(&map, &root), &model);
                    prop_assert_eq!(root.hash, rebuilt_root(&model, count).hash);
                    // Sealing again changes nothing.
                    let mut pack = packs.open_pack().expect("pack");
                    let again = map.seal(&mut *pack).expect("seal");
                    prop_assert_eq!(again.root, root);
                    prop_assert!(again.new_packs.is_empty() && pack.is_empty());
                    history.push((root, model.clone()));
                }
                MapOp::Reopen => {
                    let root = seal(&map, &packs)?;
                    let reads: Arc<dyn PackRead> = Arc::new(packs.clone());
                    map = FlatMap::open(codec(), reads, &root, count, ID_LEN).expect("open");
                    prop_assert_eq!(map.root(), root);
                    prop_assert_eq!(&map.snapshot(), &model);
                    history.push((root, model.clone()));
                }
                MapOp::Grow { by } => {
                    count += by;
                    map.grow(count).expect("grow");
                    prop_assert_eq!(map.chunk_count(), count);
                    prop_assert!(map.grow(count - 1).is_err());
                }
            }
            prop_assert_eq!(&map.snapshot(), &model);
        }

        // Every root ever sealed still reads back what it held.
        for (root, content) in &history {
            prop_assert_eq!(&leaves(&map, root), content);
        }
    }
}
