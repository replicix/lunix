//! The GC oracle (gate "GC oracle"): random workloads of writes, overwrites,
//! discards, commits, retention changes, pins added and removed, rollback
//! floor stalls, budgeted steps with and without interleaved cuts and urgent
//! passes. After every pass every block of every root reads back its model
//! content and the ledger equals the files; at quiescence the objects that
//! exist are exactly the model's reachable set (plus what the rules keep:
//! named by a retained commit or above the high-water mark), and the commit
//! objects are exactly `min(chain_floor, rollback floor) ..= tip` plus pins.
//!
//! `LUNIX_GC_CASES` (default 48 per configuration) and `LUNIX_GC_OPS`
//! (default 300) scale it; the gate run uses 2 000 cases.

use std::time::Duration;

use lunix_format::IdLen;
use proptest::prelude::*;
use proptest::test_runner::{Config, TestCaseError, TestRunner};

use super::sim::{MapKind, Sim, SimCfg};

#[derive(Clone, Debug)]
enum Op {
    Write(u64, u64),
    Discard(u64),
    Commit,
    Steps(u8),
    Advance(u16),
    Retain(u8),
    Pin(u64),
    Unpin(u64),
    FloorStall(bool),
    InterleavedPass(u64),
    Urgent(u32),
    Crash(u64),
    Faulty(u8, u8, bool, u8),
}

/// Store and index points a collector call passes (non-crash faults).
pub(crate) const FAULT_POINTS: &[&str] = &[
    "segment.read",
    "segment.footer",
    "segment.append",
    "segment.seal.before",
    "segment.seal.after",
    "segment.delete.before",
    "segment.delete.after",
    "pack.delete.before",
    "pack.delete.after",
    "commit.delete.before",
    "commit.delete.after",
    "condemned.store.before",
    "condemned.store.after",
    "evidence.put.before",
    "evidence.put.after",
    "index.condemn.before",
    "index.drop.before",
    "index.delete_run.before",
];

/// Runs `steps` collector calls with one fault armed, tolerating the errors
/// it causes, then disarms; every root must still read.
pub(crate) fn faulty_steps(
    s: &mut Sim,
    point: usize,
    nth: u64,
    nospace: bool,
    steps: u8,
) -> Result<(), String> {
    let p = FAULT_POINTS[point % FAULT_POINTS.len()];
    s.arm_fault(p, nth, nospace);
    for k in 0..steps {
        let _ = match k % 4 {
            0 => s.unit().map(|_| ()),
            3 => s.urgent(1 << 20).map(|_| ()),
            _ => s.step().map(|_| ()),
        };
        s.clock.advance(Duration::from_secs(20));
    }
    s.disarm_faults();
    s.check_roots()
        .map_err(|e| format!("after faults at {p}: {e}"))
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        40 => (0u64..64, 0u64..40).prop_map(|(c, s)| Op::Write(c, s)),
        5 => (0u64..64).prop_map(Op::Discard),
        15 => Just(Op::Commit),
        15 => (1u8..12).prop_map(Op::Steps),
        6 => (0u16..90).prop_map(Op::Advance),
        3 => (1u8..5).prop_map(Op::Retain),
        2 => any::<u64>().prop_map(Op::Pin),
        2 => any::<u64>().prop_map(Op::Unpin),
        2 => any::<bool>().prop_map(Op::FloorStall),
        4 => any::<u64>().prop_map(Op::InterleavedPass),
        2 => (1u32..64).prop_map(Op::Urgent),
        1 => any::<u64>().prop_map(Op::Crash),
        4 => (any::<u8>(), 1u8..6, any::<bool>(), 1u8..16)
            .prop_map(|(p, n, ns, k)| Op::Faulty(p, n, ns, k)),
    ]
}

fn env(name: &str, default: u32) -> u32 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn fail(what: String) -> TestCaseError {
    TestCaseError::fail(what)
}

fn after_pass(s: &Sim, ctx: &str) -> Result<(), TestCaseError> {
    s.check_roots().map_err(|e| fail(format!("{ctx}: {e}")))?;
    s.check_ledger().map_err(|e| fail(format!("{ctx}: {e}")))
}

fn run_ops(s: &mut Sim, ops: &[Op]) -> Result<u64, TestCaseError> {
    let mut passes = 0u64;
    let err = |e: lunix_iface::Error| fail(format!("{e}"));
    for (i, op) in ops.iter().enumerate() {
        match *op {
            Op::Write(c, seed) => s.write(c, seed),
            Op::Discard(c) => s.discard(c),
            Op::Commit => s.commit().map_err(err)?,
            Op::Steps(n) => {
                for _ in 0..n {
                    let p = s.step().map_err(err)?;
                    s.clock.advance(Duration::from_secs(7));
                    if p.done {
                        passes += 1;
                        after_pass(s, &format!("op {i} step"))?;
                    }
                }
            }
            Op::Advance(secs) => s.clock.advance(Duration::from_secs(u64::from(secs))),
            Op::Retain(n) => s.set_retain(u32::from(n)),
            Op::Pin(p) => s.pin(p),
            Op::Unpin(p) => s.unpin(p),
            Op::FloorStall(stall) => s.raise_floor = !stall,
            Op::InterleavedPass(seed) => {
                let mut k = seed;
                s.run_pass_with(seed % 2 == 1, &mut |s| {
                    k = k.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
                    s.write(k >> 40, (k >> 20) % 40);
                    s.commit()
                })
                .map_err(err)?;
                passes += 1;
                after_pass(s, &format!("op {i} interleaved pass"))?;
            }
            Op::Crash(seed) => {
                s.crash_reopen(seed).map_err(err)?;
                s.check_roots()
                    .map_err(|e| fail(format!("op {i} after a crash: {e}")))?;
            }
            Op::Faulty(p, nth, nospace, steps) => {
                faulty_steps(s, usize::from(p), u64::from(nth), nospace, steps)
                    .map_err(|e| fail(format!("op {i}: {e}")))?;
            }
            Op::Urgent(kib) => {
                s.urgent(u64::from(kib) << 10).map_err(err)?;
                passes += 1;
                after_pass(s, &format!("op {i} urgent"))?;
            }
        }
    }
    Ok(passes)
}

