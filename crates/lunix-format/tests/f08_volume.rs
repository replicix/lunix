//! Freeze item 8: the `volume` descriptor.
mod common;

use common::*;
use lunix_format::purpose::Purpose;
use lunix_format::volume::{CodecPolicy, LineageEntry, VolumeDescriptor};
use lunix_format::{
    CommitSeq, CryptoMode, FamilyId, FormatError, IdLen, Profile, TimeMs, VolumeUuid,
};
use proptest::prelude::*;

const A_HEX: &str = "4c554e4958564f4c 0100 02 00 00100000 00000100 0002 20 01 01 00 000000000000 \
a0a1a2a3a4a546a788a9aaabacadaeaf 404142434445464748494a4b4c4d4e4f \
16e74f8af79c8fd7560435a895c26a72 00dc0dcda1010000 \
55921593482045805e11561ca0a5eb627077f183a61f8f6919cb571187809bf4";
const B_HEX: &str = "4c554e4958564f4c 0100 01 00 00100000 00400000 0002 10 02 00 00 000000000000 \
b0b1b2b3b4b546b788b9babbbcbdbebf 505152535455565758595a5b5c5d5e5f \
92a4ec2689f62aab86bc8f278713183e 00dc0dcda1010000 \
4951c7ffd237693e73cd15e17c6916869a540c098bc6fdbe90340ceba80801b5";

fn rt(b: &[u8]) -> Result<Vec<u8>, FormatError> {
    let d = VolumeDescriptor::decode(b)?;
    let mut o = Vec::new();
    d.encode(&mut o)?;
    Ok(o)
}

fn enc(d: &VolumeDescriptor) -> Vec<u8> {
    let mut o = Vec::new();
    d.encode(&mut o).expect("encode");
    o
}

fn vol_a() -> VolumeDescriptor {
    VolumeDescriptor {
        crypto_mode: CryptoMode::Encrypted,
        profile: Profile::Default,
        block_size: 4096,
        chunk_size: 65_536,
        map_fanout: 512,
        id_len: IdLen::ThirtyTwo,
        codec_policy: CodecPolicy::Auto,
        codec_level: 1,
        volume_uuid: VolumeUuid(VOLUME_A_UUID),
        family_id: FamilyId(VOLUME_A_FAMILY),
        kmk_fingerprint: kat32("purpose_pin")[..16].try_into().unwrap(),
        created_at: TimeMs(CREATED_AT),
        lineage: vec![],
        mac: hex(&A_HEX[A_HEX.len() - 64..]).try_into().unwrap(),
    }
}

fn vol_b() -> VolumeDescriptor {
    VolumeDescriptor {
        crypto_mode: CryptoMode::Integrity,
        chunk_size: 16_384,
        id_len: IdLen::Sixteen,
        codec_policy: CodecPolicy::Lz4,
        codec_level: 0,
        volume_uuid: VolumeUuid(VOLUME_B_UUID),
        family_id: FamilyId(VOLUME_B_FAMILY),
        kmk_fingerprint: hex("92a4ec2689f62aab86bc8f278713183e").try_into().unwrap(),
        mac: hex(&B_HEX[B_HEX.len() - 64..]).try_into().unwrap(),
        ..vol_a()
    }
}

fn plain() -> VolumeDescriptor {
    let mut d = VolumeDescriptor {
        crypto_mode: CryptoMode::Plain,
        kmk_fingerprint: [0; 16],
        mac: [0; 32],
        ..vol_b()
    };
    d.mac = blake3_256(&d.mac_input().unwrap());
    d
}

fn lineage_one() -> VolumeDescriptor {
    let mut d = vol_b();
    d.lineage = vec![LineageEntry {
        volume_uuid: VolumeUuid(VOLUME_A_UUID),
        base_seq: CommitSeq(7),
        backend_hint: "s3://bucket/prefix".into(),
    }];
    d
}

#[test]
fn a0_examples_decode_and_reencode() {
    for (name, hexs, want) in [
        ("a0-volume-encrypted.bin", A_HEX, vol_a()),
        ("a0-volume-integrity.bin", B_HEX, vol_b()),
    ] {
        let bytes = hex(hexs);
        assert_eq!(bytes.len(), 120);
        assert_eq!(VolumeDescriptor::decode(&bytes).unwrap(), want);
        assert_eq!(enc(&want), bytes);
        let g = golden(name, &enc(&want));
        negative_suite(name, &rt, &g);
    }
}

