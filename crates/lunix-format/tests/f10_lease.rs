//! Freeze item 10: the lease body (section 13). Goldens are form 0 envelopes
//! of kind 2 named `lease` with the real HMAC of volume B.

mod common;

use common::*;
use lunix_format::envelope;
use lunix_format::lease::Lease;
use lunix_format::purpose::Purpose;
use lunix_format::{Epoch, FamilyId, FormatError, ObjectKind, TimeMs};
use proptest::prelude::*;

fn hmac_key() -> [u8; 32] {
    let msg = Purpose::IntegrityHmac
        .message(&FamilyId(VOLUME_B_FAMILY))
        .unwrap();
    purpose_key(&volume_b_ik(), &msg)
}

fn signed_object(l: &Lease) -> Vec<u8> {
    let mut body = Vec::new();
    l.encode(&mut body).unwrap();
    let unsigned = envelope::form0_unsigned(ObjectKind::Lease, &body).unwrap();
    let aad = envelope::meta_aad(&VOLUME_B_UUID, ObjectKind::Lease, "lease").unwrap();
    let mac = hmac_sha256(&hmac_key(), &envelope::mac_input(&aad, &unsigned));
    l.encode_object_form0(&mac).unwrap()
}

fn rt(b: &[u8]) -> Result<Vec<u8>, FormatError> {
    let (l, mac) = Lease::decode_object(b)?;
    l.encode_object_form0(&mac)
}

fn plain() -> Lease {
    Lease {
        released: false,
        epoch: Epoch(4),
        deadline: TimeMs(CREATED_AT + 30_000),
        host: "node-7.example".into(),
        cluster: "prod-east".into(),
        transferred_to: None,
    }
}

fn transferred() -> Lease {
    Lease {
        released: true,
        transferred_to: Some("s3-mirror".into()),
        cluster: String::new(),
        ..plain()
    }
}

#[test]
fn goldens() {
    for (file, l, len) in [
        ("10-lease.bin", plain(), 24 + 14 + 9),
        ("10-lease-transferred.bin", transferred(), 24 + 14 + 9),
    ] {
        let obj = signed_object(&l);
        assert_eq!(obj.len(), 44 + len, "{file}");
        let g = golden(file, &obj);
        negative_suite(file, &rt, &g);
        assert_eq!(Lease::decode_object(&g).unwrap().0, l);
    }
}

#[test]
fn layout() {
    let mut b = Vec::new();
    plain().encode(&mut b).unwrap();
    assert_eq!(b[0], 1);
    assert_eq!(b[1], 0);
    assert_eq!(u16::from_le_bytes([b[2], b[3]]), 14);
    assert_eq!(u16::from_le_bytes([b[4], b[5]]), 9);
    assert_eq!(u16::from_le_bytes([b[6], b[7]]), 0);
    assert_eq!(u64::from_le_bytes(b[8..16].try_into().unwrap()), 4);
    assert_eq!(
        u64::from_le_bytes(b[16..24].try_into().unwrap()),
        CREATED_AT + 30_000
    );
    assert_eq!(&b[24..], b"node-7.exampleprod-east");
    assert_eq!(b.len(), 24 + 14 + 9);
}

#[test]
fn rejections() {
    let mut b = Vec::new();
    transferred().encode(&mut b).unwrap();
    let m = |i: usize, v: &[u8]| {
        let mut x = b.clone();
        x[i..i + v.len()].copy_from_slice(v);
        Lease::decode(&x)
    };
    assert!(Lease::decode(&b).is_ok());
    assert!(m(0, &[2]).is_err(), "body_ver");
    assert!(m(1, &[2]).is_err(), "released 2");
    assert!(m(2, &0u16.to_le_bytes()).is_err(), "host_len 0");
    assert!(m(2, &65u16.to_le_bytes()).is_err(), "host_len 65");
    assert!(m(4, &256u16.to_le_bytes()).is_err(), "cluster_len 256");
    assert!(
        m(4, &1u16.to_le_bytes()).is_err(),
        "lengths no longer match the data"
    );
    assert!(m(6, &65u16.to_le_bytes()).is_err(), "transferred_to_len 65");
    assert!(
        m(6, &0u16.to_le_bytes()).is_err(),
        "transferred_to bytes left over"
    );
    assert!(m(24, b"N").is_err(), "uppercase host");
    assert!(m(24, &[0xff]).is_err(), "host not UTF-8");
    assert!(m(24 + 14, b"S").is_err(), "uppercase backend id");
    for n in 0..b.len() {
        assert!(Lease::decode(&b[..n]).is_err(), "prefix {n}");
    }
    let mut t = b.clone();
    t.push(b'x');
    assert!(Lease::decode(&t).is_err());
}

#[test]
fn encode_validates() {
    let enc = |l: &Lease| l.encode(&mut Vec::new());
    assert!(enc(&plain()).is_ok());
    let mut l = plain();
    l.host = String::new();
    assert!(enc(&l).is_err());
    l.host = "A".into();
    assert!(enc(&l).is_err());
    l.host = "h".repeat(65);
    assert!(enc(&l).is_err());
    let mut l = plain();
    l.cluster = "c".repeat(255);
    assert!(enc(&l).is_ok());
    l.cluster.push('c');
    assert!(enc(&l).is_err());
    l.cluster = "a\0".into();
    assert!(enc(&l).is_err());
    let mut l = plain();
    l.transferred_to = Some(String::new());
    assert!(enc(&l).is_err());
    l.transferred_to = Some("X".into());
    assert!(enc(&l).is_err());
}

#[test]
fn object_kind_is_checked() {
    let obj = plain().encode_object_form0(&[0; 32]).unwrap();
    assert_eq!(obj[5], 2);
    let mut other = obj.clone();
    other[5] = 3;
    assert!(Lease::decode_object(&other).is_err());
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(CASES))]
    #[test]
    fn roundtrip(
        released in any::<bool>(),
        epoch in any::<u64>(),
        deadline in any::<u64>(),
        host in "[a-z0-9.-]{1,64}",
        cluster in "[^\0]{0,60}",
        to in prop::option::of("[a-z0-9._-]{1,64}"),
    ) {
        prop_assume!(cluster.len() <= 255);
        let l = Lease { released, epoch: Epoch(epoch), deadline: TimeMs(deadline), host, cluster, transferred_to: to };
        let mut b = Vec::new();
        l.encode(&mut b).unwrap();
        prop_assert_eq!(Lease::decode(&b).unwrap(), l.clone());
        let obj = l.encode_object_form0(&[3; 32]).unwrap();
        let (back, mac) = Lease::decode_object(&obj).unwrap();
        prop_assert_eq!(back, l);
        prop_assert_eq!(mac, [3; 32]);
    }

    #[test]
    fn arbitrary_bytes_never_panic(b in prop::collection::vec(any::<u8>(), 0..120)) {
        let _ = Lease::decode(&b);
        let _ = Lease::decode_object(&b);
    }
}
