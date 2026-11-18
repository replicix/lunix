//! The crash sweep (gate "Crash sweep"): every GC crash point is hit by at
//! least five workloads. At the point the collector stops as a `SIGKILL`
//! would (the fakes' durable images are what a kill leaves: completed
//! creates and deletes, the index manifest and runs but not its memtable, an
//! open compaction output as a leftover at random); the volume reopens like
//! M11's recovery (orphan sweep above the high-water mark, `catch_up`), every
//! root reads back its model content, the ledger equals the files after the
//! next pass, and the work converges to the quiescent state.
//!
//! The plan asks for a child process; with the in-memory fakes a child's
//! state would die with it, so the kill is modelled in-process exactly as
//! M11's engine model test does (`GcHooks::crash_at` plus the fakes' crash
//! images). The real-stack kill loop is M17.B's.

use std::collections::BTreeMap;
use std::time::Duration;

use lunix_format::IdLen;

use super::oracle::check_quiescent;
use super::sim::{Sim, SimCfg, GRACE};
use crate::CRASH_POINTS;

fn next(z: &mut u64) -> u64 {
    *z = z.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut x = *z;
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}

/// Runs `n` random actions. Returns `Err(point)` when a GC call stopped at
/// the armed crash point (the caller reopens).
fn actions(s: &mut Sim, z: &mut u64, n: usize) -> Result<(), &'static str> {
    for _ in 0..n {
        let r = next(z) % 100;
        let res = match r {
            0..=39 => {
                s.write(next(z) % 64, next(z) % 30);
                Ok(())
            }
            40..=54 => s.commit(),
            55..=77 => {
                let p = if r.is_multiple_of(2) {
                    s.step()
                } else {
                    s.unit()
                };
                s.clock.advance(Duration::from_secs(9));
                p.map(|_| ())
            }
            78..=87 => {
                s.clock.advance(Duration::from_secs(30));
                Ok(())
            }
            88..=90 => s.urgent(next(z) % (256 << 10)).map(|_| ()),
            91..=93 => {
                s.set_retain(1 + (next(z) % 4) as u32);
                Ok(())
            }
            _ => {
                let mut k = next(z);
                s.run_pass_with(k.is_multiple_of(2), &mut |s| {
                    k = next(&mut k);
                    s.write(k % 64, (k >> 8) % 30);
                    s.commit()
                })
            }
        };
        if let Err(e) = res {
            match s.hooks.crashed() {
                Some(p) => return Err(p),
                None => panic!("a call failed without a crash: {e}"),
            }
        }
    }
    Ok(())
}

fn sweep(
    chunk_size: u32,
    id_len: IdLen,
    workloads: u64,
    pipeline: bool,
) -> BTreeMap<&'static str, u64> {
    let mut hits: BTreeMap<&'static str, u64> = BTreeMap::new();
    for (pi, &point) in CRASH_POINTS.iter().enumerate() {
        for w in 0..workloads {
            let mut z = (pi as u64) << 32 | w;
            let mut cfg = SimCfg::new(chunk_size, id_len);
            cfg.gc.pipeline = pipeline;
            let mut s = Sim::new(cfg);
            s.hooks.arm(point, 1 + w % 3);
            let mut crashed = None;
            for _ in 0..40 {
                if let Err(p) = actions(&mut s, &mut z, 25) {
                    crashed = Some(p);
                    break;
                }
            }
            let Some(p) = crashed else {
                continue;
            };
            assert_eq!(p, point);
            *hits.entry(point).or_insert(0) += 1;
            s.crash_reopen(next(&mut z)).unwrap();
            s.check_roots()
                .unwrap_or_else(|e| panic!("after a crash at {point} (workload {w}): {e}"));
            // A second pass finishes the work.
            s.clock.advance(GRACE * 2);
            s.commit().unwrap();
            s.pass().unwrap();
            s.check_roots()
                .unwrap_or_else(|e| panic!("after the pass that follows {point}: {e}"));
            s.check_ledger()
                .unwrap_or_else(|e| panic!("after the pass that follows {point}: {e}"));
            actions(&mut s, &mut z, 60).expect("no crash armed after the reopen");
            check_quiescent(&mut s)
                .unwrap_or_else(|e| panic!("convergence after {point} (workload {w}): {e}"));
        }
    }
    hits
}

fn check(chunk_size: u32, id_len: IdLen) {
    check_with(chunk_size, id_len, false);
}

fn check_with(chunk_size: u32, id_len: IdLen, pipeline: bool) {
    let workloads = std::env::var("LUNIX_GC_CRASH_WORKLOADS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(10);
    let hits = sweep(chunk_size, id_len, workloads, pipeline);
    eprintln!(
        "gc crash sweep: chunk {chunk_size}, id_len {id_len:?}, pipeline {pipeline}: {hits:?}"
    );
    for p in CRASH_POINTS {
        let needed = if id_len == IdLen::Zero && p.starts_with("gc.compact.") {
            // Lean volumes never compact.
            0
        } else {
            5
        };
        let n = hits.get(p).copied().unwrap_or(0);
        assert!(
            n >= needed,
            "crash point {p} hit by {n} workloads (need {needed})"
        );
    }
}

#[test]
fn every_crash_point_16k() {
    check(16 << 10, IdLen::Sixteen);
}

#[test]
fn every_crash_point_64k() {
    check(64 << 10, IdLen::ThirtyTwo);
}

#[test]
fn every_crash_point_lean() {
    check(16 << 10, IdLen::Zero);
}

/// The daemon's pipelined schedule (milestone gcrate): the scheduled passes
/// delete with their condemn mark as the recheck.
#[test]
fn every_crash_point_pipelined_16k() {
    check_with(16 << 10, IdLen::Sixteen, true);
}
