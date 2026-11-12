//! Extent sealing and opening: round trips, the tamper matrix, the AAD and
//! `info` fields, moves between families, kinds and id lengths, the nonce
//! discipline and the tripwire.

mod common;

use std::collections::HashSet;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Arc;

use aws_lc_rs::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM};
use common::*;
use lunix_crypto::testing::{extent_key, ReplayRng};
use lunix_crypto::{AesGcmExtents, PlainExtents, VolumeKeys, TRIPWIRE_ENABLED};
use lunix_format::extent::{self, Cch1Header, InfoParts};
use lunix_format::{
    ChunkId, Codec, CryptoMode, DekGen, ExtentKind, FamilyId, IdLen, Level, Profile, VolumeUuid,
};
use lunix_iface::crypto::{EncodingDesc, ExtentCodec, OpenExpect, OpenOutput};
use lunix_iface::ErrorKind;
use proptest::prelude::*;

fn expect_plain<'a>(chunk: usize, id: Option<&'a ChunkId>, verify: bool) -> OpenExpect<'a> {
    OpenExpect {
        chunk_size: chunk as u32,
        chunk_id: id,
        id_prefix: None,
        verify_id: verify,
        output: OpenOutput::Plaintext,
    }
}

#[test]
fn round_trips_every_mode_codec_and_chunk_size() {
    for chunk in [16 * 1024usize, 64 * 1024, 1024 * 1024] {
        let data = pattern(chunk, 3);
        for mode in MODES {
            let vol = volume(mode, chunk as u32, 11);
            let codec_impl = vol.crypto.extents();
            for codec in [Codec::Raw, Codec::Lz4, Codec::Zstd] {
                let (id, comp, bytes) = seal_data(&**codec_impl, codec, &data);
                if codec != Codec::Raw {
                    assert!(comp.len() < data.len(), "test data must compress");
                }
                let ovh = mode.extent_ovh() as usize;
                assert_eq!(bytes.len(), ovh + comp.len(), "{mode:?} {codec:?} OVH");
                for verify in [false, true] {
                    let mut out = Vec::new();
                    let meta = codec_impl
                        .open(
                            ExtentKind::Data,
                            &bytes,
                            &expect_plain(chunk, Some(&id), verify),
                            &mut out,
                        )
                        .expect("open");
                    assert_eq!(out, data, "{mode:?} {codec:?} {chunk}");
                    assert_eq!(meta.desc.codec, codec);
                    assert_eq!(meta.desc.ulen as usize, chunk);
                    // The stored form is the codec output.
                    let mut stored = Vec::new();
                    let mut e = expect_plain(chunk, Some(&id), verify);
                    e.output = OpenOutput::Stored;
                    codec_impl
                        .open(ExtentKind::Data, &bytes, &e, &mut stored)
                        .expect("open stored");
                    assert_eq!(stored, comp);
                }
            }
        }
    }
}

#[test]
fn codec_three_round_trips_the_framing_and_the_decompressor_refuses_it() {
    for mode in MODES {
        let vol = volume(mode, 16_384, 12);
        let codec = vol.crypto.extents();
        let data = pattern(16_384, 1);
        let id = codec.chunk_id(ExtentKind::Data, &data);
        let frame = zstd::bulk::compress(&data, 3).expect("zstd");
        let desc = EncodingDesc {
            codec: Codec::ZstdDict,
            level: Level(3),
            ulen: 16_384,
            dict_id: None,
        };
        let mut bytes = Vec::new();
        codec
            .seal(ExtentKind::Data, &id, &desc, &frame, &mut bytes)
            .expect("seal codec 3");
        let mut e = expect_plain(16_384, Some(&id), false);
        e.output = OpenOutput::Stored;
        let mut out = Vec::new();
        let meta = codec
            .open(ExtentKind::Data, &bytes, &e, &mut out)
            .expect("framing round trips");
        assert_eq!(out, frame);
        assert_eq!(meta.desc.codec, Codec::ZstdDict);
        // Plaintext, or the id check that needs the plaintext: Unsupported.
        for (output, verify) in [(OpenOutput::Plaintext, false), (OpenOutput::Stored, true)] {
            let mut e = expect_plain(16_384, Some(&id), verify);
            e.output = output;
            let mut out = Vec::new();
            let err = codec
                .open(ExtentKind::Data, &bytes, &e, &mut out)
                .expect_err("phase 4");
            assert_eq!(err.kind(), ErrorKind::Unsupported, "{mode:?}");
            assert!(out.is_empty());
        }
        // A dict id with another codec is a caller bug.
        let mut bad = desc;
        bad.codec = Codec::Zstd;
        bad.dict_id = Some(7);
        assert!(codec
            .seal(ExtentKind::Data, &id, &bad, &frame, &mut Vec::new())
            .is_err());
    }
}

