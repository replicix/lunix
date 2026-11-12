//! Freeze item 1: primitives and their text forms.

mod common;

use lunix_format::bytes::Reader;
use lunix_format::ids::{hex_lower, parse_hex_lower};
use lunix_format::*;
use proptest::prelude::*;

fn objid(epoch: u64, counter: u64) -> ObjId {
    ObjId::new(epoch, counter).expect("legal id")
}

/// The ids at the limits of the epoch/counter split, as stored (u64 LE).
fn limit_ids() -> Vec<ObjId> {
    vec![
        objid(0, 1),
        objid(0, 2),
        objid(0, ObjId::MAX_COUNTER),
        objid(1, 1),
        objid(1, ObjId::MAX_COUNTER),
        objid(0x00_ffff, 0x12_3456_789a),
        objid(ObjId::EPOCH_LIMIT - 1, 1),
        objid(ObjId::EPOCH_LIMIT - 1, ObjId::MAX_COUNTER),
    ]
}

fn decode_ids(b: &[u8]) -> Result<Vec<u8>, FormatError> {
    let mut r = Reader::new(b, "objid list");
    let n = r.check_count("ids", (b.len() / 8) as u64, 1024, 8)?;
    let mut out = Vec::with_capacity(b.len());
    for _ in 0..n {
        out.extend_from_slice(&ObjId::from_raw(r.u64()?)?.raw().to_le_bytes());
    }
    r.finish()?;
    Ok(out)
}

#[test]
fn objid_golden_and_limits() {
    let mut bytes = Vec::new();
    for id in limit_ids() {
        bytes.extend_from_slice(&id.raw().to_le_bytes());
    }
    // The split: counter in the low 40 bits, epoch in the high 24.
    assert_eq!(&bytes[0..8], &[1, 0, 0, 0, 0, 0, 0, 0]);
    assert_eq!(&bytes[16..24], &[0xff, 0xff, 0xff, 0xff, 0xff, 0, 0, 0]);
    assert_eq!(&bytes[24..32], &[1, 0, 0, 0, 0, 1, 0, 0]);
    assert_eq!(&bytes[56..64], &[0xff; 8]);
    let g = common::golden("01-objid.bin", &bytes);
    // Prefixes that cut an id fail; whole-id prefixes are shorter lists.
    common::mutations_are_safe("objid", &decode_ids, &g);
    common::assert_roundtrip("objid", &decode_ids, &g);
    for id in limit_ids() {
        let again = ObjId::from_raw(id.raw()).expect("round trip");
        assert_eq!(again, id);
        assert_eq!(again.epoch(), id.raw() >> 40);
        assert_eq!(again.counter(), id.raw() & ObjId::MAX_COUNTER);
    }
}

#[test]
fn objid_rejections() {
    assert!(ObjId::new(ObjId::EPOCH_LIMIT, 1).is_err());
    assert!(ObjId::new(0, 0).is_err());
    assert!(ObjId::new(0, ObjId::MAX_COUNTER + 1).is_err());
    assert!(ObjId::from_raw(0).is_err());
    assert!(
        ObjId::from_raw(1 << 40).is_err(),
        "zero counter is never allocated"
    );
    assert_eq!(ObjId::from_raw_opt(0).expect("none"), None);
    assert!(ObjId::from_raw_opt(1 << 40).is_err());
    assert!(objid(0, ObjId::MAX_COUNTER).next().is_err());
    assert_eq!(objid(3, 7).next().expect("next"), objid(3, 8));
}

#[test]
fn sequence_numbers_are_checked() {
    assert_eq!(WriteSeq(1).next().expect("next"), WriteSeq(2));
    assert!(WriteSeq(u64::MAX).next().is_err());
    assert!(CommitSeq(u64::MAX).checked_add(1).is_err());
    assert_eq!(CommitSeq(0).checked_add(5).expect("add"), CommitSeq(5));
}

#[test]
fn hint_golden() {
    let h = Hint {
        seg: objid(0, 1),
        off: 0x0123_4567,
        clen: 75,
    };
    let mut b = Vec::new();
    h.encode(&mut b);
    assert_eq!(b.len(), Hint::LEN);
    assert_eq!(
        b,
        common::hex("0100000000000000 67452301 4b000000"),
        "seg u64, off u32, clen u32, little endian"
    );
    let g = common::golden("01-hint.bin", &b);
    let rt = |x: &[u8]| -> Result<Vec<u8>, FormatError> {
        let h = Hint::decode(x)?;
        let mut o = Vec::new();
        h.encode(&mut o);
        Ok(o)
    };
    common::negative_suite("hint", &rt, &g);
    // A zero segment is "none", never a hint.
    assert!(Hint::decode(&[0u8; 16]).is_err());
}

