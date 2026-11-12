//! Freeze item 4: segment and map-pack container (trailer, footer, tiling).
mod common;

use common::{
    blake3_128, blake3_256, blake3_keyed, golden, hex, hmac_sha256, negative_suite, purpose_key,
    volume_b_ik, Rng, VOLUME_B_FAMILY, VOLUME_B_UUID,
};
use lunix_format::envelope::{meta_aad, EnvelopeBody};
use lunix_format::node::{Leaf, LeafParams};
use lunix_format::purpose::Purpose;
use lunix_format::segment::*;
use lunix_format::{
    ChunkId, Codec, FamilyId, Form, FormatError, IdLen, ObjId, ObjectKind, WriteSeq,
};
use proptest::prelude::*;

const IK_HMAC_KEY: &str = "aa13505a19bd5e783cbc31582031cb918c1dfaa9aa6b94b27057acb06306f4df";
const EXTENT_TAG_KEY: &str = "8f5a8aa5bd1c61e6d9b3e275436f35de2673144fc8d12cb7aab4b39398b09322";
const OVH: u32 = 68;

fn family() -> FamilyId {
    FamilyId(VOLUME_B_FAMILY)
}

fn hmac_key() -> [u8; 32] {
    let k = purpose_key(
        &volume_b_ik(),
        &Purpose::IntegrityHmac.message(&family()).unwrap(),
    );
    assert_eq!(k.to_vec(), hex(IK_HMAC_KEY), "volume B integrity HMAC key");
    k
}

fn tag_key() -> [u8; 32] {
    let k = purpose_key(
        &volume_b_ik(),
        &Purpose::ExtentTag.message(&family()).unwrap(),
    );
    assert_eq!(k.to_vec(), hex(EXTENT_TAG_KEY), "volume B extent tag key");
    k
}

fn footer_mac(name: &str, frame: &[u8]) -> [u8; 32] {
    let aad = meta_aad(&VOLUME_B_UUID, ObjectKind::Footer, name).unwrap();
    let signed = footer_form0_signed(frame).unwrap();
    hmac_sha256(&hmac_key(), &[aad, signed].concat())
}

/// The 143-byte extent of section 23.2, assembled from the doc's pieces.
fn a0_extent() -> Vec<u8> {
    let mut v = hex("43434831 01 0200 00 0040000000000000");
    v.extend(hex(
        "8f6cf4e5e665b21ac12bd316397d883cb284a14b4999f9ee99301971d1b55ede",
    ));
    v.extend(hex("4b000000"));
    v.extend(hex("1f410100"));
    v.extend(std::iter::repeat_n(0xff, 64));
    v.extend(hex("27 50 41 41 41 41 41"));
    assert_eq!(v.len(), 127);
    v.extend(hex("4e700344dd5d9ea60a34ba09c8a5f115"));
    v
}

const A0_SEG_HASH: &str = "2567f10bff7129dc5da5c206d1ead825adbee6d0f55d02a8baf269cef18f9162";
const A0_FOOTER_BODY_HEX: &str = "01 00 0000 01000000 0100000000000000 8f00000000000000 \
    0100000000000000 0100000000000000 \
    2567f10bff7129dc5da5c206d1ead825adbee6d0f55d02a8baf269cef18f9162 \
    8f6cf4e5e665b21ac12bd316397d883cb284a14b4999f9ee99301971d1b55ede \
    00000000 4b000000 00400000 02 000000";
const A0_MAC: &str = "a6d632645323ac81d8d6085399e74e2681506d459a90179f90f6b09c05ee3881";
const A0_TRAILER_HEX: &str = "8f00000000000000 ad000000 01000000 0100000000000000 01 00 \
    000000000000000000000000000000000000000000000000000000000000 59ed78e9 4c585354";

