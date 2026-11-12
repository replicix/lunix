//! Freeze item 7 (our part): metadata AAD, envelope forms 0 and 1, the pin
//! bodies (`snapshots/`, `readers/`, `holds/`) and the opaque bodies
//! (`heat/`, `dicts/`, `gc/space/`).
//!
//! Pin and opaque goldens are complete form 0 envelopes (integrity mode of
//! volume B, real HMACs); `07-envelope-form1.bin` is a sealed envelope with
//! stand-in cipher text.

mod common;

use common::*;
use lunix_format::envelope::{self, Envelope, EnvelopeBody};
use lunix_format::opaque::{self, OpaqueBody};
use lunix_format::pins::{Consistency, HoldPin, ReaderPin, SnapshotPin};
use lunix_format::purpose::Purpose;
use lunix_format::{CommitSeq, FamilyId, Form, FormatError, ObjectKind, TimeMs, VolumeUuid};
use proptest::prelude::*;

fn hmac_key() -> [u8; 32] {
    let msg = Purpose::IntegrityHmac
        .message(&FamilyId(VOLUME_B_FAMILY))
        .unwrap();
    purpose_key(&volume_b_ik(), &msg)
}

/// A form 0 object whose MAC is the real one for (volume B, kind, name).
fn sign(kind: ObjectKind, name: &str, body: &[u8]) -> Vec<u8> {
    let unsigned = envelope::form0_unsigned(kind, body).unwrap();
    let aad = envelope::meta_aad(&VOLUME_B_UUID, kind, name).unwrap();
    let mac = hmac_sha256(&hmac_key(), &envelope::mac_input(&aad, &unsigned));
    envelope::encode_form0(kind, body, &mac).unwrap()
}

fn rt_form0(kinds: &'static [ObjectKind]) -> impl Fn(&[u8]) -> Result<Vec<u8>, FormatError> {
    move |b| {
        let e = Envelope::decode(b, kinds, Form::Mac, u32::MAX)?;
        let EnvelopeBody::Mac { body, mac } = e.body else {
            unreachable!()
        };
        envelope::encode_form0(e.kind, body, &mac)
    }
}

fn rt_form1(kinds: &'static [ObjectKind]) -> impl Fn(&[u8]) -> Result<Vec<u8>, FormatError> {
    move |b| {
        let e = Envelope::decode(b, kinds, Form::Sealed, u32::MAX)?;
        let EnvelopeBody::Sealed {
            nonce,
            ciphertext,
            tag,
        } = e.body
        else {
            unreachable!()
        };
        envelope::encode_form1(e.kind, &nonce, ciphertext, &tag)
    }
}

// ---------------------------------------------------------------------------
// meta_aad

#[test]
fn meta_aad_golden_and_layout() {
    let name = "commits/0000000000000001";
    let aad = envelope::meta_aad(&VOLUME_B_UUID, ObjectKind::Commit, name).unwrap();
    let mut want = b"lunix meta v1\0".to_vec();
    want.extend_from_slice(&VOLUME_B_UUID);
    want.push(1);
    want.extend_from_slice(&(name.len() as u32).to_le_bytes());
    want.extend_from_slice(name.as_bytes());
    assert_eq!(aad, want);
    assert_eq!(
        &aad[..14],
        hex("6c 75 6e 69 78 20 6d 65 74 61 20 76 31 00").as_slice()
    );
    assert_eq!(aad.len(), 14 + 16 + 1 + 4 + 24);
    golden("07-meta-aad.bin", &aad);
}

#[test]
fn meta_aad_name_limits() {
    let id = [1u8; 16];
    assert!(envelope::meta_aad(&id, ObjectKind::Commit, "").is_err());
    assert!(envelope::meta_aad(&id, ObjectKind::Commit, "a").is_ok());
    assert!(envelope::meta_aad(&id, ObjectKind::Commit, &"a".repeat(1024)).is_ok());
    assert!(envelope::meta_aad(&id, ObjectKind::Commit, &"a".repeat(1025)).is_err());
    // The kind byte is part of the input.
    let a = envelope::meta_aad(&id, ObjectKind::Commit, "x").unwrap();
    let b = envelope::meta_aad(&id, ObjectKind::Delta, "x").unwrap();
    assert_ne!(a, b);
    assert_eq!(a[30], 1);
    assert_eq!(b[30], 3);
}