#[test]
fn map_nodes_round_trip_with_their_own_cap() {
    for mode in MODES {
        let vol = volume(mode, 16_384, 13);
        let codec = vol.crypto.extents();
        for size in [25usize, 4096, 65_536] {
            let node = pattern(size, 9);
            let id = codec.chunk_id(ExtentKind::Node, &node);
            let mut bytes = Vec::new();
            codec
                .seal(
                    ExtentKind::Node,
                    &id,
                    &enc_desc(Codec::Raw, size),
                    &node,
                    &mut bytes,
                )
                .expect("seal node");
            let mut out = Vec::new();
            // The chunk size is ignored for a node.
            let mut e = expect_plain(16_384, Some(&id), false);
            e.chunk_size = 0;
            codec
                .open(ExtentKind::Node, &bytes, &e, &mut out)
                .expect("open node");
            assert_eq!(out, node);
            // In an encrypted volume a node is not a data extent: the kind is
            // in the AAD and the key. The unencrypted tag (D11) covers the
            // bytes of the extent only, so there the map's node hash tells
            // them apart, not the codec.
            if mode == CryptoMode::Encrypted {
                assert!(codec
                    .open(
                        ExtentKind::Data,
                        &bytes,
                        &expect_plain(size, Some(&id), false),
                        &mut Vec::new()
                    )
                    .is_err());
            }
        }
        // Over the node cap.
        let big = pattern(65_537, 1);
        let id = codec.chunk_id(ExtentKind::Node, &big);
        assert!(codec
            .seal(
                ExtentKind::Node,
                &id,
                &enc_desc(Codec::Raw, big.len()),
                &big,
                &mut Vec::new()
            )
            .is_err());
    }
}

#[test]
fn node_ids_are_separate_from_data_ids_when_keyed() {
    let vol = volume(CryptoMode::Encrypted, 16_384, 14);
    let c = vol.crypto.extents();
    let d = pattern(300, 4);
    assert_ne!(
        c.chunk_id(ExtentKind::Data, &d),
        c.chunk_id(ExtentKind::Node, &d)
    );
    // Unkeyed modes hash the same way for both.
    let p = volume(CryptoMode::Integrity, 16_384, 14);
    assert_eq!(
        p.crypto.extents().chunk_id(ExtentKind::Data, &d),
        p.crypto.extents().chunk_id(ExtentKind::Node, &d)
    );
    assert_eq!(
        p.crypto.extents().chunk_id(ExtentKind::Data, &d).0,
        *blake3::hash(&d).as_bytes()
    );
}

/// Flipping any single byte of an extent fails `open`: header, id, `clen`,
/// salt, nonce, payload, tag. On unencrypted volumes the tag is checked first,
/// so the error is always `Auth`.
#[test]
fn tamper_matrix_every_byte_of_every_mode() {
    for chunk in [16 * 1024usize, 64 * 1024] {
        let data = pattern(chunk, 5);
        for mode in MODES {
            let vol = volume(mode, chunk as u32, 21);
            let codec = vol.crypto.extents();
            let bits: &[u8] = if chunk == 16 * 1024 {
                &[0x01, 0x80]
            } else {
                &[0x10]
            };
            for c in [Codec::Raw, Codec::Lz4] {
                let (id, _, bytes) = seal_data(&**codec, c, &data);
                for i in 0..bytes.len() {
                    for &bit in bits {
                        let mut bad = bytes.clone();
                        bad[i] ^= bit;
                        let mut out = Vec::new();
                        let r = codec.open(
                            ExtentKind::Data,
                            &bad,
                            &expect_plain(chunk, Some(&id), false),
                            &mut out,
                        );
                        let err = r.expect_err(&format!("{mode:?} {c:?} flip {i}"));
                        assert!(out.is_empty(), "output untouched at {i}");
                        if mode != CryptoMode::Encrypted {
                            assert_eq!(err.kind(), ErrorKind::Auth, "{mode:?} byte {i}");
                        }
                    }
                }
                for cut in [0usize, 1, 15, 16, 50, bytes.len() / 2, bytes.len() - 1] {
                    assert!(codec
                        .open(
                            ExtentKind::Data,
                            &bytes[..cut],
                            &expect_plain(chunk, Some(&id), false),
                            &mut Vec::new()
                        )
                        .is_err());
                }
                let mut longer = bytes.clone();
                longer.push(0);
                assert!(codec
                    .open(
                        ExtentKind::Data,
                        &longer,
                        &expect_plain(chunk, Some(&id), false),
                        &mut Vec::new()
                    )
                    .is_err());
            }
        }
    }
}