fn a0_body() -> FooterBody {
    let extent = a0_extent();
    FooterBody {
        container_kind: ContainerKind::Segment,
        object_id: ObjId::from_raw(1).unwrap(),
        extents_len: 143,
        write_seq_lo: WriteSeq(1),
        write_seq_hi: WriteSeq(1),
        seg_hash: blake3_256(&extent),
        entries: vec![FooterEntry {
            chunk_id: ChunkId(
                hex("8f6cf4e5e665b21ac12bd316397d883cb284a14b4999f9ee99301971d1b55ede")
                    .try_into()
                    .unwrap(),
            ),
            off: 0,
            clen: 75,
            ulen: 16384,
            codec: Codec::Lz4,
        }],
        dict_ids: vec![],
    }
}

fn body_bytes(b: &FooterBody) -> Vec<u8> {
    let mut v = Vec::new();
    b.encode(&mut v).unwrap();
    v
}

/// Decode a whole container and re-assemble it (what the negative suite runs).
fn container_rt(form: Form, ovh: u32) -> impl Fn(&[u8]) -> Result<Vec<u8>, FormatError> {
    move |b| {
        let parts = split_container(b)?;
        let env = decode_footer_envelope(parts.footer, form)?;
        let frame: &[u8] = match &env.body {
            EnvelopeBody::Mac { body, .. } => body,
            EnvelopeBody::Sealed { ciphertext, .. } => ciphertext,
        };
        if form == Form::Mac {
            let body = decode_footer_frame(frame, parts.trailer.footer_off)?;
            body.check_trailer(&parts.trailer)?;
            body.check_tiling(ovh)?;
            if blake3_256(parts.extents) != body.seg_hash {
                return Err(FormatError::Checksum { what: "seg_hash" });
            }
        }
        assemble_container(
            parts.extents,
            parts.footer,
            parts.trailer.object_id,
            parts.trailer.container_kind,
            parts.trailer.extent_count,
        )
    }
}

#[test]
fn a0_segment_one_extent() {
    let extent = a0_extent();
    assert_eq!(blake3_256(&extent).to_vec(), hex(A0_SEG_HASH));
    // The doc's tag is the keyed BLAKE3-128 over everything before it.
    assert_eq!(blake3_128_keyed(&tag_key(), &extent[..127]), extent[127..]);

    let body = a0_body();
    let bytes = body_bytes(&body);
    assert_eq!(bytes, hex(A0_FOOTER_BODY_HEX));
    assert_eq!(bytes.len(), 120);

    let frame = footer_frame(&body, FrameCompression::Raw).unwrap();
    assert_eq!(&frame[..9], &hex("28b52ffd 20 78 c103 00")[..]);
    assert_eq!(frame.len(), 129);
    let mac = footer_mac("segments/0000000000000001", &frame);
    assert_eq!(mac.to_vec(), hex(A0_MAC));
    let env = encode_footer_form0(&frame, &mac).unwrap();
    assert_eq!(env.len(), 173);
    assert_eq!(&env[..12], &hex("4c584d31 01 06 00 00 81000000")[..]);

    let container = assemble_container(
        &extent,
        &env,
        ObjId::from_raw(1).unwrap(),
        ContainerKind::Segment,
        1,
    )
    .unwrap();
    assert_eq!(container.len(), 380);
    assert_eq!(&container[316..], &hex(A0_TRAILER_HEX)[..]);
    let g = golden("a0-segment-one-extent.bin", &container);

    let parts = split_container(&g).unwrap();
    assert_eq!(parts.trailer.footer_off, 143);
    assert_eq!(parts.trailer.footer_len, 173);
    let e = decode_footer_envelope(parts.footer, Form::Mac).unwrap();
    let EnvelopeBody::Mac { body: fr, mac: m } = e.body else {
        panic!("form 0")
    };
    assert_eq!(m, mac);
    assert_eq!(decode_footer_frame(fr, 143).unwrap(), body);
    body.check_trailer(&parts.trailer).unwrap();
    body.check_tiling(OVH).unwrap();

    negative_suite("a0 segment", &container_rt(Form::Mac, OVH), &g);
}

