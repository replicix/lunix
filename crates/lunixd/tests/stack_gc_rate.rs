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
//!   grace the collector takes, `read_timeout` 0), 150 s of writes. After a
//!   warm-up (the first pass interval, two graces and three pass periods):
//!   the store does not keep growing (the last third's peak is at most the
//!   middle third's plus one pass period of writes), and every sample is
//!   below 1.5 x the bound, with `P` (the mean pass period), `R` (over the
//!   window `G + 3P` before the sample) and `C` measured in the run. At the
//!   end every chunk reads back what was last written to it, and
//!   `lunix-fsck` finds no error. The M17 sequence (every pass waits out its
//!   grace, then `pass_interval`) holds about `R · (2G + pass_interval + 2P)`.
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
use lunixd::testsupport::{
    close, fsck, payload, Driver, Scratch, TestRng, TestVolume, VolumeBuilder,
};

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
    /// Samples from this time on are the steady state.
    warmup: Duration,
}

/// One sample.
#[derive(Clone, Copy, Debug)]
struct Sample {
    at: Duration,
    written: u64,
    store: u64,
    passes: u64,
}

/// What a run saw.
#[derive(Debug, Default)]
struct Seen {
    written: u64,
    elapsed: Duration,
    peak: u64,
    passes: u64,
    freed: u64,
    /// Commits made (the tip's `commit_seq`).
    commits: u64,
    /// Every sample, from the start.
    samples: Vec<Sample>,
    /// The largest map, index and commit bytes seen.
    meta_peak: u64,
    /// Chunks whose read-back differed from the last write.
    bad_reads: u64,
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
    // The seed last written to each chunk (0: never written).
    let mut last = vec![0u64; usize::try_from(chunks).expect("chunks")];
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
        last[c as usize] = seed;
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
            let s = status.get();
            seen.samples.push(Sample {
                at: now,
                written: seen.written,
                store: total,
                passes: s.stats.passes,
            });
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
    seen.freed = s.stats.bytes_freed;
    seen.commits = stack.engine.tip().0 .0;
    // Every chunk reads back its last write.
    stack.engine.release_read_cache();
    for (c, &seed) in last.iter().enumerate() {
        if seed == 0 {
            continue;
        }
        let got = d.read(0, c as u64 * cs64, cs).expect("read back");
        if got != payload(seed, cs as usize) {
            seen.bad_reads += 1;
        }
    }
    drop(d);
    close(stack).expect("close");
    match fsck(&vol) {
        Some(f) => assert!(f.no_errors(), "{}: lunix-fsck:\n{}", r.tag, f.text),
        None => assert!(
            std::env::var_os("LUNIX_STACK_REQUIRE_FSCK").is_none(),
            "{}: lunix-fsck is not built",
            r.tag
        ),
    }
    seen
}

/// Measured over the steady samples: the mean pass period.
fn pass_period(steady: &[Sample]) -> Duration {
    let (Some(a), Some(b)) = (steady.first(), steady.last()) else {
        return Duration::from_secs(10);
    };
    let n = b.passes.saturating_sub(a.passes).max(1);
    (b.at.saturating_sub(a.at)) / u32::try_from(n).unwrap_or(u32::MAX)
}

/// The write rate over `window` before `at` (bytes per second).
fn rate_before(samples: &[Sample], at: Duration, window: Duration) -> f64 {
    let end = samples.iter().rfind(|s| s.at <= at).copied();
    let start = samples
        .iter()
        .find(|s| s.at + window >= at)
        .copied()
        .or(samples.first().copied());
    match (start, end) {
        (Some(a), Some(b)) if b.at > a.at => {
            (b.written - a.written) as f64 / (b.at - a.at).as_secs_f64()
        }
        _ => 0.0,
    }
}

/// `2 W + R (G + 3 P + 2 C) + slack` (see the module doc).
fn bound(ws: u64, rate: f64, grace: Duration, p: Duration, c: Duration, slack: u64) -> u64 {
    let window = grace + p * 3 + c * 2;
    2 * ws + (rate * window.as_secs_f64()) as u64 + slack
}

