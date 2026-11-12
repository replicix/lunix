//! Metadata sealing, commits, the commit chain and the lease.

mod common;

use std::collections::HashSet;

use common::*;
use lunix_crypto::{lease_is_stale, VolumeCrypto};
use lunix_format::commit::{decode_object, Commit};
use lunix_format::lease::Lease;
use lunix_format::{CommitSeq, CryptoMode, DekGen, Epoch, IdLen, ObjectKind, Profile, TimeMs};
use lunix_iface::crypto::{CommitCodec, MetaCodec};
use lunix_iface::store::CommitStore;
use lunix_iface::ErrorKind;

const VOLUME_HASH: [u8; 32] = [3; 32];

fn lease(epoch: u64) -> Lease {
    Lease {
        released: false,
        epoch: Epoch(epoch),
        deadline: TimeMs(99),
        host: "host-a".into(),
        cluster: "c1".into(),
        transferred_to: None,
    }
}

const KINDS: [ObjectKind; 11] = [
    ObjectKind::Commit,
    ObjectKind::Lease,
    ObjectKind::Delta,
    ObjectKind::Heat,
    ObjectKind::Footer,
    ObjectKind::Gc,
    ObjectKind::Readers,
    ObjectKind::Holds,
    ObjectKind::Snapshots,
    ObjectKind::Dicts,
    ObjectKind::Space,
];

#[test]
fn every_object_kind_has_its_own_metadata_key() {
    let vol = volume(CryptoMode::Encrypted, 16_384, 5);
    let keys = vol.crypto.volume_keys();
    let mut seen = HashSet::new();
    for k in KINDS {
        assert!(
            seen.insert(keys.meta_key_for_tests(k).expect("key")),
            "{k:?} shares a key"
        );
    }
    // The keyring has no metadata key: it is never sealed.
    assert!(keys.meta_key_for_tests(ObjectKind::Keyring).is_err());
    // None is the data addressing key either.
    assert!(!seen.contains(&keys.addressing_key_for_tests(false).expect("key")));
    assert!(!seen.contains(&keys.addressing_key_for_tests(true).expect("key")));
}

#[test]
fn objects_are_bound_to_kind_name_and_volume_in_every_mode() {
    for mode in MODES {
        let vol = volume(mode, 16_384, 6);
        let m = vol.crypto.meta();
        let name = "snapshots/nightly";
        let sealed = m
            .seal(ObjectKind::Snapshots, name, b"body bytes")
            .expect("seal");
        assert_eq!(
            m.open(ObjectKind::Snapshots, name, &sealed).expect("open"),
            b"body bytes"
        );
        // Another name, kind or volume fails.
        assert!(m
            .open(ObjectKind::Snapshots, "snapshots/other", &sealed)
            .is_err());
        for k in KINDS {
            if k != ObjectKind::Snapshots {
                assert!(m.open(k, name, &sealed).is_err(), "{mode:?} {k:?}");
            }
        }
        let other = volume(mode, 16_384, 7);
        let same_root_other_volume = {
            let mut d = vol.desc.clone();
            d.volume_uuid = lunix_format::VolumeUuid([0x42; 16]);
            VolumeCrypto::new(&d, root_for(mode, 6), DekGen(0)).expect("keys")
        };
        assert!(other
            .crypto
            .meta()
            .open(ObjectKind::Snapshots, name, &sealed)
            .is_err());
        assert!(
            same_root_other_volume
                .meta()
                .open(ObjectKind::Snapshots, name, &sealed)
                .is_err(),
            "{mode:?}: an object moved to another volume must fail"
        );
        // Every byte.
        for i in 0..sealed.len() {
            for bit in [0x01u8, 0x80] {
                let mut bad = sealed.clone();
                bad[i] ^= bit;
                assert!(
                    m.open(ObjectKind::Snapshots, name, &bad).is_err(),
                    "{mode:?} flip {i}"
                );
            }
        }
        for cut in 0..sealed.len() {
            assert!(m.open(ObjectKind::Snapshots, name, &sealed[..cut]).is_err());
        }
        // Bucket-relative names only, and the keyring is not an envelope.
        assert!(m.seal(ObjectKind::Gc, "", b"x").is_err());
        assert!(m
            .seal(ObjectKind::Keyring, "families/x/keyring", b"x")
            .is_err());
        assert!(m.seal(ObjectKind::Gc, &"n".repeat(1025), b"x").is_err());
    }
}

