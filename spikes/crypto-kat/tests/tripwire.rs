//! Checklist item 1: the (key, nonce) uniqueness rule, as an executable tripwire.
mod common;
use common::*;
use crypto_kat::attack::{demo_chunk, xor_leak_demo};
use crypto_kat::codec::Codec;
use crypto_kat::extent::*;
use crypto_kat::kdf::Profile;

fn writer_with<S: SaltSource>(p: Profile, salts: S) -> Writer<S> {
    Writer::new(p, dek(0), FAMILY, 0, salts).with_ledger()
}

#[test]
fn draft_scheme_recompress_trips_the_wire_and_leaks_xor() {
    let d = demo_chunk(65536, 42);
    let id = chunk_id(&d);
    let mut w = writer_with(Profile::V0Draft, OsSalt);
    assert!(w.seal(&id, &d, Codec::Raw).is_ok());
    // Same chunk, different codec (ADR-12 recompress, or a zstd upgrade): same key, same nonce.
    let r = w.seal(&id, &d, Codec::Lz4);
    assert!(
        matches!(r, Err(SealError::Tripwire(_))),
        "draft scheme must trip"
    );
    // Re-sealing the *identical* encoding is harmless (identical ciphertext).
    let mut w = writer_with(Profile::V0Draft, OsSalt);
    w.seal(&id, &d, Codec::Zstd1).unwrap();
    w.seal(&id, &d, Codec::Zstd1).unwrap();
    assert_eq!(w.ledger.as_ref().unwrap().benign_repeats, 1);

    // The concrete leak.
    let leak = xor_leak_demo(&d);
    assert!(leak.xor_equals_plaintext_xor);
    assert_eq!(leak.lz4_header_len, 5);
    assert!(
        leak.recovered_correct >= 1024,
        "recovered {}",
        leak.recovered_correct
    );
}

#[test]
fn adr_scheme_survives_10k_reencodes_of_one_chunk_without_any_reuse() {
    let d = demo_chunk(65536, 7);
    let id = chunk_id(&d);
    let mut w = writer_with(Profile::Adr, OsSalt);
    let mut salts = std::collections::HashSet::new();
    for i in 0..10_000u32 {
        // recompress, crash-retry (same codec again), compaction re-encode, level change
        let c = match i % 5 {
            0 => Codec::Raw,
            1 => Codec::Lz4,
            2 => Codec::Zstd1,
            3 => Codec::Zstd3,
            _ => Codec::Zstd1, // crash-retried destage: same encoding again
        };
        let e = w.seal(&id, &d[..4096], c).expect("no reuse");
        assert!(salts.insert(e.h.salt), "salt repeated");
    }
    let l = w.ledger.as_ref().unwrap();
    assert_eq!((l.violations, l.benign_repeats, l.seals), (0, 0, 10_000));
}

#[test]
fn same_salt_under_different_dek_epoch_or_family_does_not_collide() {
    let d = demo_chunk(4096, 1);
    let id = chunk_id(&d);
    let other_kmk = [0x21u8; 32];
    let other_dek = *crypto_kat::kdf::purpose_key(
        &other_kmk,
        &[0x41; 16],
        crypto_kat::kdf::Purpose::Dek { epoch: 0 },
    );
    // One shared ledger, a replayed (identical) salt everywhere, two encodings per DEK.
    // Reuse is possible only inside one DEK: exactly one violation per DEK (the second
    // encoding) and none across DEK epochs or families.
    let mut shared = crypto_kat::tripwire::Ledger::new();
    for dk in [dek(0), dek(1), other_dek] {
        let mut w = Writer::new(
            Profile::Adr,
            dk,
            FAMILY,
            0,
            ReplaySalt {
                list: vec![[0xAB; 24]],
                i: 0,
            },
        );
        w.ledger = Some(std::mem::take(&mut shared));
        assert!(
            w.seal(&id, &d, Codec::Raw).is_ok(),
            "first seal under a new DEK never collides"
        );
        assert!(w.seal(&id, &d, Codec::Lz4).is_err());
        shared = w.ledger.take().unwrap();
    }
    assert_eq!((shared.seals, shared.violations), (6, 3));
}

