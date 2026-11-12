//! Freeze item 9: the keyring.
mod common;

use common::*;
use lunix_format::keyring::*;
use lunix_format::purpose::Purpose;
use lunix_format::{DekGen, FamilyId, FormatError};
use proptest::prelude::*;

fn fam() -> FamilyId {
    FamilyId(VOLUME_A_FAMILY)
}

fn key_wrap(fill: u8) -> SlotWrap {
    SlotWrap::Key {
        wrap_ver: 1,
        nonce: kat("xchacha_nonce").try_into().unwrap(),
        ciphertext: [fill; 32],
        tag: [fill ^ 0xff; 16],
    }
}

fn pass_params(m: u32, t: u32, p: u32) -> Argon2Params {
    Argon2Params {
        m_kib: m,
        t,
        p,
        salt: [0x11; 16],
    }
}

fn passphrase_keyring() -> Keyring {
    let mut k = Keyring {
        root_kind: RootKind::Kmk,
        dek_gen: DekGen(7),
        family_id: fam(),
        keyring_seq: 3,
        next_slot_id: 2,
        slots: vec![Slot {
            slot_id: 1,
            params: SlotParams::Passphrase(pass_params(19_456, 2, 1)),
            wrap: key_wrap(0xaa),
        }],
        mac: [0; 32],
    };
    let key = purpose_key(&kat32("kmk"), &Purpose::KeyringMac.message(&fam()).unwrap());
    k.mac = hmac_sha256(&key, &k.mac_input().unwrap());
    k
}

fn mixed_keyring() -> Keyring {
    let mut k = Keyring {
        root_kind: RootKind::Ik,
        dek_gen: DekGen(0),
        family_id: FamilyId(VOLUME_B_FAMILY),
        keyring_seq: 9,
        next_slot_id: 6,
        slots: vec![
            Slot {
                slot_id: 1,
                params: SlotParams::KeyFile,
                wrap: key_wrap(0x01),
            },
            Slot {
                slot_id: 3,
                params: SlotParams::Passphrase(pass_params(65_536, 3, 4)),
                wrap: key_wrap(0x03),
            },
            Slot {
                slot_id: 5,
                params: SlotParams::Kms {
                    key_ref: "arn:aws:kms:eu-west-1:123456789012:key/abc".into(),
                },
                wrap: SlotWrap::Kms((0..100u8).collect()),
            },
        ],
        mac: [0; 32],
    };
    let key = purpose_key(
        &volume_b_ik(),
        &Purpose::KeyringMac.message(&k.family_id).unwrap(),
    );
    k.mac = hmac_sha256(&key, &k.mac_input().unwrap());
    k
}

fn enc(k: &Keyring) -> Vec<u8> {
    let mut o = Vec::new();
    k.encode(&mut o).expect("encode");
    o
}

fn rt(b: &[u8]) -> Result<Vec<u8>, FormatError> {
    let k = Keyring::decode(b)?;
    let mut o = Vec::new();
    k.encode(&mut o)?;
    Ok(o)
}

#[test]
fn goldens_and_negative_suites() {
    let k = passphrase_keyring();
    let g = golden("09-keyring-passphrase.bin", &enc(&k));
    assert_eq!(Keyring::decode(&g).unwrap(), k);
    assert_eq!(g.len(), 48 + 12 + 28 + 73 + 32);
    negative_suite("passphrase", &rt, &g);

    let k = mixed_keyring();
    let g = golden("09-keyring-keyfile-and-kms.bin", &enc(&k));
    assert_eq!(Keyring::decode(&g).unwrap(), k);
    negative_suite("keyfile+kms", &rt, &g);
}

#[test]
fn slot_aad_matches_the_kat() {
    let k = passphrase_keyring();
    let aad = k.slots[0].aad(&fam());
    assert_eq!(aad, kat("slot_aad_pass"));
    golden("09-slot-aad-kat.bin", &aad);
    // Other kinds.
    let m = mixed_keyring();
    let a = m.slots[0].aad(&m.family_id);
    assert_eq!(a.len(), 14 + 16 + 4 + 1);
    let a = m.slots[2].aad(&m.family_id);
    let r = "arn:aws:kms:eu-west-1:123456789012:key/abc";
    assert_eq!(&a[35..39], &(r.len() as u32).to_le_bytes());
    assert_eq!(&a[39..], r.as_bytes());
}

