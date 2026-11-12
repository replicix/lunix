//! Proptest against `FlatOverlay`: the same operations on both, and every
//! lookup, cut and the final state agree (modulo the merging that the trait
//! allows, removed by `coalesce`).

use lunix_format::WriteSeq;
use lunix_iface::journal::{JournalLoc, LaneId};
use lunix_iface::overlay::{BlockRange, Overlay, OverlayCut, Piece};
use lunix_overlay::{OverlayOptions, ShardedOverlay};
use lunix_testkit::strategies::{overlay_ops, OverlayOp};
use lunix_testkit::{coalesce, FlatOverlay};
use proptest::prelude::*;
use proptest::test_runner::Config;

const UNIVERSE: u64 = 80;
const OPS: usize = 200;

fn loc(ordinal: u32, write_seq: u64) -> JournalLoc {
    JournalLoc {
        lane: LaneId((write_seq % 3) as u16),
        ordinal,
        offset: 4096 + (write_seq << 24),
    }
}

fn lookup(o: &dyn Overlay, lba: u64, blocks: u64) -> Vec<Piece> {
    let mut out = Vec::new();
    o.lookup(BlockRange { lba, blocks }, &mut out);
    out
}

fn assert_tiles(p: &[Piece], lba: u64, blocks: u64) -> Result<(), TestCaseError> {
    let mut next = lba;
    for x in p {
        prop_assert_eq!(x.range.lba, next);
        prop_assert!(x.range.blocks >= 1);
        next += x.range.blocks;
    }
    prop_assert_eq!(next, lba + blocks);
    Ok(())
}

fn same_cut(a: &OverlayCut, b: &OverlayCut) -> Result<(), TestCaseError> {
    prop_assert_eq!(a.cut_seq, b.cut_seq);
    prop_assert_eq!(
        a.chunks.iter().map(|c| c.chunk).collect::<Vec<_>>(),
        b.chunks.iter().map(|c| c.chunk).collect::<Vec<_>>()
    );
    for (x, y) in a.chunks.iter().zip(&b.chunks) {
        prop_assert_eq!(
            coalesce(&x.pieces),
            coalesce(&y.pieces),
            "chunk {}",
            x.chunk
        );
    }
    Ok(())
}

/// Applies a write/hole to both, split in two inserts with the same
/// `write_seq` for a third of the records so that merging is exercised.
fn insert_both(
    a: &dyn Overlay,
    b: &dyn Overlay,
    lba: u64,
    blocks: u64,
    seq: u64,
    l: Option<JournalLoc>,
) {
    let parts: Vec<(u64, u64)> = if seq.is_multiple_of(3) && blocks >= 2 {
        let h = 1 + seq % (blocks - 1);
        if seq.is_multiple_of(2) {
            vec![(0, h), (h, blocks - h)]
        } else {
            vec![(h, blocks - h), (0, h)]
        }
    } else {
        vec![(0, blocks)]
    };
    for (off, n) in parts {
        let r = BlockRange {
            lba: lba + off,
            blocks: n,
        };
        for o in [a, b] {
            match l {
                Some(l) => o.insert_write(r, l.advance(off * 4096).unwrap(), WriteSeq(seq)),
                None => o.insert_hole(r, WriteSeq(seq)),
            }
        }
    }
}

fn check(
    chunk_blocks: u64,
    shards: usize,
    stripe_chunks: u64,
    ops: &[OverlayOp],
) -> Result<(), TestCaseError> {
    let mine = ShardedOverlay::with_options(OverlayOptions {
        shards,
        stripe_chunks,
        ..OverlayOptions::new(chunk_blocks)
    })
    .unwrap();
    let flat = FlatOverlay::new(chunk_blocks);
    let (mut cuts_m, mut cuts_f): (Vec<OverlayCut>, Vec<OverlayCut>) = (vec![], vec![]);
    for op in ops {
        match *op {
            OverlayOp::Write {
                lba,
                blocks,
                ordinal,
                write_seq,
            } => insert_both(
                &mine,
                &flat,
                lba,
                blocks,
                write_seq,
                Some(loc(ordinal, write_seq)),
            ),
            OverlayOp::Hole {
                lba,
                blocks,
                write_seq,
            } => insert_both(&mine, &flat, lba, blocks, write_seq, None),
            OverlayOp::Lookup { lba, blocks } => {
                let (m, f) = (lookup(&mine, lba, blocks), lookup(&flat, lba, blocks));
                assert_tiles(&m, lba, blocks)?;
                prop_assert_eq!(coalesce(&m), coalesce(&f));
            }
            OverlayOp::Cut { seq } => {
                let (m, f) = (mine.cut(WriteSeq(seq)), flat.cut(WriteSeq(seq)));
                same_cut(&m, &f)?;
                for c in &m.chunks {
                    for p in &c.pieces {
                        prop_assert!(p.write_seq.0 <= seq);
                        prop_assert!(p.range.lba / chunk_blocks == c.chunk);
                        prop_assert!((p.range.lba + p.range.blocks - 1) / chunk_blocks == c.chunk);
                    }
                }
                cuts_m.push(m);
                cuts_f.push(f);
            }
            OverlayOp::Release { n } => {
                if !cuts_m.is_empty() {
                    let i = n % cuts_m.len();
                    mine.release(&cuts_m[i]);
                    flat.release(&cuts_f[i]);
                }
            }
        }
        prop_assert_eq!(mine.dirty_bytes(), flat.dirty_bytes());
        mine.validate().map_err(TestCaseError::fail)?;
    }
    // Cuts taken earlier are still what they were when taken (owned data), and
    // a final full lookup and full cut agree.
    let (m, f) = (lookup(&mine, 0, UNIVERSE), lookup(&flat, 0, UNIVERSE));
    prop_assert_eq!(coalesce(&m), coalesce(&f));
    same_cut(&mine.cut(WriteSeq(u64::MAX)), &flat.cut(WriteSeq(u64::MAX)))
}

fn layout() -> impl Strategy<Value = (usize, u64)> {
    (
        prop::sample::select(vec![1usize, 2, 3, 5, 64]),
        prop::sample::select(vec![1u64, 2, 3, 7, 1000]),
    )
}

proptest! {
    #![proptest_config(Config { cases: 10_000, ..Config::default() })]

    #[test]
    fn agrees_with_flat_overlay_at_16_kib_chunks(
        (shards, stripe) in layout(),
        ops in overlay_ops(UNIVERSE, OPS),
    ) {
        check(4, shards, stripe, &ops)?;
    }

    #[test]
    fn agrees_with_flat_overlay_at_64_kib_chunks(
        (shards, stripe) in layout(),
        ops in overlay_ops(UNIVERSE, OPS),
    ) {
        check(16, shards, stripe, &ops)?;
    }
}
