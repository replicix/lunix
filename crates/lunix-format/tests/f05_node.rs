//! Freeze item 5: map node (leaf rb16, interior, hashes, tree shape).
mod common;

use common::{blake3_256, golden, golden_text, hex, negative_suite};
use lunix_format::node::*;
use lunix_format::{FormatError, IdLen, IdPrefix, LineageIdx, ObjId};
use proptest::prelude::*;

fn params(fanout: u16, id_len: IdLen, ovh: u32) -> LeafParams {
    LeafParams {
        fanout,
        id_len,
        ovh,
    }
}

fn p512() -> LeafParams {
    params(512, IdLen::Sixteen, 68)
}

fn prefix(len: IdLen, seed: u8) -> IdPrefix {
    let b: Vec<u8> = (0..len.len()).map(|i| seed.wrapping_add(i as u8)).collect();
    IdPrefix::from_slice(len, &b).unwrap()
}

fn data(seg: u64, off: u32, clen: u32, len: IdLen, seed: u8) -> Entry {
    Entry::Data(DataEntry {
        lineage: LineageIdx(0),
        seg: ObjId::from_raw(seg).unwrap(),
        off,
        clen,
        prefix: prefix(len, seed),
    })
}

fn enc(l: &Leaf, p: &LeafParams) -> Vec<u8> {
    let mut v = Vec::new();
    l.encode(p, &mut v).unwrap();
    v
}

fn leaf_rt(p: LeafParams) -> impl Fn(&[u8]) -> Result<Vec<u8>, FormatError> {
    move |b| {
        let l = Leaf::decode(b, &p)?;
        let mut v = Vec::with_capacity(b.len());
        l.encode(&p, &mut v)?;
        Ok(v)
    }
}

fn interior_rt(fanout: u16) -> impl Fn(&[u8]) -> Result<Vec<u8>, FormatError> {
    move |b| {
        let n = Interior::decode(b, fanout)?;
        let mut v = Vec::with_capacity(b.len());
        n.encode(&mut v)?;
        Ok(v)
    }
}

const E0_512: &str = "8cb313b371458a0bbc13829da36f70d69deb2652ae2c1b0f6318004fd1419d5e";
const LEAF_HASH: &str = "dca03248122f117b77f7f5aeefbdd4867e0b4c958a051c8a245251324ef3b573";
const ROOT_HASH: &str = "d126ea2c8ccdc011f08eeb71db94c59e659f9aba0229c8ea07bed39c2dfb5d0a";

fn a0_leaf() -> Leaf {
    let mut l = Leaf::empty(512);
    let id = hex("8f6cf4e5e665b21ac12bd316397d883c");
    l.entries[0] = Entry::Data(DataEntry {
        lineage: LineageIdx(0),
        seg: ObjId::from_raw(1).unwrap(),
        off: 0,
        clen: 75,
        prefix: IdPrefix::from_slice(IdLen::Sixteen, &id).unwrap(),
    });
    l
}

const A0_LEAF_HEX: &str = "000002 0200 0100 0100 01 00 00 07 00 0100000000000000 00 0002104000 4b \
                           8f6cf4e5e665b21ac12bd316397d883c";

#[test]
fn a0_leaf_one_entry() {
    let l = a0_leaf();
    let bytes = enc(&l, &p512());
    assert_eq!(bytes, hex(A0_LEAF_HEX));
    assert_eq!(bytes.len(), 45);
    let g = golden("a0-node-leaf-one-entry.bin", &bytes);
    assert_eq!(hex(LEAF_HASH), blake3_256(leaf_hash_input(&g)));
    negative_suite("a0 leaf", &leaf_rt(p512()), &g);
    assert_eq!(Leaf::decode(&g, &p512()).unwrap(), l);
}

#[test]
fn empty_leaves() {
    let e512 = empty_leaf_bytes(512).unwrap();
    assert_eq!(
        e512,
        hex("00 00 02 01 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00")
    );
    let e256 = empty_leaf_bytes(256).unwrap();
    assert_eq!(
        e256,
        hex("00 00 01 01 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00")
    );
    let g512 = golden("05-node-leaf-empty-512.bin", &e512);
    let g256 = golden("05-node-leaf-empty-256.bin", &e256);
    negative_suite(
        "empty 512",
        &leaf_rt(params(512, IdLen::Sixteen, 68)),
        &g512,
    );
    negative_suite(
        "empty 256",
        &leaf_rt(params(256, IdLen::ThirtyTwo, 108)),
        &g256,
    );
    assert!(Leaf::decode(&g512, &p512()).unwrap().is_empty());
}

