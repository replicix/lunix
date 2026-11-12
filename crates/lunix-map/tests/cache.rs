//! Caches, the dirty-leaf cap, lazy opening and the non-blocking lookup.

mod common;

use std::sync::Arc;

use common::{set_all, Counting, Fix, SplitMix, ID_LEN};
use lunix_iface::map::{BlockMap, Cached, ChunkNo};
use lunix_iface::{Error, ErrorKind};
use lunix_map::{MapConfig, RadixMap};
use lunix_testkit::strategies::data_entry;

fn cold(f: &Fix, chunks: u64, cfg: MapConfig) -> (RadixMap, Arc<Counting>) {
    let src = Counting::new(f.packs.clone());
    let root = f.map.root();
    let map = RadixMap::open(f.codec.clone(), src.clone(), &root, chunks, cfg).expect("open");
    (map, src)
}

#[test]
fn a_cold_open_and_first_lookup_read_exactly_depth_nodes() {
    let chunks = 600_000_000u64;
    let f = Fix::new(512, chunks);
    let depth = u64::from(f.map.levels());
    assert_eq!(depth, 4);
    set_all(&f.map, [123_456_789, 123_456_790, 5, 599_999_999], 0);
    f.seal();
    let (map, src) = cold(&f, chunks, f.cfg);
    assert_eq!(src.count(), 0, "open is lazy");
    assert_eq!(
        map.get(ChunkNo(123_456_789)).expect("get"),
        Some(data_entry(123_456_789, 0, ID_LEN))
    );
    assert_eq!(src.count(), depth);
    // The same leaf again, and its neighbour: no reads.
    map.get(ChunkNo(123_456_790)).expect("get");
    map.get(ChunkNo(123_456_789)).expect("get");
    assert_eq!(src.count(), depth);
    // An absent subtree costs the interior nodes down to the empty child only.
    assert_eq!(map.get(ChunkNo(300_000_000)).expect("get"), None);
    assert!(src.count() <= depth + 1);
}

#[test]
fn try_get_cached_never_reads() {
    let f = Fix::new(512, 5_000_000);
    set_all(&f.map, [10, 4_000_000], 0);
    f.seal();
    let (map, src) = cold(&f, 5_000_000, f.cfg);
    assert_eq!(map.try_get_cached(ChunkNo(10)), Cached::Miss);
    assert_eq!(src.count(), 0);
    map.get(ChunkNo(10)).expect("get");
    let reads = src.count();
    assert_eq!(
        map.try_get_cached(ChunkNo(10)),
        Cached::Hit(data_entry(10, 0, ID_LEN))
    );
    assert_eq!(map.try_get_cached(ChunkNo(11)), Cached::Hole);
    // A leaf that was never loaded is a miss even though its parent is
    // resident; an empty subtree is a known hole.
    assert_eq!(map.try_get_cached(ChunkNo(4_000_000)), Cached::Miss);
    assert_eq!(map.try_get_cached(ChunkNo(2_000_000)), Cached::Hole);
    assert_eq!(src.count(), reads);
    assert_eq!(map.try_get_cached(ChunkNo(5_000_000)), Cached::Miss);
}

#[test]
fn the_overlay_answers_before_anything_is_sealed() {
    let f = Fix::new(512, 5_000_000);
    set_all(&f.map, [42], 3);
    assert_eq!(
        f.map.try_get_cached(ChunkNo(42)),
        Cached::Hit(data_entry(42, 3, ID_LEN))
    );
    assert_eq!(f.map.try_get_cached(ChunkNo(43)), Cached::Hole);
}

#[test]
fn the_dirty_leaf_cap_forces_a_seal() {
    let cfg = MapConfig::new(512, ID_LEN).with_dirty_leaf_cap(4);
    let f = Fix::with(common::plain(), cfg, 1_000_000);
    // Three leaves; chunks in the same leaf cost nothing more.
    set_all(&f.map, [0, 1, 512, 1024], 0);
    assert_eq!(f.map.dirty_leaf_count(), 3);
    let mut txn = f.map.begin_update().expect("room to start");
    // Already-dirty leaves keep accepting updates.
    txn.set(ChunkNo(2), data_entry(2, 0, ID_LEN))
        .expect("same leaf");
    let moved = lunix_format::Hint {
        seg: lunix_format::ObjId::new(0, 9).expect("id"),
        off: 0,
        clen: 1000,
    };
    txn.rehint(
        ChunkNo(512),
        lunix_iface::map::entry_hint(&data_entry(512, 0, ID_LEN)),
        moved,
    )
    .expect("rehint of a dirty leaf is never MapFull");
    // The fourth leaf fits, the fifth does not.
    txn.set(ChunkNo(1536), data_entry(1536, 0, ID_LEN))
        .expect("fourth");
    let err = txn
        .set(ChunkNo(2048), data_entry(2048, 0, ID_LEN))
        .expect_err("fifth leaf");
    assert!(matches!(err, Error::MapFull));
    drop(txn);
    // The refused transaction changed nothing.
    assert_eq!(f.map.get(ChunkNo(2)).expect("get"), None);
    assert_eq!(f.map.get(ChunkNo(1536)).expect("get"), None);
    assert_eq!(f.map.dirty_leaf_count(), 3);
    // Committing the fourth leaf reaches the cap: no new transaction.
    set_all(&f.map, [1536], 0);
    assert_eq!(f.map.dirty_leaf_count(), 4);
    assert_eq!(
        f.map.begin_update().err().map(|e| e.kind()),
        Some(ErrorKind::MapFull)
    );
    f.seal();
    let mut txn = f.map.begin_update().expect("room again");
    txn.set(ChunkNo(2048), data_entry(2048, 0, ID_LEN))
        .expect("set");
    txn.commit().expect("commit");
    assert_eq!(f.map.dirty_leaf_count(), 1);
}

