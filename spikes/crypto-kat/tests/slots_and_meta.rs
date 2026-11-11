mod common;
use common::*;
use crypto_kat::codec::Codec;
use crypto_kat::extent::*;
use crypto_kat::kdf::*;
use crypto_kat::meta::*;
use crypto_kat::slots::*;

const CFG: Argon2Cfg = Argon2Cfg {
    m_kib: 19_456,
    t: 2,
    p: 1,
}; // the floor, for test speed

// ---- key slots (checklist item 7) -----------------------------------------

#[test]
fn passphrase_slot_roundtrip_and_wrong_passphrase() {
    let ring = Keyring::create(FAMILY, &KMK, NewSlot::Passphrase("hunter2", CFG)).unwrap();
    assert_eq!(
        *ring.unwrap(1, Credential::Passphrase("hunter2")).unwrap(),
        KMK
    );
    assert_eq!(
        ring.unwrap(1, Credential::Passphrase("hunter3"))
            .unwrap_err(),
        SlotError::WrongCredential
    );
    assert_eq!(
        ring.unwrap(2, Credential::Passphrase("hunter2"))
            .unwrap_err(),
        SlotError::NoSuchSlot
    );
}

#[test]
fn argon2_parameters_are_bounded_when_read_from_the_bucket() {
    let mut ring = Keyring::create(FAMILY, &KMK, NewSlot::Passphrase("pw", CFG)).unwrap();
    // A hostile keyring asks for 4 GiB: refused before any allocation (no Argon2 run).
    if let SlotKind::Passphrase { cfg, .. } = &mut ring.slots[0].kind {
        cfg.m_kib = 4 << 20;
    }
    let t = std::time::Instant::now();
    assert_eq!(
        ring.unwrap(1, Credential::Passphrase("pw")).unwrap_err(),
        SlotError::BadParams
    );
    assert!(t.elapsed().as_millis() < 50);
    // Creation enforces the floor too.
    assert_eq!(
        Keyring::create(
            FAMILY,
            &KMK,
            NewSlot::Passphrase(
                "pw",
                Argon2Cfg {
                    m_kib: 1024,
                    t: 1,
                    p: 1
                }
            )
        )
        .unwrap_err(),
        SlotError::BadParams
    );
}

#[test]
fn tampered_parameters_or_ids_fail_authentication() {
    let ring = Keyring::create(FAMILY, &KMK, NewSlot::Passphrase("pw", CFG)).unwrap();
    // AAD binds the params: lowering t (a "weakening" attempt) cannot yield a different valid unwrap.
    let mut r = ring.clone();
    if let SlotKind::Passphrase { cfg, .. } = &mut r.slots[0].kind {
        cfg.t = 1;
    }
    assert!(r.unwrap(1, Credential::Passphrase("pw")).is_err());
    // slot id swap
    let mut r = ring.clone();
    r.slots[0].id = 2;
    assert!(r.unwrap(2, Credential::Passphrase("pw")).is_err());
}

#[test]
fn slot_spliced_from_another_family_does_not_open() {
    // Constellation audit finding 12 flavour: same passphrase reused across families.
    let a = Keyring::create([1; 16], &[0x11; 32], NewSlot::Passphrase("same-pass", CFG)).unwrap();
    let mut b =
        Keyring::create([2; 16], &[0x22; 32], NewSlot::Passphrase("same-pass", CFG)).unwrap();
    b.slots[0] = a.slots[0].clone(); // bucket writer splices A's slot into B's keyring
    assert!(b.unwrap(1, Credential::Passphrase("same-pass")).is_err());
}