fn full_leaf() -> Leaf {
    let id_len = IdLen::ThirtyTwo;
    let mut entries = Vec::new();
    let mut seed = 0u8;
    // Runs of varied length over five segments, with holes in between.
    let plan: [(u64, usize); 14] = [
        (9, 40),
        (0, 7),
        (3, 60),
        (9, 1),
        (0, 100),
        (7, 33),
        (7, 1),
        (0, 2),
        (1_099_511_627_777, 120),
        (3, 15),
        (0, 30),
        (12, 64),
        (0, 3),
        (5, 36),
    ];
    for (seg, n) in plan {
        let mut off = (seed as u32) * 1000;
        for _ in 0..n {
            if entries.len() == 512 {
                break;
            }
            if seg == 0 {
                entries.push(Entry::Hole);
            } else {
                let clen = 100 + (seed as u32 % 17) * 311;
                entries.push(data(seg, off, clen, id_len, seed));
                off += 108 + clen;
                seed = seed.wrapping_add(1);
            }
        }
    }
    while entries.len() < 512 {
        entries.push(Entry::Hole);
    }
    Leaf { entries }
}

#[test]
fn full_leaf_golden_and_lookup() {
    let p = params(512, IdLen::ThirtyTwo, 108);
    let l = full_leaf();
    let bytes = enc(&l, &p);
    let g = golden("05-node-leaf-full-512.bin", &bytes);
    negative_suite("full leaf", &leaf_rt(p), &g);
    assert_eq!(Leaf::decode(&g, &p).unwrap(), l);
    let v = LeafView::parse(&g, &p).unwrap();
    for (i, e) in l.entries.iter().enumerate() {
        assert_eq!(&v.get(i).unwrap(), e, "entry {i}");
    }
    assert!(v.get(512).is_err());
}

#[test]
fn leaf_edge_cases() {
    let rt = |p: LeafParams, l: &Leaf| {
        let b = enc(l, &p);
        assert_eq!(&Leaf::decode(&b, &p).unwrap(), l);
        b
    };
    // One data entry in the last slot.
    let mut l = Leaf::empty(512);
    l.entries[511] = data(2, 4_000_000_000, 70_000, IdLen::Sixteen, 1);
    let b = rt(p512(), &l);
    assert_eq!(
        LeafView::parse(&b, &p512()).unwrap().get(511).unwrap(),
        l.entries[511]
    );
    // Zero-length ids (lean map).
    let p0 = params(256, IdLen::Zero, 68);
    let mut l = Leaf::empty(256);
    l.entries[3] = data(5, 0, 10, IdLen::Zero, 0);
    l.entries[4] = data(5, 78, 10, IdLen::Zero, 0);
    let b = rt(p0, &l);
    // 512 distinct segments, max-width offsets.
    let p = params(512, IdLen::ThirtyTwo, 108);
    let entries: Vec<Entry> = (0..512u64)
        .map(|i| {
            data(
                1 + i * 7_000_000_000,
                u32::MAX - 100_000 + i as u32,
                1_048_576,
                IdLen::ThirtyTwo,
                i as u8,
            )
        })
        .collect();
    let l = Leaf { entries };
    let b2 = rt(p, &l);
    let v = LeafView::parse(&b2, &p).unwrap();
    for i in [0usize, 1, 17, 255, 511] {
        assert_eq!(v.get(i).unwrap(), l.entries[i]);
    }
    // Adjacent same-segment entries with contiguous offsets form one run, and
    // a contiguity of 16+ entries exercises the cum column.
    let mut l = Leaf::empty(512);
    let mut off = 12u32;
    for i in 0..50 {
        l.entries[i] = data(4, off, 200 + i as u32, IdLen::Sixteen, i as u8);
        off += 68 + 200 + i as u32;
    }
    rt(p512(), &l);
    let _ = b;
}