/// Builds an encrypted extent by hand, from the doc's layout, with the AAD and
/// `info` the caller gives, so one field at a time can be wrong.
fn forge(
    dek: &[u8; 32],
    fam: &FamilyId,
    id: &ChunkId,
    salt: &[u8; 24],
    payload: &[u8],
    aad: &[u8],
    info: &[u8],
) -> Vec<u8> {
    let key = extent_key(Profile::Default, dek, info).expect("key");
    let mut ct = payload.to_vec();
    let gcm = LessSafeKey::new(UnboundKey::new(&AES_256_GCM, &key).expect("key"));
    let tag = gcm
        .seal_in_place_separate_tag(
            Nonce::assume_unique_for_key([0; 12]),
            Aad::from(aad),
            &mut ct,
        )
        .expect("seal");
    let mut out = Vec::new();
    out.extend_from_slice(b"CCH1");
    out.push(1);
    out.extend_from_slice(&0u16.to_le_bytes());
    out.push(0);
    out.extend_from_slice(&(payload.len() as u64).to_le_bytes());
    out.extend_from_slice(&id.0);
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(&7u32.to_le_bytes());
    out.extend_from_slice(salt);
    out.extend_from_slice(&[0; 12]);
    out.extend_from_slice(&ct);
    out.extend_from_slice(tag.as_ref());
    let _ = fam;
    out
}

/// One test per AAD field and per `info` field: change exactly one byte of the
/// AAD (or the `info`) while everything else is right, and `open` fails.
#[test]
fn every_aad_and_info_field_is_authenticated() {
    let dek: [u8; 32] = arr(&seq_bytes(0x00, 32));
    let fam = FamilyId(arr(&seq_bytes(0x40, 16)));
    let id = ChunkId(arr(&seq_bytes(0x80, 32)));
    let salt: [u8; 24] = arr(&seq_bytes(0xc0, 24));
    let payload = seq_bytes(0, 64);
    let header = Cch1Header {
        codec: Codec::Raw,
        level: Level(0),
        ulen: 64,
    };
    let aad = extent::aad(
        &header,
        ExtentKind::Data,
        DekGen(7),
        &fam,
        IdLen::ThirtyTwo,
        &id,
    )
    .expect("aad");
    let info = extent::info(&InfoParts {
        family: &fam,
        kind: ExtentKind::Data,
        dek_gen: DekGen(7),
        codec: Codec::Raw,
        level: Level(0),
        clen: 64,
        id_len: IdLen::ThirtyTwo,
        chunk_id: &id,
        salt: &salt,
    })
    .expect("info");

    let keys = VolumeKeys::for_vectors(
        Profile::Default,
        fam,
        VolumeUuid([0; 16]),
        IdLen::ThirtyTwo,
        DekGen(7),
        &dek,
    );
    let codec = AesGcmExtents::new(keys).expect("codec");
    let open = |bytes: &[u8]| {
        codec.open(
            ExtentKind::Data,
            bytes,
            &OpenExpect {
                chunk_size: 64,
                chunk_id: Some(&id),
                id_prefix: None,
                verify_id: false,
                output: OpenOutput::Plaintext,
            },
            &mut Vec::new(),
        )
    };
    // The hand-built baseline is the real format: it opens.
    open(&forge(&dek, &fam, &id, &salt, &payload, &aad, &info)).expect("baseline opens");

    // AAD field names by offset (on-disk-formats.md 6.3).
    let aad_fields: [(&str, std::ops::Range<usize>); 9] = [
        ("ver", 0..1),
        ("codec", 1..2),
        ("level", 2..3),
        ("ulen", 3..7),
        ("kind", 7..8),
        ("dek_gen", 8..12),
        ("family_id", 12..28),
        ("id_len", 28..29),
        ("chunk_id", 29..61),
    ];
    for (name, range) in aad_fields {
        for i in range {
            let mut bad = aad.to_vec();
            bad[i] ^= 0x01;
            let e = forge(&dek, &fam, &id, &salt, &payload, &bad, &info);
            assert!(open(&e).is_err(), "AAD field {name} byte {i} is not bound");
        }
    }
    let info_fields: [(&str, std::ops::Range<usize>); 10] = [
        ("label", 0..16),
        ("family_id", 16..32),
        ("kind", 32..33),
        ("dek_gen", 33..37),
        ("codec", 37..38),
        ("level", 38..39),
        ("clen", 39..43),
        ("id_len", 43..44),
        ("chunk_id", 44..76),
        ("salt", 76..100),
    ];
    for (name, range) in info_fields {
        for i in range {
            let mut bad = info.to_vec();
            bad[i] ^= 0x01;
            let e = forge(&dek, &fam, &id, &salt, &payload, &aad, &bad);
            assert!(open(&e).is_err(), "info field {name} byte {i} is not bound");
        }
    }
}

