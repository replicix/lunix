//! `RadixMap` against `FlatMap`, the content oracle: random update sequences
//! with seals, reopens and grows (some across a depth boundary). Content is
//! compared through `get` and `walk_leaves`, never through root hashes, which
//! differ by design.

mod common;

use std::collections::BTreeMap;
use std::sync::Arc;

use common::{leaves, plain, ID_LEN};
use lunix_format::commit::MapRoot;
use lunix_format::node::DataEntry;
use lunix_iface::map::{entry_hint, BlockMap, Cached, ChunkNo, MapTxn};
use lunix_iface::store::{PackRead, PackStore};
use lunix_map::{MapConfig, RadixMap};
use lunix_testkit::strategies::{data_entry, map_ops, MapOp, TxnOp};
use lunix_testkit::{FlatMap, MemPacks};
use proptest::prelude::*;

const OPS: usize = 300;
const POOL: u64 = 4096;

fn cases() -> u32 {
    std::env::var("LUNIX_MAP_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(5000)
}

/// Spreads the strategy's small chunk numbers over eight regions of the
/// volume, 128 neighbouring chunks each, so that leaves and subtrees are both
/// shared and far apart.
fn place(ci: u64, start: u64) -> u64 {
    let region = (ci >> 7) % 8;
    let stride = (start / 8).max(1);
    (region * stride + (ci & 0x7F)) % start
}

fn remap(op: TxnOp, start: u64) -> TxnOp {
    match op {
        TxnOp::Set { ci, version } => TxnOp::Set {
            ci: place(ci, start),
            version,
        },
        TxnOp::Hole { ci } => TxnOp::Hole {
            ci: place(ci, start),
        },
        TxnOp::Rehint { ci, from, to } => TxnOp::Rehint {
            ci: place(ci, start),
            from,
            to,
        },
    }
}

fn txn_ci(op: &TxnOp) -> u64 {
    match *op {
        TxnOp::Set { ci, .. } | TxnOp::Hole { ci } | TxnOp::Rehint { ci, .. } => ci,
    }
}

fn run_txn<'a>(map: &'a dyn BlockMap, ops: &[TxnOp]) -> Box<dyn MapTxn + 'a> {
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

