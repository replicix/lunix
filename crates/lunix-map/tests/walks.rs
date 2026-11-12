//! Walks of any root, rehint, and the old roots staying readable.

mod common;

use std::collections::{BTreeMap, BTreeSet, HashSet};

use common::{leaves, set_all, Fix, ID_LEN};
use lunix_format::commit::MapRoot;
use lunix_format::node::{DataEntry, NodeLoc};
use lunix_format::{ChunkId, ExtentKind, PackId};
use lunix_iface::crypto::{OpenExpect, OpenOutput};
use lunix_iface::map::{entry_hint, BlockMap, ChunkNo, NodeHash, NodeRef};
use lunix_iface::store::PackRead;
use lunix_testkit::strategies::data_entry;

fn nodes(f: &Fix, root: &MapRoot, skip: Option<&HashSet<NodeHash>>) -> Vec<NodeRef> {
    let mut out = Vec::new();
    let mut cb = |n: &NodeRef| out.push(*n);
    match skip {
        Some(s) => f.map.walk_nodes_skipping(root, s, &mut cb).expect("walk"),
        None => f.map.walk_nodes(root, &mut cb).expect("walk"),
    }
    out
}

fn hashes(ns: &[NodeRef]) -> HashSet<NodeHash> {
    ns.iter().map(|n| n.hash).collect()
}

/// The nodes a tree over `content` must have: one per distinct path.
fn expected_node_count(content: &BTreeMap<u64, DataEntry>, fanout: u64, levels: u32) -> usize {
    let mut count = 0;
    for level in 0..levels {
        let width = fanout.pow(level + 1);
        let at: BTreeSet<u64> = content.keys().map(|ci| ci / width).collect();
        count += at.len();
    }
    count
}

#[test]
fn walk_nodes_reports_every_node_with_the_pack_that_holds_it() {
    let f = Fix::new(256, 3_000_000);
    assert_eq!(f.map.levels(), 3);
    let mut content = BTreeMap::new();
    let mut roots = Vec::new();
    let mut packs_made: Vec<PackId> = Vec::new();
    for round in 0..4u32 {
        let cis: Vec<u64> = (0..300u64)
            .map(|i| (i * 9973 + u64::from(round) * 131) % 3_000_000)
            .collect();
        set_all(&f.map, cis.iter().copied(), round);
        for ci in cis {
            content.insert(ci, data_entry(ci, round, ID_LEN));
        }
        let res = f.seal();
        packs_made.extend(res.new_packs.iter().copied());
        roots.push((res.root, content.clone()));
    }
    assert_eq!(packs_made.len(), 4);

    for (root, content) in &roots {
        let ns = nodes(&f, root, None);
        assert_eq!(ns.len(), expected_node_count(content, 256, 3), "node count");
        let root_ref = ns.iter().find(|n| n.hash == root.hash).expect("root node");
        assert_eq!(
            root_ref.loc,
            NodeLoc {
                pack: root.pack.expect("pack"),
                off: root.off,
                len: root.len
            }
        );
        assert_eq!(root_ref.level, 2);
        let mut used = BTreeSet::new();
        for n in &ns {
            // The pack the walk names holds an extent whose id is the hash.
            let mut ext = Vec::new();
            f.packs.read_node(n.loc, &mut ext).expect("node extent");
            let id = ChunkId(n.hash);
            let expect = OpenExpect {
                chunk_size: 65_536,
                chunk_id: Some(&id),
                id_prefix: None,
                verify_id: false,
                output: OpenOutput::Plaintext,
            };
            let mut body = Vec::new();
            f.codec
                .open(ExtentKind::Node, &ext, &expect, &mut body)
                .expect("authentic and named by its hash");
            assert_eq!(body[0], n.level);
            assert!(packs_made.contains(&n.loc.pack));
            used.insert(n.loc.pack.raw());
        }
        // Every pack that holds a node of this root is reported, and no other.
        let newest = roots.iter().position(|(r, _)| r == root).expect("index");
        assert!(used.len() <= newest + 1);
        assert!(
            used.contains(&packs_made[newest].raw()),
            "the root's own pack"
        );
    }
}