#[test]
fn an_extent_moved_to_another_family_kind_or_id_len_fails() {
    let data = pattern(16_384, 6);
    let a = volume_with(
        CryptoMode::Encrypted,
        16_384,
        IdLen::ThirtyTwo,
        31,
        DekGen(0),
    );
    let (id, _, bytes) = seal_data(&**a.crypto.extents(), Codec::Raw, &data);
    let e = expect_plain(16_384, Some(&id), false);

    // Same KMK, different family id: the keys differ, the AAD differs.
    let mut other_family = a.desc.clone();
    other_family.family_id = FamilyId([0x77; 16]);
    let root = root_for(CryptoMode::Encrypted, 31);
    let f = lunix_crypto::VolumeCrypto::new(&other_family, root, DekGen(0)).expect("keys");
    assert!(f
        .extents()
        .open(ExtentKind::Data, &bytes, &e, &mut Vec::new())
        .is_err());

    // Same KMK and family, different id_len: only the AAD and info differ.
    let mut other_len = a.desc.clone();
    other_len.id_len = IdLen::Sixteen;
    let root = root_for(CryptoMode::Encrypted, 31);
    let l = lunix_crypto::VolumeCrypto::new(&other_len, root, DekGen(0)).expect("keys");
    assert!(l
        .extents()
        .open(ExtentKind::Data, &bytes, &e, &mut Vec::new())
        .is_err());

    // Sealed as a node, opened as data (and the reverse).
    let c = a.crypto.extents();
    let mut as_node = Vec::new();
    let nid = c.chunk_id(ExtentKind::Node, &data);
    c.seal(
        ExtentKind::Node,
        &nid,
        &enc_desc(Codec::Raw, data.len()),
        &data,
        &mut as_node,
    )
    .expect("seal");
    assert!(c
        .open(
            ExtentKind::Data,
            &as_node,
            &expect_plain(16_384, Some(&nid), false),
            &mut Vec::new()
        )
        .is_err());
    assert!(c
        .open(ExtentKind::Node, &bytes, &e, &mut Vec::new())
        .is_err());

    // Another volume with another KMK.
    let b = volume(CryptoMode::Encrypted, 16_384, 32);
    assert!(b
        .crypto
        .extents()
        .open(ExtentKind::Data, &bytes, &e, &mut Vec::new())
        .is_err());

    // The same family and KMK on a second volume (dedup across volumes of one
    // family): the extent is portable, which is the design.
    let mut sibling = a.desc.clone();
    sibling.volume_uuid = VolumeUuid([0x99; 16]);
    let s =
        lunix_crypto::VolumeCrypto::new(&sibling, root_for(CryptoMode::Encrypted, 31), DekGen(0))
            .expect("keys");
    s.extents()
        .open(ExtentKind::Data, &bytes, &e, &mut Vec::new())
        .expect("portable inside the family");
}

#[test]
fn unencrypted_extents_are_bound_to_their_integrity_key() {
    let data = pattern(16_384, 7);
    let a = volume(CryptoMode::Integrity, 16_384, 41);
    let (id, _, bytes) = seal_data(&**a.crypto.extents(), Codec::Raw, &data);
    let e = expect_plain(16_384, Some(&id), false);
    // Another integrity key rejects it.
    let b = volume(CryptoMode::Integrity, 16_384, 42);
    assert_eq!(
        b.crypto
            .extents()
            .open(ExtentKind::Data, &bytes, &e, &mut Vec::new())
            .expect_err("other key")
            .kind(),
        ErrorKind::Auth
    );
    // A plain-mode reader rejects the keyed tag and the reverse: the keyed
    // tag cannot be recomputed by someone who only has the bucket.
    let p = volume(CryptoMode::Plain, 16_384, 41);
    assert!(p
        .crypto
        .extents()
        .open(ExtentKind::Data, &bytes, &e, &mut Vec::new())
        .is_err());
    let (_, _, plain_bytes) = seal_data(&**p.crypto.extents(), Codec::Raw, &data);
    assert!(a
        .crypto
        .extents()
        .open(ExtentKind::Data, &plain_bytes, &e, &mut Vec::new())
        .is_err());
}

