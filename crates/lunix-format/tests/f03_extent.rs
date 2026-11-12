//! Freeze item 3: extent layouts, AAD, key-derivation `info`, tag input.

mod common;

use common::{golden, hex};
use lunix_format::extent::*;
use lunix_format::purpose::Purpose;
use lunix_format::{ChunkId, Codec, DekGen, ExtentKind, FamilyId, FormatError, IdLen, Level};
use proptest::prelude::*;

const CHUNK_ID_B: &str = "8f6cf4e5e665b21ac12bd316397d883cb284a14b4999f9ee99301971d1b55ede";
/// The unkeyed id of the same chunk on a plain-mode volume (section 23.2).
const CHUNK_ID_PLAIN: &str = "c6c4890119d834da404b94c707aa46a579d77f122211d145982c8ceb83e7f566";
const KEYED_TAG: &str = "4e700344dd5d9ea60a34ba09c8a5f115";
const UNKEYED_TAG: &str = "f21b8c973dcefd5e68954f1c2261587a";
const LEAF_HASH: &str = "dca03248122f117b77f7f5aeefbdd4867e0b4c958a051c8a245251324ef3b573";
const LEAF_TAG: &str = "26ff3ed4285b58271c9db42e836fb5b8";
const TAG_KEY_B: &str = "8f5a8aa5bd1c61e6d9b3e275436f35de2673144fc8d12cb7aab4b39398b09322";

fn params(layout: ExtentLayout, kind: ExtentKind, chunk_size: u32) -> DecodeParams {
    DecodeParams {
        layout,
        kind,
        chunk_size,
        allow_nonzero_nonce: false,
    }
}

fn rt(p: DecodeParams) -> impl Fn(&[u8]) -> Result<Vec<u8>, FormatError> {
    move |b| {
        let e = Extent::decode(b, p)?;
        let mut v = Vec::new();
        e.encode(&mut v)?;
        Ok(v)
    }
}

/// The 75-byte LZ4 block of section 23.2.
fn lz4_block() -> Vec<u8> {
    let mut v = hex("1f410100");
    v.extend_from_slice(&[0xff; 64]);
    v.extend_from_slice(&hex("27 50 41 41 41 41 41"));
    v
}

fn plain_header() -> Cch1Header {
    Cch1Header {
        codec: Codec::Lz4,
        level: Level(0),
        ulen: 16384,
    }
}

fn tag_key_b() -> [u8; 32] {
    let msg = Purpose::ExtentTag
        .message(&FamilyId(common::VOLUME_B_FAMILY))
        .unwrap();
    common::purpose_key(&common::volume_b_ik(), &msg)
}

fn addressing_key_b() -> [u8; 32] {
    let msg = Purpose::IntegrityAddressing
        .message(&FamilyId(common::VOLUME_B_FAMILY))
        .unwrap();
    common::purpose_key(&common::volume_b_ik(), &msg)
}

fn doc_plain_untagged(id: &str) -> Vec<u8> {
    let mut v = hex("43434831 01 0200 00 0040000000000000");
    v.extend_from_slice(&hex(id));
    v.extend_from_slice(&hex("4b000000"));
    v.extend_from_slice(&lz4_block());
    assert_eq!(v.len(), 127);
    v
}

fn plain_extent<'a>(id: &str, payload: &'a [u8], tag: [u8; 16]) -> Extent<'a> {
    Extent {
        header: plain_header(),
        chunk_id: ChunkId(hex(id).try_into().unwrap()),
        crypto: None,
        payload,
        tag,
    }
}

#[test]
fn derived_tag_key_matches_the_doc() {
    assert_eq!(common::to_hex(&tag_key_b()), TAG_KEY_B);
}

#[test]
fn derived_addressing_key_matches_the_doc() {
    assert_eq!(
        common::to_hex(&addressing_key_b()),
        "3e596debe98cf9c82d88e17c63d875e3f16dc9334a2dbe870f4dd827679372b8"
    );
}

