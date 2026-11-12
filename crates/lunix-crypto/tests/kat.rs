//! Known-answer tests: every field of the normative vector file
//! `bench/crypto-kat/raw/kat-vectors.json` (ADR-11), for the default and the
//! FIPS derivations. FIPS seals run only through `test-rng`.

mod common;

use std::collections::{BTreeMap, BTreeSet};

use aws_lc_rs::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM};
use common::*;
use lunix_crypto::keyring::passphrase_kek;
use lunix_crypto::testing::{extent_key, xchacha_seal, ReplayRng};
use lunix_crypto::{AesGcmExtents, Kmk, Root, VolumeKeys};
use lunix_format::extent::{self, Cch1Header, InfoParts};
use lunix_format::keyring::{Argon2Params, RootKind, Slot, SlotParams, SlotWrap};
use lunix_format::purpose::Purpose;
use lunix_format::{
    ChunkId, Codec, CryptoMode, DekGen, ExtentKind, FamilyId, IdLen, Level, ObjectKind, Profile,
    VolumeUuid,
};
use lunix_iface::crypto::{ExtentCodec, KeyMaterial, OpenExpect, OpenOutput};

struct Vectors {
    map: BTreeMap<String, String>,
    checked: BTreeSet<String>,
}

impl Vectors {
    fn load() -> Self {
        let text = include_str!("../../../bench/crypto-kat/raw/kat-vectors.json");
        let map: BTreeMap<String, String> = serde_json::from_str(text).expect("kat json");
        Self {
            map,
            checked: BTreeSet::new(),
        }
    }

    fn get(&self, k: &str) -> Vec<u8> {
        hex(self.map.get(k).unwrap_or_else(|| panic!("missing {k}")))
    }

    fn expect(&mut self, name: &str, got: &[u8]) {
        assert_eq!(
            hex::encode(got),
            self.map[name],
            "vector {name} is not reproduced"
        );
        self.checked.insert(name.to_owned());
    }
}

fn family() -> FamilyId {
    FamilyId(arr(&seq_bytes(0x40, 16)))
}

