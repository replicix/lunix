//! Map contract tests. See the [`BlockMap`] docs.

use std::collections::{BTreeMap, HashSet};

use lunix_format::commit::MapRoot;
use lunix_format::node::DataEntry;
use lunix_format::{ChunkId, IdLen, IdPrefix, LineageIdx, ObjId};

use crate::error::ErrorKind;
use crate::map::{entry_hint, BlockMap, Cached, ChunkNo, NodeHash, NodeRef};
use crate::store::PackWrite;

/// A map under test, with the pack storage behind it.
pub trait MapFixture {
    /// The map.
    fn map(&self) -> &dyn BlockMap;

    /// A pack writer on the storage the map reads from. The test seals it.
    fn open_pack(&self) -> Box<dyn PackWrite>;

    /// Replaces the map under test with one opened at `root` on the same pack
    /// storage (same size, same codec).
    fn reopen(&mut self, root: &MapRoot);

    /// The id-prefix length the map is configured for.
    fn id_len(&self) -> IdLen;
}

fn entry<F: MapFixture>(f: &F, ci: u64, version: u32) -> DataEntry {
    let mut id = [0u8; 32];
    id[..8].copy_from_slice(&ci.to_le_bytes());
    id[8..12].copy_from_slice(&version.to_le_bytes());
    DataEntry {
        lineage: LineageIdx(0),
        seg: ObjId::new(0, 1 + u64::from(version)).expect("segment id"),
        off: u32::try_from(ci % 1000).expect("small") * 64,
        clen: 1000 + version,
        prefix: IdPrefix::of(&ChunkId(id), f.id_len()),
    }
}

fn commit_sets<F: MapFixture>(f: &F, cis: &[u64], version: u32) {
    let mut txn = f.map().begin_update().expect("begin_update");
    for &ci in cis {
        txn.set(ChunkNo(ci), entry(f, ci, version)).expect("set");
    }
    txn.commit().expect("commit");
}

fn seal<F: MapFixture>(f: &F) -> MapRoot {
    let mut pack = f.open_pack();
    let res = f.map().seal(&mut *pack).expect("seal");
    assert!(res.new_packs.len() <= 1);
    if !pack.is_empty() {
        pack.seal().expect("seal pack");
    }
    assert_eq!(f.map().root(), res.root);
    assert_eq!(f.map().dirty_leaf_count(), 0, "seal leaves nothing dirty");
    res.root
}

fn collect_leaves(
    m: &dyn BlockMap,
    root: &MapRoot,
    skip: Option<&HashSet<NodeHash>>,
) -> BTreeMap<u64, DataEntry> {
    let mut out = BTreeMap::new();
    let mut last = None;
    let mut f = |ci: ChunkNo, e: &DataEntry| {
        assert!(last.is_none_or(|l| l < ci.0), "leaves ascend");
        last = Some(ci.0);
        out.insert(ci.0, *e);
    };
    match skip {
        Some(s) => m.walk_leaves_skipping(root, s, &mut f).expect("walk"),
        None => m.walk_leaves(root, &mut f).expect("walk"),
    }
    out
}

fn collect_nodes(
    m: &dyn BlockMap,
    root: &MapRoot,
    skip: Option<&HashSet<NodeHash>>,
) -> Vec<NodeRef> {
    let mut out = Vec::new();
    let mut f = |n: &NodeRef| out.push(*n);
    match skip {
        Some(s) => m.walk_nodes_skipping(root, s, &mut f).expect("walk"),
        None => m.walk_nodes(root, &mut f).expect("walk"),
    }
    out
}

/// A new map reads as holes everywhere and reports its size.
pub fn empty_map_has_only_holes<F: MapFixture, M: Fn(u64) -> F>(make: &M) {
    let f = make(1000);
    let m = f.map();
    assert_eq!(m.chunk_count(), 1000);
    assert_eq!(m.get(ChunkNo(0)).expect("get"), None);
    assert_eq!(m.get(ChunkNo(999)).expect("get"), None);
    assert!(!matches!(m.try_get_cached(ChunkNo(5)), Cached::Hit(_)));
    assert_eq!(m.dirty_leaf_count(), 0);
}

