//! The GC oracle (gate "GC oracle"): random workloads of writes, overwrites,
//! discards, commits, retention changes, pins added and removed, rollback
//! floor stalls, budgeted steps with and without interleaved cuts and urgent
//! passes. After every pass every block of every root reads back its model
//! content and the ledger equals the files; at quiescence the objects that
//! exist are exactly the model's reachable set (plus what the rules keep:
//! named by a retained commit or above the high-water mark), and the commit
//! objects are exactly `min(chain_floor, rollback floor) ..= tip` plus pins,
//! where `chain_floor` is the lowest one a retained FULL records (N24).
//!
//! "Any proposal is safe" (milestone condemncut): the workloads also offer
//! the collector arbitrary and adversarial proposals (`Op::Propose`):
//! segments no root reaches, segments every root reaches, random subsets with
//! random `since`, ids that do not exist, offers while a pin exists and while
//! a pass is running. Every root and pin must still read its model content
//! after every pass and the volume must converge, so no live data is ever
//! deleted whatever was proposed.
//!
//! `LUNIX_GC_CASES` (default 48 per configuration) and `LUNIX_GC_OPS`
//! (default 300) scale it; the gate run uses 2 000 cases.

use std::time::Duration;

use lunix_format::{CommitSeq, IdLen};
use lunix_iface::gc::{Collector, DeadSince};
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
    Pressure(bool),
    Faulty(u8, u8, bool, u8),
    /// An offer to `propose_dead`: the seed picks the segments and their
    /// `since`, the second figure the pool (0: the dead ones; else every
    /// segment, live ones included).
    Propose(u64, u8),
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
        3 => any::<bool>().prop_map(Op::Pressure),
        4 => (any::<u8>(), 1u8..6, any::<bool>(), 1u8..16)
            .prop_map(|(p, n, ns, k)| Op::Faulty(p, n, ns, k)),
        6 => (any::<u64>(), 0u8..3).prop_map(|(k, m)| Op::Propose(k, m)),
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

/// A random subset of the dead (`pool` 0) or of every segment, each with a
/// `since` that is mostly the earliest cut (only the collector's other rules
/// judge it) and sometimes a random one; a missing id now and then.
fn adversarial_offer(s: &Sim, seed: u64, pool: u8) -> Vec<DeadSince> {
    let mut k = seed | 1;
    let mut next = move || {
        k = k
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        k >> 33
    };
    let from = if pool == 0 {
        s.dead_segments()
    } else {
        s.all_segments()
    };
    let tip = s.tip.common.commit_seq.0;
    let mut offer = Vec::new();
    for d in from {
        if next().is_multiple_of(3) {
            continue;
        }
        let since = match next() % 5 {
            0 => CommitSeq(next() % (tip + 4)),
            _ => CommitSeq(0),
        };
        offer.push(DeadSince { seg: d.seg, since });
    }
    if next().is_multiple_of(4) {
        if let Ok(seg) = lunix_format::ObjId::from_raw(1 + next() % 1_000) {
            offer.push(DeadSince {
                seg,
                since: CommitSeq(0),
            });
        }
    }
    offer
}

