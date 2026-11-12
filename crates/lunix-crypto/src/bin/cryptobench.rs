//! Microbenchmark: full seal and open of 4, 16 and 64 KiB extents on an
//! `encrypted` volume (derivation, salt, key setup, AES-256-GCM, framing),
//! the same on `integrity` and `plain` volumes, and one Argon2id wrap and
//! unwrap at 64 MiB, t = 3.
//!
//! Run in release mode, pinned to one core:
//! `taskset -c 6 target/release/cryptobench`. The numbers in the M2 report are
//! compared with `spikes/crypto-kat` run in the same session.

use std::hint::black_box;
use std::time::Instant;

use lunix_crypto::keyring::{self, Argon2Cost, NewSlot, Unlock};
use lunix_crypto::{Root, VolumeCrypto};
use lunix_format::keyring::RootKind;
use lunix_format::volume::{CodecPolicy, VolumeDescriptor};
use lunix_format::{
    ChunkId, Codec, CryptoMode, ExtentKind, FamilyId, IdLen, Level, Profile, TimeMs, VolumeUuid,
};
use lunix_iface::crypto::{EncodingDesc, OpenExpect, OpenOutput};

fn descriptor(mode: CryptoMode, chunk_size: u32) -> VolumeDescriptor {
    VolumeDescriptor {
        crypto_mode: mode,
        profile: Profile::Default,
        block_size: 4096,
        chunk_size,
        map_fanout: 512,
        id_len: IdLen::ThirtyTwo,
        codec_policy: CodecPolicy::None,
        codec_level: 0,
        volume_uuid: VolumeUuid([7; 16]),
        family_id: FamilyId([9; 16]),
        kmk_fingerprint: [0; 16],
        created_at: TimeMs(0),
        lineage: Vec::new(),
        mac: [0; 32],
    }
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

/// Nanoseconds per call: median of 7 rounds of `iters` calls.
fn time_ns(iters: u64, mut f: impl FnMut()) -> f64 {
    for _ in 0..(iters / 10).max(1) {
        f();
    }
    let mut rounds = Vec::new();
    for _ in 0..7 {
        let t = Instant::now();
        for _ in 0..iters {
            f();
        }
        rounds.push(t.elapsed().as_nanos() as f64 / iters as f64);
    }
    median(rounds)
}

fn main() {
    println!("mode,size_bytes,op,ns_per_op,gb_per_s");
    for (name, mode) in [
        ("encrypted", CryptoMode::Encrypted),
        ("integrity", CryptoMode::Integrity),
        ("plain", CryptoMode::Plain),
    ] {
        for size in [4096usize, 16384, 65536] {
            let desc = descriptor(mode, 65536);
            let root = match mode {
                CryptoMode::Encrypted => Some(Root::generate(RootKind::Kmk).expect("root")),
                CryptoMode::Integrity => Some(Root::generate(RootKind::Ik).expect("root")),
                CryptoMode::Plain => None,
            };
            let crypto = VolumeCrypto::new(&desc, root, lunix_format::DekGen(0)).expect("keys");
            let codec = crypto.extents();
            let data: Vec<u8> = (0..size).map(|i| (i as u8).wrapping_mul(31)).collect();
            let id: ChunkId = codec.chunk_id(ExtentKind::Data, &data);
            let enc = EncodingDesc {
                codec: Codec::Raw,
                level: Level(0),
                ulen: u32::try_from(size).expect("size"),
                dict_id: None,
            };
            let iters = (3_000_000_000u64 / (size as u64 * 40)).clamp(2000, 200_000);
            let mut out = Vec::with_capacity(size + 128);
            let seal_ns = time_ns(iters, || {
                out.clear();
                codec
                    .seal(ExtentKind::Data, &id, &enc, black_box(&data), &mut out)
                    .expect("seal");
                black_box(&out);
            });
            let extent = out.clone();
            let mut plain = Vec::with_capacity(size);
            // The codec takes the chunk size from the caller, so 4 KiB extents
            // (below the volume minimum) can be timed too.
            let expect = OpenExpect {
                chunk_size: u32::try_from(size).expect("size"),
                chunk_id: Some(&id),
                id_prefix: None,
                verify_id: false,
                output: OpenOutput::Plaintext,
            };
            let open_ns = time_ns(iters, || {
                plain.clear();
                codec
                    .open(ExtentKind::Data, black_box(&extent), &expect, &mut plain)
                    .expect("open");
                black_box(&plain);
            });
            for (op, ns) in [("seal", seal_ns), ("open", open_ns)] {
                println!("{name},{size},{op},{ns:.1},{:.2}", size as f64 / ns);
            }
        }
    }

    let family = FamilyId([3; 16]);
    let cost = Argon2Cost::DEFAULT;
    let t = Instant::now();
    let created = keyring::create(
        Profile::Default,
        family,
        RootKind::Kmk,
        &NewSlot::Passphrase {
            passphrase: b"correct horse battery staple",
            cost,
        },
    )
    .expect("create");
    let wrap = t.elapsed();
    let t = Instant::now();
    keyring::unlock(
        &created.bytes,
        Profile::Default,
        &family,
        &Unlock::Passphrase(b"correct horse battery staple"),
        0,
        None,
    )
    .expect("unlock");
    println!(
        "# argon2id {} MiB t={} p={}: wrap {:.0} ms, unwrap {:.0} ms",
        cost.m_kib / 1024,
        cost.t,
        cost.p,
        wrap.as_secs_f64() * 1e3,
        t.elapsed().as_secs_f64() * 1e3
    );
}
