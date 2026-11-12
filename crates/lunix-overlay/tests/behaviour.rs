//! Hand-written checks of the gate items and the corners the model cannot name.

use lunix_format::WriteSeq;
use lunix_iface::journal::{JournalLoc, LaneId};
use lunix_iface::overlay::{BlockRange, Overlay, Piece, PieceSource};
use lunix_overlay::{OverlayOptions, ShardedOverlay};
use lunix_testkit::coalesce;

fn loc(offset: u64) -> JournalLoc {
    JournalLoc {
        lane: LaneId(1),
        ordinal: 7,
        offset,
    }
}

fn r(lba: u64, blocks: u64) -> BlockRange {
    BlockRange { lba, blocks }
}

fn look(o: &ShardedOverlay, lba: u64, blocks: u64) -> Vec<Piece> {
    let mut v = Vec::new();
    o.lookup(r(lba, blocks), &mut v);
    v
}

fn tiny(chunk_blocks: u64) -> ShardedOverlay {
    ShardedOverlay::with_options(OverlayOptions {
        shards: 4,
        stripe_chunks: 2,
        ..OverlayOptions::new(chunk_blocks)
    })
    .unwrap()
}

#[test]
fn a_cut_is_unaffected_by_later_inserts_and_release_keeps_overwrites() {
    let o = tiny(4);
    o.insert_write(r(0, 20), loc(4096), WriteSeq(1));
    o.insert_hole(r(5, 3), WriteSeq(2));
    let cut = o.cut(WriteSeq(2));
    let before = cut.clone();
    // Later inserts, overwriting blocks inside the cut and beyond it.
    o.insert_write(r(4, 6), loc(1 << 30), WriteSeq(3));
    o.insert_hole(r(18, 10), WriteSeq(4));
    assert_eq!(cut, before);
    o.release(&cut);
    o.validate().unwrap();
    // Only the post-cut overwrites remain.
    let held: Vec<(u64, u64)> = coalesce(&look(&o, 0, 28))
        .iter()
        .filter(|p| !matches!(p.source, PieceSource::None))
        .map(|p| (p.range.lba, p.range.blocks))
        .collect();
    assert_eq!(held, vec![(4, 6), (18, 10)]);
    assert_eq!(o.dirty_bytes(), 16 * 4096);
    // The hole at 5..8 was overwritten by seq 3: it must survive the release.
    let p = look(&o, 5, 1)[0];
    assert_eq!(p.write_seq, WriteSeq(3));
}

#[test]
fn an_insert_with_an_older_seq_never_hides_a_newer_entry() {
    let o = tiny(4);
    o.insert_write(r(10, 4), loc(8192), WriteSeq(9));
    o.insert_write(r(6, 12), loc(0x10000), WriteSeq(5)); // late, older
    o.validate().unwrap();
    let pieces = coalesce(&look(&o, 6, 12));
    let seqs: Vec<(u64, u64, u64)> = pieces
        .iter()
        .map(|p| (p.range.lba, p.range.blocks, p.write_seq.0))
        .collect();
    assert_eq!(seqs, vec![(6, 4, 5), (10, 4, 9), (14, 4, 5)]);
    // The right-hand part of the older write has its location advanced.
    assert_eq!(
        pieces[2].source,
        PieceSource::Journal(loc(0x10000 + 8 * 4096))
    );
    // The shadowed part of the older write stays as a version: 12 + 4 blocks.
    assert_eq!(o.dirty_bytes(), 16 * 4096);
}

#[test]
fn a_sequential_gigabyte_is_few_intervals() {
    // 1 GiB = 262 144 blocks, one record's range (or contiguous inserts of one
    // record): default options, 64 KiB chunks.
    let o = ShardedOverlay::new(16);
    o.insert_write(r(0, 262_144), loc(4096), WriteSeq(1));
    assert!(o.entries() <= 1000, "{} intervals", o.entries());
    o.validate().unwrap();
    assert_eq!(o.dirty_bytes(), 1 << 30);
    // Same record inserted in 1 MiB pieces with contiguous locations: merges too.
    let o = ShardedOverlay::new(16);
    for i in 0..1024u64 {
        o.insert_write(r(i * 256, 256), loc(4096 + i * 256 * 4096), WriteSeq(1));
    }
    assert!(o.entries() <= 1000, "{} intervals", o.entries());
    o.validate().unwrap();
}

#[test]
fn holes_merge_and_cut_splits_at_chunks() {
    let o = ShardedOverlay::new(16);
    o.insert_hole(r(10, 40), WriteSeq(3));
    assert_eq!(o.entries(), 1);
    let cut = o.cut(WriteSeq(3));
    let shape: Vec<(u64, Vec<(u64, u64)>)> = cut
        .chunks
        .iter()
        .map(|c| {
            (
                c.chunk,
                c.pieces
                    .iter()
                    .map(|p| (p.range.lba, p.range.blocks))
                    .collect(),
            )
        })
        .collect();
    assert_eq!(
        shape,
        vec![
            (0, vec![(10, 6)]),
            (1, vec![(16, 16)]),
            (2, vec![(32, 16)]),
            (3, vec![(48, 2)])
        ]
    );
}