fn blake3_128_keyed(key: &[u8; 32], data: &[u8]) -> Vec<u8> {
    blake3_keyed(key, data)[..16].to_vec()
}

#[test]
fn trailer_golden_and_rejections() {
    let t = Trailer {
        footer_off: 143,
        footer_len: 173,
        extent_count: 1,
        object_id: ObjId::from_raw(1).unwrap(),
        container_kind: ContainerKind::Segment,
    };
    let mut b = Vec::new();
    t.encode(&mut b).unwrap();
    assert_eq!(b, hex(A0_TRAILER_HEX));
    let t2 = Trailer {
        container_kind: ContainerKind::MapPack,
        object_id: ObjId::new(3, 0xabcdef).unwrap(),
        footer_off: 70_000,
        footer_len: 100,
        extent_count: 900,
    };
    let mut b2 = Vec::new();
    t2.encode(&mut b2).unwrap();
    let g = golden("04-segment-trailer.bin", &b2);
    let rt = |x: &[u8]| -> Result<Vec<u8>, FormatError> {
        let t = Trailer::decode(x)?;
        let mut v = Vec::new();
        t.encode(&mut v)?;
        Ok(v)
    };
    negative_suite("trailer", &rt, &g);
    assert_eq!(Trailer::decode(&g).unwrap(), t2);
    // reserved byte, version, magic, kind, extent_count too large for footer_off
    for (i, v) in [(30usize, 1u8), (24, 2), (25, 2), (63, 0)] {
        let mut m = b.clone();
        m[i] = v;
        assert!(Trailer::decode(&m).is_err(), "byte {i}");
    }
    let bad = Trailer {
        extent_count: 3,
        ..t
    };
    assert!(bad.encode(&mut Vec::new()).is_err());
    let bad = Trailer {
        footer_len: 43,
        ..t
    };
    assert!(bad.encode(&mut Vec::new()).is_err());
    let bad = Trailer {
        footer_off: u64::from(u32::MAX),
        ..t
    };
    assert!(bad.encode(&mut Vec::new()).is_err());
    // container length must match the offsets
    let mut c = vec![0u8; 143 + 173];
    c.extend_from_slice(&b);
    assert!(Trailer::decode_from_container(&c).is_ok());
    c.insert(0, 0);
    assert!(Trailer::decode_from_container(&c).is_err());
}

/// A deterministic multi-extent body that tiles its region.
fn multi_body(kind: ContainerKind, n: usize, seed: u64, dict: bool) -> FooterBody {
    let mut r = Rng(seed | 1);
    let mut entries = Vec::new();
    let mut off = 0u32;
    for i in 0..n {
        let codec = match (i % 4, dict) {
            (0, _) => Codec::Raw,
            (1, _) => Codec::Lz4,
            (2, _) => Codec::Zstd,
            (_, true) => Codec::ZstdDict,
            (_, false) => Codec::Zstd,
        };
        let ulen = if kind == ContainerKind::MapPack {
            24_579
        } else {
            16_384
        };
        let clen = if codec == Codec::Raw {
            ulen
        } else {
            1 + (r.next() % u64::from(ulen)) as u32
        };
        let mut id = [0u8; 32];
        for b in &mut id {
            *b = r.next() as u8;
        }
        entries.push(FooterEntry {
            chunk_id: ChunkId(id),
            off,
            clen,
            ulen,
            codec,
        });
        off += OVH + clen;
    }
    entries.sort_by_key(|e| (e.chunk_id, e.off));
    let users = entries
        .iter()
        .filter(|e| e.codec == Codec::ZstdDict)
        .count();
    let mut dict_ids: Vec<u32> = (0..users.min(3)).map(|_| r.next() as u32).collect();
    dict_ids.sort_unstable();
    dict_ids.dedup();
    FooterBody {
        container_kind: kind,
        object_id: ObjId::new(1, 3).unwrap(),
        extents_len: u64::from(off),
        write_seq_lo: WriteSeq(10),
        write_seq_hi: WriteSeq(99),
        seg_hash: blake3_256(&seed.to_le_bytes()),
        entries,
        dict_ids,
    }
}

