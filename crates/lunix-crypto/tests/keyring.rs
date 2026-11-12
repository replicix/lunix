//! The keyring: creation, slots, Argon2id bounds, copying slots, rollback,
//! error uniformity, and the default `integrity` volume's key-file slot (D16).

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use common::*;
use lunix_crypto::keyring::{
    self, add_slot, check_not_older, create, create_integrity_with_key_file, create_with_root,
    remove_slot, rewrap, rotate_dek, unlock, verify_mac, Argon2Cost, KeyFile, KeyringError,
    NewSlot, Unlock,
};
use lunix_crypto::Root;
use lunix_format::keyring::{Argon2Params, Keyring, RootKind, Slot, SlotParams, SlotWrap};
use lunix_format::purpose::Purpose;
use lunix_format::{DekGen, FamilyId, Profile};

const FAMILY: FamilyId = FamilyId([0x44; 16]);
const PROFILE: Profile = Profile::Default;
/// The floor: fast enough for a test, legal to write.
const COST: Argon2Cost = Argon2Cost {
    m_kib: 19_456,
    t: 2,
    p: 1,
};
const PASS: &[u8] = b"correct horse battery staple";

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lunix-crypto-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

fn new_keyfile_keyring(kind: RootKind) -> (keyring::Created, KeyFile) {
    let kf = KeyFile::generate().expect("key file");
    let c = create(PROFILE, FAMILY, kind, &NewSlot::KeyFile(&kf)).expect("create");
    (c, kf)
}

fn unlock_kf(bytes: &[u8], kf: &KeyFile, seen: u64) -> Result<keyring::Unlocked, KeyringError> {
    unlock(bytes, PROFILE, &FAMILY, &Unlock::KeyFile(kf), seen, None)
}

/// Recomputes the MAC of a hand-edited keyring under `root`.
fn remac(kr: &mut Keyring, root: &Root) {
    let key = lunix_crypto::testing::purpose_key(PROFILE, root, &kr.family_id, &Purpose::KeyringMac).expect("key");
    let input = kr.mac_input().expect("input");
    let hk = aws_lc_rs::hmac::Key::new(aws_lc_rs::hmac::HMAC_SHA256, &key[..]);
    kr.mac
        .copy_from_slice(aws_lc_rs::hmac::sign(&hk, &input).as_ref());
}

#[test]
fn create_and_unlock_with_a_key_file() {
    for kind in [RootKind::Kmk, RootKind::Ik] {
        let (c, kf) = new_keyfile_keyring(kind);
        assert_eq!(c.keyring.keyring_seq, 1);
        assert_eq!(c.keyring.dek_gen, DekGen(0));
        assert_eq!(c.keyring.root_kind, kind);
        assert_eq!(c.slot_id, 1);
        assert!(verify_mac(&c.keyring, PROFILE, &c.root));
        let u = unlock_kf(&c.bytes, &kf, 0).expect("unlock");
        assert_eq!(u.root.expose_for_tests(), c.root.expose_for_tests());
        assert_eq!(u.slot_id, 1);
        assert_eq!(u.keyring, c.keyring);
        // The bytes are the canonical encoding.
        assert_eq!(keyring::encode(&u.keyring).expect("encode"), c.bytes);
        // The wrap hides the root.
        let root = c.root.expose_for_tests();
        assert!(!c.bytes.windows(32).any(|w| w == root));
    }
}

#[test]
fn passphrase_slots_unlock_and_wrong_passphrases_do_not() {
    let created = create(
        PROFILE,
        FAMILY,
        RootKind::Kmk,
        &NewSlot::Passphrase {
            passphrase: PASS,
            cost: COST,
        },
    )
    .expect("create");
    let ok = unlock(
        &created.bytes,
        PROFILE,
        &FAMILY,
        &Unlock::Passphrase(PASS),
        0,
        None,
    )
    .expect("unlock");
    assert_eq!(ok.root.expose_for_tests(), created.root.expose_for_tests());
    // A key file does not open a passphrase slot and the reverse.
    let kf = KeyFile::generate().expect("kf");
    assert!(matches!(
        unlock_kf(&created.bytes, &kf, 0),
        Err(KeyringError::WrongCredential)
    ));
    // Empty passphrases are refused at creation.
    assert!(create(
        PROFILE,
        FAMILY,
        RootKind::Kmk,
        &NewSlot::Passphrase {
            passphrase: b"",
            cost: COST
        }
    )
    .is_err());
}