#[test]
fn mac_input_is_aad_then_signed_prefix() {
    let obj = sign(ObjectKind::Space, "gc/space/0000000000000005", &[1, 2, 3]);
    let e = Envelope::decode(&obj, &[ObjectKind::Space], Form::Mac, 100).unwrap();
    assert_eq!(e.signed, &obj[..12 + 3]);
    let aad = envelope::meta_aad(
        &VOLUME_B_UUID,
        ObjectKind::Space,
        "gc/space/0000000000000005",
    )
    .unwrap();
    let input = envelope::mac_input(&aad, e.signed);
    assert_eq!(input.len(), aad.len() + 15);
    assert_eq!(&input[..aad.len()], aad.as_slice());
    assert_eq!(&input[aad.len()..], e.signed);
    let EnvelopeBody::Mac { mac, .. } = e.body else {
        panic!()
    };
    assert_eq!(mac, hmac_sha256(&hmac_key(), &input));
}

// ---------------------------------------------------------------------------
// envelope forms

const KINDS_COMMIT: &[ObjectKind] = &[ObjectKind::Commit, ObjectKind::Delta];

#[test]
fn form0_golden() {
    let body: Vec<u8> = (0..37u8).collect();
    let obj = sign(ObjectKind::Space, "gc/space/0000000000000005", &body);
    assert_eq!(obj.len(), 37 + 44);
    assert_eq!(&obj[..12], hex("4c584d31 01 0c 00 00 25000000").as_slice());
    let g = golden("07-envelope-form0.bin", &obj);
    negative_suite("form0", &rt_form0(&[ObjectKind::Space]), &g);
}

#[test]
fn form1_golden() {
    let nonce: [u8; 24] = std::array::from_fn(|i| 0x40 + i as u8);
    let tag: [u8; 16] = std::array::from_fn(|i| 0xe0 + i as u8);
    let ct: Vec<u8> = (0..29u8).map(|i| i.wrapping_mul(11)).collect();
    let obj = envelope::encode_form1(ObjectKind::Commit, &nonce, &ct, &tag).unwrap();
    assert_eq!(obj.len(), 29 + 52);
    assert_eq!(&obj[..12], hex("4c584d31 01 01 01 00 1d000000").as_slice());
    let g = golden("07-envelope-form1.bin", &obj);
    negative_suite("form1", &rt_form1(KINDS_COMMIT), &g);
    let e = Envelope::decode(&g, KINDS_COMMIT, Form::Sealed, 1000).unwrap();
    assert_eq!(e.signed, &g[..12]);
    assert_eq!(
        e.body,
        EnvelopeBody::Sealed {
            nonce,
            ciphertext: &ct,
            tag
        }
    );
}

#[test]
fn envelope_rejections() {
    let obj = sign(ObjectKind::Lease, "lease", &[9; 10]);
    fn dec(b: &[u8]) -> Result<Envelope<'_>, FormatError> {
        Envelope::decode(b, &[ObjectKind::Lease], Form::Mac, 4096)
    }
    assert!(dec(&obj).is_ok());
    let with = |i: usize, v: u8| {
        let mut b = obj.clone();
        b[i] = v;
        b
    };
    assert!(dec(&with(0, b'X')).is_err(), "magic");
    assert!(dec(&with(4, 2)).is_err(), "env_ver");
    assert!(dec(&with(4, 0)).is_err(), "env_ver 0");
    for k in [0u8, 4, 14, 255] {
        assert!(dec(&with(5, k)).is_err(), "reserved kind {k}");
    }
    assert!(dec(&with(5, 1)).is_err(), "kind not in the list");
    assert!(dec(&with(6, 1)).is_err(), "form 1 where 0 is required");
    assert!(dec(&with(6, 2)).is_err(), "form 2 is reserved");
    assert!(dec(&with(7, 1)).is_err(), "reserved byte");
    // body_len over the kind maximum, with and without data behind it
    let mut big = obj.clone();
    big[8..12].copy_from_slice(&4097u32.to_le_bytes());
    assert!(dec(&big).is_err());
    big[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
    let (r, allocated) = measure(|| dec(&big));
    assert!(r.is_err());
    assert!(allocated < 1024);
    // body_len shorter / longer than the data
    let mut short = obj.clone();
    short[8..12].copy_from_slice(&9u32.to_le_bytes());
    assert!(dec(&short).is_err(), "trailing byte");
    let mut long = obj.clone();
    long[8..12].copy_from_slice(&11u32.to_le_bytes());
    assert!(dec(&long).is_err(), "truncated");
    // truncation at every length, and trailing bytes
    for n in 0..obj.len() {
        assert!(dec(&obj[..n]).is_err(), "prefix {n}");
    }
    let mut t = obj.clone();
    t.push(0);
    assert!(dec(&t).is_err());
    // a lower caller maximum applies
    assert!(Envelope::decode(&obj, &[ObjectKind::Lease], Form::Mac, 9).is_err());
    assert!(Envelope::decode(&obj, &[ObjectKind::Lease], Form::Mac, 10).is_ok());
    // form 0 object read as form 1
    assert!(Envelope::decode(&obj, &[ObjectKind::Lease], Form::Sealed, 4096).is_err());
}

