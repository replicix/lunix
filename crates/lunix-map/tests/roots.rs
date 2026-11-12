//! Root identity: the documented empty constants, order and location
//! independence at scale, and the cost of a sparse map.

mod common;

use std::collections::{BTreeMap, HashSet};

use common::{Blake3Codec, Fix, SplitMix, ID_LEN};
use lunix_format::node::{empty_hashes, leaf_index, DataEntry, Entry, Leaf, LeafParams, MAX_LEVEL};
use lunix_format::{CryptoMode, ExtentKind};
use lunix_iface::crypto::ExtentCodec;
use lunix_iface::map::{BlockMap, ChunkNo};
use lunix_iface::store::PackStore;
use lunix_iface::Error;
use lunix_map::MapConfig;
use lunix_testkit::strategies::data_entry;

const KIB: u64 = 1024;
const TIB: u64 = 1 << 40;

fn hex(s: &str) -> [u8; 32] {
    let mut out = [0u8; 32];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).expect("hex");
    }
    out
}

/// on-disk-formats.md section 23.7, unkeyed (plain and integrity mode).
const E512: [&str; 5] = [
    "8cb313b371458a0bbc13829da36f70d69deb2652ae2c1b0f6318004fd1419d5e",
    "10c152fbfaaab0630db8b786eebf559cbd7b38cb964a20465d68e84412d61317",
    "e76498965d1098049475b7c10a1fbce7f7e15aab71d961f209a77873b7b3e3e8",
    "23cd677335c394fd2a4ebd98a7cf7468ef67034fbda6ea526908df597c187110",
    "a0a104e5eceb69a6e727686306eb645079756ac921fb419d5a9ea12e3bef7370",
];
const E256: [&str; 5] = [
    "b66d86b2736d18b9419f57f2dd03958a9cbe6a6057c0803b5fbd43cd1e04d91a",
    "bb20922a988899f0a8f2929846bc35dee6dd0210ccf6b6157237b80e17abc8e3",
    "be662761db8a4a4ee24f872b9d38e2d46158c70a75c55874882da98377a05a9b",
    "51d932c23141d31f40276976932bb281a64d2669f6743084f48628922c53b772",
    "47f726654ae37276df5bc4c45df72e3d44022357b0d81325c70c0bd1bde11d0c",
];

fn fix(fanout: u16, chunks: u64) -> Fix {
    Fix::with(Blake3Codec::arc(), MapConfig::new(fanout, ID_LEN), chunks)
}

#[test]
fn empty_hashes_are_the_documented_constants() {
    for (fanout, want) in [(512u16, E512), (256, E256)] {
        let f = fix(fanout, 1000);
        for (level, w) in want.iter().enumerate() {
            assert_eq!(f.map.empty_hash(level as u8), Some(hex(w)), "E{level}");
        }
    }
}

#[test]
fn the_empty_volumes_root_is_the_documented_constant() {
    // 1 GiB at 16 KiB: 65 536 chunks, 2 levels, root E1.
    let f = fix(512, (1 << 30) / (16 * KIB));
    assert_eq!(f.map.root().hash, hex(E512[1]));
    assert_eq!(f.map.root().pack, None);
    // 8 TiB at 64 KiB: 3 levels, root E2.
    let f = fix(512, 8 * TIB / (64 * KIB));
    assert_eq!(f.map.levels(), 3);
    assert_eq!(f.map.root().hash, hex(E512[2]));
    // 10 TiB at 64 KiB: 4 levels.
    let f = fix(512, 10 * TIB / (64 * KIB));
    assert_eq!(f.map.levels(), 4);
    assert_eq!(f.map.root().hash, hex(E512[3]));
    // Fanout 256 at 8 TiB / 64 KiB: 134 217 728 chunks, 4 levels.
    let f = fix(256, 8 * TIB / (64 * KIB));
    assert_eq!(f.map.levels(), 4);
    assert_eq!(f.map.root().hash, hex(E256[3]));
}

#[test]
fn emptying_a_map_returns_to_the_constant_and_stores_nothing() {
    let f = fix(512, 8 * TIB / (64 * KIB));
    let res = f.seal();
    assert_eq!((res.bytes, res.dirty_leaves), (0, 0));
    assert!(res.new_packs.is_empty());
    common::set_all(&f.map, [5, 1_000_000, 100_000_000], 0);
    assert_ne!(f.seal().root.hash, hex(E512[2]));
    let mut txn = f.map.begin_update().expect("begin");
    for ci in [5, 1_000_000, 100_000_000] {
        txn.set_hole(ChunkNo(ci)).expect("hole");
    }
    txn.commit().expect("commit");
    let res = f.seal();
    assert_eq!(res.root.hash, hex(E512[2]));
    assert_eq!(res.root.pack, None, "an empty root has no location");
    assert_eq!(res.bytes, 0, "nothing is written for empty nodes");
}

