//! GC on the real stack (plan M17.B gate): the collector wired into the
//! engine (one budgeted step at every commit and idle tick) over the real
//! store, index, map and codecs, against the flat-device oracle.
//!
//! * **Churn oracle.** A region is overwritten many times (sequential
//!   rewrites and a random-overwrite mix) with commits in between; reads
//!   are checked against the oracle throughout. After the final passes the
//!   store (segments, packs, index runs, commits) is at most 1.3 x the live
//!   data plus 4 x the largest commit's map pack bytes plus one segment
//!   target; the figures are printed for `bench/phase1/gc/RESULTS.md`.
//!   `lunix-fsck` reports no error and no `index_lost`, at rest exit 0.
//! * **Compaction, then `--truncate-tip`.** A segment left mostly dead is
//!   compacted, the output named, the source deleted; `lunix-fsck --repair
//!   --truncate-tip` drops the tip and the volume mounts and reads equal to
//!   the oracle, `--deep` clean (every root of the new window resolves).
//!
//! The GC settings are shortened for a test: a pass every second, the
//! smallest grace the collector takes (10 s with `read_timeout` 0). Needs
//! the `testsupport` feature and a built `lunix-fsck`:
//!
//! ```text
//! cargo build --release -p lunix-fsck
//! cargo test --release -p lunixd --features testsupport --test stack_gc -- --nocapture
//! ```
//!
//! `LUNIX_GC_CHURN_MIB` (default 32) is the churned region,
//! `LUNIX_GC_CHURN_ROUNDS` (default 10) the overwrites of it.

#![cfg(feature = "testsupport")]

use std::collections::BTreeSet;
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use lunix_engine::EngineConfig;
use lunix_format::CryptoMode;
use lunix_gc::GcConfig;
use lunix_iface::store::SegmentStore;
use lunixd::stack::OpenParams;
use lunixd::testsupport::{
    close, fsck, fsck_binary, payload, DeviceModel, Driver, ModelOp, Scratch, TestRng, TestVolume,
    VolumeBuilder,
};

const BLOCK: u64 = 4096;

fn env_or(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// GC settings for a test: the smallest grace the collector accepts.
fn gc_tune(c: &mut EngineConfig) {
    if let Some(p) = &mut c.gc_params {
        p.read_timeout = Duration::ZERO;
        p.grace = Duration::from_secs(10);
    }
}

fn builder(parent: &std::path::Path, mode: CryptoMode, cs: u32, size: u64) -> VolumeBuilder {
    let mut b = VolumeBuilder::new(parent)
        .manual()
        .mode(mode)
        .chunk_size(cs)
        .size_bytes(size);
    let manual = b.engine.clone();
    b.engine = Some(Arc::new(move |c: &mut EngineConfig| {
        if let Some(t) = &manual {
            t(c);
        }
        gc_tune(c);
    }));
    b
}

/// A pass every second.
fn fast_gc(o: &mut OpenParams) {
    o.gc = Some(GcConfig {
        pass_interval: Duration::from_secs(1),
        ..GcConfig::default()
    });
}

fn segments(stack: &lunixd::stack::VolumeStack) -> BTreeSet<u64> {
    SegmentStore::list(&stack.store)
        .expect("list")
        .iter()
        .map(|s| s.seg.raw())
        .collect()
}

/// `lunix-fsck`: no error, no `index_lost`; exit 0 when `at_rest`.
fn fsck_ok(vol: &TestVolume, what: &str, at_rest: bool) {
    if std::env::var_os("LUNIX_STACK_REQUIRE_FSCK").is_some() {
        assert!(fsck_binary().is_some(), "lunix-fsck is not built");
    }
    let Some(f) = fsck(vol) else { return };
    assert!(f.no_errors(), "{what}: lunix-fsck errors:\n{}", f.text);
    assert!(
        !f.findings
            .iter()
            .any(|(_, c)| c == "index_lost" || c == "gc_liveness"),
        "{what}:\n{}",
        f.text
    );
    if at_rest {
        assert_eq!(f.exit, 0, "{what}: lunix-fsck at rest:\n{}", f.text);
    }
}

/// `lunix-fsck --json` with extra arguments: the parsed report.
fn fsck_json(vol: &TestVolume, extra: &[&str]) -> Option<serde_json::Value> {
    let bin = fsck_binary()?;
    let args = vol.fsck_args()?;
    let out = Command::new(&bin)
        .args(args)
        .args(extra)
        .arg("--json")
        .arg(&vol.voldir)
        .output()
        .expect("run lunix-fsck");
    Some(serde_json::from_slice(&out.stdout).expect("fsck json"))
}

fn codes(v: &serde_json::Value) -> Vec<String> {
    v["findings"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|f| format!("{} {}", f["severity"], f["code"]))
                .collect()
        })
        .unwrap_or_default()
}

