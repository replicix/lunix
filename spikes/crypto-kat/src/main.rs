//! `crypto-kat vectors|demo|bench` — emits the published vectors, the nonce-reuse
//! demonstration, and the per-extent key-schedule cost measurements.

use aws_lc_rs::aead::{Aad, LessSafeKey, Nonce, RandomizedNonceKey, UnboundKey, AES_256_GCM};
use crypto_kat::extent::*;
use crypto_kat::kdf::*;
use crypto_kat::{attack, slots};
use std::hint::black_box;
use std::time::Instant;

pub fn seq(start: u8, n: usize) -> Vec<u8> {
    (0..n).map(|i| start.wrapping_add(i as u8)).collect()
}
fn arr<const N: usize>(v: &[u8]) -> [u8; N] {
    v.try_into().unwrap()
}

/// Fixed inputs for every published vector.
struct Fixed {
    dek: [u8; 32],
    kmk: [u8; 32],
    family: [u8; 16],
    id: [u8; 32],
    salt: [u8; 24],
    payload: Vec<u8>,
}
fn fixed() -> Fixed {
    Fixed {
        dek: arr(&seq(0x00, 32)),
        kmk: arr(&seq(0x20, 32)),
        family: arr(&seq(0x40, 16)),
        id: arr(&seq(0x80, 32)),
        salt: arr(&seq(0xc0, 24)),
        payload: seq(0x00, 64),
    }
}

fn jprint(items: &[(&str, String)]) {
    println!("{{");
    for (i, (k, v)) in items.iter().enumerate() {
        println!(
            "  \"{}\": \"{}\"{}",
            k,
            v,
            if i + 1 < items.len() { "," } else { "" }
        );
    }
    println!("}}");
}

