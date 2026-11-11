mod common;
use common::*;
use crypto_kat::codec::{decompress_bounded, Codec, DecompressError};
use crypto_kat::extent::*;
use crypto_kat::kdf::*;

#[test]
fn roundtrip_every_profile_codec_and_size() {
    for p in ALL_PROFILES {
        for c in ALL_CODECS {
            for sz in [4096usize, 16384, 65536] {
                let d = data(sz, 3);
                let id = chunk_id(&d);
                let e = writer(p).seal(&id, &d, c).unwrap();
                let (r, t) = open(&reader(p), &e, &id);
                assert_eq!(r.unwrap(), d, "{p:?} {c:?} {sz}");
                assert!(t.tag_checked && t.decompress_called && t.id_recomputed);
            }
        }
    }
}

#[test]
fn fresh_salt_every_seal_and_ciphertext_differs() {
    let d = data(4096, 5);
    let id = chunk_id(&d);
    let mut w = writer(Profile::Adr);
    let a = w.seal(&id, &d, Codec::Zstd1).unwrap();
    let b = w.seal(&id, &d, Codec::Zstd1).unwrap();
    assert_ne!(a.h.salt, b.h.salt);
    assert_ne!(
        a.body, b.body,
        "same chunk, same codec, still different ciphertext"
    );
}

fn sealed(p: Profile, c: Codec) -> (Extent, [u8; 32], Vec<u8>) {
    let d = data(8192, 11);
    let id = chunk_id(&d);
    let e = writer(p).seal(&id, &d, c).unwrap();
    (e, id, d)
}

#[test]
fn tamper_each_header_and_payload_field_is_rejected() {
    for p in [Profile::Adr, Profile::Hardened, Profile::Fips] {
        let r = reader(p);
        let (e, id, _) = sealed(p, Codec::Lz4);
        // Control: untouched extent opens.
        assert!(open(&r, &e, &id).0.is_ok());

        // codec swap (AAD): Lz4 -> Zstd1, i.e. a downgrade/relabel attack
        let mut t = e.clone();
        t.h.codec = Codec::Zstd1 as u8;
        assert_eq!(open(&r, &t, &id).0, Err(OpenError::Tag), "{p:?} codec");
        // level
        let mut t = e.clone();
        t.h.level = 9;
        assert_eq!(open(&r, &t, &id).0, Err(OpenError::Tag), "{p:?} level");
        // ulen (decompression-bomb / truncation lever): must die at the tag, before any decompress
        for new_ulen in [1u32, 4096, 8191, 8193, 65536, u32::MAX] {
            let mut t = e.clone();
            t.h.ulen = new_ulen;
            let (res, trace) = open(&r, &t, &id);
            assert_eq!(res, Err(OpenError::Tag), "{p:?} ulen {new_ulen}");
            assert!(
                !trace.decompress_called,
                "decompress ran before the tag verified"
            );
        }
        // chunk_id in header differs from the map entry: caught before any crypto
        let mut t = e.clone();
        t.h.chunk_id[0] ^= 1;
        assert_eq!(open(&r, &t, &id).0, Err(OpenError::Header));
        // ... and if the attacker also controls the map's expectation, the AAD/key binding still fails
        let mut other = id;
        other[0] ^= 1;
        assert_eq!(
            open(&r, &t, &other).0,
            Err(OpenError::Tag),
            "{p:?} chunk_id binding"
        );
        // salt
        let mut t = e.clone();
        t.h.salt[5] ^= 0x80;
        assert_eq!(open(&r, &t, &id).0, Err(OpenError::Tag), "{p:?} salt");
        // DEK epoch label
        let mut t = e.clone();
        t.h.dek_epoch = 1;
        if p == Profile::Adr {
            // ADR-11 binds the epoch only through the DEK itself: the label is not in the
            // derivation or the AAD, so a reader that already holds the right DEK ignores a
            // wrong label. Harmless when the label selects the key (wrong label => wrong DEK =>
            // tag failure; see `dek_epoch_rotation_protects_only_new_writes`), but it is an
            // unauthenticated field. The proposed profile authenticates it.
            assert!(
                open(&r, &t, &id).0.is_ok(),
                "ADR profile: epoch label is not authenticated"
            );
        } else {
            assert_eq!(open(&r, &t, &id).0, Err(OpenError::Tag), "{p:?} dek_epoch");
        }
        // format version
        let mut t = e.clone();
        t.h.ver = 2;
        assert_eq!(open(&r, &t, &id).0, Err(OpenError::Header));
        // payload bit flips: first, middle, last ciphertext byte and tag byte
        for pos in [0, e.body.len() / 2, e.body.len() - 17, e.body.len() - 1] {
            let mut t = e.clone();
            t.body[pos] ^= 1;
            assert_eq!(
                open(&r, &t, &id).0,
                Err(OpenError::Tag),
                "{p:?} body[{pos}]"
            );
        }
        // truncation and extension
        let mut t = e.clone();
        t.body.truncate(t.body.len() - 1);
        assert_eq!(open(&r, &t, &id).0, Err(OpenError::Tag));
        let mut t = e.clone();
        t.body.push(0);
        assert_eq!(open(&r, &t, &id).0, Err(OpenError::Tag));
        // FIPS profile: nonce tamper
        if p == Profile::Fips {
            let mut t = e.clone();
            t.h.nonce.as_mut().unwrap()[0] ^= 1;
            assert_eq!(open(&r, &t, &id).0, Err(OpenError::Tag));
        }
    }
}