fn body_rt(b: &[u8]) -> Result<Vec<u8>, FormatError> {
    let body = FooterBody::decode(b)?;
    let mut v = Vec::with_capacity(b.len());
    body.encode(&mut v)?;
    Ok(v)
}

#[test]
fn footer_body_goldens() {
    let b = multi_body(ContainerKind::Segment, 5, 7, false);
    b.check_tiling(OVH).unwrap();
    let g = golden("04-segment-footer-body.bin", &body_bytes(&b));
    assert_eq!(g.len(), 72 + 5 * 48);
    assert_eq!(FooterBody::decode(&g).unwrap(), b);
    negative_suite("footer body", &body_rt, &g);

    let d = multi_body(ContainerKind::Segment, 9, 11, true);
    assert!(!d.dict_ids.is_empty());
    d.check_tiling(OVH).unwrap();
    let g = golden("04-segment-footer-dict-ids.bin", &body_bytes(&d));
    assert_eq!(g.len(), 72 + 9 * 48 + 4 * d.dict_ids.len());
    assert_eq!(FooterBody::decode(&g).unwrap(), d);
    negative_suite("footer dict ids", &body_rt, &g);
    // the n_dict_ids field says how many ids follow
    assert_eq!(u16::from_le_bytes([g[2], g[3]]) as usize, d.dict_ids.len());
}

#[test]
fn footer_rejections() {
    let b = multi_body(ContainerKind::Segment, 6, 3, true);
    // unsorted and duplicate keys
    let mut m = b.clone();
    m.entries.swap(0, 1);
    assert!(m.encode(&mut Vec::new()).is_err());
    let mut m = b.clone();
    m.entries[1] = m.entries[0];
    assert!(m.encode(&mut Vec::new()).is_err());
    // the same id twice at different offsets is allowed (two copies)
    let mut m = b.clone();
    m.entries[1].chunk_id = m.entries[0].chunk_id;
    m.entries[1].off = m.entries[0].off + 1;
    m.entries.sort_by_key(|e| (e.chunk_id, e.off));
    assert!(m.encode(&mut Vec::new()).is_ok());
    // dictionary ids against codec-3 extents
    let mut m = b.clone();
    m.dict_ids.clear();
    assert!(m.encode(&mut Vec::new()).is_err());
    let mut m = multi_body(ContainerKind::Segment, 5, 3, false);
    m.dict_ids = vec![1];
    assert!(m.encode(&mut Vec::new()).is_err());
    let mut m = b.clone();
    m.dict_ids = vec![5, 5];
    assert!(m.encode(&mut Vec::new()).is_err());
    let mut m = b.clone();
    m.dict_ids = vec![9, 5];
    assert!(m.encode(&mut Vec::new()).is_err());
    // map pack never has dictionary ids
    let mut m = multi_body(ContainerKind::MapPack, 3, 3, false);
    m.dict_ids = vec![1];
    assert!(m.encode(&mut Vec::new()).is_err());
    // entry rules: raw codec needs clen = ulen, ulen limits, empty list
    let mut m = b.clone();
    m.entries[0].codec = Codec::Raw;
    m.entries[0].clen = m.entries[0].ulen - 1;
    assert!(m.encode(&mut Vec::new()).is_err());
    let mut m = multi_body(ContainerKind::MapPack, 3, 3, false);
    m.entries[0].ulen = 65_537;
    assert!(m.encode(&mut Vec::new()).is_err());
    let mut m = b.clone();
    m.entries.clear();
    assert!(m.encode(&mut Vec::new()).is_err());
    let mut m = b.clone();
    m.write_seq_hi = WriteSeq(1);
    assert!(m.encode(&mut Vec::new()).is_err());
    // reserved bytes of an entry, version, container kind
    let bytes = body_bytes(&b);
    for i in [72 + 45, 72 + 46, 72 + 47, 0, 1] {
        let mut m = bytes.clone();
        m[i] = 7;
        assert!(FooterBody::decode(&m).is_err(), "byte {i}");
    }
    // extent_count beyond what the region can hold
    let mut m = bytes.clone();
    m[4..8].copy_from_slice(&1000u32.to_le_bytes());
    assert!(FooterBody::decode(&m).is_err());
    // trailer disagreement
    let t = Trailer {
        footer_off: b.extents_len,
        footer_len: 200,
        extent_count: 6,
        object_id: b.object_id,
        container_kind: ContainerKind::Segment,
    };
    b.check_trailer(&t).unwrap();
    assert!(b
        .check_trailer(&Trailer {
            extent_count: 5,
            ..t
        })
        .is_err());
    assert!(b
        .check_trailer(&Trailer {
            container_kind: ContainerKind::MapPack,
            ..t
        })
        .is_err());
}

