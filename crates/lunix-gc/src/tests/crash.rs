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
use lunix_iface::gc::Collector;

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
///
/// `propose`: some of the writes are replaced by offers to `propose_dead`
/// (the dead segments, or every segment: live ones too).
fn actions(s: &mut Sim, z: &mut u64, n: usize, propose: bool) -> Result<(), &'static str> {
    for _ in 0..n {
        let r = next(z) % 100;
        let res = match r {
            30..=39 if propose => {
                let offer = if r.is_multiple_of(2) {
                    s.dead_segments()
                } else {
                    s.all_segments()
                };
                s.propose(&offer).map(|_| ())
            }
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
                .map(|_| ())
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

/// Whether a point is reached only through a proposal.
fn is_proposal_point(p: &str) -> bool {
    p.starts_with("gc.propose.")
}

fn sweep(
    chunk_size: u32,
    id_len: IdLen,
    workloads: u64,
    pipeline: bool,
    fast: bool,
    propose: bool,
    pgrace: bool,
) -> BTreeMap<&'static str, u64> {
    let mut hits: BTreeMap<&'static str, u64> = BTreeMap::new();
    for (pi, &point) in CRASH_POINTS.iter().enumerate() {
        // The sweeps without proposals do not reach the proposal points; the
        // ones with proposals sweep every point.
        if is_proposal_point(point) && !propose {
            continue;
        }
        for w in 0..workloads {
            let mut z = (pi as u64) << 32 | w;
            let mut cfg = SimCfg::new(chunk_size, id_len);
            cfg.gc.pipeline = pipeline;
            if fast {
                // Recheck-only passes between the regular ones: the regular
                // schedule is slow enough (20 s) for the sweep's 9 s steps
                // to find a grace that ended before it.
                cfg.gc.pass_interval = Duration::from_secs(30);
                cfg.gc.busy_interval = Duration::from_secs(20);
                cfg.gc.fast_recheck = Some(Duration::from_secs(1));
                cfg.gc.fast_recheck_always = true;
            }
            if pgrace {
                cfg.gc.segment_grace_min = Duration::ZERO;
            }
            let mut s = Sim::new(cfg);
            if pgrace {
                s.gc.set_pressure(true);
                s.gc.set_segment_grace(Some(Duration::from_millis(1)));
            }
            s.hooks.arm(point, 1 + w % 3);
            let mut crashed = None;
            for _ in 0..40 {
                if let Err(p) = actions(&mut s, &mut z, 25, propose) {
                    crashed = Some(p);
                    break;
                }
            }
            let Some(p) = crashed else {
                // Not crashed (the point was not reached often enough): the
                // longest history of the sweep, checked too (review m-12).
                s.check_roots()
                    .unwrap_or_else(|e| panic!("uncrashed workload {w} armed at {point}: {e}"));
                s.hooks.arm("none", 1);
                check_quiescent(&mut s).unwrap_or_else(|e| {
                    panic!("convergence of the uncrashed workload {w} armed at {point}: {e}")
                });
                continue;
            };
            assert_eq!(p, point);
            *hits.entry(point).or_insert(0) += 1;
            s.crash_reopen(next(&mut z)).unwrap();
            if pgrace {
                s.gc.set_pressure(true);
                s.gc.set_segment_grace(Some(Duration::from_millis(1)));
            }
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
            actions(&mut s, &mut z, 60, propose).expect("no crash armed after the reopen");
            check_quiescent(&mut s)
                .unwrap_or_else(|e| panic!("convergence after {point} (workload {w}): {e}"));
        }
    }
    hits
}

fn check(chunk_size: u32, id_len: IdLen) {
    check_with(chunk_size, id_len, false, false, false);
}

fn check_with(chunk_size: u32, id_len: IdLen, pipeline: bool, fast: bool, propose: bool) {
    check_pg(chunk_size, id_len, pipeline, fast, propose, false);
}

fn check_pg(
    chunk_size: u32,
    id_len: IdLen,
    pipeline: bool,
    fast: bool,
    propose: bool,
    pgrace: bool,
) {
    let workloads = std::env::var("LUNIX_GC_CRASH_WORKLOADS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(10);
    let hits = sweep(
        chunk_size, id_len, workloads, pipeline, fast, propose, pgrace,
    );
    eprintln!(
        "gc crash sweep: chunk {chunk_size}, id_len {id_len:?}, pipeline {pipeline}, fast {fast}, propose {propose}, pgrace {pgrace}: {hits:?}"
    );
    for p in CRASH_POINTS {
        if is_proposal_point(p) && !propose {
            continue;
        }
        let needed = if id_len == IdLen::Zero && p.starts_with("gc.compact.") {
            // Lean volumes never compact.
            0
        } else if p.starts_with("gc.recheck.") && !fast {
            // Only a recheck-only pass reaches it.
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
    check_with(16 << 10, IdLen::Sixteen, true, false, false);
}

#[test]
fn every_crash_point_pipelined_64k() {
    check_with(64 << 10, IdLen::ThirtyTwo, true, false, false);
}

#[test]
fn every_crash_point_pipelined_lean() {
    check_with(16 << 10, IdLen::Zero, true, false, false);
}

/// The recheck-only passes (milestone fastrecheck): the point
/// `gc.recheck.before_delete` and every delete point inside such a pass.
#[test]
fn every_crash_point_with_recheck_passes_16k() {
    check_with(16 << 10, IdLen::Sixteen, true, true, false);
}

#[test]
fn every_crash_point_with_recheck_passes_64k() {
    check_with(64 << 10, IdLen::ThirtyTwo, true, true, false);
}

#[test]
fn every_crash_point_with_recheck_passes_lean() {
    check_with(16 << 10, IdLen::Zero, true, true, false);
}

/// Proposals (milestone condemncut): the two points of `propose_dead`, and
/// the existing condemn and index points reached through a proposal. Offers
/// are the dead segments or every segment (live ones included); the recheck
/// is the judge and the volume converges.
#[test]
fn every_crash_point_with_proposals_16k() {
    check_with(16 << 10, IdLen::Sixteen, true, true, true);
}

#[test]
fn every_crash_point_with_proposals_64k() {
    check_with(64 << 10, IdLen::ThirtyTwo, true, true, true);
}

#[test]
fn every_crash_point_with_proposals_lean() {
    check_with(16 << 10, IdLen::Zero, true, true, true);
}

/// Milestone pgrace: the same matrices with the short segment grace in force
/// (pressure on, grace 1 ms, the collector's minimum 0): a kill at any point
/// reopens to a volume whose every root reads and which converges. The
/// restart starts every grace again.
#[test]
fn every_crash_point_with_the_short_grace_16k() {
    check_pg(16 << 10, IdLen::Sixteen, true, true, false, true);
}

#[test]
fn every_crash_point_with_the_short_grace_64k() {
    check_pg(64 << 10, IdLen::ThirtyTwo, true, true, false, true);
}

#[test]
fn every_crash_point_with_the_short_grace_lean() {
    check_pg(16 << 10, IdLen::Zero, true, true, false, true);
}

#[test]
fn every_crash_point_with_the_short_grace_and_proposals_16k() {
    check_pg(16 << 10, IdLen::Sixteen, true, true, true, true);
}