/// Drives ticks (each one a GC step, or a cut when moves are pending) for
/// `secs` seconds, a commit every `commit_every` ticks.
fn idle(stack: &lunixd::stack::VolumeStack, secs: u64, commit_every: u64) {
    let end = Instant::now() + Duration::from_secs(secs);
    let mut n = 0u64;
    while Instant::now() < end {
        stack.engine.tick().expect("tick");
        n += 1;
        if n.is_multiple_of(commit_every) {
            stack.engine.commit_now(true).expect("commit");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn check_all(d: &mut Driver, model: &DeviceModel, size: u64, what: &str) {
    let all = d
        .read_all(size)
        .unwrap_or_else(|s| panic!("{what}: read: {s:?}"));
    assert!(
        all == model.bytes(),
        "{what}: the device differs from the oracle"
    );
}

fn churn(mode: CryptoMode, cs: u32) {
    let region = env_or("LUNIX_GC_CHURN_MIB", 32) << 20;
    let rounds = env_or("LUNIX_GC_CHURN_ROUNDS", 10);
    let size = region * 2;
    let tag = format!("m17b-churn-{mode:?}-{}k", cs >> 10).to_lowercase();
    let t = Scratch::new(&tag);
    let vol = builder(t.path(), mode, cs, size).create().expect("create");
    let stack = vol.open_with(fast_gc).expect("open");
    let mut d = Driver::new(&stack).expect("driver");
    let mut model = DeviceModel::new(size);
    let mut rng = TestRng::new(0x6c17_b000 ^ u64::from(cs) ^ ((mode as u64) << 32));
    let cs64 = u64::from(cs);
    let started = Instant::now();
    let mut peak_store = 0u64;
    for r in 0..rounds {
        if r % 2 == 0 {
            // A sequential rewrite of the region, one chunk per request.
            for c in 0..region / cs64 {
                let seed = rng.next_u64();
                assert!(d
                    .write(0, c * cs64, &payload(seed, cs as usize), false)
                    .is_ok());
                model.apply(ModelOp::Write {
                    offset: c * cs64,
                    len: cs,
                    seed,
                });
            }
        } else {
            // A random mix over the region: partial chunks, repeats, zeroes.
            for _ in 0..(region / cs64) {
                let blocks = 1 + rng.below(2 * cs64 / BLOCK);
                let off = rng.below((region - blocks * BLOCK) / BLOCK) * BLOCK;
                let len = (blocks * BLOCK) as u32;
                if rng.below(16) == 0 {
                    assert!(d.write_zeroes(0, off, len).is_ok());
                    model.apply(ModelOp::Zero { offset: off, len });
                } else {
                    let seed = if rng.below(4) == 0 {
                        rng.below(8)
                    } else {
                        rng.next_u64()
                    };
                    assert!(d.write(0, off, &payload(seed, len as usize), false).is_ok());
                    model.apply(ModelOp::Write {
                        offset: off,
                        len,
                        seed,
                    });
                }
            }
        }
        assert!(d.flush(0).is_ok());
        stack.engine.commit_now(false).expect("commit");
        // Reads under GC: the whole device against the oracle, every round.
        check_all(&mut d, &model, size, &format!("{tag} round {r}"));
        idle(&stack, 3, 10);
        let (bytes, _) = store_bytes(&vol);
        peak_store = peak_store.max(bytes);
    }
    // The retained window catches up with the tip, then GC finishes: three
    // forced FULLs, passes past the grace with a commit after each (a
    // compaction source goes one commit after its output is named), until
    // the store stops shrinking.
    let mut last = u64::MAX;
    for _ in 0..12 {
        idle(&stack, 12, 20);
        let (bytes, _) = store_bytes(&vol);
        if bytes >= last {
            break;
        }
        last = bytes;
    }
    check_all(&mut d, &model, size, &format!("{tag} after GC"));
    let evidence = lunix_iface::store::GcStateStore::evidence_passes(&stack.store)
        .expect("gc/")
        .len();
    drop(d);
    close(stack).expect("close");
    fsck_ok(&vol, &tag, true);
    let Some(v) = fsck_json(&vol, &[]) else {
        return;
    };
    let r = &v["ratios"];
    let live = r["unique_clen"].as_u64().expect("unique_clen");
    let b = &r["bytes"];
    let store = ["segments", "map_packs", "index", "commits"]
        .iter()
        .map(|k| b[*k].as_u64().unwrap_or(0))
        .sum::<u64>();
    let largest_pack = largest_pack(&vol);
    let target = 16u64 << 20;
    let bound = live + live * 3 / 10 + 4 * largest_pack + target;
    eprintln!(
        "gc churn {mode:?} {} KiB: region {} MiB x {rounds}; live {} KiB, store {} KiB (segments {} KiB, packs {} KiB, index {} KiB, commits {} KiB), peak during churn {} KiB, bound {} KiB, ratio {:.3}, largest pack {} KiB, {evidence} evidence objects, {:.0?}",
        cs >> 10,
        region >> 20,
        live >> 10,
        store >> 10,
        b["segments"].as_u64().unwrap_or(0) >> 10,
        b["map_packs"].as_u64().unwrap_or(0) >> 10,
        b["index"].as_u64().unwrap_or(0) >> 10,
        b["commits"].as_u64().unwrap_or(0) >> 10,
        peak_store >> 10,
        bound >> 10,
        store as f64 / live.max(1) as f64,
        largest_pack >> 10,
        started.elapsed()
    );
    assert!(evidence > 0, "{tag}: GC wrote no evidence object");
    assert!(
        store <= bound,
        "{tag}: store {store} above the bound {bound}"
    );
}

/// Segment, pack, index and commit bytes of a volume directory (files).
fn store_bytes(vol: &TestVolume) -> (u64, u64) {
    let mut total = 0;
    let mut segs = 0;
    for sub in ["segments", "map", "index", "state/commits"] {
        for e in std::fs::read_dir(vol.voldir.join(sub))
            .into_iter()
            .flatten()
            .flatten()
        {
            let n = e.metadata().map(|m| m.len()).unwrap_or(0);
            total += n;
            if sub == "segments" {
                segs += n;
            }
        }
    }
    (total, segs)
}

fn largest_pack(vol: &TestVolume) -> u64 {
    std::fs::read_dir(vol.voldir.join("map"))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.metadata().ok().map(|m| m.len()))
        .max()
        .unwrap_or(0)
}

#[test]
fn churn_plain_16k() {
    churn(CryptoMode::Plain, 16 << 10);
}

#[test]
fn churn_plain_64k() {
    churn(CryptoMode::Plain, 64 << 10);
}

#[test]
fn churn_encrypted_16k() {
    churn(CryptoMode::Encrypted, 16 << 10);
}

#[test]
fn churn_encrypted_64k() {
    churn(CryptoMode::Encrypted, 64 << 10);
}

/// Compaction at stack level, then `--truncate-tip`: the predecessor mounts
/// and reads equal to the oracle; fsck `--deep` is clean.
fn compaction_then_truncate(mode: CryptoMode, cs: u32) {
    let tag = format!("m17b-trunc-{mode:?}-{}k", cs >> 10).to_lowercase();
    let t = Scratch::new(&tag);
    let size = 64u64 << 20;
    let vol = builder(t.path(), mode, cs, size).create().expect("create");
    let stack = vol.open_with(fast_gc).expect("open");
    let mut d = Driver::new(&stack).expect("driver");
    let mut model = DeviceModel::new(size);
    let cs64 = u64::from(cs);
    let chunks = (24u64 << 20) / cs64;
    // One pass of distinct data, then three of every four chunks rewritten:
    // the first segments are a quarter live, below the threshold.
    for c in 0..chunks {
        let seed = 0x1000 + c;
        assert!(d
            .write(0, c * cs64, &payload(seed, cs as usize), false)
            .is_ok());
        model.apply(ModelOp::Write {
            offset: c * cs64,
            len: cs,
            seed,
        });
    }
    assert!(d.flush(0).is_ok());
    stack.engine.commit_now(false).expect("commit");
    for c in (0..chunks).filter(|c| c % 4 != 0) {
        let seed = 0x9000 + c;
        assert!(d
            .write(0, c * cs64, &payload(seed, cs as usize), false)
            .is_ok());
        model.apply(ModelOp::Write {
            offset: c * cs64,
            len: cs,
            seed,
        });
    }
    assert!(d.flush(0).is_ok());
    stack.engine.commit_now(false).expect("commit");
    let written = segments(&stack);
    // Age the first FULL out of GC's retained set, then let GC compact,
    // name the output (pending moves make a tick cut) and delete.
    for _ in 0..4 {
        stack.engine.commit_now(true).expect("commit");
    }
    let mut compacted = false;
    for _ in 0..8 {
        idle(&stack, 12, 20);
        let now = segments(&stack);
        if now.iter().any(|s| !written.contains(s)) && written.iter().any(|s| !now.contains(s)) {
            compacted = true;
            break;
        }
    }
    assert!(
        compacted,
        "{tag}: no compaction output appeared and no source went"
    );
    // Settle: one more FULL names everything, a pass deletes what is due.
    stack.engine.commit_now(true).expect("commit");
    idle(&stack, 12, 20);
    check_all(&mut d, &model, size, &tag);
    drop(d);
    close(stack).expect("close");
    fsck_ok(&vol, &tag, true);

    let Some(v) = fsck_json(&vol, &["--repair", "--truncate-tip"]) else {
        return;
    };
    let refused: Vec<String> = codes(&v)
        .into_iter()
        .filter(|c| c.contains("repair_refused"))
        .collect();
    assert!(refused.is_empty(), "{tag}: --truncate-tip refused: {v}");
    let stack = vol.open().expect("open the predecessor");
    let mut d = Driver::new(&stack).expect("driver");
    check_all(&mut d, &model, size, &format!("{tag} after --truncate-tip"));
    drop(d);
    close(stack).expect("close");
    let deep = fsck_json(&vol, &["--deep"]).expect("fsck");
    assert!(
        deep["exit"].as_i64().is_some_and(|e| e <= 1),
        "{tag}: --deep after the truncation: {deep}"
    );
    assert!(
        !codes(&deep)
            .iter()
            .any(|c| c.contains("index_lost") || c.contains("\"error\"")),
        "{tag}: {deep}"
    );
}

#[test]
fn compaction_then_truncate_tip_integrity_16k() {
    compaction_then_truncate(CryptoMode::Integrity, 16 << 10);
}

#[test]
fn compaction_then_truncate_tip_encrypted_64k() {
    compaction_then_truncate(CryptoMode::Encrypted, 64 << 10);
}
