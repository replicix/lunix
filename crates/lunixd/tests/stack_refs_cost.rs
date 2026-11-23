//! What the reference table costs a cut (milestone refs): the same
//! write-heavy workload with `gc_condemn_on_cut` off and `shadow`, on the
//! real stack (radix map, pipelined destage), arms interleaved. A
//! measurement, not a gate; the target is a cut time within 3 %.
//!
//! ```text
//! TMPDIR=<a directory on a disk file system> cargo test --release -p lunixd \
//!     --features testsupport --test stack_refs_cost -- --ignored --nocapture
//! ```
//!
//! Knobs: `LUNIX_REFS_CUTS` (cuts per arm and repetition, 20),
//! `LUNIX_REFS_PER_CUT` (chunks overwritten per cut, 2048),
//! `LUNIX_REFS_REPS` (repetitions of the pair of arms, 4).
//!
//! The volume (1 GiB of 64 KiB chunks) is filled first and one whole GC pass
//! rebases the table, so the table is `Ready` and the overwrites kill
//! references (the table's whole job). The collector then sleeps (pass
//! interval of an hour) so that no mark or compaction shares the
//! coordinator with the cuts being timed. The time of a cut is the wall time
//! of `commit_now`.

#![cfg(feature = "testsupport")]

use std::sync::Arc;
use std::time::{Duration, Instant};

use lunix_engine::{CondemnOnCut, EngineConfig};
use lunix_format::CryptoMode;
use lunix_gc::GcConfig;
use lunix_iface::gc::{GcControl, RefsState};
use lunixd::testsupport::{close, payload, Driver, Scratch, VolumeBuilder};

fn env_or(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

struct Arm {
    cuts_ms: Vec<f64>,
}

fn run_arm(shadow: bool, cuts: u64, per_cut: u64, tag: &str) -> Arm {
    const CS: u32 = 64 << 10;
    const CHUNKS: u64 = 16_384;
    let t = Scratch::new(tag);
    let mut b = VolumeBuilder::new(t.path())
        .mode(CryptoMode::Plain)
        .chunk_size(CS)
        .size_bytes(CHUNKS * u64::from(CS));
    b.engine = Some(Arc::new(|c: &mut EngineConfig| {
        c.background = false;
        if let Some(p) = &mut c.gc_params {
            p.read_timeout = Duration::ZERO;
            p.grace = Duration::from_secs(10);
        }
    }));
    let vol = b.create().expect("create");
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
    let cs = u64::from(CS);
    let mut seed = 1u64;
    for c in 0..CHUNKS {
        seed += 1;
        assert!(d
            .write(0, c * cs, &payload(seed, CS as usize), false)
            .is_ok());
        if c % 2048 == 2047 {
            stack.engine.commit_now(true).expect("cut");
        }
    }
    for _ in 0..3 {
        stack.engine.commit_now(true).expect("cut");
    }
    // One whole pass: the table's base (a request is the whole sequence).
    let started = Instant::now();
    GcControl::run_pass(&*stack.engine, false).expect("pass");
    eprintln!("refs cost: base pass took {:.1?}", started.elapsed());
    if shadow {
        let r = *stack.engine.gc_status_handle().get().refs;
        assert_eq!(r.state, RefsState::Ready, "{r:?}");
    }
    let mut x = 0x9e37_79b9_7f4a_7c15u64 ^ cuts;
    let mut out = Vec::new();
    for _ in 0..cuts {
        for _ in 0..per_cut {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            seed += 1;
            let c = x % CHUNKS;
            assert!(d
                .write(0, c * cs, &payload(seed, CS as usize), false)
                .is_ok());
        }
        let t0 = Instant::now();
        stack.engine.commit_now(true).expect("cut");
        out.push(t0.elapsed().as_secs_f64() * 1e3);
    }
    let refs = *stack.engine.gc_status_handle().get().refs;
    assert_eq!(refs.drift_segments, 0, "{refs:?}");
    drop(d);
    close(stack).expect("close");
    Arm { cuts_ms: out }
}

fn median(v: &mut [f64]) -> f64 {
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

#[test]
#[ignore = "a measurement; minutes (see the module doc)"]
fn the_shadow_table_costs_a_cut_within_three_percent() {
    let cuts = env_or("LUNIX_REFS_CUTS", 20);
    let per = env_or("LUNIX_REFS_PER_CUT", 2048);
    let reps = env_or("LUNIX_REFS_REPS", 4);
    let (mut off, mut on) = (Vec::new(), Vec::new());
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
                cuts,
                per,
                &format!("refscost{rep}{}", u8::from(shadow)),
            );
            let mut m = a.cuts_ms.clone();
            eprintln!(
                "refs cost: rep {rep} {}: median {:.1} ms, mean {:.1} ms over {} cuts",
                if shadow { "shadow" } else { "off   " },
                median(&mut m),
                a.cuts_ms.iter().sum::<f64>() / a.cuts_ms.len() as f64,
                a.cuts_ms.len()
            );
            if shadow {
                on.extend(a.cuts_ms);
            } else {
                off.extend(a.cuts_ms);
            }
        }
    }
    let mean = |v: &[f64]| v.iter().sum::<f64>() / v.len() as f64;
    let (mo, mn) = (median(&mut off.clone()), median(&mut on.clone()));
    let (ao, an) = (mean(&off), mean(&on));
    eprintln!(
        "refs cost: off median {mo:.2} ms mean {ao:.2} ms; shadow median {mn:.2} ms mean {an:.2} ms; \
         median {:+.2} %, mean {:+.2} %",
        (mn / mo - 1.0) * 100.0,
        (an / ao - 1.0) * 100.0
    );
}
