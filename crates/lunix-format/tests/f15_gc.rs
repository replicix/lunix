//! Freeze item 15: `gc/<pass:016x>` (envelope kind 7, opaque body ver 1) and
//! the `state/gc/condemned` envelope (section 16).

mod common;

use common::*;
use lunix_format::envelope;
use lunix_format::gcfile::{Condemned, GcPassBody, CONDEMNED_MAX_BODY};
use lunix_format::purpose::Purpose;
use lunix_format::{FamilyId, FormatError, ObjectKind};
use proptest::prelude::*;

fn hmac_key() -> [u8; 32] {
    let msg = Purpose::IntegrityHmac
        .message(&FamilyId(VOLUME_B_FAMILY))
        .unwrap();
    purpose_key(&volume_b_ik(), &msg)
}

fn pass_object(p: &GcPassBody, pass: u64) -> Vec<u8> {
    let mut body = Vec::new();
    p.encode(&mut body).unwrap();
    let unsigned = envelope::form0_unsigned(ObjectKind::Gc, &body).unwrap();
    let aad =
        envelope::meta_aad(&VOLUME_B_UUID, ObjectKind::Gc, &format!("gc/{pass:016x}")).unwrap();
    let mac = hmac_sha256(&hmac_key(), &envelope::mac_input(&aad, &unsigned));
    p.encode_object_form0(&mac).unwrap()
}

fn rt_pass(b: &[u8]) -> Result<Vec<u8>, FormatError> {
    let (p, mac) = GcPassBody::decode_object(b)?;
    p.encode_object_form0(&mac)
}

fn rt_condemned(b: &[u8]) -> Result<Vec<u8>, FormatError> {
    let c = Condemned::decode(b)?;
    let mut v = Vec::new();
    c.encode(&mut v)?;
    Ok(v)
}

fn sample_pass() -> GcPassBody {
    GcPassBody {
        bytes:
            b"{\"pass\":3,\"event\":\"condemn\",\"id\":\"0000000000000007\"}\n{\"event\":\"end\"}\n"
                .to_vec(),
    }
}

fn sample_condemned() -> Condemned {
    Condemned {
        body_ver: 1,
        body: (0..70u8).collect(),
    }
}

#[test]
fn pass_object_golden() {
    let obj = pass_object(&sample_pass(), 3);
    assert_eq!(obj[5], 7, "kind gc");
    let g = golden("15-gc-pass.bin", &obj);
    negative_suite("gc pass", &rt_pass, &g);
    assert_eq!(GcPassBody::decode_object(&g).unwrap().0, sample_pass());
    // body is an opaque body with ver 1
    let mut b = Vec::new();
    sample_pass().encode(&mut b).unwrap();
    assert_eq!(&b[..4], &[1, 0, 0, 0]);
}

#[test]
fn pass_body_version_is_fixed() {
    let mut b = Vec::new();
    sample_pass().encode(&mut b).unwrap();
    let mut v2 = b.clone();
    v2[0] = 2;
    assert!(GcPassBody::decode(&v2).is_err());
    v2[0] = 0;
    assert!(GcPassBody::decode(&v2).is_err());
    assert!(GcPassBody::decode(&b).is_ok());
    // an empty pass body is legal
    let mut e = Vec::new();
    GcPassBody { bytes: vec![] }.encode(&mut e).unwrap();
    assert_eq!(GcPassBody::decode(&e).unwrap().bytes.len(), 0);
}

#[test]
fn pass_object_kind_is_checked() {
    let o = sample_pass().encode_object_form0(&[0; 32]).unwrap();
    let mut wrong = o.clone();
    wrong[5] = 5;
    assert!(GcPassBody::decode_object(&wrong).is_err());
    assert!(GcPassBody::decode_object(&o).is_ok());
}

