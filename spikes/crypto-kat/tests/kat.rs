//! Known-answer tests. Every constant below was produced by `crypto-kat vectors`
//! and independently reproduced by `tools/crosscheck.py` (pure-Python BLAKE3,
//! `cryptography` AES-GCM/Argon2id, hmac-based HKDF).

use aws_lc_rs::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM};
use crypto_kat::kdf::*;

fn seq(start: u8, n: usize) -> Vec<u8> {
    (0..n).map(|i| start.wrapping_add(i as u8)).collect()
}
fn arr<const N: usize>(v: &[u8]) -> [u8; N] {
    v.try_into().unwrap()
}

#[test]
fn aes256_gcm_zero_key_zero_nonce_mcgrew_viega_cases_13_14() {
    let k = LessSafeKey::new(UnboundKey::new(&AES_256_GCM, &[0u8; 32]).unwrap());
    // Test case 13: empty plaintext.
    let mut b = vec![];
    k.seal_in_place_append_tag(Nonce::assume_unique_for_key([0; 12]), Aad::empty(), &mut b)
        .unwrap();
    assert_eq!(hex::encode(&b), "530f8afbc74536b9a963b4f1c4cb738b");
    // Test case 14: 16 zero bytes.
    let mut b = vec![0u8; 16];
    k.seal_in_place_append_tag(Nonce::assume_unique_for_key([0; 12]), Aad::empty(), &mut b)
        .unwrap();
    assert_eq!(
        hex::encode(&b),
        "cea7403d4d606b6e074ec5d3baf39d18d0d1c8a799996bf0265b98b5d48ab919"
    );
}

#[test]
fn blake3_empty_hash_and_cloned_context_equals_derive_key() {
    assert_eq!(
        blake3::hash(b"").to_hex().as_str(),
        "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262"
    );
    let dek = arr::<32>(&seq(0, 32));
    let id = arr::<32>(&seq(0x80, 32));
    let salt = arr::<24>(&seq(0xc0, 24));
    let mut m = dek.to_vec();
    m.extend_from_slice(&id);
    m.extend_from_slice(&salt);
    assert_eq!(
        *AdrKdf::new().key(&dek, &id, &salt),
        blake3::derive_key(CTX_EXTENT_V1, &m)
    );
}

struct Fix {
    dek: [u8; 32],
    family: [u8; 16],
    id: [u8; 32],
    salt: [u8; 24],
}
fn fix() -> Fix {
    Fix {
        dek: arr(&seq(0, 32)),
        family: arr(&seq(0x40, 16)),
        id: arr(&seq(0x80, 32)),
        salt: arr(&seq(0xc0, 24)),
    }
}

#[test]
fn extent_key_vectors() {
    let f = fix();
    let ctx = KeyCtx {
        dek: &f.dek,
        family: &f.family,
        dek_epoch: 7,
        kind: KIND_DATA,
    };
    let enc = EncDesc {
        codec: 0,
        level: 0,
        clen: 64,
    };
    let k = |p| hex::encode(&extent_key(p, &ctx, &f.id, &f.salt, &enc)[..]);
    assert_eq!(
        k(Profile::V0Draft),
        "d7ebd176b7909dfdb788a9235c0a02edf412815319fc15ede0e140abd2a2a673"
    );
    assert_eq!(
        k(Profile::Adr),
        "38db76592bf26edd34ad1ceece8b9e4c90f58c2981058b1f415d35b6af9def1a"
    );
    assert_eq!(
        k(Profile::Hardened),
        "11c0b591c327735ee6b91913514ca60e5ea143c2a1f2ebeab34a2443e75774c4"
    );
    assert_eq!(
        k(Profile::Fips),
        "f8f8b1a8c43cf9db8b1751e622fa4fff6284217c2a5652f0ab8bbee345041f16"
    );
}