#[test]
fn keyfile_and_kms_slots_and_encryption_context() {
    let kf = [0x5Au8; 32];
    let kms = FakeKms::new();
    let mut ring = Keyring::create(FAMILY, &KMK, NewSlot::Passphrase("pw", CFG)).unwrap();
    let s2 = ring.add_slot(&KMK, NewSlot::KeyFile(&kf)).unwrap();
    let s3 = ring
        .add_slot(&KMK, NewSlot::Kms(&kms, "kms-key-eu"))
        .unwrap();
    assert_eq!(*ring.unwrap(s2, Credential::KeyFile(&kf)).unwrap(), KMK);
    assert_eq!(*ring.unwrap(s3, Credential::Kms(&kms)).unwrap(), KMK);
    assert!(ring.unwrap(s2, Credential::KeyFile(&[0u8; 32])).is_err());
    // The KMS blob is bound to family+slot by the encryption context: moving it to another family fails.
    let mut other = Keyring::create([9; 16], &[0x99; 32], NewSlot::Passphrase("pw", CFG)).unwrap();
    other.slots.push(Slot {
        id: 2,
        kind: ring.slots[2].kind.clone(),
        blob: ring.slots[2].blob.clone(),
    });
    assert!(other.unwrap(2, Credential::Kms(&kms)).is_err());
    // ... and re-labelling it as a different slot id of the same family fails.
    let mut ring2 = ring.clone();
    ring2.slots[2].id = 7;
    assert!(ring2.unwrap(7, Credential::Kms(&kms)).is_err());
    // A different KMS (other account/region key) cannot open it.
    assert!(ring.unwrap(s3, Credential::Kms(&FakeKms::new())).is_err());
}

#[test]
fn removing_a_slot_revokes_nothing_already_exposed() {
    let kms = FakeKms::new();
    let mut ring = Keyring::create(FAMILY, &KMK, NewSlot::Passphrase("old-admin", CFG)).unwrap();
    let kms_slot = ring
        .add_slot(&KMK, NewSlot::Kms(&kms, "kms-key-eu"))
        .unwrap();
    let before = ring.clone(); // what a mirror, an old object version or a backup holds
    ring.remove_slot(1).unwrap();
    assert_eq!(
        ring.unwrap(1, Credential::Passphrase("old-admin"))
            .unwrap_err(),
        SlotError::NoSuchSlot
    );
    // The old admin keeps the KMK through any retained copy of the keyring...
    assert_eq!(
        *before
            .unwrap(1, Credential::Passphrase("old-admin"))
            .unwrap(),
        KMK
    );
    // ... and the KMK is unchanged, so every derived key (addressing, every DEK epoch, metadata) is too.
    let kmk2 = ring.unwrap(kms_slot, Credential::Kms(&kms)).unwrap();
    assert_eq!(
        *purpose_key(&kmk2, &FAMILY, Purpose::Dek { epoch: 0 }),
        dek(0)
    );
    // Last slot cannot be removed.
    assert_eq!(ring.remove_slot(kms_slot).unwrap_err(), SlotError::LastSlot);
}

#[test]
fn passphrase_change_is_a_rewrap_and_keeps_every_derived_key() {
    let mut ring = Keyring::create(FAMILY, &KMK, NewSlot::Passphrase("old", CFG)).unwrap();
    let old_copy = ring.clone();
    ring.rewrap_passphrase(1, "old", "new", CFG).unwrap();
    assert!(ring.unwrap(1, Credential::Passphrase("old")).is_err());
    let kmk = ring.unwrap(1, Credential::Passphrase("new")).unwrap();
    assert_eq!(*kmk, KMK);
    for p in [
        Purpose::Addressing,
        Purpose::Dek { epoch: 0 },
        Purpose::Dek { epoch: 5 },
        Purpose::Meta { kind: META_COMMIT },
    ] {
        assert_eq!(
            *purpose_key(&kmk, &FAMILY, p),
            *purpose_key(&KMK, &FAMILY, p)
        );
    }
    // Same caveat as slot removal: an old copy still opens with the old passphrase.
    assert_eq!(
        *old_copy.unwrap(1, Credential::Passphrase("old")).unwrap(),
        KMK
    );
}