/// A transaction is invisible until committed, vanishes when dropped, and is
/// exclusive.
pub fn transaction_is_atomic_and_exclusive<F: MapFixture, M: Fn(u64) -> F>(make: &M) {
    let f = make(1000);
    let m = f.map();
    {
        let mut txn = m.begin_update().expect("begin");
        assert_eq!(
            m.begin_update().err().map(|e| e.kind()),
            Some(ErrorKind::Busy),
            "a second transaction is refused"
        );
        txn.set(ChunkNo(1), entry(&f, 1, 0)).expect("set");
        assert_eq!(
            m.get(ChunkNo(1)).expect("get"),
            None,
            "uncommitted is invisible"
        );
    }
    assert_eq!(m.get(ChunkNo(1)).expect("get"), None, "dropped means gone");

    let mut txn = m.begin_update().expect("begin");
    txn.set(ChunkNo(1), entry(&f, 1, 0)).expect("set");
    txn.set(ChunkNo(2), entry(&f, 2, 0)).expect("set");
    txn.set_hole(ChunkNo(3)).expect("hole");
    let err = txn
        .set(ChunkNo(1000), entry(&f, 1000, 0))
        .expect_err("past the end");
    assert_eq!(err.kind(), ErrorKind::Invalid);
    let report = txn.commit().expect("commit");
    assert_eq!(report.sets, 3);
    assert_eq!(m.get(ChunkNo(1)).expect("get"), Some(entry(&f, 1, 0)));
    assert_eq!(m.get(ChunkNo(2)).expect("get"), Some(entry(&f, 2, 0)));
    assert_eq!(m.get(ChunkNo(3)).expect("get"), None);
    assert!(m.dirty_leaf_count() >= 1);
}

/// Sealing then reopening at the new root preserves every entry.
pub fn seal_and_reopen_preserve_content<F: MapFixture, M: Fn(u64) -> F>(make: &M) {
    let mut f = make(10_000);
    let cis = [0u64, 7, 513, 4000, 9999];
    commit_sets(&f, &cis, 0);
    let root = seal(&f);
    f.reopen(&root);
    for &ci in &cis {
        assert_eq!(
            f.map().get(ChunkNo(ci)).expect("get"),
            Some(entry(&f, ci, 0))
        );
    }
    assert_eq!(f.map().get(ChunkNo(8)).expect("get"), None);
    assert_eq!(f.map().root().hash, root.hash);
}

/// Two maps with the same content have the same root hash whatever the order
/// of updates and the pack locations.
pub fn root_is_independent_of_update_order<F: MapFixture, M: Fn(u64) -> F>(make: &M) {
    let (a, b) = (make(10_000), make(10_000));
    let cis = [3u64, 600, 601, 5000, 9000];
    commit_sets(&a, &cis, 0);
    let rev: Vec<u64> = cis.iter().rev().copied().collect();
    commit_sets(&b, &rev[..2], 0);
    commit_sets(&b, &rev[2..], 0);
    assert_eq!(seal(&a).hash, seal(&b).hash);
}