/// Wrong passphrase and corrupted wrap are one error, and so are the other
/// ways the credential fails to open a keyring.
#[test]
fn wrong_passphrase_and_corrupted_wrap_give_the_same_error() {
    let created = create(
        PROFILE,
        FAMILY,
        RootKind::Kmk,
        &NewSlot::Passphrase {
            passphrase: PASS,
            cost: COST,
        },
    )
    .expect("create");
    let try_pass = |bytes: &[u8], pass: &[u8]| {
        unlock(bytes, PROFILE, &FAMILY, &Unlock::Passphrase(pass), 0, None)
            .err()
            .expect("must fail")
    };
    let wrong = try_pass(&created.bytes, b"not the passphrase");

    // A corrupted wrap with a MAC that is valid for the corrupted keyring, so
    // only the slot's AEAD can say no.
    let mut kr = created.keyring.clone();
    let SlotWrap::Key { tag, .. } = &mut kr.slots[0].wrap else {
        panic!("key wrap")
    };
    tag[0] ^= 1;
    remac(&mut kr, &created.root);
    let corrupted = try_pass(&keyring::encode(&kr).expect("encode"), PASS);

    // A corrupted MAC only.
    let mut kr = created.keyring.clone();
    kr.mac[5] ^= 1;
    let bad_mac = try_pass(&keyring::encode(&kr).expect("encode"), PASS);

    for e in [&wrong, &corrupted, &bad_mac] {
        assert!(matches!(e, KeyringError::WrongCredential), "{e:?}");
    }
    assert_eq!(wrong.to_string(), corrupted.to_string());
    assert_eq!(wrong.to_string(), bad_mac.to_string());
    // The message names nothing secret.
    assert!(!wrong.to_string().contains("correct horse"));

    // The wrong family is the same error.
    let other = FamilyId([0x45; 16]);
    assert!(matches!(
        unlock(
            &created.bytes,
            PROFILE,
            &other,
            &Unlock::Passphrase(PASS),
            0,
            None
        ),
        Err(KeyringError::WrongCredential)
    ));
}

#[test]
fn argon2id_bounds_are_enforced_on_read() {
    let created = create(
        PROFILE,
        FAMILY,
        RootKind::Kmk,
        &NewSlot::Passphrase {
            passphrase: PASS,
            cost: COST,
        },
    )
    .expect("create");
    // m_kib, t and p of the first slot start at byte 48 + 12 of the keyring.
    let patch = |off: usize, v: u32| {
        let mut b = created.bytes.clone();
        b[off..off + 4].copy_from_slice(&v.to_le_bytes());
        b
    };
    let cases: [(&str, Vec<u8>); 7] = [
        ("m > 1 GiB", patch(60, (1 << 20) + 1)),
        ("m = 4 GiB", patch(60, u32::MAX)),
        ("t = 11", patch(64, 11)),
        ("t = 0", patch(64, 0)),
        ("p = 17", patch(68, 17)),
        ("p = 0", patch(68, 0)),
        ("m < 8p", patch(60, 7)),
    ];
    for (name, bytes) in cases {
        let r = unlock(&bytes, PROFILE, &FAMILY, &Unlock::Passphrase(PASS), 0, None);
        assert!(
            matches!(r, Err(KeyringError::Format(_))),
            "{name}: read bounds must reject before any allocation, got {:?}",
            r.err()
        );
    }
    // The limits themselves decode (m = 1 GiB is legal to read).
    Argon2Params {
        m_kib: 1 << 20,
        t: 10,
        p: 16,
        salt: [0; 16],
    }
    .check_read_bounds()
    .expect("limits are legal");
}