fn vectors() {
    let f = fixed();
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
    let mut out: Vec<(&str, String)> = vec![];
    let h = |b: &[u8]| hex::encode(b);
    out.push(("dek", h(&f.dek)));
    out.push(("kmk", h(&f.kmk)));
    out.push(("family", h(&f.family)));
    out.push(("chunk_id", h(&f.id)));
    out.push(("salt", h(&f.salt)));
    out.push(("payload_raw64", h(&f.payload)));
    out.push(("dek_epoch", "7".into()));
    out.push((
        "key_v0",
        h(&extent_key(Profile::V0Draft, &ctx, &f.id, &f.salt, &enc)[..]),
    ));
    out.push((
        "key_adr",
        h(&extent_key(Profile::Adr, &ctx, &f.id, &f.salt, &enc)[..]),
    ));
    out.push((
        "key_hardened",
        h(&extent_key(Profile::Hardened, &ctx, &f.id, &f.salt, &enc)[..]),
    ));
    out.push((
        "key_fips",
        h(&extent_key(Profile::Fips, &ctx, &f.id, &f.salt, &enc)[..]),
    ));
    out.push(("info_v2", h(&info_v2(&ctx, &f.id, &f.salt, &enc))));
    // Full extent bodies with the zero nonce. FIPS-profile *key* with an explicit
    // zero nonce (the module's own nonce is random), so the vector is deterministic.
    for (name, prof) in [
        ("adr", Profile::Adr),
        ("hardened", Profile::Hardened),
        ("fips_kdf_zero_nonce", Profile::Fips),
    ] {
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
        let key = extent_key(prof, &ctx, &f.id, &f.salt, &enc);
        let aad = aad_for(prof, &hd, &f.family, KIND_DATA);
        let k = LessSafeKey::new(UnboundKey::new(&AES_256_GCM, &key[..]).unwrap());
        let mut buf = f.payload.clone();
        k.seal_in_place_append_tag(
            Nonce::assume_unique_for_key([0u8; 12]),
            Aad::from(&aad[..]),
            &mut buf,
        )
        .unwrap();
        out.push((Box::leak(format!("aad_{name}").into_boxed_str()), h(&aad)));
        out.push((Box::leak(format!("body_{name}").into_boxed_str()), h(&buf)));
    }
    let pk = |p: Purpose| purpose_key(&f.kmk, &f.family, p);
    let pf = |p: Purpose| purpose_key_fips(&f.kmk, &f.family, p);
    for (n, p) in [
        ("addressing", Purpose::Addressing),
        ("dek0", Purpose::Dek { epoch: 0 }),
        ("dek1", Purpose::Dek { epoch: 1 }),
        ("meta_commit", Purpose::Meta { kind: META_COMMIT }),
        ("meta_lease", Purpose::Meta { kind: META_LEASE }),
        ("volume_mac", Purpose::VolumeMac),
        ("pin", Purpose::Pin),
    ] {
        out.push((
            Box::leak(format!("purpose_{n}").into_boxed_str()),
            h(&pk(p)[..]),
        ));
        out.push((
            Box::leak(format!("purpose_fips_{n}").into_boxed_str()),
            h(&pf(p)[..]),
        ));
    }
    out.push((
        "purpose_msg_dek1",
        h(&{
            let mut m = LABEL_KDF_V1.to_vec();
            m.extend_from_slice(&f.family);
            m.push(2);
            m.extend_from_slice(&1u32.to_le_bytes());
            m
        }),
    ));
    let sk = slots::slot_aad(
        &f.family,
        1,
        &slots::SlotKind::Passphrase {
            cfg: slots::Argon2Cfg {
                m_kib: 19456,
                t: 2,
                p: 1,
            },
            salt: [0x11; 16],
        },
    );
    out.push(("slot_aad_pass", h(&sk)));
    {
        use chacha20poly1305::{
            aead::{Aead, Payload},
            KeyInit, XChaCha20Poly1305, XNonce,
        };
        let nonce: [u8; 24] = arr(&seq(0x30, 24));
        let c = XChaCha20Poly1305::new((&f.dek).into());
        let ct = c
            .encrypt(
                &XNonce::try_from(&nonce[..]).unwrap(),
                Payload {
                    msg: b"lunix xchacha vector",
                    aad: b"aad-1",
                },
            )
            .unwrap();
        out.push(("xchacha_nonce", h(&nonce)));
        out.push(("xchacha_ct", h(&ct)));
    }
    {
        // Slot KEK: Argon2id, small cost so the cross-check is quick.
        use argon2::{Algorithm, Argon2, Params, Version};
        let p = Params::new(19456, 2, 1, Some(32)).unwrap();
        let mut kek = [0u8; 32];
        Argon2::new(Algorithm::Argon2id, Version::V0x13, p)
            .hash_password_into(b"correct horse battery staple", &[0x11; 16], &mut kek)
            .unwrap();
        out.push(("argon2id_kek_m19456_t2_p1", h(&kek)));
    }
    jprint(&out);
}

fn demo() {
    let chunk = attack::demo_chunk(65536, 42);
    let r = attack::xor_leak_demo(&chunk);
    println!("first-draft scheme, one chunk sealed Raw then Lz4 under the same (key, nonce):");
    println!("  overlap bytes            : {}", r.overlap);
    println!(
        "  c1^c2 == p1^p2 everywhere: {}",
        r.xor_equals_plaintext_xor
    );
    println!(
        "  lz4 literal header bytes : {} (literal run {} B)",
        r.lz4_header_len, r.literal_len
    );
    println!(
        "  attacker crib            : {} known plaintext bytes",
        r.crib_len
    );
    println!("  plaintext bytes recovered: {}", r.recovered_correct);
}

// ---- bench ---------------------------------------------------------------

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

/// ns per op, median of 7 timed rounds of `iters` calls.
fn time_ns<F: FnMut()>(iters: u64, mut f: F) -> f64 {
    for _ in 0..(iters / 10).max(1) {
        f();
    }
    let mut r = vec![];
    for _ in 0..7 {
        let t = Instant::now();
        for _ in 0..iters {
            f();
        }
        r.push(t.elapsed().as_nanos() as f64 / iters as f64);
    }
    median(r)
}

