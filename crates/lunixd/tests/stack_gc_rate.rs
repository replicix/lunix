//! GC under sustained random overwrite (milestone gcrate): a small working
//! set `W` is overwritten continuously on the stack with the daemon's engine
//! (background coordinator, the `volume.json` commit triggers) and the
//! daemon's collector settings ([`GcConfig::daemon`]), and the store is
//! sampled against the working set as the run goes.
//!
//! The bound. A segment is named by the FULL of the cut that wrote it, and
//! what the retained window (`retain_last` 2) names is never condemned: it
//! waits up to two commit spacings `C`. A dead object then waits for the
//! first mark after it died (one pass period `P`: `busy_interval` plus a
//! pass's own work), then its grace `G`, then the first mark after the grace
//! (another `P`). A compaction source waits one more pass: it goes one
//! commit after the FULL that names its output. At a write rate `R` the
//! store holds at most `R · (G + 3P + 2C)` of garbage. The live side is at most
//! `2W`: compaction leaves alone a segment at or above half live (the
//! default `gc_live_threshold` 0.5), so surviving segments hold at most as
//! many dead bytes as live ones. With the map packs, index runs and commits
//! of the retained window (`slack`):
//!
//! ```text
//! store <= 2 W + R (G + 3 P + 2 C) + slack
//! ```
//!
//! * `sustained_overwrite_stays_bounded` (CI): `G` = 10 s (the smallest
//!   grace the collector takes, `read_timeout` 0), 140 s of writes; after a
//!   warm-up (the first pass interval, then two graces and three pass periods)
//!   every sample is checked against the bound with `R` and `C` measured over
//!   the run. The M17 sequence (every pass waits out its grace, then
//!   `pass_interval`) holds about `R · (2G + pass_interval + 2P)`.
//! * `sustained_overwrite_daemon_defaults` (by hand): the `volume.json`
//!   defaults too (`G` = 60 s); `LUNIX_GC_RATE_PIPELINE=0` runs the M17
//!   sequence instead (every pass waits out its grace), for the comparison.
//!
//! `LUNIX_GC_RATE_SECS` is the run length, `LUNIX_GC_RATE_MIB` (default 256)
//! the working set, `LUNIX_GC_RATE_CAP_MIB` (default 8192) the store size
//! that stops a run early. Needs the `testsupport` feature:
//!
//! ```text
//! cargo test --release -p lunixd --features testsupport --test stack_gc_rate -- --nocapture
//! cargo test --release -p lunixd --features testsupport --test stack_gc_rate -- --ignored --nocapture
//! ```

#![cfg(feature = "testsupport")]

use std::sync::Arc;
use std::time::{Duration, Instant};

use lunix_engine::EngineConfig;
use lunix_format::CryptoMode;
use lunix_gc::GcConfig;
use lunixd::testsupport::{close, payload, Driver, Scratch, TestRng, TestVolume, VolumeBuilder};