#[test]
fn a_sparse_10_tib_map_costs_under_a_megabyte_of_nodes() {
    let chunks = 10 * TIB / (64 * KIB);
    let f = fix(512, chunks);
    assert_eq!(f.map.levels(), 4);
    let mut rng = SplitMix(7);
    common::set_all(&f.map, (0..8).map(|_| rng.below(chunks)), 0);
    let res = f.seal();
    assert!(res.bytes > 0);
    assert!(
        res.bytes <= 1 << 20,
        "8 scattered chunks cost {} bytes",
        res.bytes
    );
    assert_eq!(f.packs.sealed_bytes(), res.bytes);
}

// ---- the independent root --------------------------------------------------

/// The root hash computed from the content alone, with no map code: the
/// "flat model" of the plan's root-equality gate.
fn model_root(
    content: &BTreeMap<u64, DataEntry>,
    levels: u8,
    fanout: u16,
    codec: &dyn ExtentCodec,
) -> [u8; 32] {
    let hash = |b: &[u8]| codec.chunk_id(ExtentKind::Node, b).0;
    let empty = empty_hashes(fanout, MAX_LEVEL, hash).expect("empty");
    let params = LeafParams {
        fanout,
        id_len: ID_LEN,
        ovh: CryptoMode::Integrity.extent_ovh(),
    };
    let mut by_leaf: BTreeMap<u64, Vec<(usize, DataEntry)>> = BTreeMap::new();
    for (ci, e) in content {
        by_leaf
            .entry(leaf_index(*ci, fanout))
            .or_default()
            .push((lunix_format::node::leaf_slot(*ci, fanout), *e));
    }
    fn node(
        level: u8,
        lo: u64,
        fanout: u16,
        by_leaf: &BTreeMap<u64, Vec<(usize, DataEntry)>>,
        empty: &[[u8; 32]],
        params: &LeafParams,
        hash: &dyn Fn(&[u8]) -> [u8; 32],
    ) -> [u8; 32] {
        let width = u64::from(fanout).saturating_pow(u32::from(level));
        let hi = lo.saturating_add(width);
        if by_leaf.range(lo..hi).next().is_none() {
            return empty[usize::from(level)];
        }
        if level == 0 {
            let mut leaf = Leaf::empty(fanout);
            for (slot, e) in &by_leaf[&lo] {
                leaf.entries[*slot] = Entry::Data(*e);
            }
            let mut b = Vec::new();
            leaf.encode(params, &mut b).expect("encode");
            return hash(&b);
        }
        let sp = width / u64::from(fanout);
        let kids: Vec<[u8; 32]> = (0..u64::from(fanout))
            .map(|i| node(level - 1, lo + i * sp, fanout, by_leaf, empty, params, hash))
            .collect();
        hash(&lunix_format::node::interior_hash_input(level, fanout, &kids).expect("input"))
    }
    node(levels - 1, 0, fanout, &by_leaf, &empty, &params, &hash)
}

fn content(n: usize, chunks: u64, seed: u64) -> BTreeMap<u64, DataEntry> {
    let mut rng = SplitMix(seed);
    let mut seen = HashSet::new();
    let mut out = BTreeMap::new();
    while out.len() < n {
        let ci = rng.below(chunks);
        if seen.insert(ci) {
            out.insert(ci, data_entry(ci, 0, ID_LEN));
        }
    }
    out
}

/// Applies `order` in transactions of `batch`, sealing whenever the map says
/// it is full, then once at the end.
fn build(f: &Fix, order: &[(u64, DataEntry)], batch: usize) -> lunix_format::commit::MapRoot {
    let mut i = 0;
    while i < order.len() {
        let end = (i + batch).min(order.len());
        let mut txn = match f.map.begin_update() {
            Ok(t) => t,
            Err(Error::MapFull) => {
                f.seal();
                continue;
            }
            Err(e) => panic!("begin: {e}"),
        };
        let mut done = i;
        let mut full = false;
        for (ci, e) in &order[i..end] {
            match txn.set(ChunkNo(*ci), *e) {
                Ok(()) => done += 1,
                Err(Error::MapFull) => {
                    full = true;
                    break;
                }
                Err(e) => panic!("set: {e}"),
            }
        }
        if full {
            // Only the ops that fit are applied: re-run them in a new
            // transaction after the seal (this one is dropped).
            drop(txn);
            f.seal();
            continue;
        }
        txn.commit().expect("commit");
        i = done;
    }
    f.seal().root
}