#[test]
fn commit_kind_lists_accept_one_or_three() {
    for k in [ObjectKind::Commit, ObjectKind::Delta] {
        let obj = sign(k, "commits/0000000000000000", &[0; 4]);
        let e = Envelope::decode(&obj, KINDS_COMMIT, Form::Mac, 1 << 20).unwrap();
        assert_eq!(e.kind, k);
        assert!(Envelope::decode(&obj, &[ObjectKind::Lease], Form::Mac, 1 << 20).is_err());
    }
    let lease = sign(ObjectKind::Lease, "lease", &[0; 4]);
    assert!(Envelope::decode(&lease, KINDS_COMMIT, Form::Mac, 1 << 20).is_err());
}

#[test]
fn encoders_enforce_the_kind_maximum() {
    assert!(envelope::encode_form0(ObjectKind::Lease, &[0; 4096], &[0; 32]).is_ok());
    assert!(envelope::encode_form0(ObjectKind::Lease, &[0; 4097], &[0; 32]).is_err());
    assert!(envelope::encode_form1(ObjectKind::Holds, &[0; 24], &[0; 4097], &[0; 16]).is_err());
    assert!(envelope::form0_unsigned(ObjectKind::Readers, &[0; 4097]).is_err());
}

#[test]
fn the_keyring_kind_has_no_envelope() {
    assert!(envelope::encode_form0(ObjectKind::Keyring, &[], &[0; 32]).is_err());
    assert!(envelope::encode_form1(ObjectKind::Keyring, &[0; 24], &[], &[0; 16]).is_err());
    let mut raw = envelope::encode_form0(ObjectKind::Lease, &[], &[0; 32]).unwrap();
    raw[5] = 13;
    assert!(Envelope::decode(&raw, &[ObjectKind::Keyring], Form::Mac, 100).is_err());
}

#[test]
fn every_defined_kind_round_trips_its_id() {
    for k in ObjectKind::ALL {
        assert_eq!(ObjectKind::from_u8(k.id()).unwrap(), k);
    }
    for v in [0u8, 4, 14, 100, 255] {
        assert!(ObjectKind::from_u8(v).is_err());
    }
}

// ---------------------------------------------------------------------------
// pins

fn snapshot() -> SnapshotPin {
    SnapshotPin {
        consistency: Consistency::FsFrozen,
        commit_seq: CommitSeq(12),
        created: TimeMs(CREATED_AT + 5),
        label: "before upgrade".into(),
    }
}