#[test]
fn full_extent_body_vectors() {
    use crypto_kat::extent::{aad_for, Header, FORMAT_VER};
    let f = fix();
    let payload = seq(0, 64);
    let ctx = KeyCtx {
        dek: &f.dek,
        family: &f.family,
        dek_epoch: 7,
        kind: KIND_DATA,
    };
    let enc = EncDesc {
        codec: 0,
        level: 0,
        clen: 64,
    };
    let cases = [
        (Profile::Adr, "b2ae6b93fa4237489bbf2b8687e3edca255c14fcd775a2b844f3454cc3e57188c34fcfe5ce8396286a01d8a1601a5b48b9107e8525bbd025d318fd5e4f3556b0a2f1259968acf2f193750bc0bf1d730b"),
        (Profile::Hardened, "8c1e4b19c5324d4ec7f8e7358188479ab68cab7761f3514546e4c78421c7d834afff96e895642bd5723555eb365b317450b038fe0aaa43171217908c0da00676d4c8c1dda5af43e4797f631dd203a678"),
        (Profile::Fips, "9c0426dab191dd2a5b6c50203e2f8b3f940f2ae41376339491213b41d1958637ab64e0f390177eb83be0b8ef7b772b4255ca109fac4d0714b331586f158abb97fdb889bc5c76628cb6dbd4ebd4e86369"),
    ];
    for (p, want) in cases {
        let hd = Header {
            ver: FORMAT_VER,
            codec: 0,
            level: 0,
            ulen: 64,
            dek_epoch: 7,
            chunk_id: f.id,
            salt: f.salt,
            nonce: None,
        };
        let key = extent_key(p, &ctx, &f.id, &f.salt, &enc);
        let aad = aad_for(p, &hd, &f.family, KIND_DATA);
        let k = LessSafeKey::new(UnboundKey::new(&AES_256_GCM, &key[..]).unwrap());
        let mut b = payload.clone();
        k.seal_in_place_append_tag(
            Nonce::assume_unique_for_key([0; 12]),
            Aad::from(&aad[..]),
            &mut b,
        )
        .unwrap();
        assert_eq!(hex::encode(&b), want, "{p:?}");
    }
}

#[test]
fn purpose_key_vectors_and_domain_separation() {
    let kmk = arr::<32>(&seq(0x20, 32));
    let fam = arr::<16>(&seq(0x40, 16));
    let want = [
        (
            Purpose::Addressing,
            "51be94e46821496d256755858d4c9def5825899b520a03669cd61db4723cfd5f",
            "99106f5a6124c1ebd386b19e4c23613aa64457edc9ada6ee57191fe99557316a",
        ),
        (
            Purpose::Dek { epoch: 0 },
            "14399d9e7edf66717bfc1b86e7eccfbd18936aa5ad6dad3fbd489dc4be9329dc",
            "447a468ef6926721cb606ee1acadc09f395a046d1ad2cc4219f625caa69a516f",
        ),
        (
            Purpose::Dek { epoch: 1 },
            "e6bf9ce3d1f733ac70341fac2fd7857123fecda67ac532f181d8772f82b0d750",
            "f3a45703ced88e3b872d91e8db88e43f98785601352dfca8e0d240c87d6456be",
        ),
        (
            Purpose::Meta { kind: META_COMMIT },
            "af320957a0495146018dd3384176cda9ae47d68f82b5cb3d0d6d458b03d1829d",
            "ccc64d161c1bffcf9b25fa796284958effa1b3b187e1a22d378d919521512eca",
        ),
        (
            Purpose::Meta { kind: META_LEASE },
            "0b1fc200d9a2fa54e432ab7dd324a636d7a727e4abed98f55b0bf82e66090b17",
            "d235fe0328a0573eb7922fddc7551051578e99219cd028f6638adce7552a259e",
        ),
        (
            Purpose::VolumeMac,
            "ce2de1310616cd21c634c64d2e886042cf97b4b32f1f49942f2bd5253dc933ca",
            "1569738aa3c00458ec57f21a15d7f2490aa16cd964103e249e49909184088407",
        ),
        (
            Purpose::Pin,
            "16e74f8af79c8fd7560435a895c26a72ccf81fc766d6605e12c4f83d6b26e036",
            "a48277d5d88d21b0b09759cd7ef9e1a27561384ef85a5bb4c697b7dc878b5116",
        ),
    ];
    let mut seen = std::collections::HashSet::new();
    for (p, b3, fips) in want {
        assert_eq!(hex::encode(&purpose_key(&kmk, &fam, p)[..]), b3, "{p:?}");
        assert_eq!(
            hex::encode(&purpose_key_fips(&kmk, &fam, p)[..]),
            fips,
            "{p:?}"
        );
        assert!(seen.insert(b3), "purpose collision");
    }
    // A different family under the same KMK gives different keys for every purpose.
    let fam2 = [0x99u8; 16];
    for (p, b3, _) in want {
        assert_ne!(hex::encode(&purpose_key(&kmk, &fam2, p)[..]), b3);
    }
}

