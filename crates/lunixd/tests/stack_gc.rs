//! GC on the real stack (plan M17.B gate): the collector wired into the
//! engine (one budgeted step at every commit, every idle tick and after a
//! failed cut) over the real store, index, map and codecs, against the
//! flat-device oracle.
//!
//! * **Churn oracle.** A region is overwritten many times (sequential
//!   rewrites and a random-overwrite mix) with commits in between. Reads go
//!   to disk (the read cache is a few chunks and released before every
//!   check), a reader thread reads the whole device against the oracle while
//!   the collector deletes and compacts, and at the end the volume is reopened
//!   and read again, then `lunix-fsck --deep` (every extent opened, every id
//!   recomputed: a miscopied compaction output fails it). After the final
//!   passes the store is at most 1.3 x the live data plus 4 x the largest map
//!   pack plus one segment target.
//! * **Compaction, then `--truncate-tip` while only the tip names the output**
//!   (the B-1 sequence of the M17.B review): ticks only, no forced commit.
//! * **An idle volume after compaction settles** without a forced commit: the
//!   source goes after the one cut the collector asks for, the evidence
//!   objects stop growing (M17.B stack review S-1).
//! * **A reverted subtree** (`X -> Y -> X`): GC keeps the older root's pack
//!   (M17.B stack review B-1); `lunix-fsck` is exit 0 at rest.
//! * **Tamper with GC running** (integrity): a flipped byte in a live extent
//!   of a low-live segment: compaction skips the segment, that chunk fails to
//!   read, every other chunk equals the oracle, the segment stays.
//!
//! The GC settings are shortened for a test: a pass every second, the
//! smallest grace the collector takes (10 s with `read_timeout` 0). Needs the
//! `testsupport` feature and a built `lunix-fsck` (the tests fail without it):
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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use lunix_engine::{CommitOutcome, EngineConfig};
use lunix_format::{CryptoMode, ExtentKind};
use lunix_gc::GcConfig;
use lunix_iface::store::{GcStateStore, SegmentStore};
use lunixd::stack::{OpenParams, VolumeStack};
use lunixd::testsupport::{
    close, fsck, fsck_binary, payload, DeviceModel, Driver, ModelOp, Scratch, TestRng, TestVolume,
    VolumeBuilder,
};

const BLOCK: u64 = 4096;
const GRACE: Duration = Duration::from_secs(10);