#[test]
fn every_field_of_the_vector_file_is_reproduced() {
    let mut v = Vectors::load();
    let dek: [u8; 32] = arr(&v.get("dek"));
    let kmk_bytes: [u8; 32] = arr(&v.get("kmk"));
    let fam = FamilyId(arr(&v.get("family")));
    assert_eq!(fam, family());
    let chunk_id = ChunkId(arr(&v.get("chunk_id")));
    let salt: [u8; 24] = arr(&v.get("salt"));
    let payload = v.get("payload_raw64");
    // Inputs of the file: they must be what the doc says they are.
    assert_eq!(dek.to_vec(), seq_bytes(0x00, 32));
    assert_eq!(kmk_bytes.to_vec(), seq_bytes(0x20, 32));
    assert_eq!(fam.0.to_vec(), seq_bytes(0x40, 16));
    assert_eq!(chunk_id.0.to_vec(), seq_bytes(0x80, 32));
    assert_eq!(salt.to_vec(), seq_bytes(0xc0, 24));
    assert_eq!(payload, seq_bytes(0x00, 64));
    assert_eq!(v.map["dek_epoch"], "7");
    for k in [
        "dek",
        "kmk",
        "family",
        "chunk_id",
        "salt",
        "payload_raw64",
        "dek_epoch",
    ] {
        v.checked.insert(k.to_owned());
    }

    // Purpose keys under the KMK, default and FIPS derivations.
    let root = Root::from_bytes(RootKind::Kmk, &kmk_bytes);
    let purposes: [(&str, Purpose); 7] = [
        ("addressing", Purpose::DataAddressing),
        ("dek0", Purpose::Dek(DekGen(0))),
        ("dek1", Purpose::Dek(DekGen(1))),
        ("meta_commit", Purpose::Metadata(ObjectKind::Commit)),
        ("meta_lease", Purpose::Metadata(ObjectKind::Lease)),
        ("volume_mac", Purpose::DescriptorMac),
        ("pin", Purpose::Fingerprint),
    ];
    for (name, p) in &purposes {
        let k =
            lunix_crypto::testing::purpose_key(Profile::Default, &root, &fam, p).expect("purpose");
        v.expect(&format!("purpose_{name}"), &k[..]);
        let k = lunix_crypto::testing::purpose_key(Profile::Fips, &root, &fam, p)
            .expect("fips purpose");
        v.expect(&format!("purpose_fips_{name}"), &k[..]);
    }
    v.expect(
        "purpose_msg_dek1",
        &Purpose::Dek(DekGen(1)).message(&fam).expect("msg"),
    );

    // `info` and the AAD, then the per-extent keys.
    let header = Cch1Header {
        codec: Codec::Raw,
        level: Level(0),
        ulen: 64,
    };
    let info = extent::info(&InfoParts {
        family: &fam,
        kind: ExtentKind::Data,
        dek_gen: DekGen(7),
        codec: Codec::Raw,
        level: Level(0),
        clen: 64,
        id_len: IdLen::ThirtyTwo,
        chunk_id: &chunk_id,
        salt: &salt,
    })
    .expect("info");
    v.expect("info_v2", &info);
    let aad = extent::aad(
        &header,
        ExtentKind::Data,
        DekGen(7),
        &fam,
        IdLen::ThirtyTwo,
        &chunk_id,
    )
    .expect("aad");
    v.expect("aad_hardened", &aad);
    v.expect("aad_fips_kdf_zero_nonce", &aad);
    let key_hardened = extent_key(Profile::Default, &dek, &info).expect("key");
    v.expect("key_hardened", &key_hardened);
    let key_fips = extent_key(Profile::Fips, &dek, &info).expect("fips key");
    v.expect("key_fips", &key_fips);

    // The bodies through the real seal path, with the salt injected.
    for (name, profile, fips) in [
        ("body_hardened", Profile::Default, false),
        ("body_fips_kdf_zero_nonce", Profile::Fips, true),
    ] {
        let keys = VolumeKeys::for_vectors(
            profile,
            fam,
            VolumeUuid([0; 16]),
            IdLen::ThirtyTwo,
            DekGen(7),
            &dek,
        );
        let codec = AesGcmExtents::with_rng(keys, ReplayRng::new(vec![salt.to_vec()]), fips)
            .expect("codec");
        let mut out = Vec::new();
        codec
            .seal(
                ExtentKind::Data,
                &chunk_id,
                &enc_desc(Codec::Raw, 64),
                &payload,
                &mut out,
            )
            .expect("seal");
        // Layout of on-disk-formats.md 23.3: 92 header bytes, then body.
        assert_eq!(out.len(), 172, "{name}");
        assert_eq!(&out[..4], b"CCH1");
        assert_eq!(out[4], 1);
        assert_eq!(&out[5..8], &[0, 0, 0]);
        assert_eq!(&out[8..16], &64u64.to_le_bytes());
        assert_eq!(&out[16..48], &chunk_id.0);
        assert_eq!(&out[48..52], &64u32.to_le_bytes());
        assert_eq!(&out[52..56], &7u32.to_le_bytes());
        assert_eq!(&out[56..80], &salt);
        assert_eq!(&out[80..92], &[0u8; 12]);
        v.expect(name, &out[92..]);
        if !fips {
            // Open it back through the same path.
            let expect = OpenExpect {
                chunk_size: 64,
                chunk_id: Some(&chunk_id),
                id_prefix: None,
                verify_id: false,
                output: OpenOutput::Plaintext,
            };
            let mut plain = Vec::new();
            codec
                .open(ExtentKind::Data, &out, &expect, &mut plain)
                .expect("open");
            assert_eq!(plain, payload);
        }
    }

    // The slot AAD, the XChaCha20-Poly1305 vector and the Argon2id vector.
    let slot = Slot {
        slot_id: 1,
        params: SlotParams::Passphrase(Argon2Params {
            m_kib: 19_456,
            t: 2,
            p: 1,
            salt: [0x11; 16],
        }),
        wrap: SlotWrap::Key {
            wrap_ver: 1,
            nonce: [0; 24],
            ciphertext: [0; 32],
            tag: [0; 16],
        },
    };
    v.expect("slot_aad_pass", &slot.aad(&fam));
    let nonce: [u8; 24] = arr(&seq_bytes(0x30, 24));
    v.expect("xchacha_nonce", &nonce);
    let ct = xchacha_seal(&dek, &nonce, b"aad-1", b"lunix xchacha vector").expect("xchacha");
    v.expect("xchacha_ct", &ct);
    let SlotParams::Passphrase(p) = &slot.params else {
        unreachable!()
    };
    let kek = passphrase_kek(b"correct horse battery staple", p).expect("argon2id");
    v.expect("argon2id_kek_m19456_t2_p1", &kek[..]);

    // First-draft schemes the vector file keeps for history (ADR-11 "Amended
    // before acceptance"); they are not a product path, so they are computed
    // here with the raw primitives.
    let key_v0 = blake3::derive_key("lunix extent v0", &[&dek[..], &chunk_id.0[..]].concat());
    v.expect("key_v0", &key_v0);
    let key_adr = blake3::derive_key(
        "lunix extent v1",
        &[&dek[..], &chunk_id.0[..], &salt[..]].concat(),
    );
    v.expect("key_adr", &key_adr);
    let aad_adr = [&[1u8, 0, 0][..], &64u32.to_le_bytes(), &chunk_id.0].concat();
    v.expect("aad_adr", &aad_adr);
    let gcm = LessSafeKey::new(UnboundKey::new(&AES_256_GCM, &key_adr).expect("key"));
    let mut body = payload.clone();
    gcm.seal_in_place_append_tag(
        Nonce::assume_unique_for_key([0; 12]),
        Aad::from(&aad_adr[..]),
        &mut body,
    )
    .expect("seal");
    v.expect("body_adr", &body);

    let all: BTreeSet<String> = v.map.keys().cloned().collect();
    assert_eq!(
        v.checked,
        all,
        "fields of the vector file that no assertion covers: {:?}",
        all.difference(&v.checked).collect::<Vec<_>>()
    );
}