#[test]
fn a0_macs_recompute() {
    // Volume A: descriptor MAC key is the KAT purpose_volume_mac.
    let a = vol_a();
    assert_eq!(
        hmac_sha256(&kat32("purpose_volume_mac"), &a.mac_input().unwrap()),
        a.mac
    );
    // Volume B: key derived from the integrity key 60..7f.
    let ik = volume_b_ik();
    let b = vol_b();
    let msg = Purpose::DescriptorMac.message(&b.family_id).unwrap();
    let key = purpose_key(&ik, &msg);
    assert_eq!(
        key.to_vec(),
        hex("d56ef6bb95bf87c7b14ec9ad25b9eae3132d9120d815b04d6f467207f705d972")
    );
    assert_eq!(hmac_sha256(&key, &b.mac_input().unwrap()), b.mac);
    // Fingerprints.
    let fp = purpose_key(&ik, &Purpose::Fingerprint.message(&b.family_id).unwrap());
    assert_eq!(fp[..16], b.kmk_fingerprint);
    assert_eq!(kat32("purpose_pin")[..16], a.kmk_fingerprint);
}

#[test]
fn plain_and_lineage_goldens() {
    let p = plain();
    assert_eq!(blake3_256(&p.mac_input().unwrap()), p.mac);
    let g = golden("08-volume-plain.bin", &enc(&p));
    negative_suite("plain", &rt, &g);
    assert_eq!(VolumeDescriptor::decode(&g).unwrap(), p);

    let l = lineage_one();
    let g = golden("08-volume-lineage-one.bin", &enc(&l));
    assert_eq!(g.len(), 120 + 16 + 8 + 2 + 18);
    assert_eq!(VolumeDescriptor::decode(&g).unwrap(), l);
    negative_suite("lineage", &rt, &g);
}

#[test]
fn mac_input_is_label_then_body() {
    let d = vol_b();
    let mi = d.mac_input().unwrap();
    assert_eq!(&mi[..16], b"lunix volume v1\0");
    assert_eq!(&mi[16..], &enc(&d)[..88]);
}

fn patched(off: usize, bytes: &[u8]) -> Vec<u8> {
    let mut b = enc(&vol_b());
    b[off..off + bytes.len()].copy_from_slice(bytes);
    b
}

#[test]
fn rejects_field_violations() {
    let r = |b: Vec<u8>, what: &str| assert_rejects(what, &rt, &b);
    r(patched(0, b"LUNIXVOX"), "magic");
    r(patched(8, &[2, 0]), "format_ver");
    r(patched(10, &[3]), "crypto_mode 3");
    r(patched(11, &[2]), "profile 2");
    r(patched(11, &[1]), "fips with integrity");
    r(patched(12, &8192u32.to_le_bytes()), "block_size");
    r(patched(16, &8192u32.to_le_bytes()), "chunk_size too small");
    r(
        patched(16, &(24_576u32).to_le_bytes()),
        "chunk_size not a power of two",
    );
    r(
        patched(16, &(2 * 1_048_576u32).to_le_bytes()),
        "chunk_size too large",
    );
    r(patched(20, &128u16.to_le_bytes()), "fanout 128");
    r(patched(20, &1024u16.to_le_bytes()), "fanout 1024");
    r(patched(22, &[8]), "id_len 8");
    r(patched(23, &[4]), "policy 4");
    r(patched(24, &[5]), "lz4 with a level");
    r(patched(23, &[1, 0]), "auto with level 0");
    r(patched(23, &[1, 23]), "auto with level 23");
    r(patched(23, &[3, 0]), "zstd with level 0");
    r(patched(23, &[3, 0xf8]), "zstd with level -8");
    r(patched(25, &[1]), "lineage_count without entries");
    for i in 26..32 {
        r(patched(i, &[1]), "reserved");
    }
    // Plain with a fingerprint.
    r(patched(10, &[0]), "plain with a fingerprint");
    // Fewer or more bytes.
    let mut long = enc(&vol_b());
    long.push(0);
    r(long, "trailing byte");
}