#[test]
fn wrong_expectations_and_the_id_commitment() {
    for mode in MODES {
        let vol = volume(mode, 16_384, 51);
        let c = vol.crypto.extents();
        let data = pattern(16_384, 8);
        let (id, _, bytes) = seal_data(&**c, Codec::Raw, &data);
        // Expected full id and expected prefix.
        let mut other = id;
        other.0[0] ^= 1;
        assert_eq!(
            c.open(
                ExtentKind::Data,
                &bytes,
                &expect_plain(16_384, Some(&other), false),
                &mut Vec::new()
            )
            .expect_err("id")
            .kind(),
            ErrorKind::Auth
        );
        let prefix = lunix_format::IdPrefix::of(&id, IdLen::Sixteen);
        let mut e = expect_plain(16_384, None, false);
        e.id_prefix = Some(&prefix);
        c.open(ExtentKind::Data, &bytes, &e, &mut Vec::new())
            .expect("right prefix");
        let wrong_prefix = lunix_format::IdPrefix::of(&other, IdLen::ThirtyTwo);
        e.id_prefix = Some(&wrong_prefix);
        assert!(c
            .open(ExtentKind::Data, &bytes, &e, &mut Vec::new())
            .is_err());
        // The volume's chunk size, not the header's ulen, decides.
        for wrong in [8_192usize, 32_768, 1 << 20] {
            assert!(c
                .open(
                    ExtentKind::Data,
                    &bytes,
                    &expect_plain(wrong, Some(&id), false),
                    &mut Vec::new()
                )
                .is_err());
        }
        // Sealed under an id that is not the content's: only verify_id sees it.
        let wrong = c.chunk_id(ExtentKind::Data, &pattern(16_384, 99));
        let mut forged = Vec::new();
        c.seal(
            ExtentKind::Data,
            &wrong,
            &enc_desc(Codec::Raw, data.len()),
            &data,
            &mut forged,
        )
        .expect("seal");
        c.open(
            ExtentKind::Data,
            &forged,
            &expect_plain(16_384, Some(&wrong), false),
            &mut Vec::new(),
        )
        .expect("a local read trusts the tag");
        let mut out = Vec::new();
        let err = c
            .open(
                ExtentKind::Data,
                &forged,
                &expect_plain(16_384, Some(&wrong), true),
                &mut out,
            )
            .expect_err("key commitment");
        assert_eq!(err.kind(), ErrorKind::Auth);
        assert!(out.is_empty());
        // The same check through a compressed codec.
        let comp = compress(Codec::Lz4, &data);
        let mut forged2 = Vec::new();
        c.seal(
            ExtentKind::Data,
            &wrong,
            &enc_desc(Codec::Lz4, data.len()),
            &comp,
            &mut forged2,
        )
        .expect("seal");
        let mut e = expect_plain(16_384, Some(&wrong), true);
        e.output = OpenOutput::Stored;
        assert_eq!(
            c.open(ExtentKind::Data, &forged2, &e, &mut Vec::new())
                .expect_err("commitment through lz4")
                .kind(),
            ErrorKind::Auth
        );
    }
}

/// A payload that decodes to something other than `ulen` is rejected, and the
/// decoder never writes past the chunk size.
#[test]
fn decompression_is_capped_by_the_volume_chunk_size() {
    for mode in MODES {
        let vol = volume(mode, 16_384, 61);
        let c = vol.crypto.extents();
        let big = pattern(65_536, 2);
        let id = c.chunk_id(ExtentKind::Data, &big);
        for codec in [Codec::Lz4, Codec::Zstd] {
            // The header claims 16 KiB; the payload expands to 64 KiB.
            let comp = compress(codec, &big);
            let mut bytes = Vec::new();
            c.seal(
                ExtentKind::Data,
                &id,
                &enc_desc(codec, 16_384),
                &comp,
                &mut bytes,
            )
            .expect("seal accepts any codec output");
            let mut out = Vec::new();
            let err = c
                .open(
                    ExtentKind::Data,
                    &bytes,
                    &expect_plain(16_384, Some(&id), false),
                    &mut out,
                )
                .expect_err("bomb");
            assert_eq!(err.kind(), ErrorKind::Corrupt, "{mode:?} {codec:?}");
            assert!(out.is_empty());
            // And one that expands to less than it claims.
            let small = compress(codec, &big[..8_192]);
            let mut bytes = Vec::new();
            c.seal(
                ExtentKind::Data,
                &id,
                &enc_desc(codec, 16_384),
                &small,
                &mut bytes,
            )
            .expect("seal");
            assert!(c
                .open(
                    ExtentKind::Data,
                    &bytes,
                    &expect_plain(16_384, Some(&id), false),
                    &mut Vec::new()
                )
                .is_err());
        }
        // A header `ulen` above the volume's chunk size, authenticated or not.
        let data = pattern(65_536, 3);
        let (id2, _, bytes) = seal_data(&**c, Codec::Raw, &data);
        assert!(c
            .open(
                ExtentKind::Data,
                &bytes,
                &expect_plain(16_384, Some(&id2), false),
                &mut Vec::new()
            )
            .is_err());
        // Garbage that is not a frame.
        let mut junk = Vec::new();
        c.seal(
            ExtentKind::Data,
            &id,
            &enc_desc(Codec::Zstd, 16_384),
            &noise(500, 1),
            &mut junk,
        )
        .expect("seal");
        assert_eq!(
            c.open(
                ExtentKind::Data,
                &junk,
                &expect_plain(16_384, Some(&id), false),
                &mut Vec::new()
            )
            .expect_err("junk")
            .kind(),
            ErrorKind::Corrupt
        );
    }
}