#[test]
fn the_dirty_cap_asks_for_a_seal() {
    let o = ShardedOverlay::with_options(OverlayOptions {
        dirty_cap_bytes: 8 * 4096,
        ..OverlayOptions::new(4)
    })
    .unwrap();
    o.insert_write(r(0, 7), loc(4096), WriteSeq(1));
    assert!(!o.over_cap());
    o.insert_write(r(100, 1), loc(1 << 20), WriteSeq(2));
    assert!(o.over_cap());
    let cut = o.cut(WriteSeq(2));
    o.release(&cut);
    assert!(!o.over_cap());
    assert_eq!(o.dirty_bytes(), 0);
    assert_eq!(o.entries(), 0);
}

#[test]
fn zero_blocks_and_bad_options() {
    let o = ShardedOverlay::new(4);
    o.insert_write(r(5, 0), loc(4096), WriteSeq(1));
    assert_eq!(o.dirty_bytes(), 0);
    let mut v = vec![look(&o, 0, 1)[0]];
    o.lookup(r(5, 0), &mut v);
    assert!(v.is_empty());
    assert!(ShardedOverlay::with_options(OverlayOptions {
        shards: 0,
        ..OverlayOptions::new(4)
    })
    .is_err());
    assert!(ShardedOverlay::with_options(OverlayOptions {
        stripe_chunks: u64::MAX,
        ..OverlayOptions::new(4)
    })
    .is_err());
}

#[test]
fn the_top_of_the_address_space_works() {
    let o = tiny(4);
    let lba = u64::MAX - 8;
    o.insert_hole(r(lba, 8), WriteSeq(1));
    assert_eq!(coalesce(&look(&o, lba, 8)).len(), 1);
    o.validate().unwrap();
    let cut = o.cut(WriteSeq(1));
    assert_eq!(cut.chunks.last().unwrap().chunk, (u64::MAX - 1) / 4);
}

/// The lead's scenario: X written at 5, acknowledged by a FLUSH at 8,
/// overwritten at 12; `cut(10)` must still contain X at seq 5.
#[test]
fn a_cut_keeps_the_version_that_a_later_overwrite_shadows() {
    for o in [ShardedOverlay::new(4), tiny(4)] {
        o.insert_write(r(2, 6), loc(4096), WriteSeq(5));
        o.insert_write(r(4, 2), loc(1 << 20), WriteSeq(12));
        // Lookup sees the newest.
        let p = coalesce(&look(&o, 2, 6));
        assert_eq!(p[1].write_seq, WriteSeq(12));
        o.validate().unwrap();
        assert_eq!(o.dirty_bytes(), 8 * 4096);
        let cut = o.cut(WriteSeq(10));
        let all: Vec<Piece> = cut.chunks.iter().flat_map(|c| c.pieces.clone()).collect();
        // Blocks 2..8 all come from seq 5, also 4 and 5.
        assert!(all.iter().all(|p| p.write_seq == WriteSeq(5)));
        let blocks: u64 = all.iter().map(|p| p.range.blocks).sum();
        assert_eq!(blocks, 6);
        // Release of that cut keeps the newer overwrite only.
        o.release(&cut);
        o.validate().unwrap();
        assert_eq!(o.dirty_bytes(), 2 * 4096);
        let p = look(&o, 4, 2);
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].write_seq, WriteSeq(12));
        // A cut at 12 then holds the overwrite.
        let cut = o.cut(WriteSeq(12));
        assert_eq!(cut.chunks.iter().map(|c| c.pieces.len()).sum::<usize>(), 1);
    }
}

#[test]
fn deep_version_stacks_cut_and_release_per_seq() {
    let o = tiny(4);
    // Ten overwrites of the same 3 blocks, interleaved arrival order.
    for seq in [3u64, 9, 1, 7, 5, 10, 2, 8, 4, 6] {
        o.insert_write(r(6, 3), loc(seq << 20), WriteSeq(seq));
    }
    o.validate().unwrap();
    assert_eq!(o.dirty_bytes(), 30 * 4096);
    for c in 1..=10u64 {
        let cut = o.cut(WriteSeq(c));
        let all: Vec<Piece> = cut.chunks.iter().flat_map(|c| c.pieces.clone()).collect();
        assert_eq!(all.iter().map(|p| p.range.blocks).sum::<u64>(), 3);
        assert!(all.iter().all(|p| p.write_seq == WriteSeq(c)), "cut {c}");
    }
    // Release at 4 drops versions 1..=4 (12 blocks); 6 versions remain.
    let cut = o.cut(WriteSeq(4));
    o.release(&cut);
    o.validate().unwrap();
    assert_eq!(o.dirty_bytes(), 18 * 4096);
    assert_eq!(look(&o, 6, 3)[0].write_seq, WriteSeq(10));
    let cut = o.cut(WriteSeq(7));
    assert!(cut
        .chunks
        .iter()
        .flat_map(|c| &c.pieces)
        .all(|p| p.write_seq == WriteSeq(7)));
    o.release(&o.cut(WriteSeq(10)));
    assert_eq!(o.dirty_bytes(), 0);
    assert_eq!(o.entries(), 0);
}

#[test]
fn versions_count_towards_the_dirty_cap() {
    let o = ShardedOverlay::with_options(OverlayOptions {
        dirty_cap_bytes: 8 * 4096,
        ..OverlayOptions::new(4)
    })
    .unwrap();
    for seq in 1..=7u64 {
        o.insert_write(r(0, 1), loc(seq << 20), WriteSeq(seq));
    }
    assert!(!o.over_cap());
    o.insert_write(r(0, 1), loc(8 << 20), WriteSeq(8));
    assert!(o.over_cap(), "one block rewritten 8 times is 8 versions");
}