#[test]
fn accepts_legal_variants() {
    for (policy, level) in [(0u8, 0i8), (1, 22), (2, 0), (3, -7), (3, 22), (3, 1)] {
        let b = patched(23, &[policy, level as u8]);
        assert!(
            VolumeDescriptor::decode(&b).is_ok(),
            "policy {policy} level {level}"
        );
    }
    // fips with encrypted
    let mut b = enc(&vol_a());
    b[11] = 1;
    assert!(VolumeDescriptor::decode(&b).is_ok());
    // id_len 0 decodes
    let b = patched(22, &[0]);
    assert!(VolumeDescriptor::decode(&b).is_ok());
    // fanout 256
    let b = patched(20, &256u16.to_le_bytes());
    assert!(VolumeDescriptor::decode(&b).is_ok());
}

#[test]
fn encode_refuses_what_decode_refuses() {
    let mut d = vol_b();
    d.chunk_size = 20_000;
    assert!(d.encode(&mut Vec::new()).is_err());
    let mut d = plain();
    d.kmk_fingerprint = [1; 16];
    assert!(d.encode(&mut Vec::new()).is_err());
    let mut d = vol_b();
    d.profile = Profile::Fips;
    assert!(d.encode(&mut Vec::new()).is_err());
    let mut d = vol_b();
    d.lineage = vec![
        LineageEntry {
            volume_uuid: VolumeUuid([1; 16]),
            base_seq: CommitSeq(0),
            backend_hint: "x".repeat(1025),
        };
        1
    ];
    assert!(d.encode(&mut Vec::new()).is_err());
}

#[test]
fn lineage_rejections() {
    let g = enc(&lineage_one());
    // lineage_count says 2 but one entry present
    let mut b = g.clone();
    b[25] = 2;
    assert_rejects("count above entries", &rt, &b);
    // count 0 with an entry present
    let mut b = g.clone();
    b[25] = 0;
    assert_rejects("count below entries", &rt, &b);
    // backend hint with a NUL
    let mut b = g.clone();
    let at = 88 + 26;
    b[at] = 0;
    assert_rejects("nul in hint", &rt, &b);
    // hint length over 1024
    let mut b = g;
    b[88 + 24..88 + 26].copy_from_slice(&2000u16.to_le_bytes());
    assert_rejects("hint length", &rt, &b);
}

fn arb_desc() -> impl Strategy<Value = VolumeDescriptor> {
    let head = (
        0u8..3,
        14u32..=20,
        any::<bool>(),
        prop::sample::select(vec![0u8, 16, 32]),
        (0u8..4, any::<u8>()),
        any::<bool>(),
    );
    let ids = (
        any::<[u8; 16]>(),
        any::<[u8; 16]>(),
        any::<[u8; 16]>(),
        any::<u64>(),
        any::<[u8; 32]>(),
    );
    let lin = prop::collection::vec(
        (any::<[u8; 16]>(), any::<u64>(), "[a-z0-9:/._-]{0,40}"),
        0..4,
    );
    (head, ids, lin).prop_map(
        |((mode, shift, f256, idl, (pol, lv), fips), (u, f, fp, t, mac), lin)| {
            let policy = CodecPolicy::from_u8(pol).unwrap();
            let level = match policy {
                CodecPolicy::None | CodecPolicy::Lz4 => 0,
                CodecPolicy::Auto => 1 + (lv % 22) as i8,
                CodecPolicy::Zstd => {
                    let l = (lv % 29) as i8 - 7; // -7 ..= 21
                    if l == 0 {
                        22
                    } else {
                        l
                    }
                }
            };
            let crypto_mode = CryptoMode::from_u8(mode).unwrap();
            VolumeDescriptor {
                crypto_mode,
                profile: if fips && crypto_mode == CryptoMode::Encrypted {
                    Profile::Fips
                } else {
                    Profile::Default
                },
                block_size: 4096,
                chunk_size: 1 << shift,
                map_fanout: if f256 { 256 } else { 512 },
                id_len: IdLen::new(idl).unwrap(),
                codec_policy: policy,
                codec_level: level,
                volume_uuid: VolumeUuid(u),
                family_id: FamilyId(f),
                kmk_fingerprint: if crypto_mode == CryptoMode::Plain {
                    [0; 16]
                } else {
                    fp
                },
                created_at: TimeMs(t),
                lineage: lin
                    .into_iter()
                    .map(|(u, s, h)| LineageEntry {
                        volume_uuid: VolumeUuid(u),
                        base_seq: CommitSeq(s),
                        backend_hint: h,
                    })
                    .collect(),
                mac,
            }
        },
    )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(CASES))]
    #[test]
    fn roundtrip(d in arb_desc()) {
        let b = enc(&d);
        prop_assert_eq!(VolumeDescriptor::decode(&b).unwrap(), d);
    }
}