#[test]
fn relocation_between_chunk_ids_fails() {
    for p in [Profile::Adr, Profile::Hardened] {
        let (a, ida, _) = sealed(p, Codec::Zstd1);
        let d2 = data(8192, 99);
        let idb = chunk_id(&d2);
        let r = reader(p);
        // Put chunk A's ciphertext under chunk B's header.
        let mut moved = a.clone();
        moved.h.chunk_id = idb;
        assert_eq!(open(&r, &moved, &idb).0, Err(OpenError::Tag), "{p:?}");
        assert!(open(&r, &a, &ida).0.is_ok());
    }
}

#[test]
fn wrong_dek_epoch_family_and_kind() {
    let (e, id, _) = sealed(Profile::Hardened, Codec::Lz4);
    let mut r = reader(Profile::Hardened);
    r.dek = dek(1);
    assert_eq!(
        open(&r, &e, &id).0,
        Err(OpenError::Tag),
        "wrong DEK epoch key"
    );
    let mut r = reader(Profile::Hardened);
    r.family = [0x77; 16];
    assert_eq!(open(&r, &e, &id).0, Err(OpenError::Tag), "wrong family");
    let mut r = reader(Profile::Hardened);
    r.kind = KIND_MAP_NODE;
    assert_eq!(
        open(&r, &e, &id).0,
        Err(OpenError::Tag),
        "data extent read as map node"
    );
}

/// Checklist item 2: with the ADR derivation the family id is in neither the key
/// derivation nor the AAD. Separation relies entirely on the KMK/DEK being
/// distinct per family. If two families ever shared a DEK (key-file reuse, a
/// copied keyring, a restored backup), ADR-11 as written does not separate them;
/// the proposed profile does.
#[test]
fn adr_derivation_does_not_bind_family_hardened_does() {
    let fam_b = [0x55u8; 16];
    for (p, binds) in [(Profile::Adr, false), (Profile::Hardened, true)] {
        let (e, id, d) = sealed(p, Codec::Raw);
        let mut r = reader(p);
        r.family = fam_b; // same DEK, different family id
        let (res, _) = open(&r, &e, &id);
        if binds {
            assert_eq!(res, Err(OpenError::Tag));
        } else {
            assert_eq!(
                res.unwrap(),
                d,
                "ADR profile decrypts across family ids when the DEK is shared"
            );
        }
    }
}

#[test]
fn verify_order_tag_then_bounded_decompress_then_chunk_id() {
    let p = Profile::Adr;
    let r = reader(p);
    let d = data(8192, 1);
    let id = chunk_id(&d);

    // A compromised/buggy *writer* (valid tag) that lies about ulen: caught after the tag, before decompress.
    let mut w = writer(p);
    let e = w
        .seal_payload(&id, 1_000_000, Codec::Raw, &d, [9u8; 24])
        .unwrap();
    let (res, t) = open(&r, &e, &id);
    assert_eq!(res, Err(OpenError::Ulen));
    assert!(t.tag_checked && !t.decompress_called);

    // ulen smaller than the real output: bounded decoder refuses.
    let e = writer(p)
        .seal_payload(&id, 100, Codec::Lz4, &Codec::Lz4.compress(&d), [8u8; 24])
        .unwrap();
    let (res, _) = open(&r, &e, &id);
    assert!(matches!(res, Err(OpenError::Decompress(_))), "{res:?}");

    // Valid tag, valid codec, right length, wrong content: only the keyed id recompute notices.
    let mut other = d.clone();
    other[0] ^= 0xff;
    let e = writer(p).seal(&id, &other, Codec::Zstd1).unwrap(); // id is for `d`, bytes are `other`
    let (res, t) = open(&r, &e, &id);
    assert_eq!(res, Err(OpenError::ChunkId));
    assert!(t.decompress_called && t.id_recomputed);
}

#[test]
fn zstd_bomb_is_stopped_at_the_cap() {
    // 256 MiB of zeros compresses to a few KiB; the decoder must not materialise it.
    let zeros = vec![0u8; 256 << 20];
    let bomb = zstd::bulk::compress(&zeros, 1).unwrap();
    assert!(bomb.len() < 64 * 1024, "bomb is {} B", bomb.len());
    let t = std::time::Instant::now();
    let r = decompress_bounded(Codec::Zstd1, &bomb, 65536, 65536);
    assert_eq!(r, Err(DecompressError::TooBig));
    assert!(
        t.elapsed().as_millis() < 500,
        "bounded decode took {:?}",
        t.elapsed()
    );
}

#[test]
fn plaintext_extent_cannot_be_injected_into_an_encrypted_volume() {
    // The reader decides "encrypted" from the volume descriptor, never from the extent.
    // Even a reader that wrongly accepted unencrypted payloads is stopped by the keyed id.
    let d = data(4096, 77);
    let want = chunk_id(&d); // keyed: the attacker cannot compute it
    let attacker_id = *blake3::hash(&d).as_bytes(); // unkeyed hash is all the attacker can compute
    assert_ne!(want, attacker_id);
    let forged_plain = Codec::Raw.compress(&d);
    let got = rehash(&decompress_bounded(Codec::Raw, &forged_plain, d.len(), 65536).unwrap());
    assert_eq!(
        got, want,
        "a genuine plaintext with the right bytes would hash right..."
    );
    let mut forged = d.clone();
    forged[7] ^= 1;
    assert_ne!(
        rehash(&forged),
        want,
        "...but forged bytes cannot match the map's keyed id"
    );
}

#[test]
fn fips_profile_stores_a_fresh_random_nonce() {
    let d = data(4096, 5);
    let id = chunk_id(&d);
    let mut w = writer(Profile::Fips);
    let a = w.seal(&id, &d, Codec::Raw).unwrap();
    let b = w.seal(&id, &d, Codec::Raw).unwrap();
    assert!(a.h.nonce.is_some() && b.h.nonce.is_some());
    assert_ne!(a.h.nonce, b.h.nonce);
}