#[test]
fn seal_validates_its_descriptor_and_appends() {
    for mode in MODES {
        let vol = volume(mode, 16_384, 71);
        let c = vol.crypto.extents();
        let data = pattern(16_384, 1);
        let id = c.chunk_id(ExtentKind::Data, &data);
        let mut out = vec![0xee; 5];
        // clen 0 for codec 0, wrong level for the codec, ulen 0 and above 1 MiB.
        let bad: [EncodingDesc; 4] = [
            EncodingDesc {
                level: Level(3),
                ..enc_desc(Codec::Raw, 16_384)
            },
            EncodingDesc {
                ulen: 0,
                ..enc_desc(Codec::Raw, 16_384)
            },
            EncodingDesc {
                ulen: (1 << 20) + 1,
                ..enc_desc(Codec::Zstd, 16_384)
            },
            enc_desc(Codec::Raw, 16_000),
        ];
        for d in &bad {
            assert!(c.seal(ExtentKind::Data, &id, d, &data, &mut out).is_err());
            assert_eq!(out, vec![0xee; 5], "a failed seal leaves out unchanged");
        }
        c.seal(
            ExtentKind::Data,
            &id,
            &enc_desc(Codec::Raw, 16_384),
            &data,
            &mut out,
        )
        .expect("seal");
        assert_eq!(&out[..5], &[0xee; 5], "seal appends");
        assert_eq!(out.len(), 5 + 16_384 + mode.extent_ovh() as usize);
    }
}

fn salt_of(extent: &[u8]) -> [u8; 24] {
    arr(&extent[56..80])
}

/// Sealing the same chunk twice, and with two codecs, gives a fresh salt and so
/// a fresh key every time (1 000 each).
#[test]
fn nonce_discipline_fresh_salt_and_key_on_every_seal() {
    let dek: [u8; 32] = arr(&seq_bytes(0x10, 32));
    let fam = FamilyId([0x21; 16]);
    let keys = VolumeKeys::for_vectors(
        Profile::Default,
        fam,
        VolumeUuid([1; 16]),
        IdLen::ThirtyTwo,
        DekGen(3),
        &dek,
    );
    let codec = AesGcmExtents::new(Arc::clone(&keys)).expect("codec");
    let data = pattern(16_384, 4);
    let id = codec.chunk_id(ExtentKind::Data, &data);
    for (name, codecs) in [
        ("same chunk, same codec", vec![Codec::Lz4; 1000]),
        (
            "same chunk, two codecs",
            (0..1000)
                .map(|i| if i % 2 == 0 { Codec::Lz4 } else { Codec::Zstd })
                .collect(),
        ),
    ] {
        let mut salts = HashSet::new();
        let mut keys_seen = HashSet::new();
        let mut cts = HashSet::new();
        for c in &codecs {
            let comp = compress(*c, &data);
            let mut out = Vec::new();
            codec
                .seal(
                    ExtentKind::Data,
                    &id,
                    &enc_desc(*c, data.len()),
                    &comp,
                    &mut out,
                )
                .expect("seal");
            let salt = salt_of(&out);
            // Recompute the extent key from the stored header.
            let info = extent::info(&InfoParts {
                family: &fam,
                kind: ExtentKind::Data,
                dek_gen: DekGen(3),
                codec: *c,
                level: level_of(*c),
                clen: comp.len() as u32,
                id_len: IdLen::ThirtyTwo,
                chunk_id: &id,
                salt: &salt,
            })
            .expect("info");
            keys_seen.insert(extent_key(Profile::Default, &dek, &info).expect("key"));
            assert!(salts.insert(salt), "{name}: a salt repeated");
            cts.insert(out[92..].to_vec());
        }
        assert_eq!(salts.len(), 1000, "{name}");
        assert_eq!(keys_seen.len(), 1000, "{name}: keys must all differ");
        assert_eq!(cts.len(), 1000, "{name}: ciphertexts must all differ");
    }
}