#[test]
fn leaf_rejections() {
    let p = p512();
    let good = hex(A0_LEAF_HEX);
    assert!(Leaf::decode(&good, &p).is_ok());
    // wrong fanout / id_len / level
    assert!(Leaf::decode(&good, &params(256, IdLen::Sixteen, 68)).is_err());
    assert!(Leaf::decode(&good, &params(512, IdLen::ThirtyTwo, 68)).is_err());
    let mut b = good.clone();
    b[0] = 1;
    assert!(Leaf::decode(&b, &p).is_err());
    // non-minimal width
    let mut b = good.clone();
    b[12] = 8;
    assert!(Leaf::decode(&b, &p).is_err());
    // non-zero padding bit in the clen column (wc = 7)
    let mut b = good.clone();
    b[28] |= 0x80;
    assert!(Leaf::decode(&b, &p).is_err());
    // trailing byte
    let mut b = good.clone();
    b.push(0);
    assert!(matches!(
        Leaf::decode(&b, &p),
        Err(FormatError::Trailing { .. })
    ));
    // non-zero lineage: encoder refuses, hidden codec round trips, decoder refuses
    let mut l = a0_leaf();
    if let Entry::Data(d) = &mut l.entries[0] {
        d.lineage = LineageIdx(2);
    }
    let mut v = Vec::new();
    assert!(l.encode(&p, &mut v).is_err());
    l.encode_allow_lineage(&p, &mut v).unwrap();
    assert!(Leaf::decode(&v, &p).is_err());
    assert_eq!(Leaf::decode_allow_lineage(&v, &p).unwrap(), l);
    // clen 0 and over 1 MiB, wrong prefix length
    let mut l = a0_leaf();
    if let Entry::Data(d) = &mut l.entries[0] {
        d.clen = 0;
    }
    assert!(l.encode(&p, &mut Vec::new()).is_err());
    let l = Leaf {
        entries: vec![Entry::Hole; 3],
    };
    assert!(l.encode(&p, &mut Vec::new()).is_err());
    assert!(Leaf::decode(&good, &params(512, IdLen::Sixteen, 70)).is_err());
}

#[test]
fn interior_root_golden() {
    let e0: [u8; 32] = hex(E0_512).try_into().unwrap();
    let leaf_hash: [u8; 32] = hex(LEAF_HASH).try_into().unwrap();
    let mut children = vec![
        Child {
            hash: e0,
            loc: None
        };
        512
    ];
    children[0] = Child {
        hash: leaf_hash,
        loc: Some(NodeLoc {
            pack: ObjId::from_raw(2).unwrap(),
            off: 0,
            len: 113,
        }),
    };
    let n = Interior { level: 1, children };
    let mut bytes = Vec::new();
    n.encode(&mut bytes).unwrap();
    assert_eq!(bytes.len(), 24_579);
    assert_eq!(&bytes[..3], &hex("01 00 02")[..]);
    assert_eq!(
        &bytes[3 + 32..3 + 48],
        &hex("02000000000000000000000071000000")[..]
    );
    let g = golden("05-node-interior-root.bin", &bytes);
    assert_eq!(
        blake3_256(&n.hash_input().unwrap()),
        <[u8; 32]>::try_from(hex(ROOT_HASH)).unwrap()
    );
    assert_eq!(Interior::decode(&g, 512).unwrap(), n);
    negative_suite("interior root", &interior_rt(512), &g);
    // Reject 256 for a 512 node, and a leaf header.
    assert!(Interior::decode(&g, 256).is_err());

    // lineage_idx != 0 is rejected in v1.
    let mut bad = g.clone();
    bad[3 + 47] = 1;
    let gb = golden("05-node-interior-lineage-nonzero.bin", &bad);
    assert!(Interior::decode(&gb, 512).is_err());
    // empty child with a location, located child with len 0 or too long
    let mut bad = g.clone();
    bad[3 + 48 + 40] = 1; // child 1: pack 0, off 1
    assert!(Interior::decode(&bad, 512).is_err());
    let mut bad = g.clone();
    bad[3 + 44..3 + 47].copy_from_slice(&[0, 0, 0]);
    assert!(Interior::decode(&bad, 512).is_err());
    let mut bad = g.clone();
    bad[3 + 44..3 + 47].copy_from_slice(&[0x6d, 0x01, 0x01]); // 65 901 > 65 644
    assert!(Interior::decode(&bad, 512).is_err());
    // level 0 is not interior
    let mut bad = g.clone();
    bad[0] = 0;
    assert!(Interior::decode(&bad, 512).is_err());
    // encode refuses lineage-free invariants too
    let mut n2 = n.clone();
    n2.children[0].loc = Some(NodeLoc {
        pack: ObjId::from_raw(2).unwrap(),
        off: 0,
        len: 70_000,
    });
    assert!(n2.encode(&mut Vec::new()).is_err());
}