#[test]
fn argon2id_floors_are_enforced_on_write() {
    for (m, t) in [(19_455, 2), (4_096, 3), (19_456, 1)] {
        let r = create(
            PROFILE,
            FAMILY,
            RootKind::Kmk,
            &NewSlot::Passphrase {
                passphrase: PASS,
                cost: Argon2Cost { m_kib: m, t, p: 1 },
            },
        );
        assert!(
            matches!(r, Err(KeyringError::BelowFloor)),
            "m={m} t={t}: {:?}",
            r.err()
        );
    }
    // t = 0 and anything over the read bounds are refused on write too.
    let r = create(
        PROFILE,
        FAMILY,
        RootKind::Kmk,
        &NewSlot::Passphrase {
            passphrase: PASS,
            cost: Argon2Cost {
                m_kib: 65_536,
                t: 0,
                p: 1,
            },
        },
    );
    assert!(r.is_err());
    let r = create(
        PROFILE,
        FAMILY,
        RootKind::Kmk,
        &NewSlot::Passphrase {
            passphrase: PASS,
            cost: Argon2Cost {
                m_kib: (1 << 20) + 1,
                t: 3,
                p: 1,
            },
        },
    );
    assert!(r.is_err());
    // Exactly the floor is fine; so is a rewrap at the floor.
    create(
        PROFILE,
        FAMILY,
        RootKind::Kmk,
        &NewSlot::Passphrase {
            passphrase: PASS,
            cost: COST,
        },
    )
    .expect("the floor itself is legal");
}

#[test]
fn a_slot_copied_to_another_slot_id_or_family_fails() {
    let (c, kf) = new_keyfile_keyring(RootKind::Kmk);
    // A second key file slot so a keyring keeps a slot when we edit the first.
    let kf2 = KeyFile::generate().expect("kf2");
    let (two, id2) = add_slot(&c.keyring, PROFILE, &c.root, &NewSlot::KeyFile(&kf2)).expect("add");
    assert_eq!(id2, 2);
    unlock_kf(&keyring::encode(&two).expect("e"), &kf2, 0).expect("both slots work");

    // Slot 1's wrap and params under slot id 7, with a valid MAC.
    let mut moved = two.clone();
    moved.slots[0].slot_id = 7;
    moved.slots.sort_by_key(|s| s.slot_id);
    moved.next_slot_id = 8;
    remac(&mut moved, &c.root);
    let bytes = keyring::encode(&moved).expect("encode");
    assert!(matches!(
        unlock_kf(&bytes, &kf, 0),
        Err(KeyringError::WrongCredential)
    ));
    // Slot 2 is untouched and still opens.
    unlock_kf(&bytes, &kf2, 0).expect("untouched slot");

    // Slot 1 copied into a keyring of another family (same root, valid MAC).
    let other = FamilyId([0x45; 16]);
    let mut foreign = c.keyring.clone();
    foreign.family_id = other;
    remac(&mut foreign, &c.root);
    let bytes = keyring::encode(&foreign).expect("encode");
    assert!(matches!(
        unlock(&bytes, PROFILE, &other, &Unlock::KeyFile(&kf), 0, None),
        Err(KeyringError::WrongCredential)
    ));

    // The KDF parameters of a passphrase slot are bound too: raising `t`
    // by one with a valid MAC makes the KEK, and so the unwrap, differ.
    let p = create(
        PROFILE,
        FAMILY,
        RootKind::Kmk,
        &NewSlot::Passphrase {
            passphrase: PASS,
            cost: COST,
        },
    )
    .expect("create");
    let mut edited = p.keyring.clone();
    let SlotParams::Passphrase(a) = &mut edited.slots[0].params else {
        panic!("passphrase")
    };
    a.t += 1;
    remac(&mut edited, &p.root);
    assert!(matches!(
        unlock(
            &keyring::encode(&edited).expect("encode"),
            PROFILE,
            &FAMILY,
            &Unlock::Passphrase(PASS),
            0,
            None
        ),
        Err(KeyringError::WrongCredential)
    ));
}