#[test]
fn one_million_chunks_give_one_root_in_any_order_and_any_layout() {
    const CHUNKS: u64 = TIB / (64 * KIB);
    let codec = Blake3Codec::arc();
    let model = content(1_000_000, CHUNKS, 1);
    let want = model_root(&model, 3, 512, &*codec);

    let sorted: Vec<(u64, DataEntry)> = model.iter().map(|(c, e)| (*c, *e)).collect();
    let mut shuffled = sorted.clone();
    let mut rng = SplitMix(99);
    for i in (1..shuffled.len()).rev() {
        shuffled.swap(i, rng.below(i as u64 + 1) as usize);
    }

    // Sequential, with a small cap so that the engine's seal-on-MapFull loop
    // runs many times.
    let cfg = MapConfig::new(512, ID_LEN).with_dirty_leaf_cap(3000);
    let a = Fix::with(codec.clone(), cfg, CHUNKS);
    let ra = build(&a, &sorted, 20_000);
    assert_eq!(ra.hash, want, "sequential build equals the flat model");
    drop(a);

    // Random order, one big seal, packs pre-filled so that every location
    // differs from the first build's.
    let cfg = MapConfig::new(512, ID_LEN).with_dirty_leaf_cap(40_000);
    let b = Fix::with(codec.clone(), cfg, CHUNKS);
    for _ in 0..3 {
        let mut p = b.packs.open_pack().expect("pack");
        p.append_node(&[0; 32], &[1; 100]).expect("append");
        p.seal().expect("seal");
    }
    let rb = build(&b, &shuffled, 50_000);
    assert_eq!(rb.hash, want, "random order equals the flat model");
    assert_ne!((ra.pack, ra.off), (rb.pack, rb.off), "locations differ");

    // The sealed random-order tree reads back the model.
    let mut n = 0u64;
    let mut ok = true;
    b.map
        .walk_leaves(&rb, &mut |ci, e| {
            n += 1;
            ok &= model.get(&ci.0) == Some(e);
        })
        .expect("walk");
    assert!(ok);
    assert_eq!(n, 1_000_000);
}

#[test]
fn growing_deeper_gives_the_root_of_a_map_that_size_from_the_start() {
    // 512^2 = 262 144 chunks fit in 2 levels; one more needs 3.
    let small = 262_144u64;
    let big = small * 3;
    let codec = Blake3Codec::arc();
    let cfg = MapConfig::new(512, ID_LEN);
    let model = content(2000, small, 5);

    let g = Fix::with(codec.clone(), cfg, small);
    common::set_all(&g.map, model.keys().copied(), 0);
    let before = g.seal().root;
    assert_eq!(before.hash, model_root(&model, 2, 512, &*codec));
    g.map.grow(big).expect("grow");
    assert_eq!(g.map.levels(), 3);
    // A pure grow changes the root once sealed, and the old root still reads.
    let res = g.seal();
    assert_ne!(res.root.hash, before.hash);
    assert_eq!(res.root.hash, model_root(&model, 3, 512, &*codec));
    assert_eq!(common::leaves(&g.map, &before), model);
    assert_eq!(common::leaves(&g.map, &res.root), model);

    // Writing in the new range before the seal.
    let h = Fix::with(codec.clone(), cfg, small);
    common::set_all(&h.map, model.keys().copied(), 0);
    h.seal();
    h.map.grow(big).expect("grow");
    assert_eq!(h.map.get(ChunkNo(big - 1)).expect("get"), None);
    common::set_all(&h.map, [big - 1, small + 7], 1);
    let mut all = model.clone();
    for ci in [big - 1, small + 7] {
        all.insert(ci, data_entry(ci, 1, ID_LEN));
    }
    let r = h.seal().root;
    assert_eq!(r.hash, model_root(&all, 3, 512, &*codec));
    let reopened = h.open_at(&r, big);
    assert_eq!(common::leaves(&reopened, &r), all);
    assert_eq!(
        reopened.get(ChunkNo(big - 1)).expect("get"),
        Some(data_entry(big - 1, 1, ID_LEN))
    );

    // An empty map that grows deeper is the empty root of the new size.
    let e = Fix::with(codec.clone(), cfg, small);
    e.map.grow(big).expect("grow");
    let r = e.seal().root;
    assert_eq!(r.hash, hex(E512[2]));
    assert_eq!(r.pack, None);
}