#[test]
fn tiling_rule() {
    let b = multi_body(ContainerKind::Segment, 8, 21, false);
    b.check_tiling(OVH).unwrap();
    assert!(b.check_tiling(108).is_err(), "wrong overhead");
    // a gap
    let mut m = b.clone();
    m.entries.iter_mut().find(|e| e.off > 0).unwrap().off += 1;
    assert!(m.check_tiling(OVH).is_err());
    // an overlap
    let mut m = b.clone();
    let e = m.entries.iter_mut().find(|e| e.off > 0).unwrap();
    e.off -= 1;
    assert!(m.check_tiling(OVH).is_err());
    // a short region and a long region
    let mut m = b.clone();
    m.extents_len -= 1;
    assert!(m.check_tiling(OVH).is_err());
    let mut m = b.clone();
    m.extents_len += 1;
    assert!(m.check_tiling(OVH).is_err());
    // the same chunk id stored twice tiles when offsets are distinct
    let mut m = b.clone();
    m.entries[1].chunk_id = m.entries[0].chunk_id;
    m.entries.sort_by_key(|e| (e.chunk_id, e.off));
    m.check_tiling(OVH).unwrap();
}

#[test]
fn frames_all_content_size_classes() {
    for (n, seed) in [(5usize, 1u64), (100, 2), (1400, 3)] {
        let body = multi_body(ContainerKind::Segment, n, seed, false);
        let raw_len = body_bytes(&body).len();
        for how in [
            FrameCompression::Raw,
            FrameCompression::Zstd(3),
            FrameCompression::Zstd(-5),
        ] {
            let f = footer_frame(&body, how).unwrap();
            assert_eq!(
                decode_footer_frame(&f, body.extents_len).unwrap(),
                body,
                "{n} {how:?}"
            );
            if how == FrameCompression::Raw {
                let fcs = if raw_len <= 255 {
                    1
                } else if raw_len <= 65_791 {
                    2
                } else {
                    4
                };
                assert_eq!(f.len(), raw_len + 4 + 1 + fcs + 3);
            }
        }
    }
}

