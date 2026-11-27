//! What the reference table costs (milestone refs). Measurements, not gates
//! (the host decides): run by hand on a quiet host, in release, with
//! `TMPDIR` on a disk file system:
//!
//! ```text
//! cargo test --release -p lunixd --features testsupport --test stack_refs_cost \
//!     -- --ignored --nocapture --test-threads=1 2>&1 | tee refs-cost.log
//! ```
//!
//! * `the_shadow_table_cut_cost_is_reported` reports the cut time against a
//!   target of 3 % (nothing is asserted; read it from the log): the
//!   cut time per journal byte (the wall time of `commit_now` at a fixed
//!   number of journal bytes per cut), off against shadow, at 16 KiB and
//!   64 KiB chunks, on the real stack (radix map, pipelined destage), arms
//!   interleaved. The volume is overwritten once before the base pass, so
//!   the pass condemns and writes evidence: its `gc pass done` line carries
//!   `mark_ms` (read it in the log, with `tip_refs` off in the off arm and on
//!   in the shadow arm: `grep 'gc pass done'`).
//! * `the_map_transaction_costs_per_chunk` is `map_ns` per chunk (a real
//!   `RadixMap`, the destager's transaction shape: ascending chunks, one
//!   transaction), raw against wrapped. No bound: it is reported.
//! * `the_table_snapshot_ready_and_rebase_cost_per_call` is the clone of the
//!   counts a mark start takes under the coordinator mutex, `ready()` at
//!   every `gc_step` and `rebase` (the mark's comparison and repair, under
//!   the coordinator mutex and the table mutex), for volumes of 1 k to 1 M
//!   segments. Reported, no bound.
//!
//! A run at the defaults takes about an hour (about 4 minutes per arm: the
//! fill, one requested pass and the measured cuts; 16 arms). Every phase
//! prints a line (`fill` every 32 MiB), and a watchdog aborts the process
//! naming the phase when an arm makes no progress for `LUNIX_REFS_STALL_S`
//! seconds (default 600). The exact cost per journal byte is read from the
//! engine's cumulative cut counters (`cuts`, `cut_bytes_sum`, `cut_us_sum`).
//!
//! Knobs: `LUNIX_REFS_CUTS` (cuts per arm and repetition, 20),
//! `LUNIX_REFS_MIB_PER_CUT` (journal MiB overwritten per cut, 128),
//! `LUNIX_REFS_REPS` (repetitions of the pair of arms, 4).

#![cfg(feature = "testsupport")]

use std::sync::Arc;
use std::time::{Duration, Instant};

use lunix_engine::{CondemnOnCut, EngineConfig};
use lunix_format::node::DataEntry;
use lunix_format::{CryptoMode, IdLen, IdPrefix, LineageIdx, SegId};
use lunix_gc::GcConfig;
use lunix_iface::gc::{GcControl, RefsState};
use lunix_iface::map::{BlockMap, ChunkNo, NodeSource};
use lunix_map::{MapConfig, RadixMap};
use lunix_testkit::{MemPacks, PlainCodec};
use lunixd::testsupport::{close, payload, Driver, Scratch, VolumeBuilder};