#[test]
fn pin_goldens() {
    let hold_uuid = VolumeUuid(VOLUME_A_UUID);
    let snap = snapshot();
    let mut body = Vec::new();
    snap.encode(&mut body).unwrap();
    assert_eq!(body.len(), 24 + 14);
    let obj = sign(ObjectKind::Snapshots, "snapshots/nightly", &body);
    let g = golden("07-pin-snapshot.bin", &obj);
    let rt = |b: &[u8]| {
        let (p, mac) = SnapshotPin::decode_object(b)?;
        p.encode_object_form0(&mac)
    };
    negative_suite("snapshot pin", &rt, &g);
    assert_eq!(SnapshotPin::decode_object(&g).unwrap().0, snap);

    let rp = ReaderPin {
        commit_seq: CommitSeq(9),
        deadline: TimeMs(CREATED_AT + 60_000),
    };
    let mut body = Vec::new();
    rp.encode(&mut body);
    assert_eq!(body.len(), 24);
    let obj = sign(
        ObjectKind::Readers,
        "readers/node-1.example-00000000000000a1",
        &body,
    );
    let g = golden("07-pin-reader.bin", &obj);
    let rt = |b: &[u8]| {
        let (p, mac) = ReaderPin::decode_object(b)?;
        p.encode_object_form0(&mac)
    };
    negative_suite("reader pin", &rt, &g);
    assert_eq!(ReaderPin::decode_object(&g).unwrap().0, rp);

    let hp = HoldPin {
        base_seq: CommitSeq(77),
    };
    let mut body = Vec::new();
    hp.encode(&mut body);
    assert_eq!(body.len(), 16);
    let obj = sign(ObjectKind::Holds, &format!("holds/{hold_uuid}"), &body);
    let g = golden("07-pin-hold.bin", &obj);
    let rt = |b: &[u8]| {
        let (p, mac) = HoldPin::decode_object(b)?;
        p.encode_object_form0(&mac)
    };
    negative_suite("hold pin", &rt, &g);
    assert_eq!(HoldPin::decode_object(&g).unwrap().0, hp);
}

#[test]
fn pin_objects_do_not_cross_kinds() {
    let rp = ReaderPin {
        commit_seq: CommitSeq(1),
        deadline: TimeMs(2),
    };
    let obj = rp.encode_object_form0(&[0; 32]).unwrap();
    assert!(ReaderPin::decode_object(&obj).is_ok());
    assert!(HoldPin::decode_object(&obj).is_err());
    assert!(SnapshotPin::decode_object(&obj).is_err());
}

#[test]
fn pin_body_rejections() {
    let mut b = Vec::new();
    snapshot().encode(&mut b).unwrap();
    let mutated = |i: usize, v: &[u8]| {
        let mut x = b.clone();
        x[i..i + v.len()].copy_from_slice(v);
        SnapshotPin::decode(&x)
    };
    assert!(SnapshotPin::decode(&b).is_ok());
    assert!(mutated(0, &[2]).is_err(), "body_ver");
    assert!(mutated(1, &[0]).is_err(), "consistency 0");
    assert!(mutated(1, &[3]).is_err(), "consistency 3");
    assert!(mutated(4, &[1]).is_err(), "reserved");
    assert!(
        mutated(2, &15u16.to_le_bytes()).is_err(),
        "label_len beyond data / trailing"
    );
    assert!(
        mutated(2, &1025u16.to_le_bytes()).is_err(),
        "label_len over the maximum"
    );
    assert!(mutated(24, &[0]).is_err(), "NUL in the label");
    assert!(mutated(24, &[0xff]).is_err(), "not UTF-8");

    let mut p = snapshot();
    p.label = "x".repeat(1024);
    assert!(p.encode(&mut Vec::new()).is_ok());
    p.label.push('x');
    assert!(p.encode(&mut Vec::new()).is_err());
    p.label = "a\0b".into();
    assert!(p.encode(&mut Vec::new()).is_err());
    p.label = String::new();
    let mut e = Vec::new();
    p.encode(&mut e).unwrap();
    assert_eq!(e.len(), 24);
    assert!(SnapshotPin::decode(&e).is_ok());

    let mut r = Vec::new();
    ReaderPin {
        commit_seq: CommitSeq(1),
        deadline: TimeMs(2),
    }
    .encode(&mut r);
    for n in 0..r.len() {
        assert!(ReaderPin::decode(&r[..n]).is_err());
    }
    let mut x = r.clone();
    x[3] = 1;
    assert!(ReaderPin::decode(&x).is_err(), "reserved");
    x = r.clone();
    x[0] = 0;
    assert!(ReaderPin::decode(&x).is_err(), "body_ver");
    x = r;
    x.push(0);
    assert!(ReaderPin::decode(&x).is_err(), "trailing");

    let mut h = Vec::new();
    HoldPin {
        base_seq: CommitSeq(1),
    }
    .encode(&mut h);
    for n in 0..h.len() {
        assert!(HoldPin::decode(&h[..n]).is_err());
    }
    h[7] = 1;
    assert!(HoldPin::decode(&h).is_err());
}