#[test]
fn frame_cap_and_bombs() {
    let body = multi_body(ContainerKind::Segment, 5, 1, false);
    let f = footer_frame(&body, FrameCompression::Raw).unwrap();
    // wrong extents_len from the trailer
    assert!(decode_footer_frame(&f, body.extents_len + 1).is_err());
    // cap: 72 + 52 * floor(len / 69); a 312-byte body needs at least 5 * 69 bytes
    assert_eq!(footer_body_cap(143).unwrap(), 72 + 52 * 2);
    // trailing bytes and a second frame
    let mut t = f.clone();
    t.push(0);
    assert!(decode_footer_frame(&t, body.extents_len).is_err());
    let mut two = f.clone();
    two.extend_from_slice(&f);
    assert!(decode_footer_frame(&two, body.extents_len).is_err());
    // a frame that declares 2^40 bytes of content is refused without allocating
    let mut huge = hex("28b52ffd e0");
    huge.extend_from_slice(&(1u64 << 40).to_le_bytes());
    huge.extend(hex("010000"));
    let (res, allocated) = common::measure(|| decode_footer_frame(&huge, 1 << 20));
    assert!(res.is_err());
    assert!(allocated < 4096, "allocated {allocated}");
    // a real zstd bomb: 8 MiB of zeros in a few bytes, tiny cap
    let bomb = zstd::bulk::compress(&vec![0u8; 8 << 20], 19).unwrap();
    assert!(bomb.len() < 1000);
    let (res, allocated) = common::measure(|| decode_footer_frame(&bomb, 1000));
    assert!(res.is_err());
    assert!(allocated < 4096, "allocated {allocated}");
    // no content size, a dictionary id, skippable frames
    assert!(decode_footer_frame(&hex("28b52ffd 00 00 010000 00"), 1000).is_err());
    assert!(decode_footer_frame(&hex("28b52ffd 21 78 010000 00"), 1000).is_err());
    assert!(decode_footer_frame(&hex("502a4d18 00000000"), 1000).is_err());
    assert!(decode_footer_frame(&[], 1000).is_err());
}

fn sealed_envelope() -> Vec<u8> {
    let body = multi_body(ContainerKind::Segment, 9, 11, true);
    let frame = footer_frame(&body, FrameCompression::Zstd(3)).unwrap();
    // This crate seals nothing: a fixed pseudo-ciphertext stands in for it.
    let ct: Vec<u8> = frame.iter().map(|b| b ^ 0x5a).collect();
    let nonce: [u8; 24] = std::array::from_fn(|i| 0x10 + i as u8);
    let tag: [u8; 16] = std::array::from_fn(|i| 0xe0 + i as u8);
    encode_footer_form1(&nonce, &ct, &tag).unwrap()
}

#[test]
fn sealed_footer_envelope() {
    let g = golden("04-segment-footer-sealed.bin", &sealed_envelope());
    let e = decode_footer_envelope(&g, Form::Sealed).unwrap();
    assert_eq!(e.kind, ObjectKind::Footer);
    let EnvelopeBody::Sealed {
        nonce,
        ciphertext,
        tag,
    } = e.body
    else {
        panic!("form 1")
    };
    assert_eq!(nonce[0], 0x10);
    assert_eq!(tag[15], 0xef);
    assert_eq!(g.len(), 12 + 24 + ciphertext.len() + 16);
    let rt = |b: &[u8]| -> Result<Vec<u8>, FormatError> {
        let e = decode_footer_envelope(b, Form::Sealed)?;
        let EnvelopeBody::Sealed {
            nonce,
            ciphertext,
            tag,
        } = e.body
        else {
            return Err(FormatError::invalid("test", "form"));
        };
        encode_footer_form1(&nonce, ciphertext, &tag)
    };
    negative_suite("sealed footer", &rt, &g);
    // a sealed envelope is not a form 0 envelope
    assert!(decode_footer_envelope(&g, Form::Mac).is_err());
}

/// A plain/integrity extent of `OVH + payload` bytes, tag keyed with volume B.
fn node_extent(chunk_id: [u8; 32], payload: &[u8]) -> Vec<u8> {
    let mut v = hex("43434831 01 0000 00");
    v.extend_from_slice(&(payload.len() as u64).to_le_bytes());
    v.extend_from_slice(&chunk_id);
    v.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    v.extend_from_slice(payload);
    let tag = blake3_128_keyed(&tag_key(), &v);
    v.extend_from_slice(&tag);
    v
}

