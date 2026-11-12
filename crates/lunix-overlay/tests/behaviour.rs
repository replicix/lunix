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
    assert_eq!(o.dirty_bytes(), 12 * 4096);
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
