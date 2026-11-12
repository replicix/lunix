//! The proptest strategies produce what their documentation says.

use std::collections::HashSet;

use lunix_iface::block::IoOp;
use lunix_testkit::strategies::{
    io_ops, map_ops, overlay_ops, pattern, IoShape, IoSpec, MapOp, OverlayOp, MAX_IO_BYTES,
};
use proptest::prelude::*;
use proptest::strategy::{Strategy, ValueTree};
use proptest::test_runner::TestRunner;

const DEV: u64 = 64 << 20;
const CHUNK: u64 = 64 << 10;

fn sample<S: Strategy>(s: &S, n: usize) -> Vec<S::Value> {
    let mut runner = TestRunner::deterministic();
    (0..n)
        .map(|_| s.new_tree(&mut runner).expect("tree").current())
        .collect()
}

fn all_ops() -> Vec<IoSpec> {
    sample(&io_ops(IoShape::new(DEV, CHUNK), 100..=100), 100)
        .into_iter()
        .flatten()
        .collect()
}

#[test]
fn valid_shape_yields_valid_requests_of_every_kind() {
    let ops = all_ops();
    assert_eq!(ops.len(), 10_000);
    let mut kinds = HashSet::new();
    for op in &ops {
        assert!(op.is_valid(DEV), "{op:?}");
        if op.op != IoOp::Flush {
            assert!((4096..=MAX_IO_BYTES).contains(&u64::from(op.len)), "{op:?}");
        } else {
            assert_eq!(op.len, 0);
        }
        kinds.insert(if op.fua {
            "fua"
        } else {
            match op.op {
                IoOp::Read => "read",
                IoOp::Write => "write",
                IoOp::Flush => "flush",
                IoOp::Discard => "discard",
                IoOp::WriteZeroes => "zeroes",
            }
        });
        assert!(!op.fua || op.op == IoOp::Write);
    }
    assert_eq!(kinds.len(), 6, "{kinds:?}");
}

#[test]
fn offsets_and_lengths_cover_chunk_alignments_and_extremes() {
    let ops: Vec<IoSpec> = all_ops().into_iter().filter(|o| o.len > 0).collect();
    let aligned = ops.iter().filter(|o| o.offset_bytes % CHUNK == 0).count();
    let unaligned = ops.iter().filter(|o| o.offset_bytes % CHUNK != 0).count();
    let straddle = ops
        .iter()
        .filter(|o| {
            let end = o.offset_bytes + u64::from(o.len);
            o.offset_bytes / CHUNK != (end - 1) / CHUNK
        })
        .count();
    assert!(aligned > 500 && unaligned > 500 && straddle > 500);
    assert!(ops.iter().any(|o| u64::from(o.len) == 4096));
    assert!(ops.iter().any(|o| u64::from(o.len) == MAX_IO_BYTES));
    assert!(ops.iter().any(|o| u64::from(o.len) == CHUNK));
}

#[test]
fn invalid_shape_yields_some_invalid_requests() {
    let shape = IoShape::new(DEV, CHUNK).with_invalid();
    let ops: Vec<IoSpec> = sample(&io_ops(shape, 100..=100), 100)
        .into_iter()
        .flatten()
        .collect();
    let bad = ops.iter().filter(|o| !o.is_valid(DEV)).count();
    assert!((100..2000).contains(&bad), "{bad} invalid of {}", ops.len());
}

#[test]
fn a_tiny_device_still_works() {
    let shape = IoShape::new(4096, 4096);
    for op in sample(&io_ops(shape, 50..=50), 20).into_iter().flatten() {
        assert!(op.is_valid(4096), "{op:?}");
    }
}

#[test]
fn payloads_depend_on_the_seed_and_have_the_length() {
    assert_eq!(pattern(1, 5000), pattern(1, 5000));
    assert_ne!(pattern(1, 4096), pattern(2, 4096));
    assert_eq!(pattern(9, 0).len(), 0);
    assert_eq!(pattern(9, 13).len(), 13);
    assert_eq!(&pattern(9, 13)[..], &pattern(9, 4096)[..13]);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    #[test]
    fn overlay_ops_have_distinct_seqs_and_stay_in_the_universe(
        ops in overlay_ops(64, 100),
    ) {
        let mut seqs = HashSet::new();
        for op in &ops {
            match *op {
                OverlayOp::Write { lba, blocks, write_seq, .. }
                | OverlayOp::Hole { lba, blocks, write_seq } => {
                    prop_assert!(blocks >= 1 && lba + blocks <= 64);
                    prop_assert!(write_seq >= 1);
                    seqs.insert(write_seq);
                }
                OverlayOp::Lookup { lba, blocks } => {
                    prop_assert!(blocks >= 1 && lba + blocks <= 64);
                }
                OverlayOp::Cut { seq } => prop_assert!(seq <= 100),
                OverlayOp::Release { .. } => {}
            }
        }
        let inserts = ops.iter().filter(|o| matches!(o, OverlayOp::Write{..} | OverlayOp::Hole{..})).count();
        prop_assert_eq!(seqs.len(), inserts);
    }

    #[test]
    fn map_ops_stay_in_range(ops in map_ops(2000, 100)) {
        for op in &ops {
            if let MapOp::Commit(t) | MapOp::Abort(t) = op {
                prop_assert!(!t.is_empty());
            }
            if let MapOp::Get { ci } = op {
                prop_assert!(*ci < 2000);
            }
        }
    }
}