#[test]
fn mac_input_is_aad_then_body() {
    let k = passphrase_keyring();
    let mi = k.mac_input().unwrap();
    let name = format!("families/{}/keyring", "404142434445464748494a4b4c4d4e4f");
    let aad = lunix_format::envelope::meta_aad(
        &VOLUME_A_FAMILY,
        lunix_format::ObjectKind::Keyring,
        &name,
    )
    .unwrap();
    assert_eq!(&mi[..aad.len()], &aad[..]);
    assert_eq!(&mi[aad.len()..], &enc(&k)[..enc(&k).len() - 32]);
    assert_eq!(aad[30], 13);
}

fn patched(k: &Keyring, off: usize, bytes: &[u8]) -> Vec<u8> {
    let mut b = enc(k);
    b[off..off + bytes.len()].copy_from_slice(bytes);
    b
}

#[test]
fn rejects_violations() {
    let k = passphrase_keyring();
    let r = |b: Vec<u8>, w: &str| assert_rejects(w, &rt, &b);
    r(patched(&k, 0, b"LUNIXKRX"), "magic");
    r(patched(&k, 8, &[2, 0]), "ver");
    r(patched(&k, 10, &[3]), "root_kind 3");
    r(patched(&k, 10, &[0]), "root_kind 0");
    r(patched(&k, 11, &[0]), "slot_count 0");
    r(patched(&k, 11, &[2]), "slot_count above slots");
    r(patched(&k, 11, &[33]), "slot_count 33");
    for i in 44..48 {
        r(patched(&k, i, &[1]), "reserved header");
    }
    r(patched(&k, 32, &0u64.to_le_bytes()), "keyring_seq 0");
    r(
        patched(&k, 40, &1u32.to_le_bytes()),
        "next_slot_id not above",
    );
    r(patched(&k, 48, &0u32.to_le_bytes()), "slot_id 0");
    r(patched(&k, 52, &[4]), "slot kind 4");
    r(patched(&k, 52, &[5]), "slot kind 5");
    r(patched(&k, 52, &[2]), "kind 2 with 28-byte params");
    r(patched(&k, 53, &[1]), "slot reserved");
    r(patched(&k, 58, &[1]), "slot reserved 2");
    r(patched(&k, 54, &27u16.to_le_bytes()), "params_len 27");
    r(patched(&k, 56, &72u16.to_le_bytes()), "wrap_len 72");
    r(patched(&k, 88, &[2]), "wrap_ver 2");
    // IK keyring with a DEK generation.
    let m = mixed_keyring();
    r(patched(&m, 12, &[1, 0, 0, 0]), "IK with dek_gen");
    // Argon2id read bounds.
    r(
        patched(&k, 60, &(2_000_000u32).to_le_bytes()),
        "m_kib too large",
    );
    r(patched(&k, 64, &0u32.to_le_bytes()), "t 0");
    r(patched(&k, 64, &11u32.to_le_bytes()), "t 11");
    r(patched(&k, 68, &0u32.to_le_bytes()), "p 0");
    r(patched(&k, 68, &17u32.to_le_bytes()), "p 17");
    r(patched(&k, 60, &4u32.to_le_bytes()), "m_kib below 8p");
    // Too long.
    r(vec![0u8; MAX_LEN + 1], "over 192 KiB");
}

#[test]
fn rejects_unsorted_and_duplicate_slots() {
    let mut k = mixed_keyring();
    k.slots.swap(0, 1);
    assert!(k.encode(&mut Vec::new()).is_err());
    let mut k = mixed_keyring();
    k.slots[1].slot_id = 1;
    assert!(k.encode(&mut Vec::new()).is_err());
    let mut k = mixed_keyring();
    k.next_slot_id = 5;
    assert!(k.encode(&mut Vec::new()).is_err());
    let mut k = mixed_keyring();
    k.slots.clear();
    assert!(k.encode(&mut Vec::new()).is_err());
    // Kind/wrap disagreement.
    let mut k = mixed_keyring();
    k.slots[0].wrap = SlotWrap::Kms(vec![1]);
    assert!(k.encode(&mut Vec::new()).is_err());
    // Empty key_ref.
    let mut k = mixed_keyring();
    k.slots[2].params = SlotParams::Kms {
        key_ref: String::new(),
    };
    assert!(k.encode(&mut Vec::new()).is_err());
}