#[test]
fn derive_key_mode_is_separated_from_keyed_hash_mode() {
    // Same bytes through the two BLAKE3 modes the design uses must not coincide.
    let key = [5u8; 32];
    let msg = b"lunix extent v1";
    assert_ne!(
        blake3::derive_key("lunix extent v1", &key),
        *blake3::keyed_hash(&key, msg).as_bytes()
    );
    assert_ne!(
        blake3::derive_key("lunix extent v1", &key),
        *blake3::hash(&key).as_bytes()
    );
}

#[test]
fn extent_key_input_encoding_is_injective_across_id_lengths() {
    // ADR encoding DEK||chunk_id||salt has one variable-length field in the
    // middle; total length determines id_len, so (id16,salt) cannot alias (id32,salt').
    let dek = [1u8; 32];
    let fam = [2u8; 16];
    let ctx = KeyCtx {
        dek: &dek,
        family: &fam,
        dek_epoch: 0,
        kind: KIND_DATA,
    };
    let enc = EncDesc {
        codec: 0,
        level: 0,
        clen: 0,
    };
    let id32 = [9u8; 32];
    let salt = [3u8; 24];
    let id16 = &id32[..16];
    let a = extent_key(Profile::Adr, &ctx, id16, &salt, &enc);
    let b = extent_key(Profile::Adr, &ctx, &id32, &salt, &enc);
    assert_ne!(a[..], b[..]);
    let a = extent_key(Profile::Hardened, &ctx, id16, &salt, &enc);
    let b = extent_key(Profile::Hardened, &ctx, &id32, &salt, &enc);
    assert_ne!(a[..], b[..]);
}

#[test]
fn argon2id_rfc9106_vector() {
    use argon2::{Algorithm, Argon2, AssociatedData, ParamsBuilder, Version};
    let ad = AssociatedData::new(&[4u8; 12]).unwrap();
    let params = ParamsBuilder::new()
        .m_cost(32)
        .t_cost(3)
        .p_cost(4)
        .data(ad)
        .output_len(32)
        .build()
        .unwrap();
    let a =
        Argon2::new_with_secret(&[3u8; 8], Algorithm::Argon2id, Version::V0x13, params).unwrap();
    let mut out = [0u8; 32];
    a.hash_password_into(&[1u8; 32], &[2u8; 16], &mut out)
        .unwrap();
    assert_eq!(
        hex::encode(out),
        "0d640df58d78766c08c037a34a8b53c9d01ef0452d75b65eb52520e96b01e659"
    );
}

#[test]
fn slot_kek_and_xchacha_vectors() {
    use argon2::{Algorithm, Argon2, Params, Version};
    use chacha20poly1305::{
        aead::{Aead, Payload},
        KeyInit, XChaCha20Poly1305, XNonce,
    };
    let p = Params::new(19456, 2, 1, Some(32)).unwrap();
    let mut kek = [0u8; 32];
    Argon2::new(Algorithm::Argon2id, Version::V0x13, p)
        .hash_password_into(b"correct horse battery staple", &[0x11; 16], &mut kek)
        .unwrap();
    assert_eq!(
        hex::encode(kek),
        "064401872f712d299da5dc8fb3058e4af259cf62e8c3d78419ee23295cc88a19"
    );

    let dek = arr::<32>(&seq(0, 32));
    let nonce = arr::<24>(&seq(0x30, 24));
    let ct = XChaCha20Poly1305::new((&dek).into())
        .encrypt(
            &XNonce::try_from(&nonce[..]).unwrap(),
            Payload {
                msg: b"lunix xchacha vector",
                aad: b"aad-1",
            },
        )
        .unwrap();
    assert_eq!(
        hex::encode(ct),
        "3e2c41cfa2fa2d7db2ff252dab6d85252591313d6778eb12fe9079aa31a7420270de6b30"
    );
}