fn env_or(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Segment, pack, index and commit bytes of a volume directory.
fn store_bytes(vol: &TestVolume) -> [u64; 4] {
    let mut out = [0u64; 4];
    for (i, sub) in ["segments", "map", "index", "state/commits"]
        .iter()
        .enumerate()
    {
        for e in std::fs::read_dir(vol.voldir.join(sub))
            .into_iter()
            .flatten()
            .flatten()
        {
            out[i] += e.metadata().map(|m| m.len()).unwrap_or(0);
        }
    }
    out
}

/// One run's settings.
struct Run {
    tag: &'static str,
    secs: u64,
    ws: u64,
    /// `Some(grace)`: a test grace (`read_timeout` 0); `None`: `volume.json`.
    grace: Option<Duration>,
    gc: GcConfig,
    /// Samples from this time on are checked against the bound.
    warmup: Duration,
}

/// What a run saw after its warm-up.
#[derive(Debug, Default)]
struct Seen {
    written: u64,
    elapsed: Duration,
    peak: u64,
    /// The largest store seen after the warm-up, with the bytes written then.
    steady_peak: u64,
    passes: u64,
    freed: u64,
    /// Commits made (the tip's `commit_seq`).
    commits: u64,
    /// `(elapsed, written, store)` after the warm-up.
    samples: Vec<(Duration, u64, u64)>,
    /// The largest map, index and commit bytes seen.
    meta_peak: u64,
}

fn run(r: &Run) -> Seen {
    let cap = env_or("LUNIX_GC_RATE_CAP_MIB", 8192) << 20;
    let cs: u32 = 64 << 10;
    let cs64 = u64::from(cs);
    let t = Scratch::new(r.tag);
    let mut b = VolumeBuilder::new(t.path())
        .mode(CryptoMode::Integrity)
        .chunk_size(cs)
        .size_bytes(r.ws * 2);
    if let Some(g) = r.grace {
        b.engine = Some(Arc::new(move |c: &mut EngineConfig| {
            if let Some(p) = &mut c.gc_params {
                p.read_timeout = Duration::ZERO;
                p.grace = g;
            }
        }));
    }
    let vol = b.create().expect("create");
    let gc = r.gc;
    let stack = vol.open_with(|o| o.gc = Some(gc)).expect("open");
    let status = stack.engine.gc_status_handle();
    let mut d = Driver::new(&stack).expect("driver");
    let mut rng = TestRng::new(0x6763_7261_7465);
    let chunks = r.ws / cs64;
    let started = Instant::now();
    let mut next_sample = Duration::ZERO;
    let mut seen = Seen::default();
    while started.elapsed() < Duration::from_secs(r.secs) {
        let c = rng.below(chunks);
        // Seeds with the low bits 3: incompressible, never a dedup hit.
        let seed = rng.next_u64() | 3;
        assert!(d
            .write(0, c * cs64, &payload(seed, cs as usize), false)
            .is_ok());
        seen.written += cs64;
        if seen.written % (256 << 20) == 0 {
            assert!(d.flush(0).is_ok());
        }
        let now = started.elapsed();
        if now >= next_sample {
            next_sample += Duration::from_secs(2);
            let b = store_bytes(&vol);
            let total: u64 = b.iter().sum();
            seen.peak = seen.peak.max(total);
            seen.meta_peak = seen.meta_peak.max(b[1] + b[2] + b[3]);
            if now >= r.warmup {
                seen.steady_peak = seen.steady_peak.max(total);
                seen.samples.push((now, seen.written, total));
            }
            let s = status.get();
            if now.as_secs() % 10 < 2 {
                eprintln!(
                    "{} t={:>4}s written={:>6} MiB store={:>6} MiB (seg {} map {} idx {} commits {}) x{:.2} state={:?} pending={} passes={} freed={} MiB",
                    r.tag,
                    now.as_secs(),
                    seen.written >> 20,
                    total >> 20,
                    b[0] >> 20,
                    b[1] >> 20,
                    b[2] >> 20,
                    b[3] >> 20,
                    total as f64 / r.ws as f64,
                    s.state,
                    s.condemned_pending,
                    s.stats.passes,
                    s.stats.bytes_freed >> 20,
                );
            }
            if total > cap {
                eprintln!("{}: store above the cap; stopping early", r.tag);
                break;
            }
        }
    }
    seen.elapsed = started.elapsed();
    let s = status.get();
    seen.passes = s.stats.passes;
    seen.commits = stack.engine.tip().0 .0;
    seen.freed = s.stats.bytes_freed;
    drop(d);
    close(stack).expect("close");
    seen
}

/// `2 W + R (G + 3 P + 2 C) + slack` (see the module doc), with `R` the
/// run's mean write rate and `C` its mean commit spacing. `P` is
/// `busy_interval` plus `work` per pass.
fn bound(r: &Run, seen: &Seen, grace: Duration, work: Duration) -> u64 {
    let secs = seen.elapsed.as_secs_f64().max(1.0);
    let rate = seen.written as f64 / secs;
    let spacing = Duration::from_secs_f64(secs / seen.commits.max(1) as f64);
    let p = r.gc.busy_interval + work;
    let window = grace + p * 3 + spacing * 2;
    let slack = seen.meta_peak + (64 << 20);
    2 * r.ws + (rate * window.as_secs_f64()) as u64 + slack
}

fn report(r: &Run, seen: &Seen, bound: u64) {
    let rate = seen.written as f64 / seen.elapsed.as_secs_f64().max(1.0);
    eprintln!(
        "{}: {:.0?}, W {} MiB, {} commits, written {} MiB at {:.1} MiB/s, peak store {} MiB (x{:.2} W), after warm-up {} MiB (x{:.2} W), bound {} MiB (x{:.2} W), {} passes, {} MiB freed",
        r.tag,
        seen.elapsed,
        r.ws >> 20,
        seen.commits,
        seen.written >> 20,
        rate / f64::from(1 << 20),
        seen.peak >> 20,
        seen.peak as f64 / r.ws as f64,
        seen.steady_peak >> 20,
        seen.steady_peak as f64 / r.ws as f64,
        bound >> 20,
        bound as f64 / r.ws as f64,
        seen.passes,
        seen.freed >> 20,
    );
}

/// CI: the store stays below `2 W + R (G + 3 P + 2 C) + slack` once the first
/// grace and pass interval are over.
#[test]
fn sustained_overwrite_stays_bounded() {
    lunixd::log::StderrLog::install(tracing::Level::INFO);
    let grace = Duration::from_secs(10);
    // A pass's own work at this size: about 3 s on the test host (a
    // compaction of about 100 MiB at `gc_rate`, about 100 deletes and 50
    // condemns at a few directory syncs each); 4 s allows for load.
    let work = Duration::from_secs(4);
    let gc = GcConfig::daemon();
    let r = Run {
        tag: "gcrate-bounded",
        secs: env_or("LUNIX_GC_RATE_SECS", 140),
        ws: env_or("LUNIX_GC_RATE_MIB", 256) << 20,
        grace: Some(grace),
        gc,
        warmup: gc.pass_interval + grace * 2 + (gc.busy_interval + work) * 3,
    };
    let seen = run(&r);
    let b = bound(&r, &seen, grace, work);
    report(&r, &seen, b);
    assert!(seen.freed > 0, "{}: GC freed nothing", r.tag);
    assert!(
        seen.samples.len() > 10,
        "{}: too few samples after the warm-up",
        r.tag
    );
    for &(at, written, store) in &seen.samples {
        assert!(
            store <= b,
            "{}: at {at:.0?} ({} MiB written) the store is {} MiB, above the bound of {} MiB",
            r.tag,
            written >> 20,
            store >> 20,
            b >> 20
        );
    }
}

/// By hand: the daemon's settings and the `volume.json` defaults (grace
/// 60 s); `LUNIX_GC_RATE_PIPELINE=0` for the M17 sequence.
#[test]
#[ignore = "minutes; run by hand (see the module doc)"]
fn sustained_overwrite_daemon_defaults() {
    lunixd::log::StderrLog::install(tracing::Level::INFO);
    let pipeline = env_or("LUNIX_GC_RATE_PIPELINE", 1) != 0;
    let gc = GcConfig {
        pipeline,
        ..GcConfig::daemon()
    };
    let grace = Duration::from_secs(60);
    let r = Run {
        tag: if pipeline {
            "gcrate-defaults-pipelined"
        } else {
            "gcrate-defaults-sequential"
        },
        secs: env_or("LUNIX_GC_RATE_SECS", 300),
        ws: env_or("LUNIX_GC_RATE_MIB", 256) << 20,
        grace: None,
        gc,
        warmup: grace + gc.pass_interval,
    };
    let seen = run(&r);
    let b = bound(&r, &seen, grace, Duration::from_secs(4));
    report(&r, &seen, b);
}