/// The segments a pin reaches and no retained root does, and the ones only
/// the oldest retained FULL reaches (by hint), now.
fn exclusive_segments(
    s: &Sim,
) -> (
    std::collections::BTreeSet<lunix_format::SegId>,
    std::collections::BTreeSet<lunix_format::SegId>,
) {
    use std::collections::BTreeSet;
    let retained: BTreeSet<u64> = s
        .gc_retained()
        .iter()
        .map(|f| f.common.commit_seq.0)
        .collect();
    let mut sets: Vec<(u64, BTreeSet<lunix_format::SegId>)> = Vec::new();
    for (seq, root) in s.roots() {
        let mut set = BTreeSet::new();
        if s.map
            .walk_leaves(&root, &mut |_, e| {
                set.insert(e.seg);
            })
            .is_ok()
        {
            sets.push((seq, set));
        }
    }
    let others = |keep: &dyn Fn(u64) -> bool| -> BTreeSet<lunix_format::SegId> {
        sets.iter()
            .filter(|(q, _)| keep(*q))
            .flat_map(|(_, x)| x.iter().copied())
            .collect()
    };
    let mut only_pin = BTreeSet::new();
    for (q, set) in &sets {
        if !retained.contains(q) {
            let rest = others(&|o| o != *q);
            only_pin.extend(set.difference(&rest).copied());
        }
    }
    let mut only_old = BTreeSet::new();
    if let Some(&oldest) = retained.iter().next() {
        if let Some((_, set)) = sets.iter().find(|(q, _)| *q == oldest) {
            let rest = others(&|o| o != oldest);
            only_old.extend(set.difference(&rest).copied());
        }
    }
    (only_pin, only_old)
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
                note_fast(s);
                s.crash_reopen(seed).map_err(err)?;
                s.check_roots()
                    .map_err(|e| fail(format!("op {i} after a crash: {e}")))?;
            }
            Op::Pressure(raised) => s.gc.set_pressure(raised),
            Op::Propose(seed, pool) => {
                let offer = adversarial_offer(s, seed, pool);
                let (only_pin, only_old) = exclusive_segments(s);
                let before: std::collections::BTreeSet<_> = s.gc.condemned().into_iter().collect();
                s.propose(&offer).map_err(err)?;
                let new: Vec<_> =
                    s.gc.condemned()
                        .into_iter()
                        .filter(|o| !before.contains(o))
                        .filter_map(|o| match o {
                            crate::condemned::ObjectRef::Segment(x) => Some(x),
                            _ => None,
                        })
                        .collect();
                EXCLUSIVE.with(|c| {
                    let (a, b) = c.get();
                    c.set((
                        a + new.iter().filter(|x| only_pin.contains(x)).count() as u64,
                        b + new.iter().filter(|x| only_old.contains(x)).count() as u64,
                    ));
                });
                // Not the ledger: a pass may be in progress or a failed
                // delete may be waiting for its release.
                s.check_roots()
                    .map_err(|e| fail(format!("op {i} proposal: {e}")))?;
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

thread_local! {
    /// Recheck-only passes and the segments they deleted, over the cases of
    /// the test on this thread (a crash replaces the collector, so the
    /// counters are taken before it).
    static FAST: std::cell::Cell<(u64, u64)> = const { std::cell::Cell::new((0, 0)) };
    /// Segments a proposal condemned that only a pin reached, and that only
    /// the oldest retained FULL reached (the unfiltered configuration).
    static EXCLUSIVE: std::cell::Cell<(u64, u64)> = const { std::cell::Cell::new((0, 0)) };
    /// Proposals condemned, un-condemned by a recheck and deleted.
    static PROPOSED: std::cell::Cell<(u64, u64, u64)> = const { std::cell::Cell::new((0, 0, 0)) };
}

fn note_fast(s: &Sim) {
    let r = s.gc.status().reclaim;
    FAST.with(|f| {
        let (p, d) = f.get();
        f.set((p + r.fast_passes, d + r.fast_segments_deleted));
    });
    PROPOSED.with(|f| {
        let (c, u, d) = f.get();
        f.set((
            c + r.proposal_condemned,
            u + r.proposal_uncondemned,
            d + r.proposal_deleted,
        ));
    });
}

fn oracle(chunk_size: u32, id_len: IdLen, salt: u64) {
    oracle_cfg(chunk_size, id_len, salt, false, false);
}

fn oracle_with(chunk_size: u32, id_len: IdLen, salt: u64, fast: bool) {
    oracle_cfg(chunk_size, id_len, salt, fast, false);
}

/// `fast`: the daemon's pipelined schedule with recheck-only passes
/// (`fast_recheck`), the pressure toggled by the workload. `unfiltered`: the
/// collector's pin and `since` rejects are off (`propose_unfiltered`), so the
/// proposals the filters would drop (a segment only a pin reaches, one only
/// the oldest retained FULL reaches) are condemned and only the recheck
/// stands between them and a delete.
fn oracle_cfg(chunk_size: u32, id_len: IdLen, salt: u64, fast: bool, unfiltered: bool) {
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
            if fast {
                cfg.gc = crate::config::GcConfig {
                    pass_interval: Duration::from_secs(30),
                    busy_interval: Duration::from_secs(20),
                    fast_recheck: Some(Duration::from_secs(1)),
                    ..crate::config::GcConfig::daemon()
                };
            }
            cfg.gc.propose_unfiltered = unfiltered;
            let mut s = Sim::new(cfg);
            total_passes.set(total_passes.get() + run_ops(&mut s, &ops)?);
            check_quiescent(&mut s)?;
            note_fast(&s);
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
    let (fast_passes, fast_deleted) = FAST.with(std::cell::Cell::get);
    eprintln!("gc oracle: recheck-only passes {fast_passes}, segments they deleted {fast_deleted}");
    let (condemned, uncondemned, deleted) = PROPOSED.with(std::cell::Cell::get);
    let (only_pin, only_old) = EXCLUSIVE.with(std::cell::Cell::get);
    eprintln!("gc oracle: proposals condemned that only a pin reached {only_pin}, only the oldest retained FULL {only_old}");
    if unfiltered {
        assert!(
            only_pin > 0 && only_old > 0,
            "the unfiltered oracle condemned no segment only a pin ({only_pin}) or only the oldest retained FULL ({only_old}) reaches"
        );
    }
    eprintln!(
        "gc oracle: proposals condemned {condemned}, un-condemned by a recheck {uncondemned}, deleted {deleted}"
    );
    assert!(
        condemned > 0 && uncondemned > 0 && deleted > 0,
        "the proposals exercised nothing: condemned {condemned}, un-condemned {uncondemned}, deleted {deleted}"
    );
    if fast {
        assert!(
            fast_passes > 0 && fast_deleted > 0,
            "the fast oracle ran no recheck pass that deleted"
        );
    }
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

/// Recheck-only passes under random pressure: every root reads, the ledger
/// equals the files, and the volume converges to the quiescent state.
#[test]
fn oracle_fast_recheck_16k_id16() {
    oracle_with(16 << 10, IdLen::Sixteen, 6, true);
}

#[test]
fn oracle_fast_recheck_64k_id32() {
    oracle_with(64 << 10, IdLen::ThirtyTwo, 7, true);
}

/// The proposals' own filters off: only the recheck stands between an
/// arbitrary proposal and a delete (milestone condemncut, review S2).
#[test]
fn oracle_unfiltered_proposals_16k_id16() {
    oracle_cfg(16 << 10, IdLen::Sixteen, 8, false, true);
}

#[test]
fn oracle_unfiltered_proposals_64k_id32() {
    oracle_cfg(64 << 10, IdLen::ThirtyTwo, 9, false, true);
}

#[test]
fn oracle_unfiltered_proposals_with_recheck_passes_16k_id16() {
    oracle_cfg(16 << 10, IdLen::Sixteen, 10, true, true);
}
