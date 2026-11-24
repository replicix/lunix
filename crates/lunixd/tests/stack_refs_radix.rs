//! The reference table's wrapper over the real radix map (milestone
//! condemncut, the `RadixMap` proptest of the refs review): random
//! transactions of the shapes the cut produces (ascending destage sets and
//! holes, compaction re-hints, a re-hint whose old hint is not there, a
//! dropped transaction), over a map of eight leaves (fanout 256, so chunk
//! runs cross leaf boundaries and one transaction touches many leaves). After
//! every commit the table equals a count of the map's entries, chunk by chunk,
//! and no transaction report differed from the prediction.

#![cfg(feature = "testsupport")]

use std::collections::BTreeMap;
use std::sync::Arc;

use lunix_format::node::DataEntry;
use lunix_format::{Hint, IdLen, IdPrefix, LineageIdx, SegId};
use lunix_iface::map::{entry_hint, BlockMap, ChunkNo, NodeSource};
use lunix_map::{MapConfig, RadixMap};
use lunix_testkit::{MemPacks, PlainCodec};

const CHUNKS: u64 = 2_000;

fn seg(n: u64) -> SegId {
    SegId::new(0, n).expect("segment")
}

fn entry(s: u64, off: u32) -> DataEntry {
    DataEntry {
        lineage: LineageIdx(0),
        seg: seg(s),
        off,
        clen: 4096,
        prefix: IdPrefix::from_slice(IdLen::Sixteen, &[3; 16]).expect("prefix"),
    }
}

fn radix() -> Arc<dyn BlockMap> {
    let source: Arc<dyn NodeSource> = Arc::new(MemPacks::new());
    Arc::new(
        RadixMap::new(
            Arc::new(PlainCodec::new()),
            source,
            CHUNKS,
            MapConfig::new(256, IdLen::Sixteen),
        )
        .expect("map"),
    )
}

/// The entries per segment of the whole map, chunk by chunk (the in-memory
/// map: the unsealed leaves are not in `root()` yet).
fn walked(m: &dyn BlockMap) -> BTreeMap<SegId, i64> {
    let mut out = BTreeMap::new();
    for ci in 0..CHUNKS {
        if let Some(e) = m.get(ChunkNo(ci)).expect("get") {
            *out.entry(e.seg).or_insert(0) += 1;
        }
    }
    out
}

/// A small deterministic generator (the crate has no proptest).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

#[derive(Clone, Copy, Debug)]
enum Raw {
    Set(u64, u64, u32),
    Hole(u64),
    /// A re-hint of what the entry has now.
    Move(u64, u64),
    /// A re-hint from a hint that is not there.
    Stale(u64, u64),
}

impl Raw {
    fn chunk(self) -> u64 {
        match self {
            Self::Set(c, ..) | Self::Hole(c) | Self::Move(c, _) | Self::Stale(c, _) => c,
        }
    }
}

fn raw(r: &mut Rng) -> Raw {
    let c = r.below(CHUNKS);
    let s = 1 + r.below(8);
    match r.below(11) {
        0..=4 => Raw::Set(c, s, r.below(256) as u32),
        5 | 6 => Raw::Hole(c),
        7..=9 => Raw::Move(c, s),
        _ => Raw::Stale(c, s),
    }
}

fn run(seed: u64) {
    let mut rng = Rng(seed | 1);
    let inner = radix();
    let (m, read) = lunix_engine::ref_map_counted(Arc::clone(&inner));
    let (mut sets, mut moves, mut ascending_txns, mut dropped_txns) = (0u64, 0u64, 0u64, 0u64);
    for _ in 0..14 {
        let n = rng.below(60);
        let mut ops: Vec<Raw> = (0..n).map(|_| raw(&mut rng)).collect();
        // A destage's shape (sorted, each chunk once) or any order with repeats.
        let ascending = rng.below(2) == 0;
        if ascending {
            ops.sort_by_key(|r| r.chunk());
            ops.dedup_by_key(|r| r.chunk());
            ascending_txns += 1;
        }
        let dropped = rng.below(10) == 0;
        let mut txn = m.begin_update().expect("begin");
        for op in ops {
            match op {
                Raw::Set(c, s, o) => {
                    sets += 1;
                    txn.set(ChunkNo(c), entry(s, o)).expect("set");
                }
                Raw::Hole(c) => txn.set_hole(ChunkNo(c)).expect("hole"),
                Raw::Move(c, s) => {
                    moves += 1;
                    let ci = ChunkNo(c);
                    let old = inner.get(ci).expect("get").map_or(
                        Hint {
                            seg: seg(1),
                            off: 0,
                            clen: 4096,
                        },
                        |e| entry_hint(&e),
                    );
                    txn.rehint(
                        ci,
                        old,
                        Hint {
                            seg: seg(s),
                            off: 4242,
                            clen: 4096,
                        },
                    )
                    .expect("rehint");
                }
                Raw::Stale(c, s) => txn
                    .rehint(
                        ChunkNo(c),
                        Hint {
                            seg: seg(99),
                            off: 1,
                            clen: 4096,
                        },
                        Hint {
                            seg: seg(s),
                            off: 7,
                            clen: 4096,
                        },
                    )
                    .expect("rehint"),
            }
        }
        if dropped {
            dropped_txns += 1;
            drop(txn);
        } else {
            txn.commit().expect("commit");
        }
        let (counts, mismatches) = read();
        assert_eq!(counts, walked(&*inner), "seed {seed:#x}");
        assert_eq!(mismatches, 0, "seed {seed:#x}");
    }
    assert!(sets > 0 && moves > 0 && ascending_txns > 0 && dropped_txns < 14);
}

#[test]
fn the_table_equals_a_walk_of_the_radix_map_after_every_commit() {
    for seed in 0..300u64 {
        run(0x9e37_79b9_7f4a_7c15u64.wrapping_mul(seed + 1));
    }
}