#[test]
fn a_keyring_older_than_one_seen_is_rejected() {
    let (c, kf) = new_keyfile_keyring(RootKind::Ik);
    let kf2 = KeyFile::generate().expect("kf");
    let (v2, _) = add_slot(&c.keyring, PROFILE, &c.root, &NewSlot::KeyFile(&kf2)).expect("add");
    assert_eq!(v2.keyring_seq, 2);
    let old = c.bytes.clone();
    let new = keyring::encode(&v2).expect("encode");
    unlock_kf(&new, &kf, 2).expect("current");
    unlock_kf(&old, &kf, 1).expect("same seq as seen");
    assert!(matches!(
        unlock_kf(&old, &kf, 2),
        Err(KeyringError::Rollback)
    ));
    assert!(check_not_older(&v2, 2).is_ok());
    assert!(check_not_older(&c.keyring, 2).is_err());
    // A rollback is checked after the credential: a wrong key is still one
    // error and does not reveal the sequence number.
    let wrong = KeyFile::generate().expect("kf");
    assert!(matches!(
        unlock_kf(&old, &wrong, 9),
        Err(KeyringError::WrongCredential)
    ));
}

#[test]
fn slot_changes_raise_the_sequence_and_revoke_nothing_already_seen() {
    let (c, kf1) = new_keyfile_keyring(RootKind::Kmk);
    let kf2 = KeyFile::generate().expect("kf2");
    let (k2, id2) = add_slot(&c.keyring, PROFILE, &c.root, &NewSlot::KeyFile(&kf2)).expect("add");
    let (k3, id3) = add_slot(
        &k2,
        PROFILE,
        &c.root,
        &NewSlot::Passphrase {
            passphrase: PASS,
            cost: COST,
        },
    )
    .expect("add pass");
    assert_eq!((id2, id3), (2, 3));
    assert_eq!(k3.keyring_seq, 3);
    assert_eq!(k3.next_slot_id, 4);
    // Remove slot 1: slot 1's credential no longer opens this keyring, but the
    // root is unchanged for anyone who has seen it.
    let k4 = remove_slot(&k3, PROFILE, &c.root, 1).expect("remove");
    assert_eq!(k4.keyring_seq, 4);
    let b4 = keyring::encode(&k4).expect("e");
    assert!(unlock_kf(&b4, &kf1, 0).is_err());
    let after = unlock_kf(&b4, &kf2, 0).expect("slot 2");
    assert_eq!(after.root.expose_for_tests(), c.root.expose_for_tests());
    // Slot ids are never reused.
    let (k5, id5) = add_slot(&k4, PROFILE, &c.root, &NewSlot::KeyFile(&kf1)).expect("re-add");
    assert_eq!(id5, 4);
    assert_eq!(k5.next_slot_id, 5);
    // The last slot cannot be removed; unknown slots are errors.
    let (one, _) = new_keyfile_keyring(RootKind::Kmk);
    assert!(remove_slot(&one.keyring, PROFILE, &one.root, 1).is_err());
    assert!(remove_slot(&k3, PROFILE, &c.root, 99).is_err());
    // Rewrap: a new key file for slot 2, same root, higher sequence.
    let kf2b = KeyFile::generate().expect("kf");
    let k6 = rewrap(&k5, PROFILE, &c.root, 2, &NewSlot::KeyFile(&kf2b)).expect("rewrap");
    assert_eq!(k6.keyring_seq, 6);
    let b6 = keyring::encode(&k6).expect("e");
    assert!(unlock_kf(&b6, &kf2, 0).is_err());
    unlock_kf(&b6, &kf2b, 0).expect("new key file");
    // Operations need the keyring's own root.
    let stranger = Root::from_bytes(RootKind::Kmk, &root_bytes(200));
    assert!(matches!(
        add_slot(&k6, PROFILE, &stranger, &NewSlot::KeyFile(&kf1)),
        Err(KeyringError::WrongCredential)
    ));
    // At most 32 slots.
    let mut full = c.keyring.clone();
    let mut last = full.clone();
    for _ in 0..31 {
        let kf = KeyFile::generate().expect("kf");
        let (n, _) = add_slot(&full, PROFILE, &c.root, &NewSlot::KeyFile(&kf)).expect("add");
        full = n;
        last = full.clone();
    }
    assert_eq!(last.slots.len(), 32);
    let extra = KeyFile::generate().expect("kf");
    assert!(add_slot(&full, PROFILE, &c.root, &NewSlot::KeyFile(&extra)).is_err());
}