// ---------------------------------------------------------------------------
// opaque bodies

fn opaque_sample(seed: u8, n: usize) -> OpaqueBody {
    OpaqueBody {
        ver: 1 + u16::from(seed),
        bytes: (0..n).map(|i| (i as u8).wrapping_add(seed)).collect(),
    }
}

#[test]
fn opaque_goldens() {
    let cases = [
        (
            "07-opaque-heat.bin",
            ObjectKind::Heat,
            "heat/0000000000000005",
            opaque_sample(1, 33),
        ),
        (
            "07-opaque-dicts.bin",
            ObjectKind::Dicts,
            "dicts/0000002a",
            opaque_sample(0, 100),
        ),
        (
            "07-opaque-space.bin",
            ObjectKind::Space,
            "gc/space/0000000000000005",
            opaque_sample(2, 0),
        ),
    ];
    for (file, kind, name, body) in cases {
        let mut b = Vec::new();
        body.encode(&mut b).unwrap();
        assert_eq!(b.len(), 8 + body.bytes.len());
        let obj = sign(kind, name, &b);
        let g = golden(file, &obj);
        let rt = move |x: &[u8]| {
            let (o, mac) = OpaqueBody::decode_object(x, kind)?;
            o.encode_object_form0(kind, &mac)
        };
        negative_suite(file, &rt, &g);
        assert_eq!(OpaqueBody::decode_object(&g, kind).unwrap().0, body);
    }
}

#[test]
fn opaque_rejections() {
    let mut b = Vec::new();
    opaque_sample(0, 16).encode(&mut b).unwrap();
    let mutated = |i: usize, v: &[u8]| {
        let mut x = b.clone();
        x[i..i + v.len()].copy_from_slice(v);
        OpaqueBody::decode(&x)
    };
    assert!(OpaqueBody::decode(&b).is_ok());
    assert!(mutated(0, &[0, 0]).is_err(), "ver 0");
    assert!(mutated(2, &[1]).is_err(), "reserved");
    assert!(mutated(4, &17u32.to_le_bytes()).is_err(), "len beyond data");
    assert!(mutated(4, &15u32.to_le_bytes()).is_err(), "trailing");
    let (r, allocated) = measure(|| mutated(4, &opaque::MAX_LEN.to_le_bytes()));
    assert!(r.is_err());
    assert!(allocated < 1024, "no allocation from an unchecked length");
    assert!(mutated(4, &(opaque::MAX_LEN + 1).to_le_bytes()).is_err());
    for n in 0..b.len() {
        assert!(OpaqueBody::decode(&b[..n]).is_err());
    }
    assert!(OpaqueBody {
        ver: 0,
        bytes: vec![]
    }
    .encode(&mut Vec::new())
    .is_err());
    let o = opaque_sample(0, 4);
    assert!(
        o.encode_object_form0(ObjectKind::Commit, &[0; 32]).is_err(),
        "kind has no opaque body"
    );
    let obj = o.encode_object_form0(ObjectKind::Heat, &[0; 32]).unwrap();
    assert!(
        OpaqueBody::decode_object(&obj, ObjectKind::Dicts).is_err(),
        "wrong kind"
    );
}

// ---------------------------------------------------------------------------
// properties

fn cfg() -> ProptestConfig {
    ProptestConfig::with_cases(CASES)
}

fn arb_kind() -> impl Strategy<Value = ObjectKind> {
    prop::sample::select(
        ObjectKind::ALL
            .iter()
            .copied()
            .filter(|k| *k != ObjectKind::Keyring)
            .collect::<Vec<_>>(),
    )
}