fn env_or(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// GC settings for a test: the smallest grace the collector accepts. The
/// read cache is a few chunks, so reads come from disk.
fn gc_tune(c: &mut EngineConfig) {
    if let Some(p) = &mut c.gc_params {
        p.read_timeout = Duration::ZERO;
        p.grace = GRACE;
    }
    c.read_cache_max = 256 << 10;
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

fn segments(stack: &VolumeStack) -> BTreeSet<u64> {
    SegmentStore::list(&stack.store)
        .expect("list")
        .iter()
        .map(|s| s.seg.raw())
        .collect()
}

fn evidence(stack: &VolumeStack) -> usize {
    stack.store.evidence_passes().expect("gc/").len()
}

/// `lunix-fsck` (required): no error, no `index_lost`, no `gc_liveness`;
/// exit 0 when `at_rest`.
fn fsck_ok(vol: &TestVolume, what: &str, at_rest: bool) {
    assert!(fsck_binary().is_some(), "lunix-fsck is not built");
    let f = fsck(vol).expect("lunix-fsck runs on this volume");
    assert!(f.no_errors(), "{what}: lunix-fsck errors:\n{}", f.text);
    assert!(
        !f.findings
            .iter()
            .any(|(_, c)| c == "index_lost" || c == "gc_liveness" || c == "gc_unchecked"),
        "{what}:\n{}",
        f.text
    );
    if at_rest {
        assert_eq!(f.exit, 0, "{what}: lunix-fsck at rest:\n{}", f.text);
    }
}

/// `lunix-fsck --json` with extra arguments (required): the parsed report.
fn fsck_json(vol: &TestVolume, extra: &[&str]) -> serde_json::Value {
    let bin = fsck_binary().expect("lunix-fsck is not built");
    let args = vol.fsck_args().expect("lunix-fsck runs on this volume");
    let out = Command::new(&bin)
        .args(args)
        .args(extra)
        .arg("--json")
        .arg(&vol.voldir)
        .output()
        .expect("run lunix-fsck");
    serde_json::from_slice(&out.stdout).expect("fsck json")
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

/// Ticks only for `secs` seconds (each tick a GC step, or a cut for pending
/// moves or a cut the collector asked for). Returns the cuts the ticks made.
fn ticks(stack: &VolumeStack, secs: u64) -> u64 {
    let end = Instant::now() + Duration::from_secs(secs);
    let mut cuts = 0;
    while Instant::now() < end {
        if matches!(
            stack.engine.tick().expect("tick"),
            CommitOutcome::Committed { .. }
        ) {
            cuts += 1;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    cuts
}

/// Ticks for `secs` seconds on another thread while this one reads the
/// whole device against `model`; returns the reads made.
fn ticks_with_reader(
    stack: &VolumeStack,
    d: &mut Driver,
    model: &DeviceModel,
    secs: u64,
    what: &str,
) -> u64 {
    let done = AtomicBool::new(false);
    let engine = Arc::clone(&stack.engine);
    let size = model.size();
    let mut reads = 0u64;
    std::thread::scope(|sc| {
        sc.spawn(|| {
            // Set even when a tick panics, or the reader would loop for ever.
            struct Done<'a>(&'a AtomicBool);
            impl Drop for Done<'_> {
                fn drop(&mut self) {
                    self.0.store(true, Ordering::Release);
                }
            }
            let _done = Done(&done);
            let end = Instant::now() + Duration::from_secs(secs);
            while Instant::now() < end {
                engine.tick().expect("tick");
                std::thread::sleep(Duration::from_millis(100));
            }
        });
        while !done.load(Ordering::Acquire) {
            stack.engine.release_read_cache();
            let all = d
                .read_all(size)
                .unwrap_or_else(|s| panic!("{what}: read under GC: {s:?}"));
            assert!(all == model.bytes(), "{what}: a read under GC differs");
            reads += 1;
        }
    });
    reads
}

fn check_all(stack: &VolumeStack, d: &mut Driver, model: &DeviceModel, what: &str) {
    stack.engine.release_read_cache();
    let all = d
        .read_all(model.size())
        .unwrap_or_else(|s| panic!("{what}: read: {s:?}"));
    assert!(
        all == model.bytes(),
        "{what}: the device differs from the oracle"
    );
}

fn write_chunk(d: &mut Driver, model: &mut DeviceModel, cs: u32, c: u64, seed: u64) {
    let off = c * u64::from(cs);
    assert!(d.write(0, off, &payload(seed, cs as usize), false).is_ok());
    model.apply(ModelOp::Write {
        offset: off,
        len: cs,
        seed,
    });
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
    let mut concurrent_reads = 0u64;
    for r in 0..rounds {
        // A cut every 64 MiB written, so the journal ring (manual engine: no
        // background commit) never fills on a large region.
        let every = (64u64 << 20) / cs64;
        if r % 2 == 0 {
            for c in 0..region / cs64 {
                write_chunk(&mut d, &mut model, cs, c, rng.next_u64());
                if (c + 1) % every == 0 {
                    stack.engine.commit_now(false).expect("commit");
                }
            }
        } else {
            for i in 0..(region / cs64) {
                if (i + 1) % every == 0 {
                    stack.engine.commit_now(false).expect("commit");
                }
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
        check_all(&stack, &mut d, &model, &format!("{tag} round {r}"));
        concurrent_reads +=
            ticks_with_reader(&stack, &mut d, &model, 3, &format!("{tag} round {r}"));
        let (bytes, _) = store_bytes(&vol);
        peak_store = peak_store.max(bytes);
    }
    // The window catches up with the tip and GC finishes: ticks only (the
    // collector asks for the cuts it needs), a reader all along, until the
    // store stops shrinking.
    stack.engine.commit_now(true).expect("commit");
    stack.engine.commit_now(true).expect("commit");
    // A window long enough for a whole pass at this size (a mark, the grace,
    // the deletes, a compaction at `gc_rate`); settled after two windows
    // without a shrink.
    let window = 12 + (region >> 20) / 32;
    let (mut last, mut flat) = (u64::MAX, 0);
    for _ in 0..40 {
        concurrent_reads +=
            ticks_with_reader(&stack, &mut d, &model, window, &format!("{tag} settling"));
        let (bytes, _) = store_bytes(&vol);
        flat = if bytes >= last { flat + 1 } else { 0 };
        if flat == 2 {
            break;
        }
        last = last.min(bytes);
    }
    check_all(&stack, &mut d, &model, &format!("{tag} after GC"));
    let evidence = evidence(&stack);
    drop(d);
    close(stack).expect("close");
    // From disk, after a reopen.
    let stack = vol.open_with(fast_gc).expect("reopen");
    let mut d = Driver::new(&stack).expect("driver");
    check_all(&stack, &mut d, &model, &format!("{tag} reopened"));
    drop(d);
    close(stack).expect("close");
    fsck_ok(&vol, &tag, true);
    let deep = fsck_json(&vol, &["--deep"]);
    assert_eq!(deep["exit"].as_i64(), Some(0), "{tag}: --deep: {deep}");
    let v = fsck_json(&vol, &[]);
    assert!(no_gc_work(&v), "{tag}: GC left work at rest: {v}");
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
        "gc churn {mode:?} {} KiB: region {} MiB x {rounds}; live {} KiB, store {} KiB (segments {} KiB, packs {} KiB, index {} KiB, commits {} KiB), peak during churn {} KiB, bound {} KiB, ratio {:.3}, largest pack {} KiB, {evidence} evidence objects, {concurrent_reads} whole-device reads under GC, {:.0?}",
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
    assert!(concurrent_reads > 0, "{tag}: no read ran under GC");
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

/// A volume whose first segments are a quarter live, the FULL with all of it
/// aged out of GC's retained set: the next pass compacts them. Returns the
/// segments before compaction.
fn low_live(
    stack: &VolumeStack,
    d: &mut Driver,
    model: &mut DeviceModel,
    cs: u32,
    chunks: u64,
) -> BTreeSet<u64> {
    for c in 0..chunks {
        write_chunk(d, model, cs, c, 0x1000 + c);
    }
    assert!(d.flush(0).is_ok());
    stack.engine.commit_now(false).expect("commit");
    for c in (0..chunks).filter(|c| c % 4 != 0) {
        write_chunk(d, model, cs, c, 0x9000 + c);
    }
    assert!(d.flush(0).is_ok());
    stack.engine.commit_now(false).expect("commit");
    let written = segments(stack);
    for _ in 0..4 {
        stack.engine.commit_now(true).expect("commit");
    }
    written
}

/// The B-1 sequence of the M17.B review on the stack: compaction, the cut
/// that names the output, a whole pass while only the tip names it (the
/// source must stay), then `--truncate-tip` drops the tip.
fn compaction_then_truncate(mode: CryptoMode, cs: u32) {
    let tag = format!("m17b-trunc-{mode:?}-{}k", cs >> 10).to_lowercase();
    let t = Scratch::new(&tag);
    let size = 64u64 << 20;
    let vol = builder(t.path(), mode, cs, size).create().expect("create");
    let stack = vol.open_with(fast_gc).expect("open");
    let mut d = Driver::new(&stack).expect("driver");
    let mut model = DeviceModel::new(size);
    let written = low_live(
        &stack,
        &mut d,
        &mut model,
        cs,
        (24u64 << 20) / u64::from(cs),
    );
    // Ticks until the cut that applies the moves and names the output.
    let start = Instant::now();
    let named_at = loop {
        assert!(
            start.elapsed() < Duration::from_secs(60),
            "{tag}: no compaction"
        );
        if let CommitOutcome::Committed { commit_seq, .. } = stack.engine.tick().expect("tick") {
            break commit_seq;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let outputs: BTreeSet<u64> = segments(&stack).difference(&written).copied().collect();
    assert!(!outputs.is_empty(), "{tag}: the cut names an output");
    // A whole pass (condemn, grace, recheck) while only the tip names the
    // output: the source must stay. The collector then asks for one cut;
    // stop at it (the tip it makes is the one --truncate-tip drops).
    let start = Instant::now();
    let asked = loop {
        assert!(
            start.elapsed() < GRACE + Duration::from_secs(40),
            "{tag}: the collector never asked for the cut that names the output below the tip"
        );
        if let CommitOutcome::Committed { commit_seq, .. } = stack.engine.tick().expect("tick") {
            break commit_seq;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    assert_eq!(asked.0, named_at.0 + 1, "{tag}: the collector's cut");
    assert!(
        written.is_subset(&segments(&stack)),
        "{tag}: a source went while only the tip (commit {}) named its output",
        named_at.0
    );
    check_all(&stack, &mut d, &model, &tag);
    drop(d);
    close(stack).expect("close");
    fsck_ok(&vol, &tag, true);

    // Drop every commit down to the one before the output was named: the
    // output is then above the chain's mark and the open sweeps it, and
    // the older roots need the sources (still there) again.
    let mut tip = fsck_json(&vol, &[])["tip"].as_u64().expect("tip");
    while tip >= named_at.0 {
        let v = fsck_json(&vol, &["--repair", "--truncate-tip"]);
        let refused: Vec<String> = codes(&v)
            .into_iter()
            .filter(|c| c.contains("repair_refused"))
            .collect();
        assert!(refused.is_empty(), "{tag}: --truncate-tip refused: {v}");
        let now = v["tip"].as_u64().expect("tip");
        assert_eq!(now + 1, tip, "{tag}: one commit dropped");
        tip = now;
    }
    let stack = vol.open_with(fast_gc).expect("open the predecessor");
    for o in &outputs {
        assert!(
            !segments(&stack).contains(o),
            "{tag}: the output {o:x} named only by a dropped commit was swept"
        );
    }
    let mut d = Driver::new(&stack).expect("driver");
    check_all(
        &stack,
        &mut d,
        &model,
        &format!("{tag} after --truncate-tip"),
    );
    drop(d);
    close(stack).expect("close");
    let deep = fsck_json(&vol, &["--deep"]);
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

/// M17.B stack review S-1: after a compaction an idle volume (ticks only)
/// reaches no pending work: the source goes, the evidence objects and the
/// cuts stop.
#[test]
fn an_idle_volume_after_compaction_settles() {
    let tag = "m17b-settle";
    let t = Scratch::new(tag);
    let cs = 16u32 << 10;
    let size = 64u64 << 20;
    let vol = builder(t.path(), CryptoMode::Integrity, cs, size)
        .create()
        .expect("create");
    let stack = vol.open_with(fast_gc).expect("open");
    let mut d = Driver::new(&stack).expect("driver");
    let mut model = DeviceModel::new(size);
    let written = low_live(
        &stack,
        &mut d,
        &mut model,
        cs,
        (24u64 << 20) / u64::from(cs),
    );
    // Ticks until a source has gone (compaction, the cut that names the
    // output, a pass, the cut the collector asks for, the next pass), with a
    // deadline rather than a fixed window.
    let start = Instant::now();
    while written.is_subset(&segments(&stack)) {
        assert!(
            start.elapsed() < Duration::from_secs(180),
            "{tag}: no compaction source went without a forced commit"
        );
        ticks(&stack, 1);
    }
    // Then until nothing is condemned and no cut came for a grace and more.
    let mut last_cut = Instant::now();
    loop {
        if ticks(&stack, 1) > 0 {
            last_cut = Instant::now();
        }
        let listed = stack
            .store
            .load_condemned()
            .expect("list")
            .as_ref()
            .and_then(lunix_gc::CondemnedList::from_file)
            .map_or(0, |l| l.entries.len());
        if listed == 0 && last_cut.elapsed() > GRACE + Duration::from_secs(3) {
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(300),
            "{tag}: GC does not settle ({listed} condemned)"
        );
    }
    // Settled: a grace and more past it change nothing.
    let (e0, s0, tip0) = (evidence(&stack), segments(&stack), stack.engine.tip().0);
    let cuts = ticks(&stack, GRACE.as_secs() + 5);
    assert_eq!(cuts, 0, "{tag}: an idle volume keeps cutting");
    assert_eq!(evidence(&stack), e0, "{tag}: evidence keeps growing");
    assert_eq!(segments(&stack), s0, "{tag}: segments keep changing");
    assert_eq!(stack.engine.tip().0, tip0);
    check_all(&stack, &mut d, &model, tag);
    drop(d);
    close(stack).expect("close");
    fsck_ok(&vol, tag, true);
    // Nothing left for a pass to condemn or delete (the leak check).
    assert!(
        no_gc_work(&fsck_json(&vol, &[])),
        "{tag}: GC left work at rest"
    );
}

/// `lunix-fsck`'s dry run finds nothing to condemn or delete.
fn no_gc_work(v: &serde_json::Value) -> bool {
    v["findings"].as_array().is_none_or(|a| {
        a.iter().filter(|f| f["code"] == "gc_dry_run").all(|f| {
            let m = f["message"].as_str().unwrap_or("");
            !m.contains("would condemn")
                || m.contains(
                    "condemn 0 segments and delete 0 segments, 0 packs, 0 index runs and 0 commits",
                )
        })
    })
}

/// M17.B stack review B-1 on the stack: leaf 0 goes `X -> Y -> X` (chunk 0
/// rewritten, then written with its first content again), the tip's root has the hash of an older retained root,
/// stored again with other child locations. GC keeps the older root's pack;
/// `lunix-fsck` (which walks every root from disk) is exit 0 at rest.
#[test]
fn a_reverted_subtree_keeps_the_older_roots_pack() {
    let tag = "m17b-revert";
    let t = Scratch::new(tag);
    let cs = 16u32 << 10;
    let size = 64u64 << 20;
    let vol = builder(t.path(), CryptoMode::Integrity, cs, size)
        .create()
        .expect("create");
    let stack = vol.open_with(fast_gc).expect("open");
    let mut d = Driver::new(&stack).expect("driver");
    let mut model = DeviceModel::new(size);
    write_chunk(&mut d, &mut model, cs, 0, 1);
    assert!(d.flush(0).is_ok());
    stack.engine.commit_now(false).expect("C1");
    write_chunk(&mut d, &mut model, cs, 512, 2);
    assert!(d.flush(0).is_ok());
    stack.engine.commit_now(false).expect("C2");
    // Chunk 0 goes Y, then X again: dedup maps it to X's extent, so leaf 0
    // is byte for byte what C1 and C2 hold, sealed once more elsewhere.
    write_chunk(&mut d, &mut model, cs, 0, 3);
    assert!(d.flush(0).is_ok());
    stack.engine.commit_now(false).expect("C3");
    write_chunk(&mut d, &mut model, cs, 0, 1);
    assert!(d.flush(0).is_ok());
    stack.engine.commit_now(false).expect("C4");
    // Passes with the tip at C4: ticks only.
    let cuts = ticks(&stack, GRACE.as_secs() + 15);
    assert_eq!(
        cuts,
        0,
        "{tag}: the ticks cut (tip {:?})",
        stack.engine.tip()
    );
    // Every root of GC's retained set under C4 (C2, C3, C4), walked from a
    // fresh map over the stored packs only, finds every node (fsck after a
    // close would not see it: the close's commit moves the window past C2).
    for q in 2..=4u64 {
        let nodes = nodes_from_disk(&stack, q, size / u64::from(cs))
            .unwrap_or_else(|e| panic!("{tag}: commit {q}'s map does not walk from disk: {e}"));
        assert!(!nodes.is_empty());
    }
    check_all(&stack, &mut d, &model, tag);
    drop(d);
    close(stack).expect("close");
    fsck_ok(&vol, tag, true);
}

/// Every node of commit `seq`'s map, walked from a fresh map instance over
/// the stored packs (nothing resident).
fn nodes_from_disk(
    stack: &VolumeStack,
    seq: u64,
    chunks: u64,
) -> lunix_iface::Result<Vec<lunix_iface::map::NodeRef>> {
    use lunix_iface::crypto::CommitCodec;
    use lunix_iface::map::BlockMap;
    use lunix_iface::store::CommitStore;
    let q = lunix_format::CommitSeq(seq);
    let bytes = stack.store.get(q)?;
    let lunix_format::commit::Commit::Full(f) = stack.crypto.meta().open_commit(q, &bytes)? else {
        panic!("commit {seq} is not a FULL");
    };
    let source: Arc<dyn lunix_iface::map::NodeSource> = Arc::new(stack.store.clone());
    let map = lunix_map::RadixMap::open(
        Arc::clone(stack.crypto.extents()),
        source,
        &f.map_root,
        chunks,
        lunix_map::MapConfig::new(stack.desc.map_fanout, stack.desc.id_len),
    )?;
    let mut out = Vec::new();
    map.walk_nodes(&f.map_root, &mut |n| out.push(*n))?;
    Ok(out)
}

/// A flipped byte in a live extent of a low-live segment with GC running
/// (integrity mode detects it): compaction skips that segment, that chunk
/// fails to read, every other chunk equals the oracle, the segment stays.
#[test]
fn a_tampered_live_extent_is_skipped_by_compaction_and_fails_its_read() {
    let tag = "m17b-tamper";
    let t = Scratch::new(tag);
    let cs = 16u32 << 10;
    let size = 64u64 << 20;
    let vol = builder(t.path(), CryptoMode::Integrity, cs, size)
        .create()
        .expect("create");
    let stack = vol.open_with(fast_gc).expect("open");
    let mut d = Driver::new(&stack).expect("driver");
    let mut model = DeviceModel::new(size);
    let chunks = (24u64 << 20) / u64::from(cs);
    let written = low_live(&stack, &mut d, &mut model, cs, chunks);
    // Chunk 0 is live (every fourth chunk kept its first write): its extent.
    let id = stack
        .crypto
        .extents()
        .chunk_id(ExtentKind::Data, &payload(0x1000, cs as usize));
    let (seg, off) = written
        .iter()
        .find_map(|&s| {
            let seg = lunix_format::ObjId::from_raw(s).ok()?;
            let f = stack.store.footer(seg).ok()?;
            f.entries
                .iter()
                .find(|e| e.chunk_id == id)
                .map(|e| (seg, e.off))
        })
        .expect("the extent of chunk 0");
    drop(d);
    close(stack).expect("close");
    let path = vol
        .voldir
        .join(lunix_format::paths::local::segment_sealed(seg));
    let mut bytes = std::fs::read(&path).expect("segment");
    bytes[off as usize + 40] ^= 0x5a;
    std::fs::write(&path, &bytes).expect("tamper");

    let stack = vol.open_with(fast_gc).expect("open");
    let mut d = Driver::new(&stack).expect("driver");
    // Until compaction has run and skipped the segment: a `skip` line for it
    // in an evidence object (integrity mode: the lines are readable).
    let want = format!(
        "\"action\":\"skip\",\"kind\":\"segment\",\"id\":\"{:016x}\"",
        seg.raw()
    );
    let start = Instant::now();
    loop {
        stack.engine.tick().expect("tick");
        let skipped = std::fs::read_dir(vol.voldir.join("gc"))
            .into_iter()
            .flatten()
            .flatten()
            .any(|e| {
                std::fs::read(e.path()).is_ok_and(|b| String::from_utf8_lossy(&b).contains(&want))
            });
        if skipped {
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(120),
            "{tag}: compaction never ran over the tampered segment"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        segments(&stack).contains(&seg.raw()),
        "{tag}: the segment with the bad extent stays"
    );
    stack.engine.release_read_cache();
    let mut bad = 0;
    for c in 0..size / u64::from(cs) {
        let off = c * u64::from(cs);
        match d.read(0, off, cs) {
            Ok(b) => assert!(
                b == model.read(off, u64::from(cs)),
                "{tag}: chunk {c} differs"
            ),
            Err(_) => {
                assert_eq!(c, 0, "{tag}: chunk {c} fails to read");
                bad += 1;
            }
        }
    }
    assert_eq!(bad, 1, "{tag}: the tampered chunk fails to read");
    drop(d);
    close(stack).expect("close");
}