/// 1 000 000 seals in the tripwire build never repeat a `(key, nonce)` pair.
#[test]
fn a_million_seals_never_repeat_a_pair() {
    const { assert!(TRIPWIRE_ENABLED, "this test runs in the tripwire build") };
    let vol = volume(CryptoMode::Encrypted, 16_384, 81);
    let keys = vol.crypto.volume_keys();
    let codec = AesGcmExtents::new(Arc::clone(keys)).expect("codec");
    let payload = seq_bytes(0, 64);
    let id = ChunkId([5; 32]);
    let desc = enc_desc(Codec::Raw, 64);
    let mut out = Vec::with_capacity(256);
    let mut salts = HashSet::with_capacity(1_000_000);
    for _ in 0..1_000_000 {
        out.clear();
        codec
            .seal(ExtentKind::Data, &id, &desc, &payload, &mut out)
            .expect("seal");
        salts.insert(salt_of(&out));
    }
    assert_eq!(codec.tripwire_len(), 1_000_000);
    assert_eq!(salts.len(), 1_000_000);
}

/// A test that replays an RNG makes the tripwire fire.
#[test]
fn a_replayed_rng_fires_the_tripwire() {
    const { assert!(TRIPWIRE_ENABLED) };
    let vol = volume(CryptoMode::Encrypted, 16_384, 82);
    let salt = seq_bytes(0xc0, 24);
    let codec = AesGcmExtents::with_rng(
        Arc::clone(vol.crypto.volume_keys()),
        ReplayRng::new(vec![salt]),
        false,
    )
    .expect("codec");
    let payload = seq_bytes(0, 64);
    let id = ChunkId([5; 32]);
    let desc = enc_desc(Codec::Raw, 64);
    let mut seal = || {
        codec
            .seal(ExtentKind::Data, &id, &desc, &payload, &mut Vec::new())
            .expect("seal");
    };
    seal();
    let second = catch_unwind(AssertUnwindSafe(&mut seal));
    let msg = second.expect_err("the tripwire must fire on a repeated pair");
    let text = msg
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| msg.downcast_ref::<&str>().map(|s| (*s).to_owned()))
        .unwrap_or_default();
    assert!(text.contains("repeated"), "{text}");
    assert!(!text.contains("0x"), "the message shows no key material");
    // Distinct salts under the same replay source do not fire it.
    let codec = AesGcmExtents::with_rng(
        Arc::clone(vol.crypto.volume_keys()),
        ReplayRng::new(vec![seq_bytes(1, 24), seq_bytes(2, 24), seq_bytes(3, 24)]),
        false,
    )
    .expect("codec");
    for _ in 0..3 {
        codec
            .seal(ExtentKind::Data, &id, &desc, &payload, &mut Vec::new())
            .expect("seal");
    }
}

/// An older DEK generation still opens; a newer one than the keyring's does
/// not; rotation changes the key of new extents only.
#[test]
fn dek_generations() {
    let data = pattern(16_384, 2);
    let g0 = volume_with(
        CryptoMode::Encrypted,
        16_384,
        IdLen::ThirtyTwo,
        91,
        DekGen(0),
    );
    let g2 = volume_with(
        CryptoMode::Encrypted,
        16_384,
        IdLen::ThirtyTwo,
        91,
        DekGen(2),
    );
    let (id, _, old) = seal_data(&**g0.crypto.extents(), Codec::Raw, &data);
    let (_, _, new) = seal_data(&**g2.crypto.extents(), Codec::Raw, &data);
    assert_eq!(&old[52..56], &0u32.to_le_bytes());
    assert_eq!(&new[52..56], &2u32.to_le_bytes());
    let e = expect_plain(16_384, Some(&id), true);
    // The newer keyring reads both generations.
    for bytes in [&old, &new] {
        let mut out = Vec::new();
        g2.crypto
            .extents()
            .open(ExtentKind::Data, bytes, &e, &mut out)
            .expect("derivable generation");
        assert_eq!(out, data);
    }
    // A keyring at generation 0 cannot derive generation 2; before the tag a
    // forged header cannot be told from a writer bug, so it is `Auth`.
    assert_eq!(
        g0.crypto
            .extents()
            .open(ExtentKind::Data, &new, &e, &mut Vec::new())
            .expect_err("dek_gen above the keyring's")
            .kind(),
        ErrorKind::Auth
    );
}