#[test]
fn dek_rotation_raises_the_generation() {
    let (c, kf) = new_keyfile_keyring(RootKind::Kmk);
    let r = rotate_dek(&c.keyring, PROFILE, &c.root).expect("rotate");
    assert_eq!((r.dek_gen, r.keyring_seq), (DekGen(1), 2));
    let u = unlock_kf(&keyring::encode(&r).expect("e"), &kf, 0).expect("unlock");
    assert_eq!(u.keyring.dek_gen, DekGen(1));
    // The generation is covered by the MAC.
    let mut bad = r.clone();
    bad.dek_gen = DekGen(0);
    assert!(unlock_kf(&keyring::encode(&bad).expect("e"), &kf, 0).is_err());
    // An integrity keyring has no DEK.
    let (ik, _) = new_keyfile_keyring(RootKind::Ik);
    assert!(rotate_dek(&ik.keyring, PROFILE, &ik.root).is_err());
}

#[test]
fn kms_slots_decode_and_answer_unsupported() {
    let (c, kf) = new_keyfile_keyring(RootKind::Kmk);
    assert!(matches!(
        add_slot(
            &c.keyring,
            PROFILE,
            &c.root,
            &NewSlot::Kms { key_ref: "arn:x" }
        ),
        Err(KeyringError::Unsupported(_))
    ));
    // A keyring that holds a KMS slot (written by a later phase) decodes, the
    // other slots still work, and a KMS credential is unsupported.
    let mut kr = c.keyring.clone();
    kr.slots.push(Slot {
        slot_id: 2,
        params: SlotParams::Kms {
            key_ref: "arn:aws:kms:eu-west-1:1:key/x".into(),
        },
        wrap: SlotWrap::Kms(vec![1, 2, 3, 4]),
    });
    kr.next_slot_id = 3;
    kr.keyring_seq = 2;
    remac(&mut kr, &c.root);
    let bytes = keyring::encode(&kr).expect("encode");
    unlock_kf(&bytes, &kf, 0).expect("key-file slot beside a KMS slot");
    assert!(matches!(
        unlock(&bytes, PROFILE, &FAMILY, &Unlock::Kms("arn:x"), 0, None),
        Err(KeyringError::Unsupported(_))
    ));
    assert!(matches!(
        unlock(&bytes, PROFILE, &FAMILY, &Unlock::Passphrase(PASS), 0, None),
        Err(KeyringError::WrongCredential)
    ));
}

#[test]
fn hostile_keyrings_are_rejected_without_panics() {
    let (c, kf) = new_keyfile_keyring(RootKind::Kmk);
    for i in 0..c.bytes.len() {
        let mut bad = c.bytes.clone();
        bad[i] ^= 0x01;
        assert!(unlock_kf(&bad, &kf, 0).is_err(), "flip {i}");
    }
    for cut in 0..c.bytes.len() {
        assert!(unlock_kf(&c.bytes[..cut], &kf, 0).is_err());
    }
    assert!(unlock_kf(&vec![0u8; 300 * 1024], &kf, 0).is_err());
}