#[test]
fn condemned_golden() {
    let mut b = Vec::new();
    sample_condemned().encode(&mut b).unwrap();
    assert_eq!(b.len(), 16 + 70 + 4);
    assert_eq!(&b[..8], b"LUNIXGCC");
    assert_eq!(&b[8..12], &[1, 0, 1, 0]);
    let want_crc = crc32c_of(&b[..b.len() - 4]);
    assert_eq!(&b[b.len() - 4..], &want_crc.to_le_bytes());
    let g = golden("15-gc-condemned-envelope.bin", &b);
    negative_suite("condemned", &rt_condemned, &g);
    assert_eq!(Condemned::decode(&g).unwrap(), sample_condemned());
}

/// Independent bitwise CRC-32C, so the test does not use the code under test.
fn crc32c_of(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0x82F6_3B78
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

#[test]
fn condemned_rejections() {
    let mut b = Vec::new();
    sample_condemned().encode(&mut b).unwrap();
    let with_crc = |mut x: Vec<u8>| {
        let n = x.len() - 4;
        let c = crc32c_of(&x[..n]);
        x[n..].copy_from_slice(&c.to_le_bytes());
        x
    };
    let dec = |x: &[u8]| Condemned::decode(x);
    assert!(dec(&b).is_ok());
    // bad crc
    let mut bad = b.clone();
    let n = bad.len();
    bad[n - 1] ^= 1;
    assert!(matches!(dec(&bad), Err(FormatError::Checksum { .. })));
    // flipped body byte
    let mut bad = b.clone();
    bad[20] ^= 1;
    assert!(matches!(dec(&bad), Err(FormatError::Checksum { .. })));
    // unknown envelope version, even with a correct crc
    let mut v = b.clone();
    v[8] = 2;
    assert!(matches!(
        dec(&with_crc(v)),
        Err(FormatError::BadVersion { .. })
    ));
    // bad magic
    let mut v = b.clone();
    v[0] = b'X';
    assert!(matches!(
        dec(&with_crc(v)),
        Err(FormatError::BadMagic { .. })
    ));
    // body_ver 0
    let mut v = b.clone();
    v[10] = 0;
    assert!(dec(&with_crc(v)).is_err());
    // unknown body_ver is fine for this layer
    let mut v = b.clone();
    v[10] = 9;
    assert_eq!(dec(&with_crc(v)).unwrap().body_ver, 9);
    // body_len claims
    for claim in [
        71u32,
        69,
        CONDEMNED_MAX_BODY,
        CONDEMNED_MAX_BODY + 1,
        u32::MAX,
    ] {
        let mut v = b.clone();
        v[12..16].copy_from_slice(&claim.to_le_bytes());
        let (r, allocated) = measure(|| dec(&with_crc(v.clone())));
        assert!(r.is_err(), "claim {claim}");
        assert!(allocated < 2 * v.len() + 64 * 1024 + 4096);
    }
    // trailing byte
    let mut t = b.clone();
    t.push(0);
    assert!(dec(&t).is_err());
    // empty body is legal
    let mut e = Vec::new();
    Condemned {
        body_ver: 1,
        body: vec![],
    }
    .encode(&mut e)
    .unwrap();
    assert_eq!(e.len(), 20);
    assert!(dec(&e).is_ok());
    // encoder limits
    assert!(Condemned {
        body_ver: 0,
        body: vec![]
    }
    .encode(&mut Vec::new())
    .is_err());
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(CASES))]
    #[test]
    fn roundtrip(bv in 1..=u16::MAX, body in prop::collection::vec(any::<u8>(), 0..400)) {
        let c = Condemned { body_ver: bv, body: body.clone() };
        let mut b = Vec::new();
        c.encode(&mut b).unwrap();
        prop_assert_eq!(Condemned::decode(&b).unwrap(), c);
        let p = GcPassBody { bytes: body };
        let obj = p.encode_object_form0(&[5; 32]).unwrap();
        let (back, mac) = GcPassBody::decode_object(&obj).unwrap();
        prop_assert_eq!(back, p);
        prop_assert_eq!(mac, [5u8; 32]);
    }

    #[test]
    fn arbitrary_bytes_never_panic(b in prop::collection::vec(any::<u8>(), 0..120)) {
        let _ = Condemned::decode(&b);
        let _ = GcPassBody::decode_object(&b);
    }
}
