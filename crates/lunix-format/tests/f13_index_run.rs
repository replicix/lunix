//! Freeze item 13: index runs.

mod common;

use common::golden;
use lunix_format::index_run::*;
use lunix_format::{ChunkId, FormatError, ObjId};
use proptest::prelude::*;

fn seg(n: u64) -> ObjId {
    ObjId::new(0, n).unwrap()
}

fn entry(i: u64) -> IndexEntry {
    let mut id = [0u8; 32];
    id[..8].copy_from_slice(&(i * 7 + 1).to_be_bytes());
    id[8..16].copy_from_slice(&i.to_le_bytes());
    IndexEntry {
        chunk_id: ChunkId(id),
        seg: seg(1 + i % 3),
        off: (i * 4000) as u32,
        clen: 1000 + i as u32,
    }
}

fn run(n: u64, covered: &[u64]) -> IndexRun {
    IndexRun::seal(
        seg(9),
        (0..n).map(entry).collect(),
        covered.iter().map(|&c| seg(c)).collect(),
        common::blake3_256,
    )
    .unwrap()
}

fn rt(b: &[u8]) -> Result<Vec<u8>, FormatError> {
    let r = IndexRun::decode(b)?;
    let mut v = Vec::new();
    r.encode(&mut v)?;
    Ok(v)
}

fn enc(r: &IndexRun) -> Vec<u8> {
    let mut v = Vec::new();
    r.encode(&mut v).unwrap();
    v
}

#[test]
fn one_block_run() {
    let r = run(1, &[1]);
    let v = enc(&r);
    assert_eq!(v.len(), 4096 + 32 + 8 + 64);
    let g = golden("13-index-run-one-block.bin", &v);
    common::negative_suite("one block", &rt, &g);
    let d = IndexRun::decode(&g).unwrap();
    assert_eq!(d, r);
    assert_eq!(
        common::blake3_256(IndexRun::hash_input(&g).unwrap()),
        d.run_hash
    );
    // trailer fields at their documented offsets
    let t = &g[g.len() - 64..];
    assert_eq!(&t[60..], b"LXIR");
    assert_eq!(t[56], 1);
    assert_eq!(&t[16..20], &1u32.to_le_bytes());
}

#[test]
fn three_block_run() {
    let r = run(85 + 85 + 10, &[1, 2, 5]);
    let v = enc(&r);
    assert_eq!(v.len(), 3 * 4096 + 3 * 32 + 3 * 8 + 64);
    let g = golden("13-index-run-three-blocks.bin", &v);
    common::negative_suite("three blocks", &rt, &g);
    let d = IndexRun::decode(&g).unwrap();
    assert_eq!(d, r);
    assert_eq!(d.entries.len(), 180);
    assert_eq!(&g[2..4], &[0, 0]);
    assert_eq!(u16::from_le_bytes([g[0], g[1]]), 85);
    assert_eq!(u16::from_le_bytes([g[8192], g[8193]]), 10);
}

#[test]
fn structural_rejections() {
    let g = enc(&run(100, &[1, 2]));
    let f = &rt;
    assert!(f(&g).is_ok());
    let fix_block = |mut b: Vec<u8>, blk: usize| {
        let s = blk * 4096;
        let c = lunix_format::bytes::crc32c(&b[s..s + 4092]);
        b[s + 4092..s + 4096].copy_from_slice(&c.to_le_bytes());
        b
    };
    // non-final block short
    let mut b = g.clone();
    b[0] = 84;
    common::assert_rejects("short first block", f, &fix_block(b, 0));
    // reserved bytes in a block
    let mut b = g.clone();
    b[2] = 1;
    common::assert_rejects("block reserved", f, &fix_block(b, 0));
    let mut b = g.clone();
    b[4086] = 1;
    common::assert_rejects("block tail reserved", f, &fix_block(b, 0));
    // unused entry not zero
    let mut b = g.clone();
    let off = 4096 + 4 + 20 * 48;
    b[off] = 1;
    common::assert_rejects("unused entry", f, &fix_block(b, 1));
    // chunk ids out of order: swap two ids' first byte
    let mut b = g.clone();
    b[4 + 48] = 0xff;
    common::assert_rejects("order", f, &fix_block(b, 0));
    // crc
    let mut b = g.clone();
    b[100] ^= 1;
    common::assert_rejects("crc", f, &b);
    // trailer: version, reserved, magic, counts
    let n = g.len();
    for (name, idx) in [
        ("ver", n - 8),
        ("reserved", n - 6),
        ("magic", n - 1),
        ("entries", n - 64),
        ("nblocks", n - 48),
        ("ncovered", n - 44),
    ] {
        let mut b = g.clone();
        b[idx] ^= 1;
        common::assert_rejects(name, f, &b);
    }
    // block index mismatch
    let mut b = g.clone();
    b[2 * 4096] ^= 1;
    common::assert_rejects("block index", f, &b);
    // covered not ascending: swap the two covered ids
    let mut b = g.clone();
    let c = n - 64 - 16;
    let (x, y) = (b[c..c + 8].to_vec(), b[c + 8..c + 16].to_vec());
    b[c..c + 8].copy_from_slice(&y);
    b[c + 8..c + 16].copy_from_slice(&x);
    common::assert_rejects("covered order", f, &b);
    // zero object ids
    let mut b = g.clone();
    b[n - 64 - 16..n - 64 - 8].fill(0);
    common::assert_rejects("covered zero", f, &b);
    // encode-side rules
    let dup = IndexRun::seal(seg(1), vec![entry(0), entry(0)], vec![], common::blake3_256);
    assert!(dup.is_err());
    assert!(IndexRun::seal(seg(1), vec![], vec![], common::blake3_256).is_err());
    assert!(IndexRun::seal(
        seg(1),
        vec![entry(0)],
        vec![seg(2), seg(2)],
        common::blake3_256
    )
    .is_err());
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(common::CASES))]

    #[test]
    fn round_trip(
        ids in proptest::collection::btree_set(any::<[u8; 32]>(), 1..200),
        covered in proptest::collection::btree_set(1u64..1000, 0..10),
        run_id in 1u64..1000,
        seg_n in 1u64..100,
        off in any::<u32>(),
        clen in any::<u32>(),
    ) {
        let entries: Vec<IndexEntry> = ids.into_iter().map(|id| IndexEntry {
            chunk_id: ChunkId(id), seg: seg(seg_n), off, clen,
        }).collect();
        let r = IndexRun::seal(seg(run_id), entries.clone(), covered.iter().map(|&c| seg(c)).collect(), common::blake3_256).unwrap();
        let v = enc(&r);
        prop_assert_eq!(v.len(), entries.len().div_ceil(85) * (4096 + 32) + covered.len() * 8 + 64);
        let d = IndexRun::decode(&v).unwrap();
        prop_assert_eq!(&d, &r);
        prop_assert_eq!(common::blake3_256(IndexRun::hash_input(&v).unwrap()), d.run_hash);
    }
}