#[test]
fn a0_plain_keyed_extent() {
    let payload = lz4_block();
    let untagged = doc_plain_untagged(CHUNK_ID_B);
    let keyed: [u8; 16] = hex(KEYED_TAG).try_into().unwrap();
    // the doc's tag is the first 16 bytes of keyed BLAKE3 over extent[0..127]
    assert_eq!(common::blake3_keyed(&tag_key_b(), &untagged)[..16], keyed);
    let e = plain_extent(CHUNK_ID_B, &payload, keyed);
    let mut v = Vec::new();
    e.encode(&mut v).unwrap();
    let mut doc = untagged.clone();
    doc.extend_from_slice(&keyed);
    assert_eq!(v, doc);
    assert_eq!(v.len(), 143);
    assert_eq!(tag_input(&v).unwrap(), &untagged[..]);
    let g = golden("a0-extent-plain-keyed.bin", &v);
    let p = params(ExtentLayout::Unencrypted, ExtentKind::Data, 16384);
    common::negative_suite("plain extent", &rt(p), &g);
    let d = Extent::decode(&g, p).unwrap();
    assert_eq!(d, e);
    assert_eq!(d.len(), ExtentLayout::Unencrypted.ovh() as usize + 75);
    // the CCH1 object extent[0..16] || payload decodes with chunkfmt
    let mut obj = g[..16].to_vec();
    obj.extend_from_slice(d.payload);
    let chunk = replicix_chunkfmt::format::decode_object(&obj).unwrap();
    assert_eq!(chunk, vec![0x41u8; 16384]);
    // the id is keyed BLAKE3 under the integrity addressing key (0x0a)
    assert_eq!(
        common::blake3_keyed(&addressing_key_b(), &chunk),
        d.chunk_id.0
    );
    // header helpers
    assert_eq!(Cch1Header::parse(&g).unwrap(), plain_header());
    let (h, id, clen) = parse_plain_prefix(&g).unwrap();
    assert_eq!((h, id, clen), (plain_header(), d.chunk_id, 75));
}

#[test]
fn plain_unkeyed_extent() {
    let payload = lz4_block();
    let untagged = doc_plain_untagged(CHUNK_ID_PLAIN);
    let unkeyed: [u8; 16] = hex(UNKEYED_TAG).try_into().unwrap();
    assert_eq!(common::blake3_128(&untagged), unkeyed);
    let e = plain_extent(CHUNK_ID_PLAIN, &payload, unkeyed);
    let mut v = Vec::new();
    e.encode(&mut v).unwrap();
    let g = golden("03-extent-plain-unkeyed.bin", &v);
    common::negative_suite(
        "unkeyed extent",
        &rt(params(ExtentLayout::Unencrypted, ExtentKind::Data, 16384)),
        &g,
    );
}

#[test]
fn a0_encrypted_kat_extent() {
    let body = common::kat("body_hardened");
    assert_eq!(body.len(), 80);
    let e = Extent {
        header: Cch1Header {
            codec: Codec::Raw,
            level: Level(0),
            ulen: 64,
        },
        chunk_id: ChunkId(common::kat32("chunk_id")),
        crypto: Some(CryptoRegion {
            dek_gen: DekGen(7),
            salt: common::kat("salt").try_into().unwrap(),
            nonce: [0; 12],
        }),
        payload: &body[..64],
        tag: body[64..].try_into().unwrap(),
    };
    let mut v = Vec::new();
    e.encode(&mut v).unwrap();
    assert_eq!(v.len(), 172);
    assert_eq!(&v[..16], &hex("43434831 01 0000 00 4000000000000000")[..]);
    assert_eq!(&v[48..56], &hex("40000000 07000000")[..]);
    assert_eq!(&v[80..92], &[0u8; 12]);
    let g = golden("a0-extent-encrypted-kat.bin", &v);
    let p = params(ExtentLayout::Encrypted, ExtentKind::Data, 64);
    common::negative_suite("encrypted extent", &rt(p), &g);
    assert_eq!(Extent::decode(&g, p).unwrap(), e);
    assert_eq!(tag_input(&g).unwrap().len(), 156);
    // a non-zero nonce is refused in the default profile, accepted in FIPS
    let mut bad = g.clone();
    bad[85] = 1;
    assert!(Extent::decode(&bad, p).is_err());
    let fips = DecodeParams {
        allow_nonzero_nonce: true,
        ..p
    };
    assert_eq!(
        Extent::decode(&bad, fips).unwrap().crypto.unwrap().nonce[5],
        1
    );
    // the data rule: ulen must equal the chunk size
    assert!(Extent::decode(&g, params(ExtentLayout::Encrypted, ExtentKind::Data, 65536)).is_err());
}

