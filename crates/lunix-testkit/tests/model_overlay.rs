//! Self-consistency of `FlatOverlay`: random sequences against a dense model
//! with a different representation (one slot per block of a small universe).

use lunix_format::WriteSeq;
use lunix_iface::journal::{JournalLoc, LaneId};
use lunix_iface::overlay::{BlockRange, Overlay, OverlayCut, Piece, PieceSource};
use lunix_testkit::strategies::{overlay_ops, OverlayOp};
use lunix_testkit::{coalesce, FlatOverlay};
use proptest::prelude::*;

const UNIVERSE: u64 = 48;
const CHUNK_BLOCKS: u64 = 8;
const BLOCK: u64 = 4096;

type Slot = Option<(PieceSource, u64)>;
/// Every version a block holds, by `write_seq` (ADR-3: superseded versions
/// `<= c` stay until a release).
type Versions = std::collections::BTreeMap<u64, PieceSource>;

fn newest(v: &Versions, c: u64) -> Slot {
    v.range(..=c).next_back().map(|(&seq, &src)| (src, seq))
}
type CutBlocks = Vec<(u64, PieceSource, u64)>;

fn loc(ordinal: u32, seq: u64) -> JournalLoc {
    JournalLoc {
        lane: LaneId(0),
        ordinal,
        offset: 4096 + seq * (1 << 24),
    }
}

fn model_put(
    model: &mut [Versions],
    lba: u64,
    blocks: u64,
    seq: u64,
    src: impl Fn(u64) -> PieceSource,
) {
    for i in 0..blocks {
        model[(lba + i) as usize].insert(seq, src(i));
    }
}

fn per_block(pieces: &[Piece], from: u64, to: u64) -> Result<Vec<Slot>, TestCaseError> {
    let mut out: Vec<Slot> = vec![None; (to - from) as usize];
    let mut next = from;
    for p in pieces {
        prop_assert_eq!(p.range.lba, next, "tiles in order");
        prop_assert!(p.range.blocks >= 1);
        for i in 0..p.range.blocks {
            let src = match p.source {
                PieceSource::Journal(l) => PieceSource::Journal(l.advance(i * BLOCK).expect("adv")),
                s => s,
            };
            out[(p.range.lba + i - from) as usize] = match src {
                PieceSource::None => None,
                s => Some((s, p.write_seq.0)),
            };
        }
        next += p.range.blocks;
    }
    prop_assert_eq!(next, to, "tiles the whole range");
    Ok(out)
}

fn expected_cut(model: &[Versions], c: u64) -> Vec<(u64, CutBlocks)> {
    let mut chunks: Vec<(u64, CutBlocks)> = Vec::new();
    for (lba, versions) in model.iter().enumerate() {
        let Some((src, seq)) = newest(versions, c) else {
            continue;
        };
        let chunk = lba as u64 / CHUNK_BLOCKS;
        match chunks.last_mut() {
            Some((ch, v)) if *ch == chunk => v.push((lba as u64, src, seq)),
            _ => chunks.push((chunk, vec![(lba as u64, src, seq)])),
        }
    }
    chunks
}

fn check_cut(cut: &OverlayCut, model: &[Versions], c: u64) -> Result<(), TestCaseError> {
    prop_assert_eq!(cut.cut_seq, WriteSeq(c));
    let want = expected_cut(model, c);
    prop_assert_eq!(cut.chunks.len(), want.len());
    for (got, (chunk, blocks)) in cut.chunks.iter().zip(&want) {
        prop_assert_eq!(got.chunk, *chunk);
        let mut flat = Vec::new();
        let mut prev_end = 0;
        for p in &got.pieces {
            prop_assert!(!matches!(p.source, PieceSource::None));
            prop_assert!(p.range.lba >= prev_end, "ascending, disjoint");
            prop_assert_eq!(p.range.lba / CHUNK_BLOCKS, *chunk);
            prop_assert_eq!((p.range.lba + p.range.blocks - 1) / CHUNK_BLOCKS, *chunk);
            prev_end = p.range.lba + p.range.blocks;
            for i in 0..p.range.blocks {
                let src = match p.source {
                    PieceSource::Journal(l) => {
                        PieceSource::Journal(l.advance(i * BLOCK).expect("adv"))
                    }
                    s => s,
                };
                flat.push((p.range.lba + i, src, p.write_seq.0));
            }
        }
        prop_assert_eq!(&flat, blocks);
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1000))]

    #[test]
    fn flat_overlay_matches_the_dense_model(ops in overlay_ops(UNIVERSE, 100)) {
        let o = FlatOverlay::new(CHUNK_BLOCKS);
        let mut model: Vec<Versions> = vec![Versions::new(); UNIVERSE as usize];
        let mut cuts: Vec<(OverlayCut, u64)> = Vec::new();
        for op in ops {
            match op {
                OverlayOp::Write { lba, blocks, ordinal, write_seq } => {
                    o.insert_write(BlockRange { lba, blocks }, loc(ordinal, write_seq), WriteSeq(write_seq));
                    model_put(&mut model, lba, blocks, write_seq, |i| {
                        PieceSource::Journal(loc(ordinal, write_seq).advance(i * BLOCK).expect("adv"))
                    });
                }
                OverlayOp::Hole { lba, blocks, write_seq } => {
                    o.insert_hole(BlockRange { lba, blocks }, WriteSeq(write_seq));
                    model_put(&mut model, lba, blocks, write_seq, |_| PieceSource::Hole);
                }
                OverlayOp::Lookup { lba, blocks } => {
                    let mut out = vec![Piece {
                        range: BlockRange { lba: 999, blocks: 1 },
                        source: PieceSource::Hole,
                        write_seq: WriteSeq(7),
                    }];
                    o.lookup(BlockRange { lba, blocks }, &mut out);
                    let want: Vec<Slot> = model[lba as usize..(lba + blocks) as usize]
                        .iter()
                        .map(|v| newest(v, u64::MAX))
                        .collect();
                    let want = &want[..];
                    prop_assert_eq!(&per_block(&out, lba, lba + blocks)?[..], want);
                    // Merging keeps the content and never crosses a difference.
                    let merged = coalesce(&out);
                    prop_assert!(merged.len() <= out.len());
                    prop_assert_eq!(&per_block(&merged, lba, lba + blocks)?[..], want);
                    prop_assert_eq!(coalesce(&merged), merged);
                }
                OverlayOp::Cut { seq } => {
                    let cut = o.cut(WriteSeq(seq));
                    check_cut(&cut, &model, seq)?;
                    cuts.push((cut, seq));
                }
                OverlayOp::Release { n } => {
                    if let Some((cut, seq)) = cuts.get(n % cuts.len().max(1)) {
                        o.release(cut);
                        for versions in &mut model {
                            versions.retain(|s, _| s > seq);
                        }
                    }
                }
            }
            let held: u64 = model.iter().map(|v| v.len() as u64).sum();
            prop_assert_eq!(o.dirty_bytes(), held * BLOCK);
        }
    }
}