/// The FIPS profile derives keys but cannot seal in phase 1.
#[test]
fn the_fips_profile_cannot_seal() {
    let desc = descriptor(
        CryptoMode::Encrypted,
        Profile::Fips,
        16_384,
        IdLen::ThirtyTwo,
        [3; 16],
        [4; 16],
    );
    let crypto =
        lunix_crypto::VolumeCrypto::new(&desc, root_for(CryptoMode::Encrypted, 5), DekGen(0))
            .expect("keys derive");
    let data = pattern(16_384, 1);
    let c = crypto.extents();
    let id = c.chunk_id(ExtentKind::Data, &data);
    // The chunk id is HMAC-SHA-256 under the addressing key.
    let key = crypto
        .volume_keys()
        .addressing_key_for_tests(false)
        .expect("key");
    let hk = aws_lc_rs::hmac::Key::new(aws_lc_rs::hmac::HMAC_SHA256, &key);
    assert_eq!(id.0.as_slice(), aws_lc_rs::hmac::sign(&hk, &data).as_ref());
    let mut out = Vec::new();
    for r in [
        c.seal(
            ExtentKind::Data,
            &id,
            &enc_desc(Codec::Raw, data.len()),
            &data,
            &mut out,
        ),
        crypto.meta().seal_lease(&lease()).map(|_| ()),
    ] {
        assert_eq!(r.expect_err("unsupported").kind(), ErrorKind::Unsupported);
    }
    assert!(out.is_empty());
}

fn lease() -> lunix_format::lease::Lease {
    lunix_format::lease::Lease {
        released: false,
        epoch: lunix_format::Epoch(3),
        deadline: lunix_format::TimeMs(10),
        host: "host-a".into(),
        cluster: String::new(),
        transferred_to: None,
    }
}

#[test]
fn codec_constructors_refuse_the_wrong_mode() {
    let enc = volume(CryptoMode::Encrypted, 16_384, 1);
    let int = volume(CryptoMode::Integrity, 16_384, 1);
    assert!(PlainExtents::new(Arc::clone(enc.crypto.volume_keys())).is_err());
    assert!(AesGcmExtents::new(Arc::clone(int.crypto.volume_keys())).is_err());
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    #[test]
    fn any_payload_round_trips_and_any_flip_is_caught(
        payload in proptest::collection::vec(any::<u8>(), 16_384),
        flip in 0usize..(16_384 + 108),
        bit in 0u8..8,
        mode_ix in 0usize..3,
    ) {
        let mode = MODES[mode_ix];
        let vol = volume(mode, 16_384, 17);
        let c = vol.crypto.extents();
        let (id, _, bytes) = seal_data(&**c, Codec::Raw, &payload);
        let mut out = Vec::new();
        c.open(ExtentKind::Data, &bytes, &expect_plain(16_384, Some(&id), true), &mut out).unwrap();
        prop_assert_eq!(&out, &payload);
        let mut bad = bytes.clone();
        let i = flip % bad.len();
        bad[i] ^= 1 << bit;
        prop_assert!(c
            .open(ExtentKind::Data, &bad, &expect_plain(16_384, Some(&id), false), &mut Vec::new())
            .is_err());
    }
}

/// The ledger is bounded: past its cap it stops recording and says so.
#[test]
fn the_tripwire_ledger_is_capped() {
    const { assert!(TRIPWIRE_ENABLED) };
    let vol = volume(CryptoMode::Encrypted, 16_384, 83);
    let codec = AesGcmExtents::new(Arc::clone(vol.crypto.volume_keys())).expect("codec");
    let payload = seq_bytes(0, 64);
    let id = ChunkId([6; 32]);
    let desc = enc_desc(Codec::Raw, 64);
    let mut out = Vec::with_capacity(256);
    let cap = 4 * 1024 * 1024;
    for _ in 0..cap {
        out.clear();
        codec
            .seal(ExtentKind::Data, &id, &desc, &payload, &mut out)
            .expect("seal");
    }
    assert_eq!(codec.tripwire_len(), cap);
    assert!(!codec.tripwire_overflowed());
    out.clear();
    codec
        .seal(ExtentKind::Data, &id, &desc, &payload, &mut out)
        .expect("seal past the cap still works");
    assert_eq!(codec.tripwire_len(), cap, "nothing recorded past the cap");
    assert!(codec.tripwire_overflowed());
}