#[test]
fn codec_level_kind_idlen() {
    for (v, c) in [
        (0, Codec::Raw),
        (1, Codec::Zstd),
        (2, Codec::Lz4),
        (3, Codec::ZstdDict),
    ] {
        assert_eq!(Codec::from_u16(v).expect("codec"), c);
        assert_eq!(c.id(), v);
    }
    assert!(Codec::from_u16(4).is_err());
    assert!(Codec::from_u16(u16::MAX).is_err());
    assert!(Level(0).validate(Codec::Raw).is_ok());
    assert!(Level(1).validate(Codec::Raw).is_err());
    assert!(Level(0).validate(Codec::Lz4).is_ok());
    assert!(Level(3).validate(Codec::Lz4).is_err());
    assert!(Level(-7).validate(Codec::Zstd).is_ok());
    assert!(Level(22).validate(Codec::ZstdDict).is_ok());
    assert!(Level(23).validate(Codec::Zstd).is_err());
    assert!(Level(-8).validate(Codec::ZstdDict).is_err());
    assert_eq!(ExtentKind::from_u8(1).expect("kind"), ExtentKind::Node);
    assert!(ExtentKind::from_u8(2).is_err());
    for v in [0u8, 16, 32] {
        assert_eq!(IdLen::new(v).expect("id_len").as_u8(), v);
    }
    for v in [1u8, 8, 15, 17, 31, 33, 255] {
        assert!(IdLen::new(v).is_err());
    }
    let id = ChunkId(std::array::from_fn(|i| i as u8));
    let p = IdPrefix::of(&id, IdLen::Sixteen);
    assert_eq!(p.as_bytes(), &id.0[..16]);
    assert!(p.matches(&id));
    let mut other = id;
    other.0[20] ^= 1;
    assert!(p.matches(&other), "bytes after the prefix do not matter");
    other.0[3] ^= 1;
    assert!(!p.matches(&other));
    assert!(IdPrefix::of(&id, IdLen::Zero).as_bytes().is_empty());
    assert!(IdPrefix::from_slice(IdLen::Sixteen, &[0; 15]).is_err());
}

#[test]
fn text_forms_golden() {
    let fam = FamilyId(common::VOLUME_A_FAMILY);
    let uuid = VolumeUuid(common::VOLUME_A_UUID);
    let mut t = String::new();
    t.push_str(&format!("family_id  {fam}\n"));
    t.push_str(&format!("volume_uuid {uuid}\n"));
    t.push_str(&format!("objid      {}\n", objid(0, 1)));
    t.push_str(&format!("objid      {}\n", objid(0, ObjId::MAX_COUNTER)));
    t.push_str(&format!("objid      {}\n", objid(0x00_0001, 0x2a)));
    t.push_str(&format!("commit_seq {}\n", CommitSeq(7)));
    t.push_str(&format!("write_seq  {}\n", WriteSeq(0x1_0000_0001)));
    assert_eq!(fam.to_string(), "404142434445464748494a4b4c4d4e4f");
    assert_eq!(uuid.to_string(), "a0a1a2a3-a4a5-46a7-88a9-aaabacadaeaf");
    assert_eq!(objid(1, 0x2a).to_string(), "000001000000002a");
    common::golden_text("01-text-forms.txt", &t);

    assert_eq!(fam.to_string().parse::<FamilyId>().expect("parse"), fam);
    assert_eq!(uuid.to_string().parse::<VolumeUuid>().expect("parse"), uuid);
    assert_eq!(
        "000001000000002a".parse::<ObjId>().expect("parse"),
        objid(1, 0x2a)
    );
}

#[test]
fn text_form_rejections() {
    // Uppercase, wrong length, wrong separators, non-hex.
    assert!("404142434445464748494A4B4C4D4E4F"
        .parse::<FamilyId>()
        .is_err());
    assert!("4041".parse::<FamilyId>().is_err());
    assert!("g0414243444546474849 4a4b4c4d4e4f"
        .parse::<FamilyId>()
        .is_err());
    assert!("a0a1a2a3-a4a5-46a7-88a9-aaabacadaeaf0"
        .parse::<VolumeUuid>()
        .is_err());
    assert!("a0a1a2a3a4a546a788a9aaabacadaeaf"
        .parse::<VolumeUuid>()
        .is_err());
    assert!("A0A1A2A3-A4A5-46A7-88A9-AAABACADAEAF"
        .parse::<VolumeUuid>()
        .is_err());
    assert!("a0a1a2a3_a4a5_46a7_88a9_aaabacadaeaf"
        .parse::<VolumeUuid>()
        .is_err());
    assert!("0000000000000000".parse::<ObjId>().is_err());
    assert!("000000000000002".parse::<ObjId>().is_err());
    let mut out = [0u8; 2];
    assert!(parse_hex_lower("zz00", &mut out).is_err());
    assert_eq!(hex_lower(&[0x0a, 0xff]), "0aff");
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(common::CASES))]

    #[test]
    fn objid_roundtrip(epoch in 0..ObjId::EPOCH_LIMIT, counter in 1..=ObjId::MAX_COUNTER) {
        let id = ObjId::new(epoch, counter).unwrap();
        prop_assert_eq!(ObjId::from_raw(id.raw()).unwrap(), id);
        prop_assert_eq!(id.epoch(), epoch);
        prop_assert_eq!(id.counter(), counter);
        prop_assert_eq!(id.to_string().parse::<ObjId>().unwrap(), id);
    }

    #[test]
    fn text_roundtrip(a in any::<[u8; 16]>()) {
        let f = FamilyId(a);
        prop_assert_eq!(f.to_string().parse::<FamilyId>().unwrap(), f);
        let u = VolumeUuid(a);
        prop_assert_eq!(u.to_string().parse::<VolumeUuid>().unwrap(), u);
    }

    #[test]
    fn hint_roundtrip(seg in 1..=u64::MAX, off in any::<u32>(), clen in any::<u32>()) {
        prop_assume!(ObjId::from_raw(seg).is_ok());
        let h = Hint { seg: ObjId::from_raw(seg).unwrap(), off, clen };
        let mut b = Vec::new();
        h.encode(&mut b);
        prop_assert_eq!(Hint::decode(&b).unwrap(), h);
    }
}