#[test]
fn encrypted_metadata_uses_a_fresh_nonce_and_hides_the_body() {
    let vol = volume(CryptoMode::Encrypted, 16_384, 8);
    let m = vol.crypto.meta();
    let body = b"visible plaintext marker 0123456789";
    let a = m
        .seal(ObjectKind::Gc, "gc/0000000000000001", body)
        .expect("seal");
    let b = m
        .seal(ObjectKind::Gc, "gc/0000000000000001", body)
        .expect("seal");
    assert_ne!(a, b, "random 192-bit nonces");
    assert_ne!(a[12..36], b[12..36]);
    assert!(!a.windows(body.len()).any(|w| w == body));
    // Envelope form 1: header 12 + nonce 24 + body + tag 16.
    assert_eq!(a.len(), 12 + 24 + body.len() + 16);
    // Unencrypted modes keep the body readable and add a 32-byte MAC.
    let p = volume(CryptoMode::Integrity, 16_384, 8);
    let c = p
        .crypto
        .meta()
        .seal(ObjectKind::Gc, "gc/0000000000000001", body)
        .expect("seal");
    assert_eq!(c.len(), 12 + body.len() + 32);
    assert_eq!(&c[12..12 + body.len()], body);
}

/// The five commit kinds: FULL built here, the others decoded from the
/// golden objects of `lunix-format`.
fn five_kinds() -> Vec<(&'static str, Commit)> {
    macro_rules! golden {
        ($n:literal) => {
            decode_object(include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../lunix-format/tests/golden/",
                $n
            )))
            .expect("golden")
            .commit
        };
    }
    vec![
        ("full", full(4, [1; 32], 0, 0)),
        ("delta", golden!("06-commit-delta.bin")),
        ("epoch", golden!("06-commit-epoch.bin")),
        ("config", golden!("06-commit-config.bin")),
        ("transfer", golden!("06-commit-transfer.bin")),
    ]
}

#[test]
fn a_missing_or_wrong_mac_on_each_of_the_five_commit_kinds_is_rejected() {
    let kinds = five_kinds();
    let kinds_seen: HashSet<_> = kinds.iter().map(|(_, c)| c.kind()).collect();
    assert_eq!(kinds_seen.len(), 5);
    for mode in [
        CryptoMode::Integrity,
        CryptoMode::Plain,
        CryptoMode::Encrypted,
    ] {
        let vol = volume(mode, 16_384, 9);
        let m = vol.crypto.meta();
        for (name, c) in &kinds {
            let seq = c.common().commit_seq;
            let sealed = m.seal_commit(c).expect("seal");
            assert_eq!(
                &m.open_commit(seq, &sealed.bytes).expect("open"),
                c,
                "{name}"
            );
            // The last 32 bytes are the MAC (or the tag and part of the
            // ciphertext, for encrypted): wrong in every position.
            let n = sealed.bytes.len();
            for i in n - 32..n {
                let mut bad = sealed.bytes.clone();
                bad[i] ^= 0x01;
                let err = m.open_commit(seq, &bad).expect_err("wrong mac");
                assert_eq!(err.kind(), ErrorKind::Auth, "{mode:?} {name} byte {i}");
            }
            // An all-zero MAC.
            let mut zero = sealed.bytes.clone();
            zero[n - 32..].fill(0);
            assert!(m.open_commit(seq, &zero).is_err(), "{mode:?} {name}");
            // A missing MAC: the object ends where the body ends.
            assert!(m.open_commit(seq, &sealed.bytes[..n - 32]).is_err());
            assert!(m.open_commit(seq, &sealed.bytes[..n - 1]).is_err());
            // Wrong sequence number (the name is in the MAC or AAD).
            assert!(m.open_commit(CommitSeq(seq.0 + 1), &sealed.bytes).is_err());
        }
    }
}