#[test]
fn dek_epoch_rotation_protects_only_new_writes() {
    let d = data(8192, 5);
    let id = chunk_id(&d);
    let old = Writer::new(Profile::Adr, dek(0), FAMILY, 0, OsSalt)
        .seal(&id, &d, Codec::Zstd1)
        .unwrap();
    let new = Writer::new(Profile::Adr, dek(1), FAMILY, 1, OsSalt)
        .seal(&id, &d, Codec::Zstd1)
        .unwrap();
    // The reader must pick the key by the epoch recorded in the extent (finding F2).
    let read = |dek_of: &dyn Fn(u32) -> [u8; 32], e: &Extent| {
        let r = Reader {
            profile: Profile::Adr,
            dek: dek_of(e.h.dek_epoch),
            family: FAMILY,
            kind: KIND_DATA,
            max_ulen: 65536,
        };
        open(&r, e, &id).0
    };
    assert!(read(&dek, &old).is_ok() && read(&dek, &new).is_ok());
    // An attacker who obtained only the epoch-0 DEK (the leaked key) opens old extents, not new ones.
    let leaked = |_e: u32| dek(0);
    assert!(read(&leaked, &old).is_ok());
    assert_eq!(read(&leaked, &new).unwrap_err(), OpenError::Tag);
    // KMK compromise opens everything: every DEK epoch is derivable.
    assert_eq!(
        *purpose_key(&KMK, &FAMILY, Purpose::Dek { epoch: 1 }),
        dek(1)
    );
}

// ---- metadata, chain, lease, descriptor (items 6, 10) -------------------------

const VOL: [u8; 16] = [0x61; 16];

#[test]
fn metadata_aead_binds_kind_key_identity_and_is_randomised() {
    let kc = meta_key(&KMK, &FAMILY, META_COMMIT);
    let kl = meta_key(&KMK, &FAMILY, META_LEASE);
    assert_ne!(kc, kl);
    let id = ObjId {
        volume: &VOL,
        kind: META_COMMIT,
        name: "commits/0000000000000007",
    };
    let a = seal_meta(&kc, &id, b"body");
    let b = seal_meta(&kc, &id, b"body");
    assert_ne!(a, b, "random 192-bit nonce");
    assert_eq!(open_meta(&kc, &id, &a).unwrap(), b"body");
    assert!(open_meta(&kl, &id, &a).is_err(), "other object kind's key");
    let moved = ObjId {
        volume: &VOL,
        kind: META_COMMIT,
        name: "commits/0000000000000008",
    };
    assert!(open_meta(&kc, &moved, &a).is_err(), "moved to another seq");
    let other_vol = ObjId {
        volume: &[0x62; 16],
        kind: META_COMMIT,
        name: "commits/0000000000000007",
    };
    assert!(
        open_meta(&kc, &other_vol, &a).is_err(),
        "moved to another volume"
    );
}

fn chain(
    key: &[u8; 32],
    n: u64,
    epoch: u64,
    tag: u8,
    from: Option<&[(u64, Vec<u8>)]>,
) -> Vec<(u64, Vec<u8>)> {
    let mut v: Vec<(u64, Vec<u8>)> = from.map(|f| f.to_vec()).unwrap_or_default();
    while (v.len() as u64) < n {
        let seq = v.len() as u64 + 1;
        let parent_hash = v.last().map(|(_, s)| object_hash(s)).unwrap_or([0; 32]);
        let c = Commit {
            seq,
            epoch,
            parent_hash,
            body: vec![tag, seq as u8],
        };
        v.push((seq, seal_commit(key, &VOL, &c)));
    }
    v
}

#[test]
fn commit_fork_splice_is_invisible_without_parent_hash() {
    let key = meta_key(&KMK, &FAMILY, META_COMMIT);
    let main = chain(&key, 6, 3, b'A', None);
    assert!(verify_chain(&key, &VOL, &main, true).is_ok());
    // DR-steal scenario (ADR-24): an abandoned branch from seq 3 exists in the bucket/mirrors.
    let fork = chain(&key, 6, 3, b'B', Some(&main[..3]));
    // A bucket writer replaces commits 4..6 with the abandoned branch. Same seq names, same keys,
    // valid AEAD, same epoch: structurally a perfectly good chain.
    let mut spliced = main[..3].to_vec();
    spliced.extend_from_slice(&fork[3..]);
    assert!(
        verify_chain(&key, &VOL, &spliced, true).is_ok(),
        "the fork is itself a consistent chain"
    );
    // Splice at one point only (take main's 4th, then fork's 5th): `seq`-only linking cannot see it...
    let mut mixed = main[..4].to_vec();
    mixed.push(fork[4].clone());
    assert!(
        verify_chain(&key, &VOL, &mixed, false).is_ok(),
        "seq-only chain accepts a mixed chain (DESIGN today)"
    );
    // ... parent_hash linking does.
    assert_eq!(
        verify_chain(&key, &VOL, &mixed, true).unwrap_err(),
        ChainError::ParentHash(5)
    );
    // Dropping a commit in the middle is caught by the seq name binding either way.
    let mut gap = main.clone();
    gap.remove(2);
    assert!(verify_chain(&key, &VOL, &gap, true).is_err());
}