#[test]
fn node_extent() {
    // the 45-byte leaf of section 23.5
    let leaf = hex(
        "00 0002 0200 0100 0100 01 00 00 07 00 0100000000000000 00 0002104000 4b 8f6cf4e5e665b21ac12bd316397d883c",
    );
    assert_eq!(leaf.len(), 45);
    assert_eq!(common::to_hex(&common::blake3_256(&leaf)), LEAF_HASH);
    let e = Extent {
        header: Cch1Header {
            codec: Codec::Raw,
            level: Level(0),
            ulen: 45,
        },
        chunk_id: ChunkId(hex(LEAF_HASH).try_into().unwrap()),
        crypto: None,
        payload: &leaf,
        tag: hex(LEAF_TAG).try_into().unwrap(),
    };
    let mut untagged = Vec::new();
    e.encode_untagged(&mut untagged).unwrap();
    assert_eq!(common::blake3_keyed(&tag_key_b(), &untagged)[..16], e.tag);
    let mut v = untagged;
    v.extend_from_slice(&e.tag);
    assert_eq!(v.len(), 113);
    let g = golden("03-extent-node.bin", &v);
    let p = params(ExtentLayout::Unencrypted, ExtentKind::Node, 65536);
    common::negative_suite("node extent", &rt(p), &g);
    assert_eq!(Extent::decode(&g, p).unwrap(), e);
    // a data extent must have ulen == chunk size, so the same bytes fail as data
    assert!(Extent::decode(
        &g,
        params(ExtentLayout::Unencrypted, ExtentKind::Data, 65536)
    )
    .is_err());
}

fn kat_aad() -> ([u8; AAD_LEN], Vec<u8>) {
    let h = Cch1Header {
        codec: Codec::Raw,
        level: Level(0),
        ulen: 64,
    };
    let a = aad(
        &h,
        ExtentKind::Data,
        DekGen(7),
        &FamilyId(common::kat16("family")),
        IdLen::ThirtyTwo,
        &ChunkId(common::kat32("chunk_id")),
    )
    .unwrap();
    (a, common::kat("aad_hardened"))
}

#[test]
fn aad_reproduces_the_kat() {
    let (a, want) = kat_aad();
    assert_eq!(a.to_vec(), want);
    assert_eq!(want, common::kat("aad_fips_kdf_zero_nonce"));
    let g = golden("03-aad-kat.bin", &a);
    assert_eq!(g, want);
    golden(
        "03-aad-fips-kat.bin",
        &common::kat("aad_fips_kdf_zero_nonce"),
    );
}

#[test]
fn info_reproduces_the_kat() {
    let family = FamilyId(common::kat16("family"));
    let chunk_id = ChunkId(common::kat32("chunk_id"));
    let salt: [u8; 24] = common::kat("salt").try_into().unwrap();
    let i = info(&InfoParts {
        family: &family,
        kind: ExtentKind::Data,
        dek_gen: DekGen(7),
        codec: Codec::Raw,
        level: Level(0),
        clen: 64,
        id_len: IdLen::ThirtyTwo,
        chunk_id: &chunk_id,
        salt: &salt,
    })
    .unwrap();
    assert_eq!(i.len(), INFO_LEN);
    assert_eq!(i.to_vec(), common::kat("info_v2"));
    let g = golden("03-info-kat.bin", &i);
    assert_eq!(g, common::kat("info_v2"));
}

#[test]
fn aad_depends_on_id_len_not_field_length() {
    let h = plain_header();
    let f = FamilyId([1; 16]);
    let c = ChunkId([2; 32]);
    let a16 = aad(&h, ExtentKind::Data, DekGen(0), &f, IdLen::Sixteen, &c).unwrap();
    let a32 = aad(&h, ExtentKind::Data, DekGen(0), &f, IdLen::ThirtyTwo, &c).unwrap();
    assert_ne!(a16, a32);
    assert_eq!(a16.len(), 61);
    let big = Cch1Header {
        ulen: (1 << 20) + 1,
        ..h
    };
    assert!(aad(&big, ExtentKind::Data, DekGen(0), &f, IdLen::Sixteen, &c).is_err());
}