#[test]
fn map_pack_two_nodes() {
    let p = LeafParams {
        fanout: 256,
        id_len: IdLen::Sixteen,
        ovh: OVH,
    };
    let mut extents = Vec::new();
    let mut entries = Vec::new();
    for i in 0..2u8 {
        let mut leaf = Leaf::empty(256);
        leaf.entries[usize::from(i)] =
            lunix_format::node::Entry::Data(lunix_format::node::DataEntry {
                lineage: lunix_format::LineageIdx(0),
                seg: ObjId::from_raw(1).unwrap(),
                off: u32::from(i) * 500,
                clen: 75,
                prefix: lunix_format::IdPrefix::from_slice(IdLen::Sixteen, &[i + 1; 16]).unwrap(),
            });
        let mut node = Vec::new();
        leaf.encode(&p, &mut node).unwrap();
        let id = blake3_256(&node);
        entries.push(FooterEntry {
            chunk_id: ChunkId(id),
            off: extents.len() as u32,
            clen: node.len() as u32,
            ulen: node.len() as u32,
            codec: Codec::Raw,
        });
        extents.extend(node_extent(id, &node));
    }
    entries.sort_by_key(|e| (e.chunk_id, e.off));
    let body = FooterBody {
        container_kind: ContainerKind::MapPack,
        object_id: ObjId::from_raw(2).unwrap(),
        extents_len: extents.len() as u64,
        write_seq_lo: WriteSeq(0),
        write_seq_hi: WriteSeq(0),
        seg_hash: blake3_256(&extents),
        entries,
        dict_ids: vec![],
    };
    body.check_tiling(OVH).unwrap();
    let frame = footer_frame(&body, FrameCompression::Zstd(3)).unwrap();
    let mac = footer_mac("map/0000000000000002", &frame);
    let env = encode_footer_form0(&frame, &mac).unwrap();
    let c = assemble_container(&extents, &env, body.object_id, ContainerKind::MapPack, 2).unwrap();
    let g = golden("04-mappack-two-nodes.bin", &c);
    let parts = split_container(&g).unwrap();
    assert_eq!(parts.trailer.container_kind, ContainerKind::MapPack);
    negative_suite("map pack", &container_rt(Form::Mac, OVH), &g);
}

fn random_body(seed: u64) -> FooterBody {
    let mut r = Rng(seed | 1);
    let kind = if r.below(4) == 0 {
        ContainerKind::MapPack
    } else {
        ContainerKind::Segment
    };
    multi_body(
        kind,
        1 + r.below(60),
        r.next(),
        kind == ContainerKind::Segment && r.below(2) == 0,
    )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(common::CASES))]

    #[test]
    fn footer_round_trip(seed in any::<u64>()) {
        let b = random_body(seed);
        let bytes = body_bytes(&b);
        prop_assert_eq!(&FooterBody::decode(&bytes).unwrap(), &b);
        prop_assert!(b.check_tiling(OVH).is_ok());
        let how = if seed % 3 == 0 { FrameCompression::Raw } else { FrameCompression::Zstd((seed % 20) as i32) };
        let f = footer_frame(&b, how).unwrap();
        prop_assert_eq!(&decode_footer_frame(&f, b.extents_len).unwrap(), &b);
    }

    #[test]
    fn trailer_round_trip(
        count in 1u32..100_000,
        extra in 0u64..1_000_000,
        flen in 44u32..100_000,
        epoch in 0u64..(1 << 24),
        counter in 1u64..(1 << 40),
        map in any::<bool>(),
    ) {
        let t = Trailer {
            footer_off: u64::from(count) * 69 + extra,
            footer_len: flen,
            extent_count: count,
            object_id: ObjId::new(epoch, counter).unwrap(),
            container_kind: if map { ContainerKind::MapPack } else { ContainerKind::Segment },
        };
        let mut b = Vec::new();
        t.encode(&mut b).unwrap();
        prop_assert_eq!(b.len(), 64);
        prop_assert_eq!(Trailer::decode(&b).unwrap(), t);
    }
}

#[test]
fn blake3_128_helper_is_consistent() {
    // keeps the shared helper exercised
    assert_eq!(blake3_128(b"x").to_vec(), blake3_256(b"x")[..16].to_vec());
}