#[test]
fn lease_replay_passes_the_aead_and_needs_an_epoch_floor() {
    let key = meta_key(&KMK, &FAMILY, META_LEASE);
    let old = seal_lease(
        &key,
        &VOL,
        &Lease {
            epoch: 3,
            released: true,
        },
    );
    let cur = seal_lease(
        &key,
        &VOL,
        &Lease {
            epoch: 5,
            released: false,
        },
    );
    // The mutable `lease` object has constant identity, so any earlier version replays cleanly.
    assert_eq!(
        open_lease(&key, &VOL, &old).unwrap(),
        Lease {
            epoch: 3,
            released: true
        }
    );
    assert_eq!(open_lease(&key, &VOL, &cur).unwrap().epoch, 5);
    // Proposed rule: reject a lease below the newest epoch known from the verified chain.
    assert!(lease_is_stale(&open_lease(&key, &VOL, &old).unwrap(), 5));
    assert!(!lease_is_stale(&open_lease(&key, &VOL, &cur).unwrap(), 5));
}

#[test]
fn attach_policy_blocks_the_encryption_downgrade() {
    let fp = kmk_fingerprint(&KMK, &FAMILY);
    let good = Descriptor {
        encrypted: true,
        family: FAMILY,
        chunk_size: 65536,
        id_len: 32,
        kmk_fp: fp,
    };
    let mac = descriptor_mac(&KMK, &good);
    // MAC covers every field.
    for t in [
        Descriptor {
            encrypted: false,
            ..good.clone()
        },
        Descriptor {
            chunk_size: 4096,
            ..good.clone()
        },
        Descriptor {
            id_len: 8,
            ..good.clone()
        },
        Descriptor {
            kmk_fp: [0; 16],
            ..good.clone()
        },
        Descriptor {
            family: [1; 16],
            ..good.clone()
        },
    ] {
        assert_ne!(descriptor_mac(&KMK, &t), mac);
    }
    let expect = Expect::Encrypted { kmk_fp: Some(fp) };
    assert_eq!(check_attach(&expect, &good, Some(true)), Ok(()));
    // Bucket writer deletes `volume` and recreates it as plaintext: the caller supplied key
    // material, so a plaintext descriptor is refused even on a host with no local pin.
    let plain = Descriptor {
        encrypted: false,
        ..good.clone()
    };
    assert_eq!(
        check_attach(&expect, &plain, None),
        Err(AttachError::Downgrade)
    );
    // Attacker's own encrypted volume with its own KMK: the fingerprint supplied with the credential refuses it.
    let evil_fp = kmk_fingerprint(&[0xEE; 32], &FAMILY);
    let evil = Descriptor {
        kmk_fp: evil_fp,
        ..good.clone()
    };
    assert_eq!(
        check_attach(&expect, &evil, Some(true)),
        Err(AttachError::WrongKmk)
    );
    // A tampered descriptor under the right KMK fails the MAC.
    assert_eq!(
        check_attach(&expect, &good, Some(descriptor_mac(&KMK, &plain) == mac)),
        Err(AttachError::BadMac)
    );
    // Plain expected but the volume is encrypted.
    assert_eq!(
        check_attach(&Expect::Plain, &good, None),
        Err(AttachError::UnexpectedEncrypted)
    );
    // Without any pin or supplied credential an attacker-made plaintext volume is indistinguishable:
    assert_eq!(check_attach(&Expect::Plain, &plain, None), Ok(()));
}

#[test]
fn xchacha_random_nonce_budget() {
    // 192-bit random nonces: P[collision among n messages] <= n^2 / 2^193.
    // Even 2^50 metadata objects per key (1 PUT/ms for 35,000 years) is 2^-93.
    let n_log2 = std::hint::black_box(50.0f64);
    assert!(2.0 * n_log2 - 193.0 < -90.0);
}