#[test]
fn thirty_two_slots_fit_and_thirty_three_do_not() {
    let mk = |n: u32| {
        let mut k = passphrase_keyring();
        k.slots = (1..=n)
            .map(|i| Slot {
                slot_id: i,
                params: SlotParams::KeyFile,
                wrap: key_wrap(i as u8),
            })
            .collect();
        k.next_slot_id = n + 1;
        k
    };
    let b = enc(&mk(32));
    assert_eq!(Keyring::decode(&b).unwrap(), mk(32));
    assert!(mk(33).encode(&mut Vec::new()).is_err());
}

#[test]
fn argon2_bounds() {
    assert!(pass_params(19_456, 2, 1).check_write_bounds().is_ok());
    assert!(pass_params(19_455, 2, 1).check_write_bounds().is_err());
    assert!(pass_params(19_456, 1, 1).check_write_bounds().is_err());
    assert!(pass_params(1_048_576, 10, 16).check_read_bounds().is_ok());
    assert!(pass_params(1_048_577, 10, 16).check_read_bounds().is_err());
    assert!(pass_params(8, 1, 1).check_read_bounds().is_ok());
    assert!(pass_params(7, 1, 1).check_read_bounds().is_err());
    // Below the write bounds still decodes and round-trips (old keyrings).
    let mut k = passphrase_keyring();
    k.slots[0].params = SlotParams::Passphrase(pass_params(1024, 1, 1));
    assert_eq!(Keyring::decode(&enc(&k)).unwrap(), k);
}

fn arb_slot(id: u32) -> impl Strategy<Value = Slot> {
    prop_oneof![
        (
            any::<[u8; 16]>(),
            8u32..=4096,
            1u32..=10,
            1u32..=1,
            any::<[u8; 24]>(),
            any::<[u8; 32]>(),
            any::<[u8; 16]>()
        )
            .prop_map(move |(salt, m, t, p, nonce, ciphertext, tag)| Slot {
                slot_id: id,
                params: SlotParams::Passphrase(Argon2Params {
                    m_kib: m,
                    t,
                    p,
                    salt
                }),
                wrap: SlotWrap::Key {
                    wrap_ver: 1,
                    nonce,
                    ciphertext,
                    tag
                },
            }),
        (any::<[u8; 24]>(), any::<[u8; 32]>(), any::<[u8; 16]>()).prop_map(
            move |(nonce, ciphertext, tag)| Slot {
                slot_id: id,
                params: SlotParams::KeyFile,
                wrap: SlotWrap::Key {
                    wrap_ver: 1,
                    nonce,
                    ciphertext,
                    tag
                },
            }
        ),
        (
            "[a-z0-9:/-]{1,60}",
            prop::collection::vec(any::<u8>(), 1..200)
        )
            .prop_map(move |(r, w)| Slot {
                slot_id: id,
                params: SlotParams::Kms { key_ref: r },
                wrap: SlotWrap::Kms(w),
            }),
    ]
}

fn arb_keyring() -> impl Strategy<Value = Keyring> {
    (1usize..6).prop_flat_map(|n| {
        let slots: Vec<_> = (0..n).map(|i| arb_slot(1 + 2 * i as u32)).collect();
        (
            slots,
            any::<bool>(),
            any::<u32>(),
            any::<[u8; 16]>(),
            1u64..,
            any::<[u8; 32]>(),
            0u32..10,
        )
            .prop_map(move |(slots, kmk, dek, fam, seq, mac, extra)| Keyring {
                root_kind: if kmk { RootKind::Kmk } else { RootKind::Ik },
                dek_gen: DekGen(if kmk { dek } else { 0 }),
                family_id: FamilyId(fam),
                keyring_seq: seq,
                next_slot_id: 2 * n as u32 + extra,
                slots,
                mac,
            })
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(CASES))]
    #[test]
    fn roundtrip(k in arb_keyring()) {
        let b = enc(&k);
        prop_assert_eq!(Keyring::decode(&b).unwrap(), k);
    }
}