#[test]
fn range_and_tamper_rejections() {
    let payload = lz4_block();
    let mut good = Vec::new();
    plain_extent(CHUNK_ID_B, &payload, [7; 16])
        .encode(&mut good)
        .unwrap();
    let p = params(ExtentLayout::Unencrypted, ExtentKind::Data, 16384);
    let f = rt(p);
    assert!(f(&good).is_ok());
    let mut cases: Vec<(&str, Vec<u8>)> = Vec::new();
    let mut b = good.clone();
    b[0] = b'X';
    cases.push(("magic", b));
    let mut b = good.clone();
    b[4] = 2;
    cases.push(("ver", b));
    let mut b = good.clone();
    b[5] = 4; // codec 4
    cases.push(("codec", b));
    let mut b = good.clone();
    b[7] = 5; // LZ4 with level 5
    cases.push(("level", b));
    let mut b = good.clone();
    b[8] = 0;
    b[9] = 0x20; // ulen != chunk size
    cases.push(("ulen", b));
    let mut b = good.clone();
    b[48..52].copy_from_slice(&0u32.to_le_bytes());
    cases.push(("clen 0", b));
    let mut b = good.clone();
    b[48..52].copy_from_slice(&16385u32.to_le_bytes());
    cases.push(("clen > ulen", b));
    let mut b = good.clone();
    b.push(0);
    cases.push(("trailing", b));
    for (name, b) in cases {
        common::assert_rejects(name, &f, &b);
    }
    // raw codec needs clen == ulen
    let raw = [0u8; 100];
    let e = Extent {
        header: Cch1Header {
            codec: Codec::Raw,
            level: Level(0),
            ulen: 101,
        },
        chunk_id: ChunkId([0; 32]),
        crypto: None,
        payload: &raw,
        tag: [0; 16],
    };
    assert!(e.encode(&mut Vec::new()).is_err());
    // a huge claimed clen does not allocate or panic
    let mut b = good.clone();
    b[48..52].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(Extent::decode(&b, p).is_err());
    assert!(tag_input(&[0; 3]).is_err());
}

fn arb_header() -> impl Strategy<Value = (Cch1Header, usize)> {
    prop_oneof![
        (1u64..2000).prop_map(|u| (
            Cch1Header {
                codec: Codec::Raw,
                level: Level(0),
                ulen: u
            },
            u as usize
        )),
        (1u64..2000, 1usize..2000, -7i8..=22).prop_map(|(u, c, l)| (
            Cch1Header {
                codec: Codec::Zstd,
                level: Level(l),
                ulen: u
            },
            c.min(u as usize)
        )),
        (1u64..2000, 1usize..2000).prop_map(|(u, c)| (
            Cch1Header {
                codec: Codec::Lz4,
                level: Level(0),
                ulen: u
            },
            c.min(u as usize)
        )),
        (1u64..2000, 1usize..2000, -7i8..=22).prop_map(|(u, c, l)| (
            Cch1Header {
                codec: Codec::ZstdDict,
                level: Level(l),
                ulen: u
            },
            c.min(u as usize)
        )),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(common::CASES))]

    #[test]
    fn extent_round_trip(
        (header, clen) in arb_header(),
        id in any::<[u8; 32]>(),
        tag in any::<[u8; 16]>(),
        encrypted in any::<bool>(),
        dek_gen in any::<u32>(),
        salt in any::<[u8; 24]>(),
        fill in any::<u8>(),
        node in any::<bool>(),
    ) {
        let payload = vec![fill; clen];
        let layout = if encrypted { ExtentLayout::Encrypted } else { ExtentLayout::Unencrypted };
        let e = Extent {
            header,
            chunk_id: ChunkId(id),
            crypto: encrypted.then_some(CryptoRegion { dek_gen: DekGen(dek_gen), salt, nonce: [0; 12] }),
            payload: &payload,
            tag,
        };
        let mut v = Vec::new();
        e.encode(&mut v).unwrap();
        prop_assert_eq!(v.len(), layout.ovh() as usize + clen);
        let (kind, chunk_size) = if node { (ExtentKind::Node, 0) } else { (ExtentKind::Data, header.ulen as u32) };
        let p = params(layout, kind, chunk_size);
        prop_assert_eq!(Extent::decode(&v, p).unwrap(), e);
        let (d, used) = Extent::decode_prefix(&v, p).unwrap();
        prop_assert_eq!(used, v.len());
        prop_assert_eq!(d, e);
    }
}