fn seal(map: &RadixMap, packs: &MemPacks) -> Result<MapRoot, TestCaseError> {
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

fn seal_flat(map: &FlatMap, packs: &MemPacks) -> MapRoot {
    let mut pack = packs.open_pack().expect("pack");
    let res = map.seal(&mut *pack).expect("seal");
    if !pack.is_empty() {
        pack.seal().expect("seal pack");
    }
    res.root
}

/// What a fresh map holding exactly `content` seals to.
fn rebuilt_root(content: &BTreeMap<u64, DataEntry>, count: u64, fanout: u16) -> MapRoot {
    let packs = MemPacks::new();
    let map = RadixMap::new(
        plain(),
        Arc::new(packs.clone()),
        count,
        MapConfig::new(fanout, ID_LEN),
    )
    .expect("new");
    let mut txn = map.begin_update().expect("begin");
    for (ci, e) in content {
        txn.set(ChunkNo(*ci), *e).expect("set");
    }
    txn.commit().expect("commit");
    seal(&map, &packs).expect("seal")
}

fn run(
    fanout: u16,
    volume_bytes: u64,
    chunk_size: u64,
    sel: u8,
    ops: Vec<MapOp>,
) -> Result<(), TestCaseError> {
    let f = u64::from(fanout);
    let start = match sel {
        0 => volume_bytes / chunk_size,
        1 => f * f - 300,
        _ => 1000,
    };
    let cfg = MapConfig::new(fanout, ID_LEN);
    let codec = plain();
    let (fp, rp) = (MemPacks::new(), MemPacks::new());
    let flat = FlatMap::new(codec.clone(), Arc::new(fp.clone()), start, ID_LEN);
    let mut map = RadixMap::new(codec.clone(), Arc::new(rp.clone()), start, cfg).expect("new");
    let mut flat = flat;
    let mut count = start;
    let mut history: Vec<(MapRoot, BTreeMap<u64, DataEntry>)> = Vec::new();

    for op in ops {
        match op {
            MapOp::Commit(txn_ops) => {
                let txn_ops: Vec<TxnOp> = txn_ops.into_iter().map(|o| remap(o, start)).collect();
                let want = run_txn(&flat, &txn_ops).commit().expect("flat commit");
                let got = run_txn(&map, &txn_ops).commit().expect("commit");
                prop_assert_eq!(got, want);
                for op in &txn_ops {
                    let ci = ChunkNo(txn_ci(op));
                    prop_assert_eq!(map.get(ci).expect("get"), flat.get(ci).expect("get"));
                }
            }
            MapOp::Abort(txn_ops) => {
                let txn_ops: Vec<TxnOp> = txn_ops.into_iter().map(|o| remap(o, start)).collect();
                drop(run_txn(&map, &txn_ops));
                prop_assert!(map.begin_update().is_ok(), "the map was released");
                for op in &txn_ops {
                    let ci = ChunkNo(txn_ci(op));
                    prop_assert_eq!(map.get(ci).expect("get"), flat.get(ci).expect("get"));
                }
            }
            MapOp::Get { ci } => {
                let ci = ChunkNo(place(ci, start));
                let want = flat.get(ci).expect("flat get");
                prop_assert_eq!(map.get(ci).expect("get"), want);
                match map.try_get_cached(ci) {
                    Cached::Hit(e) => prop_assert_eq!(Some(e), want),
                    Cached::Hole => prop_assert!(want.is_none()),
                    Cached::Miss => {}
                }
            }
            MapOp::Seal => {
                let root = seal(&map, &rp)?;
                let froot = seal_flat(&flat, &fp);
                let content = flat.snapshot();
                prop_assert_eq!(&leaves(&map, &root), &content);
                prop_assert_eq!(&leaves(&flat, &froot), &content);
                prop_assert_eq!(root.hash, rebuilt_root(&content, count, fanout).hash);
                let mut pack = rp.open_pack().expect("pack");
                let again = map.seal(&mut *pack).expect("seal");
                prop_assert_eq!(again.root, root);
                prop_assert!(again.new_packs.is_empty() && pack.is_empty());
                history.push((root, content));
            }
            MapOp::Reopen => {
                let root = seal(&map, &rp)?;
                let froot = seal_flat(&flat, &fp);
                map = RadixMap::open(codec.clone(), Arc::new(rp.clone()), &root, count, cfg)
                    .expect("open");
                let freads: Arc<dyn PackRead> = Arc::new(fp.clone());
                flat = FlatMap::open(codec.clone(), freads, &froot, count, ID_LEN).expect("open");
                prop_assert_eq!(map.root(), root);
                let content = flat.snapshot();
                prop_assert_eq!(&leaves(&map, &root), &content);
                history.push((root, content));
            }
            MapOp::Grow { by } => {
                count += by * 977;
                map.grow(count).expect("grow");
                flat.grow(count).expect("grow");
                prop_assert_eq!(map.chunk_count(), count);
                prop_assert!(map.grow(count - 1).is_err());
            }
        }
    }

    // Every root ever sealed still reads back what it held, and the final
    // state matches the oracle through get.
    for (root, content) in &history {
        prop_assert_eq!(&leaves(&map, root), content);
    }
    for (ci, e) in flat.snapshot() {
        prop_assert_eq!(map.get(ChunkNo(ci)).expect("get"), Some(e));
    }
    Ok(())
}

const KIB: u64 = 1024;
const TIB: u64 = 1 << 40;

macro_rules! model_test {
    ($name:ident, $fanout:expr, $chunk:expr) => {
        proptest! {
            #![proptest_config(ProptestConfig::with_cases(cases()))]
            #[test]
            fn $name(sel in 0u8..3, ops in map_ops(POOL, OPS)) {
                run($fanout, TIB, $chunk, sel, ops)?;
            }
        }
    };
}

model_test!(fanout_512_chunk_16k, 512, 16 * KIB);
model_test!(fanout_512_chunk_64k, 512, 64 * KIB);
model_test!(fanout_256_chunk_16k, 256, 16 * KIB);
model_test!(fanout_256_chunk_64k, 256, 64 * KIB);