const EMPTY_TABLE: [[&str; 2]; 5] = [
    [
        "8cb313b371458a0bbc13829da36f70d69deb2652ae2c1b0f6318004fd1419d5e",
        "b66d86b2736d18b9419f57f2dd03958a9cbe6a6057c0803b5fbd43cd1e04d91a",
    ],
    [
        "10c152fbfaaab0630db8b786eebf559cbd7b38cb964a20465d68e84412d61317",
        "bb20922a988899f0a8f2929846bc35dee6dd0210ccf6b6157237b80e17abc8e3",
    ],
    [
        "e76498965d1098049475b7c10a1fbce7f7e15aab71d961f209a77873b7b3e3e8",
        "be662761db8a4a4ee24f872b9d38e2d46158c70a75c55874882da98377a05a9b",
    ],
    [
        "23cd677335c394fd2a4ebd98a7cf7468ef67034fbda6ea526908df597c187110",
        "51d932c23141d31f40276976932bb281a64d2669f6743084f48628922c53b772",
    ],
    [
        "a0a104e5eceb69a6e727686306eb645079756ac921fb419d5a9ea12e3bef7370",
        "47f726654ae37276df5bc4c45df72e3d44022357b0d81325c70c0bd1bde11d0c",
    ],
];

#[test]
fn empty_hashes_match_the_doc() {
    let mut text = String::new();
    for (col, fanout) in [(0usize, 512u16), (1, 256)] {
        let hs = empty_hashes(fanout, 4, blake3_256).unwrap();
        assert_eq!(hs.len(), 5);
        for (l, h) in hs.iter().enumerate() {
            assert_eq!(
                common::to_hex(h),
                EMPTY_TABLE[l][col],
                "E{l} fanout {fanout}"
            );
            text.push_str(&format!("fanout {fanout} E{l} {}\n", common::to_hex(h)));
        }
    }
    text.push_str(&format!(
        "leaf hash (23.5) {LEAF_HASH}\nroot hash (23.5) {ROOT_HASH}\n"
    ));
    golden_text("05-empty-hashes.txt", &text);
    assert!(empty_hashes(512, 16, blake3_256).is_err());
}

#[test]
fn tree_shape() {
    // 1 GiB at 16 KiB: 65 536 chunks, 2 levels.
    assert_eq!(levels_for(1 << 30, 16384, 512).unwrap(), 2);
    // 8 TiB at 64 KiB, fanout 512: 2^27 chunks, 3 levels.
    assert_eq!(levels_for(8 << 40, 65536, 512).unwrap(), 3);
    assert_eq!(levels_for(4096, 16384, 256).unwrap(), 2);
    // exact powers: 256^2 chunks need 2 levels, one more chunk needs 3.
    assert_eq!(levels_for(65_536 * 16384, 16384, 256).unwrap(), 2);
    assert_eq!(levels_for(65_537 * 16384, 16384, 256).unwrap(), 3);
    // the doc's bound: size 2^56 at 16 KiB, fanout 256 -> 6
    assert_eq!(levels_for(1 << 56, 16384, 256).unwrap(), 6);
    assert!(levels_for(1, 0, 256).is_err());
    assert!(levels_for(1, 4096, 100).is_err());
    assert_eq!(leaf_index(1000, 512), 1);
    assert_eq!(leaf_slot(1000, 512), 488);
    assert_eq!(child_slot(0x1_0003, 1, 256).unwrap(), 3);
    assert_eq!(child_slot(0x1_0003, 2, 256).unwrap(), 0);
    assert_eq!(child_slot(0x1_0003, 3, 256).unwrap(), 1);
    assert_eq!(child_slot(5, 12, 512).unwrap(), 0);
    assert!(child_slot(0, 0, 512).is_err());
}

// ---------------------------------------------------------------------------
// Property tests.