/// The run's numbers, and its steady samples with their bound.
fn report(r: &Run, seen: &Seen, grace: Duration) -> (Duration, Vec<(Sample, u64)>) {
    let secs = seen.elapsed.as_secs_f64().max(1.0);
    let rate = seen.written as f64 / secs;
    let c = Duration::from_secs_f64(secs / seen.commits.max(1) as f64);
    let steady: Vec<Sample> = seen
        .samples
        .iter()
        .copied()
        .filter(|s| s.at >= r.warmup)
        .collect();
    let p = pass_period(&steady);
    let slack = seen.meta_peak + (64 << 20);
    let checked: Vec<(Sample, u64)> = steady
        .iter()
        .map(|s| {
            let local = rate_before(&seen.samples, s.at, grace + p * 3);
            (*s, bound(r.ws, local, grace, p, c, slack))
        })
        .collect();
    let steady_peak = steady.iter().map(|s| s.store).max().unwrap_or(0);
    let mean_bound = bound(r.ws, rate, grace, p, c, slack);
    eprintln!(
        "{}: {:.0?}, W {} MiB, {} commits (C {:.1?}), {} passes (P {:.1?} in the steady state), written {} MiB at {:.1} MiB/s, peak store {} MiB (x{:.2} W), steady peak {} MiB (x{:.2} W), bound at the mean rate {} MiB (x{:.2} W), {} MiB freed, {} bad reads",
        r.tag,
        seen.elapsed,
        r.ws >> 20,
        seen.commits,
        c,
        seen.passes,
        p,
        seen.written >> 20,
        rate / f64::from(1 << 20),
        seen.peak >> 20,
        seen.peak as f64 / r.ws as f64,
        steady_peak >> 20,
        steady_peak as f64 / r.ws as f64,
        mean_bound >> 20,
        mean_bound as f64 / r.ws as f64,
        seen.freed >> 20,
        seen.bad_reads,
    );
    (p, checked)
}

/// CI: after the warm-up the store stops growing and stays below 1.5 x
/// `2 W + R (G + 3 P + 2 C) + slack`, every chunk reads back its last write
/// and `lunix-fsck` is clean.
#[test]
fn sustained_overwrite_stays_bounded() {
    lunixd::log::StderrLog::install(tracing::Level::INFO);
    let grace = Duration::from_secs(10);
    // The warm-up allows passes of up to 6 s of work.
    let work = Duration::from_secs(6);
    let gc = GcConfig::daemon();
    let r = Run {
        tag: "gcrate-bounded",
        secs: env_or("LUNIX_GC_RATE_SECS", 150),
        ws: env_or("LUNIX_GC_RATE_MIB", 256) << 20,
        grace: Some(grace),
        gc,
        warmup: gc.pass_interval + grace * 2 + (gc.busy_interval + work) * 3,
    };
    let seen = run(&r);
    let (p, checked) = report(&r, &seen, grace);
    assert_eq!(seen.bad_reads, 0, "{}: chunks read back wrong", r.tag);
    assert!(seen.freed > 0, "{}: GC freed nothing", r.tag);
    assert!(
        checked.len() >= 15,
        "{}: too few samples after the warm-up",
        r.tag
    );
    // No growth: the last third's peak is not above the middle third's by
    // more than one pass period of writes.
    let third = checked.len() / 3;
    let peak = |s: &[(Sample, u64)]| s.iter().map(|(x, _)| x.store).max().unwrap_or(0);
    let middle = peak(&checked[third..2 * third]);
    let tail = peak(&checked[2 * third..]);
    let rate = rate_before(&seen.samples, seen.elapsed, seen.elapsed);
    let allowance = (rate * p.as_secs_f64()) as u64 + (64 << 20);
    assert!(
        tail <= middle + allowance,
        "{}: the store keeps growing: last third peak {} MiB, middle third {} MiB, allowance {} MiB",
        r.tag,
        tail >> 20,
        middle >> 20,
        allowance >> 20
    );
    // The absolute backstop, with margin.
    for (s, b) in &checked {
        let backstop = b + b / 2;
        assert!(
            s.store <= backstop,
            "{}: at {:.0?} ({} MiB written) the store is {} MiB, above 1.5 x the bound ({} MiB)",
            r.tag,
            s.at,
            s.written >> 20,
            s.store >> 20,
            backstop >> 20
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
        warmup: gc.pass_interval + grace * 2,
    };
    let seen = run(&r);
    report(&r, &seen, grace);
    assert_eq!(seen.bad_reads, 0, "{}: chunks read back wrong", r.tag);
}