#[test]
fn the_real_hierarchy_derives_the_vector_keys() {
    let v = Vectors::load();
    let mut desc = descriptor(
        CryptoMode::Encrypted,
        Profile::Default,
        65_536,
        IdLen::ThirtyTwo,
        family().0,
        [0xa0; 16],
    );
    let root = Root::Kmk(Kmk::from_bytes(&arr(&v.get("kmk"))));
    // The fingerprint stored in the descriptor is the first 16 bytes of the
    // `pin` vector, and the descriptor MAC key is `volume_mac`.
    let fp = lunix_crypto::kmk_fingerprint(&desc, &root).expect("fp");
    assert_eq!(fp.to_vec(), v.get("purpose_pin")[..16].to_vec());
    let keys = VolumeKeys::derive(&desc, Some(root), DekGen(0)).expect("keys");
    assert_eq!(keys.fingerprint(), fp);
    assert_eq!(
        keys.meta_key_for_tests(ObjectKind::Commit).expect("key"),
        arr::<32>(&v.get("purpose_meta_commit"))
    );
    assert_eq!(
        keys.meta_key_for_tests(ObjectKind::Lease).expect("key"),
        arr::<32>(&v.get("purpose_meta_lease"))
    );
    assert_eq!(
        keys.addressing_key_for_tests(false).expect("key"),
        arr::<32>(&v.get("purpose_addressing"))
    );
    // Descriptor MAC under the `volume_mac` key.
    let root = Root::Kmk(Kmk::from_bytes(&arr(&v.get("kmk"))));
    desc.kmk_fingerprint = fp;
    let mac = lunix_crypto::descriptor_mac(&desc, Some(&root)).expect("mac");
    let key: [u8; 32] = arr(&v.get("purpose_volume_mac"));
    let input = desc.mac_input().expect("input");
    let hk = aws_lc_rs::hmac::Key::new(aws_lc_rs::hmac::HMAC_SHA256, &key);
    assert_eq!(mac.as_slice(), aws_lc_rs::hmac::sign(&hk, &input).as_ref());
}