fn env_or(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Cuts, journal bytes and microseconds, from the engine's cumulative
/// counters (`write_path.cuts`, `cut_bytes_sum`, `cut_us_sum`): the cost per
/// journal byte without sampling.
type Counted = (u64, u64, u64);

struct Arm {
    cuts_ms: Vec<f64>,
    /// Over the measured cuts.
    counted: Counted,
}

/// Progress of one arm, for the watchdog: a phase name and a counter that
/// moves while the arm works. A stuck arm is reported (and aborted) instead
/// of silent (the cost test once ran for 55 minutes without a line).
struct Progress {
    phase: std::sync::Mutex<&'static str>,
    ticks: std::sync::atomic::AtomicU64,
}

impl Progress {
    fn set(&self, phase: &'static str) {
        *self.phase.lock().unwrap_or_else(|p| p.into_inner()) = phase;
        self.tick();
    }

    fn tick(&self) {
        self.ticks
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Aborts the process when `progress` does not move for `LUNIX_REFS_STALL_S`
/// seconds (default 600), naming the phase it stood in.
fn watchdog(progress: &Arc<Progress>, tag: &str) -> Arc<std::sync::atomic::AtomicBool> {
    let stall = Duration::from_secs(env_or("LUNIX_REFS_STALL_S", 600));
    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (p, d, tag) = (Arc::clone(progress), Arc::clone(&done), tag.to_string());
    std::thread::spawn(move || {
        let mut last = (u64::MAX, Instant::now());
        while !d.load(std::sync::atomic::Ordering::Relaxed) {
            std::thread::sleep(Duration::from_secs(1));
            let now = p.ticks.load(std::sync::atomic::Ordering::Relaxed);
            if now != last.0 {
                last = (now, Instant::now());
            } else if last.1.elapsed() >= stall {
                let phase = *p.phase.lock().unwrap_or_else(|e| e.into_inner());
                eprintln!(
                    "refs cost: {tag} made no progress for {stall:?} in phase {phase:?} (tick {now}); aborting"
                );
                std::process::abort();
            }
        }
    });
    done
}

fn run_arm(shadow: bool, cs: u32, cuts: u64, mib_per_cut: u64, tag: &str) -> Arm {
    lunixd::log::StderrLog::install(tracing::Level::INFO);
    let progress = Arc::new(Progress {
        phase: std::sync::Mutex::new("create"),
        ticks: std::sync::atomic::AtomicU64::new(0),
    });
    let finished = watchdog(&progress, tag);
    let chunks = (1u64 << 30) / u64::from(cs);
    let per_cut = (mib_per_cut << 20) / u64::from(cs);
    let t = Scratch::new(tag);
    let mut b = VolumeBuilder::new(t.path())
        .mode(CryptoMode::Plain)
        .chunk_size(cs)
        .size_bytes(chunks * u64::from(cs));
    b.engine = Some(Arc::new(|c: &mut EngineConfig| {
        c.background = false;
        if let Some(p) = &mut c.gc_params {
            p.read_timeout = Duration::ZERO;
            p.grace = Duration::from_secs(10);
        }
    }));
    let vol = b.create().expect("create");
    progress.set("open");
    let stack = vol
        .open_with(|o| {
            o.gc = Some(GcConfig {
                pass_interval: Duration::from_secs(3600),
                ..GcConfig::daemon()
            });
            o.gc_condemn_on_cut = if shadow {
                CondemnOnCut::Shadow
            } else {
                CondemnOnCut::Off
            };
        })
        .expect("open");
    let mut d = Driver::new(&stack).expect("driver");
    let csz = u64::from(cs);
    let mut seed = 1u64;
    progress.set("fill");
    // The volume twice over: the second pass of writes kills the first, so
    // the base pass below has garbage to condemn.
    for round in 0..2 {
        for c in 0..chunks {
            seed += 1;
            assert!(d
                .write(0, c * csz, &payload(seed, cs as usize), false)
                .is_ok());
            progress.tick();
            if c % 2048 == 2047 {
                stack.engine.commit_now(true).expect("cut");
                eprintln!("refs cost: {tag}: fill {round} chunk {}/{chunks}", c + 1);
            }
        }
        progress.set("fill cuts");
        for _ in 0..3 {
            stack.engine.commit_now(true).expect("cut");
        }
        progress.set("fill");
    }
    progress.set("base pass");
    // One whole pass: the table's base (a request is the whole sequence);
    // its `gc pass done` line has `mark_ms`.
    let started = Instant::now();
    GcControl::run_pass(&*stack.engine, false).expect("pass");
    eprintln!(
        "refs cost: {} KiB {} arm: base pass took {:.1?}",
        cs >> 10,
        if shadow { "shadow" } else { "off" },
        started.elapsed()
    );
    if shadow {
        let r = stack
            .engine
            .gc_status_handle()
            .get()
            .refs
            .map(|b| *b)
            .expect("the table exists");
        assert_eq!(r.state, RefsState::Ready, "{r:?}");
    }
    let mut x = 0x9e37_79b9_7f4a_7c15u64 ^ cuts;
    let mut out = Vec::new();
    let c0 = stack.engine.cut_stats();
    progress.set("measured cuts");
    for _ in 0..cuts {
        for _ in 0..per_cut {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            seed += 1;
            let c = x % chunks;
            assert!(d
                .write(0, c * csz, &payload(seed, cs as usize), false)
                .is_ok());
            progress.tick();
        }
        let t0 = Instant::now();
        stack.engine.commit_now(true).expect("cut");
        out.push(t0.elapsed().as_secs_f64() * 1e3);
    }
    let c1 = stack.engine.cut_stats();
    let counted = (
        c1.cuts - c0.cuts,
        c1.cut_bytes_sum - c0.cut_bytes_sum,
        c1.cut_us_sum - c0.cut_us_sum,
    );
    if shadow {
        let refs = stack
            .engine
            .gc_status_handle()
            .get()
            .refs
            .map(|b| *b)
            .expect("the table exists");
        assert_eq!(refs.drift_segments, 0, "{refs:?}");
        assert_eq!(refs.report_mismatches, 0, "{refs:?}");
    }
    progress.set("close");
    drop(d);
    close(stack).expect("close");
    finished.store(true, std::sync::atomic::Ordering::Relaxed);
    Arm {
        cuts_ms: out,
        counted,
    }
}

fn median(v: &[f64]) -> f64 {
    let mut v = v.to_vec();
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

fn mean(v: &[f64]) -> f64 {
    v.iter().sum::<f64>() / v.len() as f64
}

#[test]
#[ignore = "a measurement; minutes (see the module doc)"]
fn the_shadow_table_cut_cost_is_reported() {
    let cuts = env_or("LUNIX_REFS_CUTS", 20);
    let mib = env_or("LUNIX_REFS_MIB_PER_CUT", 128);
    let reps = env_or("LUNIX_REFS_REPS", 4);
    for cs in [16u32 << 10, 64 << 10] {
        let (mut off, mut on) = (Vec::new(), Vec::new());
        let (mut counted_off, mut counted_on): (Vec<Counted>, Vec<Counted>) =
            (Vec::new(), Vec::new());
        for rep in 0..reps {
            // Alternate which arm goes first.
            let order = if rep % 2 == 0 {
                [false, true]
            } else {
                [true, false]
            };
            for shadow in order {
                let a = run_arm(
                    shadow,
                    cs,
                    cuts,
                    mib,
                    &format!("refscost{}{rep}{}", cs >> 10, u8::from(shadow)),
                );
                eprintln!(
                    "refs cost: {} KiB rep {rep} {}: median {:.1} ms, mean {:.1} ms over {} cuts; \
                     counters: {} cuts, {} journal bytes, {:.3} ms per MiB of journal",
                    cs >> 10,
                    if shadow { "shadow" } else { "off   " },
                    median(&a.cuts_ms),
                    mean(&a.cuts_ms),
                    a.cuts_ms.len(),
                    a.counted.0,
                    a.counted.1,
                    a.counted.2 as f64 / 1e3 / (a.counted.1 as f64 / (1u64 << 20) as f64),
                );
                if shadow {
                    counted_on.push(a.counted);
                } else {
                    counted_off.push(a.counted);
                }
                if shadow {
                    on.extend(a.cuts_ms);
                } else {
                    off.extend(a.cuts_ms);
                }
            }
        }
        // The journal bytes per cut are the same in every cut: the time per
        // journal byte is the time per cut over `mib` MiB.
        // From the cumulative counters: the time per journal MiB over every
        // cut of the arm, exactly (not a sample).
        let per_mib = |v: &[Counted]| {
            let (bytes, us) = v.iter().fold((0u64, 0u64), |a, c| (a.0 + c.1, a.1 + c.2));
            us as f64 / 1e3 / (bytes as f64 / (1u64 << 20) as f64)
        };
        let (co, cn) = (per_mib(&counted_off), per_mib(&counted_on));
        eprintln!(
            "refs cost: {} KiB chunks, from the counters: off {co:.3} ms per MiB of journal, shadow {cn:.3} ({:+.2} %)",
            cs >> 10,
            (cn / co - 1.0) * 100.0
        );
        let (mo, mn) = (median(&off), median(&on));
        let (ao, an) = (mean(&off), mean(&on));
        eprintln!(
            "refs cost: {} KiB chunks: off median {mo:.2} ms mean {ao:.2} ms; shadow median {mn:.2} ms \
             mean {an:.2} ms ({:.3} / {:.3} ms per MiB of journal); median {:+.2} %, mean {:+.2} % \
             (target: cut time per journal byte within 3 % of off; not asserted)",
            cs >> 10,
            mo / mib as f64,
            mn / mib as f64,
            (mn / mo - 1.0) * 100.0,
            (an / ao - 1.0) * 100.0
        );
    }
}

fn entry(seg: u64, off: u32) -> DataEntry {
    DataEntry {
        lineage: LineageIdx(0),
        seg: SegId::new(0, seg).expect("segment"),
        off,
        clen: 4096,
        prefix: IdPrefix::from_slice(IdLen::Sixteen, &[3; 16]).expect("prefix"),
    }
}

fn radix(chunks: u64) -> Arc<dyn BlockMap> {
    let source: Arc<dyn NodeSource> = Arc::new(MemPacks::new());
    Arc::new(
        RadixMap::new(
            Arc::new(PlainCodec::new()),
            source,
            chunks,
            MapConfig::new(512, IdLen::Sixteen),
        )
        .expect("map"),
    )
}

/// One destager-shaped transaction: `n` ascending chunks of `chunks`, spread
/// evenly, each set to an entry of one of 64 segments (so counts move).
fn txn(m: &dyn BlockMap, chunks: u64, n: u64, round: u64) -> Duration {
    let step = (chunks / n).max(1);
    let t0 = Instant::now();
    let mut t = m.begin_update().expect("begin");
    for i in 0..n {
        let seg = 1 + (round * 7 + i) % 64;
        t.set(ChunkNo(i * step), entry(seg, (i % 1000) as u32))
            .expect("set");
    }
    t.commit().expect("commit");
    t0.elapsed()
}

#[test]
#[ignore = "a measurement (see the module doc)"]
fn the_map_transaction_costs_per_chunk() {
    // 128 MiB of journal per cut: 8192 chunks of 16 KiB, 2048 of 64 KiB, on
    // a 4 GiB map (the leaves stay in the map's cache: the table's `get`s hit
    // leaves the commit loads anyway; this is the favourable case).
    for (cs, per_txn) in [(16u64 << 10, 8192u64), (64 << 10, 2048)] {
        let chunks = (4u64 << 30) / cs;
        let raw = radix(chunks);
        let wrapped = lunix_engine::ref_map_for_bench(radix(chunks));
        // Fill both: every chunk written once (the old entries are there).
        for m in [&raw, &wrapped] {
            let step = chunks / per_txn;
            for r in 0..(step.min(16)) {
                let mut t = m.begin_update().expect("begin");
                for i in 0..per_txn {
                    t.set(ChunkNo(i * step + r), entry(1 + i % 64, 0))
                        .expect("set");
                }
                t.commit().expect("commit");
            }
        }
        let (mut a, mut b) = (Vec::new(), Vec::new());
        for round in 0..20u64 {
            a.push(txn(&*raw, chunks, per_txn, round).as_secs_f64() * 1e9 / per_txn as f64);
            b.push(txn(&*wrapped, chunks, per_txn, round).as_secs_f64() * 1e9 / per_txn as f64);
        }
        let (ma, mb) = (median(&a), median(&b));
        eprintln!(
            "refs cost: {} KiB chunks, {per_txn} chunks per transaction: map_ns per chunk raw {ma:.0} ns, \
             wrapped {mb:.0} ns ({:+.1} %; the table's `get` and counting)",
            cs >> 10,
            (mb / ma - 1.0) * 100.0
        );
    }
}

#[test]
#[ignore = "a measurement (see the module doc)"]
fn the_table_snapshot_ready_and_rebase_cost_per_call() {
    for segments in [1_000usize, 16_000, 128_000, 1_000_000] {
        for zeros in [0usize, segments / 100, segments / 2] {
            let (start_ns, ready_ns, rebase_ns, slow_ns) =
                lunix_engine::table_costs(segments, zeros, 20);
            eprintln!(
                "refs cost: table of {segments} segments, {zeros} waiting: mark start {:.2} us, \
                 ready() {:.4} ms, rebase {:.2} ms, slow-path rebase {:.2} ms per call",
                start_ns as f64 / 1e3,
                ready_ns as f64 / 1e6,
                rebase_ns as f64 / 1e6,
                slow_ns as f64 / 1e6
            );
        }
    }
}