#[test]
fn skipping_visits_exactly_the_difference_between_two_roots() {
    let f = Fix::new(512, 40_000_000);
    set_all(&f.map, (0..2000).map(|i| i * 17_011 % 40_000_000), 0);
    let a = f.seal().root;
    let a_nodes = nodes(&f, &a, None);
    let set_a = hashes(&a_nodes);

    // Change a few chunks, add some, remove one.
    set_all(&f.map, [5, 17_011 * 3, 39_999_999, 20_000_000], 1);
    let mut txn = f.map.begin_update().expect("begin");
    txn.set_hole(ChunkNo(17_011 * 5)).expect("hole");
    txn.commit().expect("commit");
    let b = f.seal().root;
    let b_nodes = nodes(&f, &b, None);
    let set_b = hashes(&b_nodes);

    let new_in_b: HashSet<NodeHash> = set_b.difference(&set_a).copied().collect();
    let gone_from_a: HashSet<NodeHash> = set_a.difference(&set_b).copied().collect();
    assert!(!new_in_b.is_empty() && !gone_from_a.is_empty());
    assert_eq!(hashes(&nodes(&f, &b, Some(&set_a))), new_in_b);
    assert_eq!(hashes(&nodes(&f, &a, Some(&set_b))), gone_from_a);
    assert!(nodes(&f, &a, Some(&set_a)).is_empty());
    assert!(nodes(&f, &b, Some(&set_b)).is_empty());
    // Few nodes are visited: a handful of paths, not the tree.
    assert!(nodes(&f, &b, Some(&set_a)).len() < 20);

    // The leaf walk skips shared subtrees and reports only changed chunks'
    // leaves, and both roots still read in full.
    let mut changed = BTreeMap::new();
    f.map
        .walk_leaves_skipping(&b, &set_a, &mut |ci, e| {
            changed.insert(ci.0, *e);
        })
        .expect("walk");
    assert_eq!(changed.get(&5), Some(&data_entry(5, 1, ID_LEN)));
    assert!(changed.len() < 20 * 512);
    let full_a = leaves(&f.map, &a);
    let full_b = leaves(&f.map, &b);
    assert_eq!(full_a.len(), 2000);
    assert_eq!(
        full_b.get(&(17_011 * 3)),
        Some(&data_entry(17_011 * 3, 1, ID_LEN))
    );
    assert!(!full_b.contains_key(&(17_011 * 5)));
    assert!(full_a.contains_key(&(17_011 * 5)));
}

#[test]
fn an_empty_root_walks_as_nothing() {
    let f = Fix::new(512, 1_000_000);
    let root = f.map.root();
    assert!(nodes(&f, &root, None).is_empty());
    assert!(leaves(&f.map, &root).is_empty());
}

#[test]
fn rehint_follows_the_committed_entry_in_overlay_and_in_the_tree() {
    let f = Fix::new(512, 100_000);
    let e = data_entry(77, 0, ID_LEN);
    let moved = lunix_format::Hint {
        seg: lunix_format::ObjId::new(0, 500).expect("id"),
        off: 4096,
        clen: e.clen,
    };

    // The entry is in the overlay: the rehint finds it there.
    set_all(&f.map, [77], 0);
    let mut txn = f.map.begin_update().expect("begin");
    txn.rehint(ChunkNo(77), entry_hint(&e), moved)
        .expect("rehint");
    assert_eq!(txn.commit().expect("commit").rehints_applied, 1);
    assert_eq!(
        entry_hint(&f.map.get(ChunkNo(77)).expect("get").expect("entry")),
        moved
    );

    // The entry is in the sealed tree: a clean leaf is loaded for the rehint.
    let sealed = f.seal().root;
    let map = f.open_at(&sealed, 100_000);
    let mut txn = map.begin_update().expect("begin");
    txn.rehint(ChunkNo(77), moved, entry_hint(&e))
        .expect("rehint");
    // A hole and a stale hint are skipped.
    txn.rehint(ChunkNo(78), entry_hint(&e), moved)
        .expect("hole");
    txn.rehint(ChunkNo(77), moved, moved)
        .expect("stale after the first");
    let rep = txn.commit().expect("commit");
    assert_eq!((rep.rehints_applied, rep.rehints_skipped), (1, 2));
    assert_eq!(map.get(ChunkNo(77)).expect("get"), Some(e));
    assert_eq!(map.get(ChunkNo(78)).expect("get"), None);
    assert_eq!(map.dirty_leaf_count(), 1);
}

#[test]
fn a_noop_transaction_dirties_nothing() {
    let f = Fix::new(512, 100_000);
    set_all(&f.map, [10], 0);
    let r = f.seal().root;
    let map = f.open_at(&r, 100_000);
    set_all(&map, [10], 0);
    assert_eq!(map.dirty_leaf_count(), 0, "same value, same leaf");
    let mut txn = map.begin_update().expect("begin");
    txn.set_hole(ChunkNo(11)).expect("hole");
    txn.commit().expect("commit");
    assert_eq!(map.dirty_leaf_count(), 0, "a hole over a hole");
}

#[test]
fn entries_the_leaf_codec_cannot_hold_are_refused_at_set() {
    let f = Fix::new(512, 1000);
    let mut txn = f.map.begin_update().expect("begin");
    let mut e = data_entry(1, 0, ID_LEN);
    e.clen = 0;
    assert!(txn.set(ChunkNo(1), e).is_err());
    let mut e = data_entry(1, 0, ID_LEN);
    e.lineage = lunix_format::LineageIdx(1);
    assert!(txn.set(ChunkNo(1), e).is_err());
    let e = data_entry(1, 0, lunix_format::IdLen::ThirtyTwo);
    assert!(txn.set(ChunkNo(1), e).is_err());
    assert!(txn.set(ChunkNo(1), data_entry(1, 0, ID_LEN)).is_ok());
}