pub(crate) fn check_quiescent(s: &mut Sim) -> Result<(), TestCaseError> {
    s.quiesce().map_err(|e| fail(format!("quiesce: {e}")))?;
    after_pass(s, "quiescent")?;
    let kept = s.kept_by_rule();
    let existing: std::collections::BTreeSet<_> = s
        .existing()
        .into_iter()
        .filter(|(_, id)| !kept.contains(id))
        .collect();
    let reachable: std::collections::BTreeSet<_> = s
        .model_reachable()
        .into_iter()
        .filter(|(_, id)| !kept.contains(id))
        .collect();
    if existing != reachable {
        let extra: Vec<_> = existing.difference(&reachable).collect();
        let missing: Vec<_> = reachable.difference(&existing).collect();
        let l = s
            .with_view(|v| crate::liveness(v, s.cfg.gc))
            .map_err(|e| fail(e.to_string()))?;
        let detail: Vec<String> = extra
            .iter()
            .map(|(_, id)| {
                let seg = lunix_format::ObjId::from_raw(*id).expect("id");
                format!(
                    "{id}: live {} deps {:?} output {} hwm {}",
                    l.live_bytes(seg),
                    l.deps.get(&seg),
                    s.gc.unnamed_outputs().contains(&seg),
                    super::sim::high_water(&s.retained)
                )
            })
            .collect();
        return Err(fail(format!(
            "quiescent objects differ: unreferenced still present {extra:?} ({detail:?}), reachable missing {missing:?}"
        )));
    }
    let commits: std::collections::BTreeSet<u64> = lunix_iface::store::CommitStore::list(&s.store)
        .map_err(|e| fail(e.to_string()))?
        .iter()
        .map(|c| c.seq.0)
        .collect();
    let want = s.expected_commits();
    if commits != want {
        return Err(fail(format!("commits {commits:?} != expected {want:?}")));
    }
    if !s.gc.condemned().is_empty() {
        return Err(fail(format!(
            "still condemned at quiescence: {:?}",
            s.gc.condemned()
        )));
    }
    Ok(())
}

fn oracle(chunk_size: u32, id_len: IdLen, salt: u64) {
    let cases = env("LUNIX_GC_CASES", 48);
    let ops = env("LUNIX_GC_OPS", 300) as usize;
    let mut runner = TestRunner::new(Config {
        cases,
        failure_persistence: None,
        rng_seed: proptest::test_runner::RngSeed::Fixed(0x6c75_6e69_7867_6300 ^ salt),
        ..Config::default()
    });
    let strat = (
        proptest::collection::vec(op(), ops..=ops),
        0u8..4,
        any::<bool>(),
    );
    let total_passes = std::cell::Cell::new(0u64);
    let totals = std::cell::RefCell::new(lunix_iface::gc::GcPassSummary::default());
    runner
        .run(&strat, |(ops, variant, encrypted)| {
            let mut cfg = SimCfg::new(chunk_size, id_len);
            cfg.map = if variant == 0 {
                MapKind::Radix
            } else {
                MapKind::Flat
            };
            cfg.encrypted = encrypted && variant == 1;
            let mut s = Sim::new(cfg);
            total_passes.set(total_passes.get() + run_ops(&mut s, &ops)?);
            check_quiescent(&mut s)?;
            let x = s.gc.totals();
            let mut t = totals.borrow_mut();
            t.segments_deleted += x.segments_deleted;
            t.packs_deleted += x.packs_deleted;
            t.runs_deleted += x.runs_deleted;
            t.commits_deleted += x.commits_deleted;
            t.segments_compacted += x.segments_compacted;
            t.bytes_freed += x.bytes_freed;
            Ok(())
        })
        .unwrap();
    eprintln!(
        "gc oracle: chunk {chunk_size}, id_len {id_len:?}: {cases} cases x {ops} ops, {} passes checked; deleted {} segments, {} packs, {} runs, {} commits; {} segments compacted",
        total_passes.get(),
        totals.borrow().segments_deleted,
        totals.borrow().packs_deleted,
        totals.borrow().runs_deleted,
        totals.borrow().commits_deleted,
        totals.borrow().segments_compacted,
    );
}

#[test]
fn oracle_16k_id16() {
    oracle(16 << 10, IdLen::Sixteen, 1);
}

#[test]
fn oracle_64k_id32() {
    oracle(64 << 10, IdLen::ThirtyTwo, 2);
}

#[test]
fn oracle_16k_id32() {
    oracle(16 << 10, IdLen::ThirtyTwo, 3);
}

#[test]
fn oracle_64k_id16() {
    oracle(64 << 10, IdLen::Sixteen, 4);
}

#[test]
fn oracle_lean_id0() {
    oracle(16 << 10, IdLen::Zero, 5);
}