#[test]
fn stuck_rng_is_the_residual_risk_of_the_adr_design() {
    // Models a VM snapshot restore / forked RNG state: the salt sequence replays.
    let d = demo_chunk(4096, 2);
    let id = chunk_id(&d);
    let mut w = writer_with(
        Profile::Adr,
        ReplaySalt {
            list: vec![[1; 24]],
            i: 0,
        },
    );
    w.seal(&id, &d, Codec::Raw).unwrap();
    // identical re-encode with a replayed salt: identical message, harmless
    w.seal(&id, &d, Codec::Raw).unwrap();
    assert_eq!(w.ledger.as_ref().unwrap().benign_repeats, 1);
    // a different encoding of the same chunk with the replayed salt: catastrophe
    assert!(matches!(
        w.seal(&id, &d, Codec::Lz4),
        Err(SealError::Tripwire(_))
    ));
}

#[test]
fn fixes_under_a_stuck_rng() {
    let d = demo_chunk(65536, 3);
    let id = chunk_id(&d);
    let stuck = || ReplaySalt {
        list: vec![[1; 24]],
        i: 0,
    };

    // Fix A (cheap): the proposed derivation also takes codec, level and clen.
    // Different codec/level/length => different key even with a replayed salt.
    let mut w = writer_with(Profile::Hardened, stuck());
    for c in [Codec::Raw, Codec::Lz4, Codec::Zstd1, Codec::Zstd3] {
        w.seal(&id, &d, c)
            .expect("fix A covers codec/level changes");
    }
    // ... but not two different encodings with equal (codec, level, clen), e.g. a zstd
    // upgrade that changes bytes but not length. Craft two equal-length payloads.
    let mut w = writer_with(Profile::Hardened, stuck());
    let p1 = vec![0x11u8; 1000];
    let mut p2 = p1.clone();
    p2[500] ^= 1;
    w.seal_payload(&id, 65536, Codec::Zstd1, &p1, [1; 24])
        .unwrap();
    assert!(
        matches!(
            w.seal_payload(&id, 65536, Codec::Zstd1, &p2, [1; 24]),
            Err(SealError::Tripwire(_))
        ),
        "fix A alone is not complete"
    );

    // Fix B: hedged salt = keyed hash(rng || chunk_id || encoding || payload bytes).
    let hedge = |inner| HedgedSalt {
        inner,
        key: *blake3::hash(&dek(0)).as_bytes(),
    };
    let mut w = writer_with(Profile::Adr, hedge(stuck()));
    for c in [Codec::Raw, Codec::Lz4, Codec::Zstd1, Codec::Zstd3] {
        w.seal(&id, &d, c)
            .expect("fix B: different payload => different salt");
    }
    // identical payload twice => identical salt => identical ciphertext => benign repeat
    w.seal(&id, &d, Codec::Zstd1).unwrap();
    let l = w.ledger.as_ref().unwrap();
    assert_eq!((l.violations, l.benign_repeats), (0, 1));
}

#[test]
fn cached_salt_anti_pattern_trips() {
    // "Optimisation": keep the extent header (with its salt) from the first attempt and
    // only redo the payload when the pipeline degrades from zstd to LZ4 under backpressure.
    let d = demo_chunk(8192, 4);
    let id = chunk_id(&d);
    let mut w = writer_with(Profile::Adr, OsSalt);
    let first = w.seal(&id, &d, Codec::Zstd1).unwrap();
    let r = w.seal_payload(
        &id,
        d.len() as u32,
        Codec::Zstd1,
        &Codec::Zstd1.compress(&d[..4096]),
        first.h.salt,
    );
    assert!(matches!(r, Err(SealError::Tripwire(_))));
}

#[test]
fn salts_from_the_os_rng_do_not_repeat_in_200k_draws() {
    let mut s = OsSalt;
    let mut set = std::collections::HashSet::new();
    for _ in 0..200_000 {
        let v = s.next_salt(&SaltReq {
            chunk_id: &[0; 32],
            codec: 0,
            level: 0,
            payload: &[],
        });
        assert!(set.insert(v));
    }
}

#[test]
fn birthday_bounds_are_negligible() {
    // log2 of the birthday bound n^2 / 2^bits for n = 2^n_log2 draws.
    fn log2_bound(n_log2: f64, bits: f64) -> f64 {
        2.0 * n_log2 - bits
    }
    // P[two extents of the same (DEK, chunk_id) share a 192-bit salt] <= k^2 / 2^193.
    assert!(log2_bound(40.0, 193.0) < -100.0); // 2^40 re-encodes of one chunk
                                               // Across a whole family: sum_c k_c^2 / 2^193 <= (sum k_c)^2 / 2^193 < 2^-100 for 2^46 extents.
    assert!(log2_bound(46.0, 193.0) < -100.0);
    // Distinct derivation inputs collide in the 256-bit key only at N^2 / 2^257.
    assert!(log2_bound(50.0, 257.0) < -150.0);
}