// ---- D16: the default integrity volume's key-file slot ----------------------

#[test]
fn the_default_integrity_volume_gets_a_0600_key_file_slot() {
    let dir = scratch("d16");
    let path = dir.join("integrity.key");
    let c = create_integrity_with_key_file(FAMILY, &path).expect("create");
    assert_eq!(c.keyring.root_kind, RootKind::Ik);
    assert_eq!(c.keyring.slots.len(), 1);
    assert!(matches!(c.keyring.slots[0].params, SlotParams::KeyFile));
    let mode = std::fs::metadata(&path).expect("stat").permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "integrity.key must be 0600");
    assert_eq!(std::fs::metadata(&path).expect("stat").len(), 32);
    // Attach with no extra step: the key file at its place unwraps the slot.
    let u = unlock(
        &c.bytes,
        PROFILE,
        &FAMILY,
        &Unlock::KeyFilePath(&path),
        0,
        None,
    )
    .expect("unlock from the file");
    assert_eq!(u.root.expose_for_tests(), c.root.expose_for_tests());
    // create is create-only: it never overwrites a key.
    assert!(create_integrity_with_key_file(FAMILY, &path).is_err());
    assert_eq!(
        KeyFile::read(&path).expect("read").expose_for_tests().len(),
        32
    );

    // A missing, wrong, short or long key file is a wrong credential, not a panic.
    let missing = dir.join("nope.key");
    let wrong = dir.join("wrong.key");
    KeyFile::generate()
        .expect("kf")
        .write_new(&wrong)
        .expect("write");
    let short = dir.join("short.key");
    std::fs::write(&short, [1u8; 31]).expect("write");
    let long = dir.join("long.key");
    std::fs::write(&long, [1u8; 33]).expect("write");
    let empty = dir.join("empty.key");
    std::fs::write(&empty, []).expect("write");
    for p in [&missing, &wrong, &short, &long, &empty, &dir] {
        let r = unlock(&c.bytes, PROFILE, &FAMILY, &Unlock::KeyFilePath(p), 0, None);
        assert!(
            matches!(r, Err(KeyringError::WrongCredential)),
            "{p:?}: {:?}",
            r.err()
        );
    }
    // `--passphrase` adds a passphrase slot for a volume that moves.
    let (two, id) = add_slot(
        &c.keyring,
        PROFILE,
        &c.root,
        &NewSlot::Passphrase {
            passphrase: PASS,
            cost: COST,
        },
    )
    .expect("add passphrase");
    assert_eq!(id, 2);
    let bytes = keyring::encode(&two).expect("e");
    let moved = unlock(&bytes, PROFILE, &FAMILY, &Unlock::Passphrase(PASS), 0, None)
        .expect("passphrase unwraps");
    assert_eq!(moved.slot_id, 2);
    assert_eq!(moved.root.expose_for_tests(), c.root.expose_for_tests());
    // Only that slot, when asked.
    assert!(unlock(
        &bytes,
        PROFILE,
        &FAMILY,
        &Unlock::Passphrase(PASS),
        0,
        Some(1)
    )
    .is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_failed_creation_leaves_no_orphan_key_file() {
    let dir = scratch("orphan");
    let path = dir.join("missing-dir").join("integrity.key");
    assert!(create_integrity_with_key_file(FAMILY, &path).is_err());
    assert!(!path.exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn create_with_a_known_root_is_deterministic_in_what_it_wraps() {
    let root = Root::from_bytes(RootKind::Ik, &root_bytes(77));
    let kf = KeyFile::generate().expect("kf");
    let c = create_with_root(PROFILE, FAMILY, root, &NewSlot::KeyFile(&kf)).expect("create");
    assert_eq!(c.root.expose_for_tests(), root_bytes(77));
    let u = unlock_kf(&c.bytes, &kf, 0).expect("unlock");
    assert_eq!(u.root.expose_for_tests(), root_bytes(77));
}