fn random_leaf(seed: u64) -> (LeafParams, Leaf) {
    let mut r = common::Rng(seed | 1);
    let fanout = if r.below(2) == 0 { 256u16 } else { 512 };
    let id_len = [IdLen::Zero, IdLen::Sixteen, IdLen::ThirtyTwo][r.below(3)];
    let ovh = if r.below(2) == 0 { 68 } else { 108 };
    let p = params(fanout, id_len, ovh);
    let nsegs = 1 + r.below(6);
    let segs: Vec<u64> = (0..nsegs)
        .map(|_| match r.below(3) {
            0 => 1 + r.below(50) as u64,
            1 => ((1 + r.below(1000) as u64) << 40) | (1 + r.below(100) as u64),
            _ => 1 + r.next() % ((1 << 40) - 1),
        })
        .collect();
    let mut entries: Vec<Entry> = Vec::new();
    let mut seed_b = 0u8;
    while entries.len() < usize::from(fanout) {
        let big = r.below(4) == 0;
        let n = 1 + r.below(if big { 200 } else { 20 });
        if r.below(3) == 0 {
            for _ in 0..n {
                entries.push(Entry::Hole);
            }
        } else {
            let seg = segs[r.below(nsegs)];
            let mut off = match r.below(3) {
                0 => 0,
                1 => r.below(1 << 20) as u32,
                _ => (r.next() % 3_000_000_000) as u32,
            };
            for _ in 0..n {
                let clen = match r.below(3) {
                    0 => 1 + r.below(4096) as u32,
                    1 => 1 + r.below(70_000) as u32,
                    _ => 1 + r.below(1 << 20) as u32,
                };
                entries.push(data(seg, off, clen, id_len, seed_b));
                seed_b = seed_b.wrapping_add(3);
                off = off.saturating_add(ovh + clen);
                if u64::from(off) + u64::from(ovh) + 1_048_576 > u64::from(u32::MAX) {
                    off = 0;
                }
            }
        }
    }
    entries.truncate(usize::from(fanout));
    (p, Leaf { entries })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(common::CASES))]

    #[test]
    fn leaf_round_trip_and_lookup(seed in any::<u64>()) {
        let (p, l) = random_leaf(seed);
        let mut b = Vec::new();
        l.encode(&p, &mut b).unwrap();
        prop_assert!(b.len() <= MAX_NODE_LEN);
        prop_assert_eq!(&Leaf::decode(&b, &p).unwrap(), &l);
        let v = LeafView::parse(&b, &p).unwrap();
        for i in (0..l.entries.len()).step_by(7).chain([l.entries.len() - 1]) {
            prop_assert_eq!(&v.get(i).unwrap(), &l.entries[i]);
        }
    }

    #[test]
    fn interior_round_trip(seed in any::<u64>()) {
        let mut r = common::Rng(seed | 1);
        let fanout = if r.below(2) == 0 { 256usize } else { 512 };
        let level = 1 + r.below(15) as u8;
        let children: Vec<Child> = (0..fanout).map(|_| {
            let mut hash = [0u8; 32];
            for b in &mut hash { *b = r.next() as u8; }
            let loc = if r.below(3) == 0 { None } else {
                Some(NodeLoc {
                    pack: ObjId::from_raw(1 + r.next() % ((1 << 62) - 1)).unwrap(),
                    off: r.next() as u32,
                    len: 1 + r.below(65_644) as u32,
                })
            };
            Child { hash, loc }
        }).collect();
        let n = Interior { level, children };
        let mut b = Vec::new();
        n.encode(&mut b).unwrap();
        prop_assert_eq!(b.len(), 3 + 48 * fanout);
        prop_assert_eq!(Interior::decode(&b, fanout as u16).unwrap(), n);
    }
}

#[test]
fn nonzero_lineage_round_trips_in_the_hidden_codec() {
    for seed in 0..300u64 {
        let (p, mut l) = random_leaf(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15));
        let mut i = 0u8;
        for e in &mut l.entries {
            if let Entry::Data(d) = e {
                d.lineage = LineageIdx(i % 3);
                if d.seg.counter() % 2 == 0 {
                    d.lineage = LineageIdx(2);
                }
                i = i.wrapping_add(1);
            }
        }
        // Runs split by lineage; the dictionary pairs stay canonical.
        let mut b = Vec::new();
        l.encode_allow_lineage(&p, &mut b).unwrap();
        assert_eq!(Leaf::decode_allow_lineage(&b, &p).unwrap(), l);
    }
}