#[test]
fn the_leaf_cache_stays_inside_its_budget() {
    const BUDGET: usize = 200 << 10;
    let chunks = 2_000_000u64;
    let cfg = MapConfig::new(512, ID_LEN).with_cache_max(BUDGET);
    let f = Fix::with(common::plain(), cfg, chunks);
    let mut rng = SplitMix(3);
    let cis: Vec<u64> = (0..600).map(|_| rng.below(chunks)).collect();
    // One entry per leaf is the smallest leaf; ~2 KiB each.
    set_all(&f.map, cis.iter().copied(), 0);
    f.seal();
    let (map, _src) = cold(&f, chunks, cfg);
    for round in 0..3 {
        for &ci in &cis {
            let want = data_entry(ci, 0, ID_LEN);
            assert_eq!(
                map.get(ChunkNo(ci)).expect("get"),
                Some(want),
                "round {round}"
            );
            assert!(map.cache_stats().cached_bytes <= BUDGET);
        }
    }
    let s = map.cache_stats();
    assert!(s.cached_leaves > 10 && s.cached_leaves < cis.len(), "{s:?}");
    assert!(s.resident_interior >= 2);
}

#[test]
fn hot_leaves_are_promoted_to_the_decoded_tier() {
    let f = Fix::new(512, 100_000);
    set_all(&f.map, [10, 700], 0);
    f.seal();
    let (map, _src) = cold(&f, 100_000, f.cfg);
    for _ in 0..10 {
        assert_eq!(
            map.get(ChunkNo(10)).expect("get"),
            Some(data_entry(10, 0, ID_LEN))
        );
        assert_eq!(map.get(ChunkNo(11)).expect("get"), None);
    }
    let s = map.cache_stats();
    assert_eq!(s.decoded_leaves, 1, "{s:?}");
    assert_eq!(
        map.get(ChunkNo(700)).expect("get"),
        Some(data_entry(700, 0, ID_LEN))
    );

    let cfg = f.cfg.with_decoded_tier(0);
    let (map, _src) = cold(&f, 100_000, cfg);
    for _ in 0..10 {
        map.get(ChunkNo(10)).expect("get");
    }
    assert_eq!(map.cache_stats().decoded_leaves, 0);
}

#[test]
fn a_cache_too_small_for_one_leaf_still_answers() {
    let cfg = MapConfig::new(512, ID_LEN).with_cache_max(100);
    let f = Fix::with(common::plain(), cfg, 100_000);
    set_all(&f.map, [10], 0);
    f.seal();
    let (map, _src) = cold(&f, 100_000, cfg);
    for _ in 0..3 {
        assert_eq!(
            map.get(ChunkNo(10)).expect("get"),
            Some(data_entry(10, 0, ID_LEN))
        );
    }
    assert_eq!(map.cache_stats().cached_leaves, 0);
}

#[test]
fn readers_run_while_a_seal_and_updates_run() {
    let chunks = 3_000_000u64;
    let f = Arc::new(Fix::new(512, chunks));
    set_all(&f.map, (0..2000).map(|i| i * 1499), 0);
    f.seal();
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut readers = Vec::new();
    for t in 0..3u64 {
        let (f, stop) = (f.clone(), stop.clone());
        readers.push(std::thread::spawn(move || {
            let mut rng = SplitMix(t);
            let mut n = 0u64;
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                let i = rng.below(2000);
                let ci = i * 1499;
                // Version 0 or 1: the writer only ever moves forward.
                let got = f.map.get(ChunkNo(ci)).expect("get").expect("present");
                assert!(got == data_entry(ci, 0, ID_LEN) || got == data_entry(ci, 1, ID_LEN));
                if let Cached::Hit(e) = f.map.try_get_cached(ChunkNo(ci)) {
                    assert!(e == data_entry(ci, 0, ID_LEN) || e == data_entry(ci, 1, ID_LEN));
                }
                n += 1;
            }
            n
        }));
    }
    for round in 0..6u64 {
        set_all(
            &f.map,
            (0..2000).filter(|i| i % 6 == round).map(|i| i * 1499),
            1,
        );
        f.seal();
    }
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    for r in readers {
        assert!(r.join().expect("reader") > 0);
    }
}