/// `rehint` replaces a hint only if the entry still has the old one.
pub fn rehint_applies_only_to_the_old_hint<F: MapFixture, M: Fn(u64) -> F>(make: &M) {
    let f = make(1000);
    let m = f.map();
    let e0 = entry(&f, 5, 0);
    commit_sets(&f, &[5], 0);
    let moved = lunix_format::Hint {
        seg: ObjId::new(0, 99).expect("id"),
        off: 0,
        clen: e0.clen,
    };
    let mut txn = m.begin_update().expect("begin");
    txn.rehint(ChunkNo(5), entry_hint(&e0), moved)
        .expect("rehint");
    let report = txn.commit().expect("commit");
    assert_eq!((report.rehints_applied, report.rehints_skipped), (1, 0));
    let now = m.get(ChunkNo(5)).expect("get").expect("entry");
    assert_eq!(entry_hint(&now), moved);
    assert_eq!(now.prefix, e0.prefix, "only the hint changes");

    // A newer write wins over a stale move.
    commit_sets(&f, &[5], 2);
    let mut txn = m.begin_update().expect("begin");
    txn.rehint(ChunkNo(5), moved, entry_hint(&e0))
        .expect("rehint");
    let report = txn.commit().expect("commit");
    assert_eq!((report.rehints_applied, report.rehints_skipped), (0, 1));
    assert_eq!(m.get(ChunkNo(5)).expect("get"), Some(entry(&f, 5, 2)));
}

/// `walk_leaves` and `walk_nodes` agree with the content, and the skipping
/// variants visit exactly what changed between two roots.
pub fn walks_agree_with_content<F: MapFixture, M: Fn(u64) -> F>(make: &M) {
    let f = make(10_000);
    let m = f.map();
    let cis = [2u64, 3, 700, 5000];
    commit_sets(&f, &cis, 0);
    let root1 = seal(&f);

    let leaves = collect_leaves(m, &root1, None);
    let want: BTreeMap<u64, DataEntry> = cis.iter().map(|&c| (c, entry(&f, c, 0))).collect();
    assert_eq!(leaves, want);

    let nodes1 = collect_nodes(m, &root1, None);
    assert!(
        nodes1.iter().any(|n| n.hash == root1.hash),
        "the root is reported"
    );
    assert!(nodes1.iter().any(|n| n.level == 0), "leaves are reported");
    let set1: HashSet<NodeHash> = nodes1.iter().map(|n| n.hash).collect();
    assert!(collect_nodes(m, &root1, Some(&set1)).is_empty());

    commit_sets(&f, &[5000], 1);
    let root2 = seal(&f);
    let nodes2 = collect_nodes(m, &root2, None);
    let changed: HashSet<NodeHash> = nodes2
        .iter()
        .map(|n| n.hash)
        .filter(|h| !set1.contains(h))
        .collect();
    assert!(!changed.is_empty());
    let visited: HashSet<NodeHash> = collect_nodes(m, &root2, Some(&set1))
        .iter()
        .map(|n| n.hash)
        .collect();
    assert_eq!(visited, changed, "skipping visits the difference only");

    let only_new = collect_leaves(m, &root2, Some(&set1));
    assert_eq!(only_new.get(&5000), Some(&entry(&f, 5000, 1)));
    assert!(!only_new.contains_key(&2), "unchanged subtrees are skipped");
    assert_eq!(
        collect_leaves(m, &root1, None),
        want,
        "the old root is still readable"
    );
}

/// Growing keeps content, extends the range and survives seal and reopen.
pub fn grow_keeps_content<F: MapFixture, M: Fn(u64) -> F>(make: &M) {
    let mut f = make(1000);
    commit_sets(&f, &[3], 0);
    f.map().grow(5000).expect("grow");
    assert_eq!(f.map().chunk_count(), 5000);
    assert_eq!(f.map().get(ChunkNo(3)).expect("get"), Some(entry(&f, 3, 0)));
    assert_eq!(
        f.map().grow(10).expect_err("shrink").kind(),
        ErrorKind::Invalid
    );
    commit_sets(&f, &[4500], 0);
    let root = seal(&f);
    f.reopen(&root);
    assert_eq!(f.map().chunk_count(), 5000);
    assert_eq!(f.map().get(ChunkNo(3)).expect("get"), Some(entry(&f, 3, 0)));
    assert_eq!(
        f.map().get(ChunkNo(4500)).expect("get"),
        Some(entry(&f, 4500, 0))
    );
}