fn bench() {
    let f = fixed();
    let adr = AdrKdf::new();
    println!("size_bytes,variant,ns_per_op,mb_per_s");
    for &sz in &[4096usize, 16384, 65536] {
        let pt: Vec<u8> = attack::demo_chunk(sz, 9);
        let iters: u64 = (3_000_000_000u64 / (sz as u64 * 40)).clamp(2000, 200_000);
        let ctx = KeyCtx {
            dek: &f.dek,
            family: &f.family,
            dek_epoch: 7,
            kind: KIND_DATA,
        };
        let enc = EncDesc {
            codec: 0,
            level: 0,
            clen: sz as u32,
        };
        let aad = [0x5au8; 70];
        let mut buf = Vec::with_capacity(sz + 16);
        let emit = |name: &str, ns: f64| {
            println!("{sz},{name},{ns:.1},{:.0}", sz as f64 / ns * 1000.0);
        };
        // 1. AES-256-GCM seal with a pre-built key (ring-style baseline).
        let k0 = LessSafeKey::new(UnboundKey::new(&AES_256_GCM, &f.dek).unwrap());
        emit(
            "aes_seal_prekeyed",
            time_ns(iters, || {
                buf.clear();
                buf.extend_from_slice(&pt);
                k0.seal_in_place_append_tag(
                    Nonce::assume_unique_for_key([0; 12]),
                    Aad::from(&aad[..]),
                    &mut buf,
                )
                .unwrap();
                black_box(&buf);
            }),
        );
        // 2. key setup only (AES key schedule + GHASH tables).
        emit(
            "aes_key_setup",
            time_ns(iters * 4, || {
                black_box(LessSafeKey::new(
                    UnboundKey::new(&AES_256_GCM, black_box(&f.dek)).unwrap(),
                ));
            }),
        );
        // 3. derivations only.
        emit(
            "kdf_adr_derive_key",
            time_ns(iters * 4, || {
                let mut m = [0u8; 88];
                m[..32].copy_from_slice(&f.dek);
                m[32..64].copy_from_slice(&f.id);
                m[64..].copy_from_slice(&f.salt);
                black_box(blake3::derive_key(CTX_EXTENT_V1, black_box(&m)));
            }),
        );
        emit(
            "kdf_adr_cloned_ctx",
            time_ns(iters * 4, || {
                black_box(adr.key(&f.dek, &f.id, black_box(&f.salt)));
            }),
        );
        emit(
            "kdf_hardened_keyed",
            time_ns(iters * 4, || {
                black_box(extent_key(
                    Profile::Hardened,
                    &ctx,
                    &f.id,
                    black_box(&f.salt),
                    &enc,
                ));
            }),
        );
        emit(
            "kdf_fips_hkdf_sha256",
            time_ns(iters * 4, || {
                black_box(extent_key(
                    Profile::Fips,
                    &ctx,
                    &f.id,
                    black_box(&f.salt),
                    &enc,
                ));
            }),
        );
        // 4. salt generation.
        emit(
            "salt_os_rng_24B",
            time_ns(iters * 4, || {
                let mut s = [0u8; 24];
                aws_lc_rs::rand::fill(&mut s).unwrap();
                black_box(s);
            }),
        );
        // 5. full per-extent pipelines (derive + key setup + seal), payload = raw chunk.
        emit(
            "full_adr_as_written",
            time_ns(iters, || {
                let key = extent_key(Profile::Adr, &ctx, &f.id, &f.salt, &enc);
                let k = LessSafeKey::new(UnboundKey::new(&AES_256_GCM, &key[..]).unwrap());
                buf.clear();
                buf.extend_from_slice(&pt);
                k.seal_in_place_append_tag(
                    Nonce::assume_unique_for_key([0; 12]),
                    Aad::from(&aad[..]),
                    &mut buf,
                )
                .unwrap();
                black_box(&buf);
            }),
        );
        emit(
            "full_adr_cloned_ctx",
            time_ns(iters, || {
                let key = adr.key(&f.dek, &f.id, &f.salt);
                let k = LessSafeKey::new(UnboundKey::new(&AES_256_GCM, &key[..]).unwrap());
                buf.clear();
                buf.extend_from_slice(&pt);
                k.seal_in_place_append_tag(
                    Nonce::assume_unique_for_key([0; 12]),
                    Aad::from(&aad[..]),
                    &mut buf,
                )
                .unwrap();
                black_box(&buf);
            }),
        );
        emit(
            "full_hardened_plus_salt",
            time_ns(iters, || {
                let mut s = [0u8; 24];
                aws_lc_rs::rand::fill(&mut s).unwrap();
                let key = extent_key(Profile::Hardened, &ctx, &f.id, &s, &enc);
                let k = LessSafeKey::new(UnboundKey::new(&AES_256_GCM, &key[..]).unwrap());
                buf.clear();
                buf.extend_from_slice(&pt);
                k.seal_in_place_append_tag(
                    Nonce::assume_unique_for_key([0; 12]),
                    Aad::from(&aad[..]),
                    &mut buf,
                )
                .unwrap();
                black_box(&buf);
            }),
        );
        let mut hedge = HedgedSalt {
            inner: OsSalt,
            key: *blake3::hash(&f.dek).as_bytes(),
        };
        emit(
            "full_hardened_hedged_salt",
            time_ns(iters, || {
                let s = hedge.next_salt(&SaltReq {
                    chunk_id: &f.id,
                    codec: 0,
                    level: 0,
                    payload: &pt,
                });
                let key = extent_key(Profile::Hardened, &ctx, &f.id, &s, &enc);
                let k = LessSafeKey::new(UnboundKey::new(&AES_256_GCM, &key[..]).unwrap());
                buf.clear();
                buf.extend_from_slice(&pt);
                k.seal_in_place_append_tag(
                    Nonce::assume_unique_for_key([0; 12]),
                    Aad::from(&aad[..]),
                    &mut buf,
                )
                .unwrap();
                black_box(&buf);
            }),
        );
        emit(
            "full_fips_hkdf_randnonce",
            time_ns(iters, || {
                let mut s = [0u8; 24];
                aws_lc_rs::rand::fill(&mut s).unwrap();
                let key = extent_key(Profile::Fips, &ctx, &f.id, &s, &enc);
                let k = RandomizedNonceKey::new(&AES_256_GCM, &key[..]).unwrap();
                buf.clear();
                buf.extend_from_slice(&pt);
                black_box(
                    k.seal_in_place_append_tag(Aad::from(&aad[..]), &mut buf)
                        .unwrap(),
                );
                black_box(&buf);
            }),
        );
    }
    // One Argon2id derivation at Constellation's defaults, for the slot-unlock cost.
    let t = Instant::now();
    let ring = slots::Keyring::create(
        [1; 16],
        &f.kmk,
        slots::NewSlot::Passphrase("pw", slots::Argon2Cfg::DEFAULT),
    )
    .unwrap();
    let c = t.elapsed();
    let t = Instant::now();
    ring.unwrap(1, slots::Credential::Passphrase("pw")).unwrap();
    println!(
        "# argon2id 64MiB t=3 p=1: wrap {:.0} ms, unwrap {:.0} ms",
        c.as_secs_f64() * 1e3,
        t.elapsed().as_secs_f64() * 1e3
    );
}

fn main() {
    match std::env::args().nth(1).as_deref() {
        Some("vectors") => vectors(),
        Some("demo") => demo(),
        Some("bench") => bench(),
        _ => eprintln!("usage: crypto-kat vectors|demo|bench"),
    }
}