#[test]
fn a_checksum_is_not_an_hmac() {
    // An attacker who can write the bucket can recompute the unkeyed
    // checksum of a plain volume, but must not pass for an integrity one.
    let plain = volume(CryptoMode::Plain, 16_384, 10);
    let keyed = volume(CryptoMode::Integrity, 16_384, 10);
    for (name, c) in five_kinds() {
        let forged = plain.crypto.meta().seal_commit(&c).expect("seal");
        let seq = c.common().commit_seq;
        plain
            .crypto
            .meta()
            .open_commit(seq, &forged.bytes)
            .expect("plain accepts it");
        assert_eq!(
            keyed
                .crypto
                .meta()
                .open_commit(seq, &forged.bytes)
                .expect_err(name)
                .kind(),
            ErrorKind::Auth
        );
        // And a sealed (form 1) object is the wrong form for an unencrypted
        // volume, and the reverse.
        let enc = volume(CryptoMode::Encrypted, 16_384, 10);
        let sealed = enc.crypto.meta().seal_commit(&c).expect("seal");
        assert!(keyed.crypto.meta().open_commit(seq, &sealed.bytes).is_err());
        assert!(enc.crypto.meta().open_commit(seq, &forged.bytes).is_err());
    }
}

fn build_chain(m: &dyn CommitCodec, store: &dyn CommitStore, n: u64, salt: u64, epochs: &[u64]) {
    let mut parent = VOLUME_HASH;
    for seq in 0..=n {
        let epoch = epochs.get(seq as usize).copied().unwrap_or(0);
        let sealed = m
            .seal_commit(&full(seq, parent, epoch, salt))
            .expect("seal");
        store.put(CommitSeq(seq), &sealed.bytes).expect("put");
        parent = sealed.hash;
    }
}

#[test]
fn chain_rules_in_every_mode() {
    for mode in MODES {
        let vol = volume(mode, 16_384, 12);
        let m = vol.crypto.meta();

        // A good chain verifies, to genesis and to a floor.
        let good = MemCommits::new();
        build_chain(&**m, &good, 5, 0, &[]);
        m.verify_chain(&good, CommitSeq(5), CommitSeq(0), &VOLUME_HASH)
            .expect("good");
        m.verify_chain(&good, CommitSeq(5), CommitSeq(3), &[0; 32])
            .expect("floor needs no genesis");
        assert!(m
            .verify_chain(&good, CommitSeq(5), CommitSeq(0), &[8; 32])
            .is_err());

        // A broken parent_hash.
        let broken = MemCommits::new();
        let mut parent = VOLUME_HASH;
        for seq in 0..=4u64 {
            let ph = if seq == 3 { [0xee; 32] } else { parent };
            let sealed = m.seal_commit(&full(seq, ph, 0, 0)).expect("seal");
            broken.put(CommitSeq(seq), &sealed.bytes).expect("put");
            parent = sealed.hash;
        }
        assert!(m
            .verify_chain(&broken, CommitSeq(4), CommitSeq(0), &VOLUME_HASH)
            .is_err());
        // The part above the break verifies on its own.
        m.verify_chain(&broken, CommitSeq(4), CommitSeq(3), &VOLUME_HASH)
            .expect("above");

        // A decreasing epoch.
        let down = MemCommits::new();
        build_chain(&**m, &down, 3, 0, &[0, 5, 5, 4]);
        assert!(m
            .verify_chain(&down, CommitSeq(3), CommitSeq(0), &VOLUME_HASH)
            .is_err());
        // A rising or equal epoch is fine.
        let up = MemCommits::new();
        build_chain(&**m, &up, 3, 0, &[0, 1, 1, 7]);
        m.verify_chain(&up, CommitSeq(3), CommitSeq(0), &VOLUME_HASH)
            .expect("rising");

        // A splice of two valid histories: both verify alone, the join does not.
        let a = MemCommits::new();
        let b = MemCommits::new();
        build_chain(&**m, &a, 4, 0, &[]);
        build_chain(&**m, &b, 4, 1_000, &[]);
        m.verify_chain(&a, CommitSeq(4), CommitSeq(0), &VOLUME_HASH)
            .expect("a");
        m.verify_chain(&b, CommitSeq(4), CommitSeq(0), &VOLUME_HASH)
            .expect("b");
        for cut in 1..=4u64 {
            let spliced = MemCommits::new();
            for seq in 0..=4u64 {
                let src = if seq < cut { &a } else { &b };
                spliced
                    .put(CommitSeq(seq), &src.get(CommitSeq(seq)).expect("get"))
                    .expect("put");
            }
            assert!(
                m.verify_chain(&spliced, CommitSeq(4), CommitSeq(0), &VOLUME_HASH)
                    .is_err(),
                "{mode:?}: a splice at {cut} must be rejected"
            );
        }

        // A missing link is NotFound.
        let gap = MemCommits::new();
        for seq in [0u64, 1, 3] {
            gap.put(CommitSeq(seq), &good.get(CommitSeq(seq)).expect("get"))
                .expect("put");
        }
        assert_eq!(
            m.verify_chain(&gap, CommitSeq(3), CommitSeq(0), &VOLUME_HASH)
                .expect_err("gap")
                .kind(),
            ErrorKind::NotFound
        );
        // A commit swapped in under another sequence number.
        let swap = MemCommits::new();
        for seq in 0..=2u64 {
            let from = if seq == 2 { 1 } else { seq };
            swap.put(CommitSeq(seq), &good.get(CommitSeq(from)).expect("get"))
                .expect("put");
        }
        assert!(m
            .verify_chain(&swap, CommitSeq(2), CommitSeq(0), &VOLUME_HASH)
            .is_err());
    }
}

