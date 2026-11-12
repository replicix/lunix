//! `FlatMap` beyond the contract suite: the dirty cap, corruption, size rules.

use std::sync::Arc;

use lunix_format::node::DataEntry;
use lunix_format::IdLen;
use lunix_iface::crypto::ExtentCodec;
use lunix_iface::map::{entry_hint, BlockMap, ChunkNo};
use lunix_iface::store::{PackRead, PackStore};
use lunix_iface::ErrorKind;
use lunix_testkit::strategies::data_entry;
use lunix_testkit::{FlatMap, MemPacks, PlainCodec};

const ID_LEN: IdLen = IdLen::Sixteen;

fn codec() -> Arc<dyn ExtentCodec> {
    Arc::new(PlainCodec::new())
}

fn map(packs: &MemPacks, chunks: u64) -> FlatMap {
    FlatMap::new(codec(), Arc::new(packs.clone()), chunks, ID_LEN)
}

#[test]
fn the_dirty_leaf_cap_refuses_atomically() {
    let packs = MemPacks::new();
    let m = map(&packs, 100_000).with_dirty_leaf_cap(2);
    // Leaves are 512 chunks wide: chunks 0, 600 and 1200 are three leaves.
    let mut txn = m.begin_update().expect("begin");
    for ci in [0u64, 600, 1200] {
        txn.set(ChunkNo(ci), data_entry(ci, 0, ID_LEN))
            .expect("set");
    }
    assert_eq!(
        txn.commit().expect_err("three leaves").kind(),
        ErrorKind::MapFull
    );
    assert_eq!(m.dirty_leaf_count(), 0, "nothing was applied");
    assert_eq!(m.get(ChunkNo(0)).expect("get"), None);

    let mut txn = m.begin_update().expect("begin");
    for ci in [0u64, 1, 600] {
        txn.set(ChunkNo(ci), data_entry(ci, 0, ID_LEN))
            .expect("set");
    }
    txn.commit().expect("two leaves fit");
    assert_eq!(m.dirty_leaf_count(), 2);
    assert_eq!(
        m.begin_update().err().map(|e| e.kind()),
        Some(ErrorKind::MapFull),
        "the cap is already hit"
    );

    let mut pack = packs.open_pack().expect("pack");
    m.seal(&mut *pack).expect("seal");
    pack.seal().expect("pack");
    assert!(m.begin_update().is_ok(), "sealing frees the cap");
}

#[test]
fn a_transaction_within_the_cap_may_rehint_what_it_dirtied() {
    let packs = MemPacks::new();
    let m = map(&packs, 10_000).with_dirty_leaf_cap(1);
    let (e0, e1) = (data_entry(5, 0, ID_LEN), data_entry(5, 1, ID_LEN));
    let mut txn = m.begin_update().expect("begin");
    txn.set(ChunkNo(5), e0).expect("set");
    txn.rehint(ChunkNo(5), entry_hint(&e0), entry_hint(&e1))
        .expect("rehint");
    let report = txn.commit().expect("commit");
    assert_eq!((report.sets, report.rehints_applied), (1, 1));
    assert_eq!(
        m.get(ChunkNo(5)).expect("get"),
        Some(e1_with_prefix_of(&e0, &e1))
    );
}

fn e1_with_prefix_of(old: &DataEntry, new: &DataEntry) -> DataEntry {
    DataEntry {
        prefix: old.prefix,
        ..*new
    }
}

#[test]
fn open_rejects_a_wrong_size_and_a_missing_pack() {
    let packs = MemPacks::new();
    let m = map(&packs, 1000);
    let mut txn = m.begin_update().expect("begin");
    txn.set(ChunkNo(1), data_entry(1, 0, ID_LEN)).expect("set");
    txn.commit().expect("commit");
    let mut pack = packs.open_pack().expect("pack");
    let root = m.seal(&mut *pack).expect("seal").root;
    pack.seal().expect("pack");

    let reads: Arc<dyn PackRead> = Arc::new(packs.clone());
    assert!(FlatMap::open(codec(), reads.clone(), &root, 1000, ID_LEN).is_ok());
    let e = FlatMap::open(codec(), reads, &root, 999, ID_LEN)
        .err()
        .expect("wrong size");
    assert_eq!(e.kind(), ErrorKind::Corrupt);

    let other: Arc<dyn PackRead> = Arc::new(MemPacks::new());
    let e = FlatMap::open(codec(), other, &root, 1000, ID_LEN)
        .err()
        .expect("no such pack");
    assert_eq!(e.kind(), ErrorKind::NotFound);
}

#[test]
fn an_empty_map_reopens_with_the_size_the_caller_names() {
    let packs = MemPacks::new();
    let m = map(&packs, 77);
    let mut pack = packs.open_pack().expect("pack");
    let res = m.seal(&mut *pack).expect("seal");
    assert!(res.new_packs.is_empty() && res.root.pack.is_none());
    let reads: Arc<dyn PackRead> = Arc::new(packs.clone());
    let again = FlatMap::open(codec(), reads.clone(), &res.root, 77, ID_LEN).expect("open");
    assert_eq!(again.chunk_count(), 77);
    assert!(FlatMap::open(codec(), reads, &res.root, 78, ID_LEN).is_err());
}

#[test]
fn nodes_sealed_under_another_key_do_not_open() {
    // Seal under one key, read back under another: every node fails to open.
    let packs = MemPacks::new();
    let m = map(&packs, 1000);
    let mut txn = m.begin_update().expect("begin");
    txn.set(ChunkNo(1), data_entry(1, 0, ID_LEN)).expect("set");
    txn.commit().expect("commit");
    let mut pack = packs.open_pack().expect("pack");
    let root = m.seal(&mut *pack).expect("seal").root;
    pack.seal().expect("pack");
    let foreign: Arc<dyn ExtentCodec> = Arc::new(PlainCodec::with_key([5; 16]));
    let reads: Arc<dyn PackRead> = Arc::new(packs);
    let e = FlatMap::open(foreign, reads, &root, 1000, ID_LEN)
        .err()
        .expect("foreign key");
    assert_eq!(e.kind(), ErrorKind::Auth);
}

#[test]
fn growing_changes_the_root_but_not_the_content() {
    let packs = MemPacks::new();
    let m = map(&packs, 1000);
    let mut txn = m.begin_update().expect("begin");
    txn.set(ChunkNo(1), data_entry(1, 0, ID_LEN)).expect("set");
    txn.commit().expect("commit");
    let mut pack = packs.open_pack().expect("pack");
    let first = m.seal(&mut *pack).expect("seal").root;
    pack.seal().expect("pack");
    m.grow(2000).expect("grow");
    let mut pack = packs.open_pack().expect("pack");
    let res = m.seal(&mut *pack).expect("seal");
    pack.seal().expect("pack");
    assert_ne!(res.root.hash, first.hash);
    assert_eq!(res.dirty_leaves, 0, "no leaf was rewritten");
    assert_eq!(
        m.get(ChunkNo(1)).expect("get"),
        Some(data_entry(1, 0, ID_LEN))
    );
}