proptest! {
    #![proptest_config(cfg())]

    #[test]
    fn form0_roundtrip(kind in arb_kind(), body in prop::collection::vec(any::<u8>(), 0..300), mac in any::<[u8; 32]>()) {
        let obj = envelope::encode_form0(kind, &body, &mac).unwrap();
        prop_assert_eq!(obj.len(), body.len() + 44);
        let e = Envelope::decode(&obj, &[kind], Form::Mac, u32::MAX).unwrap();
        prop_assert_eq!(e.kind, kind);
        prop_assert_eq!(e.body, EnvelopeBody::Mac { body: &body, mac });
    }

    #[test]
    fn form1_roundtrip(kind in arb_kind(), ct in prop::collection::vec(any::<u8>(), 0..300), n in any::<[u8; 24]>(), t in any::<[u8; 16]>()) {
        let obj = envelope::encode_form1(kind, &n, &ct, &t).unwrap();
        prop_assert_eq!(obj.len(), ct.len() + 52);
        let e = Envelope::decode(&obj, &[kind], Form::Sealed, u32::MAX).unwrap();
        prop_assert_eq!(e.body, EnvelopeBody::Sealed { nonce: n, ciphertext: &ct, tag: t });
    }

    #[test]
    fn meta_aad_shape(id in any::<[u8; 16]>(), kind in arb_kind(), name in "[a-z0-9/._-]{1,200}") {
        let a = envelope::meta_aad(&id, kind, &name).unwrap();
        prop_assert_eq!(a.len(), 35 + name.len());
        prop_assert_eq!(&a[14..30], &id[..]);
        prop_assert_eq!(a[30], kind.id());
        prop_assert_eq!(&a[35..], name.as_bytes());
    }

    #[test]
    fn snapshot_roundtrip(c in prop::sample::select(vec![Consistency::Crash, Consistency::FsFrozen]), s in any::<u64>(), t in any::<u64>(), label in "[^\0]{0,100}") {
        let p = SnapshotPin { consistency: c, commit_seq: CommitSeq(s), created: TimeMs(t), label };
        let mut b = Vec::new();
        p.encode(&mut b).unwrap();
        prop_assert_eq!(SnapshotPin::decode(&b).unwrap(), p);
    }

    #[test]
    fn reader_hold_roundtrip(s in any::<u64>(), t in any::<u64>()) {
        let r = ReaderPin { commit_seq: CommitSeq(s), deadline: TimeMs(t) };
        let mut b = Vec::new();
        r.encode(&mut b);
        prop_assert_eq!(ReaderPin::decode(&b).unwrap(), r);
        let h = HoldPin { base_seq: CommitSeq(s) };
        let mut b = Vec::new();
        h.encode(&mut b);
        prop_assert_eq!(HoldPin::decode(&b).unwrap(), h);
    }

    #[test]
    fn opaque_roundtrip(ver in 1..=u16::MAX, bytes in prop::collection::vec(any::<u8>(), 0..300)) {
        let o = OpaqueBody { ver, bytes };
        let mut b = Vec::new();
        o.encode(&mut b).unwrap();
        prop_assert_eq!(OpaqueBody::decode(&b).unwrap(), o);
    }

    #[test]
    fn arbitrary_bytes_never_panic(b in prop::collection::vec(any::<u8>(), 0..200)) {
        let _ = Envelope::decode(&b, KINDS_COMMIT, Form::Mac, u32::MAX);
        let _ = Envelope::decode(&b, KINDS_COMMIT, Form::Sealed, u32::MAX);
        let _ = SnapshotPin::decode(&b);
        let _ = ReaderPin::decode(&b);
        let _ = HoldPin::decode(&b);
        let _ = OpaqueBody::decode(&b);
    }
}

#[test]
fn body_len_is_capped_per_kind_even_with_a_looser_caller_bound() {
    // A kind-1 commit envelope claiming 4 MiB + 1: the caller's bound is the
    // delta maximum (68 MiB), the kind's own maximum (4 MiB) must still apply.
    let over = (4u32 << 20) + 1;
    let mut b = b"LXM1\x01\x01\x00\x00".to_vec();
    b.extend_from_slice(&over.to_le_bytes());
    let loose = ObjectKind::Delta.max_body_len();
    let r = Envelope::decode(
        &b,
        &[ObjectKind::Commit, ObjectKind::Delta],
        Form::Mac,
        loose,
    );
    assert!(r.is_err());
    assert!(lunix_format::commit::decode_envelope(&b, Form::Mac).is_err());
    // A delta (kind 3) may exceed 4 MiB: the length check passes and the
    // input is then truncated.
    let mut d = b"LXM1\x01\x03\x00\x00".to_vec();
    d.extend_from_slice(&over.to_le_bytes());
    let e = lunix_format::commit::decode_envelope(&d, Form::Mac).unwrap_err();
    assert!(matches!(e, FormatError::Truncated { .. }), "{e}");
}