#[test]
fn the_object_hash_is_the_blake3_of_the_stored_bytes() {
    let vol = volume(CryptoMode::Integrity, 16_384, 13);
    let m = vol.crypto.meta();
    let s = m.seal_commit(&full(1, [1; 32], 0, 0)).expect("seal");
    assert_eq!(s.hash, *blake3::hash(&s.bytes).as_bytes());
    assert_eq!(m.object_hash(&s.bytes), s.hash);
}

#[test]
fn a_replayed_older_lease_is_rejected() {
    for mode in MODES {
        let vol = volume(mode, 16_384, 14);
        let m = vol.crypto.meta();
        let old = m.seal_lease(&lease(5)).expect("seal");
        let new = m.seal_lease(&lease(6)).expect("seal");
        // Both authenticate (an older sealed version of a mutable object
        // would pass the AEAD or MAC).
        assert_eq!(m.open_lease(&old).expect("open"), lease(5));
        // The epoch floor refuses the old one.
        assert!(lease_is_stale(&lease(5), Epoch(6)));
        assert!(!lease_is_stale(&lease(6), Epoch(6)));
        assert_eq!(
            m.open_lease_fresh(&old, Epoch(6))
                .expect_err("stale")
                .kind(),
            ErrorKind::Refused
        );
        assert_eq!(m.open_lease_fresh(&new, Epoch(6)).expect("fresh"), lease(6));
        assert_eq!(
            m.open_lease_fresh(&old, Epoch(0)).expect("no floor"),
            lease(5)
        );
        // The lease epoch is inside the authenticated body: changing it in
        // place fails.
        for i in 0..old.len() {
            let mut bad = old.clone();
            bad[i] ^= 0x04;
            assert!(m.open_lease(&bad).is_err(), "{mode:?} flip {i}");
        }
        // A lease is bound to its kind and name.
        assert!(m.open(ObjectKind::Gc, "lease", &old).is_err());
        assert!(m.open(ObjectKind::Lease, "lease2", &old).is_err());
    }
}

#[test]
fn the_fips_profile_derives_keys_but_does_not_seal_metadata() {
    let desc = descriptor(
        CryptoMode::Encrypted,
        Profile::Fips,
        16_384,
        IdLen::ThirtyTwo,
        [1; 16],
        [2; 16],
    );
    let crypto =
        VolumeCrypto::new(&desc, root_for(CryptoMode::Encrypted, 3), DekGen(0)).expect("keys");
    let m = crypto.meta();
    assert_eq!(
        m.seal(ObjectKind::Gc, "gc/0000000000000001", b"x")
            .expect_err("form 2")
            .kind(),
        ErrorKind::Unsupported
    );
    assert_eq!(
        m.open(ObjectKind::Gc, "gc/0000000000000001", &[0; 80])
            .expect_err("form 2")
            .kind(),
        ErrorKind::Unsupported
    );
}
