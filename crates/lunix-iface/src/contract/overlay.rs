//! Overlay contract tests. See the [`Overlay`] docs.

use std::collections::BTreeMap;

use lunix_format::WriteSeq;

use crate::journal::{JournalLoc, LaneId};
use crate::overlay::{BlockRange, Overlay, Piece, PieceSource};

fn loc(tag: u32) -> JournalLoc {
    JournalLoc {
        lane: LaneId(0),
        ordinal: tag,
        offset: 4096,
    }
}

fn range(lba: u64, blocks: u64) -> BlockRange {
    BlockRange { lba, blocks }
}

/// Per-block view of pieces: block -> (source with the location advanced to
/// that block, write_seq). Implementations may split or merge pieces freely.
fn per_block(pieces: &[Piece]) -> BTreeMap<u64, (PieceSource, u64)> {
    let mut m = BTreeMap::new();
    for p in pieces {
        for i in 0..p.range.blocks {
            let src = match p.source {
                PieceSource::Journal(l) => {
                    PieceSource::Journal(l.advance(i * 4096).expect("advance"))
                }
                other => other,
            };
            let prev = m.insert(p.range.lba + i, (src, p.write_seq.0));
            assert!(
                prev.is_none(),
                "pieces overlap at block {}",
                p.range.lba + i
            );
        }
    }
    m
}

fn lookup<O: Overlay>(o: &O, r: BlockRange) -> Vec<Piece> {
    let mut out = vec![Piece {
        range: range(u64::MAX - 1, 1),
        source: PieceSource::Hole,
        write_seq: WriteSeq(9),
    }];
    o.lookup(r, &mut out);
    out
}

fn journal_at(l: JournalLoc, blocks_in: u64) -> PieceSource {
    PieceSource::Journal(l.advance(blocks_in * 4096).expect("advance"))
}

/// The newest `write_seq` wins per block; older data survives where it is not
/// overwritten.
pub fn newest_write_wins<O: Overlay>(o: &O, _chunk_blocks: u64) {
    o.insert_write(range(0, 8), loc(1), WriteSeq(1));
    o.insert_write(range(4, 8), loc(2), WriteSeq(2));
    let m = per_block(&lookup(o, range(0, 12)));
    for b in 0..4 {
        assert_eq!(m[&b], (journal_at(loc(1), b), 1), "block {b}");
    }
    for b in 4..12 {
        assert_eq!(m[&b], (journal_at(loc(2), b - 4), 2), "block {b}");
    }
}

/// `lookup` clears `out`, tiles exactly the requested range in ascending
/// order, and reports `None` for blocks the overlay does not hold.
pub fn lookup_tiles_the_range<O: Overlay>(o: &O, _chunk_blocks: u64) {
    o.insert_write(range(10, 2), loc(1), WriteSeq(1));
    let pieces = lookup(o, range(8, 8));
    let mut next = 8;
    for p in &pieces {
        assert_eq!(p.range.lba, next, "pieces must tile in order");
        assert!(p.range.blocks >= 1);
        next += p.range.blocks;
    }
    assert_eq!(next, 16, "pieces must cover the whole range");
    let m = per_block(&pieces);
    for b in 8..16 {
        match b {
            10 | 11 => assert_eq!(m[&b], (journal_at(loc(1), b - 10), 1)),
            _ => assert!(matches!(m[&b].0, PieceSource::None), "block {b}"),
        }
    }
}

/// A hole hides older data in its range, and newer data hides the hole.
pub fn hole_hides_data<O: Overlay>(o: &O, _chunk_blocks: u64) {
    o.insert_write(range(0, 8), loc(1), WriteSeq(1));
    o.insert_hole(range(2, 4), WriteSeq(2));
    o.insert_write(range(3, 1), loc(3), WriteSeq(3));
    let m = per_block(&lookup(o, range(0, 8)));
    assert_eq!(m[&1], (journal_at(loc(1), 1), 1));
    assert_eq!(m[&2], (PieceSource::Hole, 2));
    assert_eq!(m[&3], (journal_at(loc(3), 0), 3));
    assert_eq!(m[&5], (PieceSource::Hole, 2));
    assert_eq!(m[&6], (journal_at(loc(1), 6), 1));
}

/// A cut holds exactly the entries with `write_seq <= c` as of the call; later
/// inserts, overwrites included, do not change it.
pub fn cut_is_frozen<O: Overlay>(o: &O, _chunk_blocks: u64) {
    o.insert_write(range(0, 2), loc(1), WriteSeq(1));
    o.insert_write(range(2, 2), loc(2), WriteSeq(5));
    let cut = o.cut(WriteSeq(3));
    assert_eq!(cut.cut_seq, WriteSeq(3));
    o.insert_write(range(0, 2), loc(9), WriteSeq(4));
    let all: Vec<Piece> = cut.chunks.iter().flat_map(|c| c.pieces.clone()).collect();
    let m = per_block(&all);
    assert_eq!(m.len(), 2, "only blocks 0 and 1 are at or below the cut");
    assert_eq!(m[&0], (journal_at(loc(1), 0), 1));
    assert_eq!(m[&1], (journal_at(loc(1), 1), 1));
}

/// A cut groups pieces by chunk, chunks ascending, pieces inside their chunk.
pub fn cut_groups_by_chunk<O: Overlay>(o: &O, chunk_blocks: u64) {
    let start = chunk_blocks - 2;
    o.insert_write(range(start, 4), loc(1), WriteSeq(1));
    o.insert_write(range(chunk_blocks * 5, 1), loc(2), WriteSeq(2));
    let cut = o.cut(WriteSeq(2));
    let chunks: Vec<u64> = cut.chunks.iter().map(|c| c.chunk).collect();
    assert_eq!(chunks, vec![0, 1, 5]);
    for c in &cut.chunks {
        let (lo, hi) = (c.chunk * chunk_blocks, (c.chunk + 1) * chunk_blocks);
        let mut prev_end = lo;
        for p in &c.pieces {
            assert!(p.range.lba >= prev_end, "pieces ascend without overlap");
            assert!(p.range.lba + p.range.blocks <= hi, "piece inside its chunk");
            assert!(!matches!(p.source, PieceSource::None));
            prev_end = p.range.lba + p.range.blocks;
        }
    }
}

/// `release` removes the entries of the cut that are still visible and keeps
/// anything written after.
pub fn release_keeps_later_overwrites<O: Overlay>(o: &O, _chunk_blocks: u64) {
    o.insert_write(range(0, 4), loc(1), WriteSeq(1));
    let cut = o.cut(WriteSeq(1));
    o.insert_write(range(2, 2), loc(2), WriteSeq(2));
    o.release(&cut);
    let m = per_block(&lookup(o, range(0, 4)));
    assert!(matches!(m[&0].0, PieceSource::None));
    assert!(matches!(m[&1].0, PieceSource::None));
    assert_eq!(m[&2], (journal_at(loc(2), 0), 2));
    assert_eq!(m[&3], (journal_at(loc(2), 1), 2));
}

/// `dirty_bytes` rises with inserts and returns to zero after a full release.
pub fn dirty_bytes_follow_inserts<O: Overlay>(o: &O, _chunk_blocks: u64) {
    assert_eq!(o.dirty_bytes(), 0);
    o.insert_write(range(0, 4), loc(1), WriteSeq(1));
    assert_eq!(o.dirty_bytes(), 4 * 4096);
    let cut = o.cut(WriteSeq(1));
    o.release(&cut);
    assert_eq!(o.dirty_bytes(), 0);
}
