//! The engine's reference table in shadow mode (milestone refs): over random
//! workloads with the real collector, the table matches every mark exactly
//! (`drift_segments == 0`), and shadow mode changes no decision of the
//! collector (the segment set and the collector's counters after every step
//! equal those of a run with the mode off). The scripts include restarts
//! (the `Rebuilding` path with deltas before and during the first mark),
//! urgent passes (marks without a preceding offer), dedup and compaction;
//! the proptest drivers assert that each of them happened.
//!
//! R2-1, the exact oracle: at every offer of the table, a ground-truth walk of
//! every retained root (the collector's own root set, through the map) must
//! find no entry hinting at an offered segment. The check runs inside the
//! collector call that follows the offer (same call, same coordinator mutex,
//! same roots), see `Oracle`.
//!
//! `LUNIX_REFS_CASES` raises the number of cases (default 48).

mod common;

use std::cell::Cell;
use std::collections::{BTreeSet, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::*;
use lunix_engine::{CondemnOnCut, EngineConfig};
use lunix_format::{IdLen, SegId};
use lunix_gc::{FloorSource, Gc, GcConfig};
use lunix_iface::block::{BlockEngine, IoOp};
use lunix_iface::clock::ManualClock;
use lunix_iface::gc::{
    Collector, DeadSince, GcBudget, GcControl, GcParams, GcPassSummary, GcProgress, GcStats,
    GcStatusSnapshot, GcView, MarkReport, Moved, RefsState, RefsStats,
};
use lunix_iface::store::SegmentStore;
use lunix_iface::Result;
use lunix_testkit::JournalMode;
use proptest::prelude::*;
use proptest::test_runner::{Config as RunnerConfig, TestRunner};

const CS: u32 = 16 << 10;
const CHUNKS: u64 = 12;

struct NoFloor;

impl FloorSource for NoFloor {
    fn current_floor(&self) -> Result<Option<lunix_format::local::RollbackFloor>> {
        Ok(None)
    }
}

/// The table's mode in a run, and whether the collector compacts: `OFF` and
/// `SHADOW` compact eagerly (threshold 0.9); `ON` is the active mode, and
/// `ON_PLAIN` / `OFF_PLAIN` (the two arms of the equivalence test) compact
/// nothing, so that two runs end with the same segment ids.
const OFF: u8 = 0;
const SHADOW: u8 = 1;
const ON: u8 = 2;
const ON_PLAIN: u8 = 3;
const OFF_PLAIN: u8 = 4;

/// Engine settings for `RETAIN` retained FULLs and one of the modes above.
/// The smallest grace.
fn cfg<const RETAIN: u32, const MODE: u8>(c: &mut EngineConfig) {
    c.gc_budget = Some(GcBudget {
        max_duration: Duration::from_secs(1),
        max_bytes: 1 << 30,
    });
    c.gc_params = Some(GcParams {
        retain_last: RETAIN,
        grace: Duration::from_secs(10),
        read_timeout: Duration::ZERO,
        rate_bytes_per_s: 1 << 30,
        live_threshold: if MODE == ON_PLAIN || MODE == OFF_PLAIN {
            0.001
        } else {
            0.9
        },
        chunk_size: CS,
        id_len: IdLen::Sixteen,
    });
    c.retain_last = RETAIN;
    c.gc_condemn_on_cut = match MODE {
        SHADOW => CondemnOnCut::Shadow,
        ON | ON_PLAIN => CondemnOnCut::On,
        _ => CondemnOnCut::Off,
    };
}

fn tweak(retain: u32, mode: u8) -> fn(&mut EngineConfig) {
    match (retain, mode) {
        (1, OFF) => cfg::<1, OFF>,
        (1, SHADOW) => cfg::<1, SHADOW>,
        (1, ON) => cfg::<1, ON>,
        (1, ON_PLAIN) => cfg::<1, ON_PLAIN>,
        (1, _) => cfg::<1, OFF_PLAIN>,
        (3, OFF) => cfg::<3, OFF>,
        (3, SHADOW) => cfg::<3, SHADOW>,
        (3, ON) => cfg::<3, ON>,
        (3, ON_PLAIN) => cfg::<3, ON_PLAIN>,
        (3, _) => cfg::<3, OFF_PLAIN>,
        (_, OFF) => cfg::<2, OFF>,
        (_, SHADOW) => cfg::<2, SHADOW>,
        (_, ON) => cfg::<2, ON>,
        (_, ON_PLAIN) => cfg::<2, ON_PLAIN>,
        (_, _) => cfg::<2, OFF_PLAIN>,
    }
}

/// The real collector. `GcConfig::tip_refs` is left off on purpose: opening
/// the engine in shadow mode switches it on (one switch), and a table that
/// stayed `Rebuilding` would fail every test here.
fn collector(pipeline: bool) -> Gc {
    collector_every(pipeline, Duration::ZERO)
}

/// The same with `interval` between regular passes.
fn collector_every(pipeline: bool, interval: Duration) -> Gc {
    Gc::new(
        Arc::new(NoFloor),
        GcConfig {
            pass_interval: interval,
            busy_interval: interval,
            pipeline,
            // The configuration C5 ships with: recheck-only passes on (forced
            // here; the engine's pressure does not exist in these tests).
            fast_recheck: pipeline.then_some(Duration::from_secs(1)),
            fast_recheck_always: pipeline,
            ..GcConfig::default()
        },
    )
}

/// What the oracle saw, over one run.
#[derive(Default)]
struct OracleLog {
    /// Offered segments checked against the ground truth.
    checked: AtomicU64,
    /// Offers whose tip was not the tip of the next collector call (not
    /// checked: the roots would be newer than at the offer).
    skipped: AtomicU64,
    /// Offered segments a retained root named, and failures of the walk.
    violations: Mutex<Vec<String>>,
    /// While set, every collector step first moves the manual clock a grace
    /// forward: a requested pass (`run_pass`) waits out its grace in the
    /// calling thread, so the clock has to move by itself, the same way in
    /// every run.
    auto_advance: AtomicBool,
    /// `propose_dead` calls and the most segments one of them was offered.
    proposals: AtomicU64,
    max_offer: AtomicU64,
}

/// The collector under test, wrapped: in every call it first checks the offers
/// the table made right before it (`TestHooks::offers`) against the entries of
/// every retained root (`mark::root_set`: the retained FULLs and the pins),
/// walked through the map. No offered segment may be hinted at by any of them
/// (the lag rule); a table that offers early is caught here even when the
/// mark would have kept the segment alive anyway.
struct Oracle {
    inner: Box<dyn Collector>,
    hooks: Arc<TestHooks>,
    log: Arc<OracleLog>,
    clock: Arc<ManualClock>,
}

impl Oracle {
    fn auto_advance(&self) {
        if self.log.auto_advance.load(Ordering::Relaxed) {
            self.clock.advance(Duration::from_secs(11));
        }
    }

    fn check(&self, v: &GcView<'_>) {
        let offers = std::mem::take(&mut *lock(&self.hooks.offers));
        if offers.is_empty() {
            return;
        }
        let mut named: BTreeSet<SegId> = BTreeSet::new();
        let walked = lunix_gc::mark::root_set(v).and_then(|(roots, _)| {
            for r in roots {
                v.map
                    .walk_leaves_skipping(&r.root, &HashSet::new(), &mut |_, e| {
                        named.insert(e.seg);
                    })?;
            }
            Ok(())
        });
        let mut bad = lock(&self.log.violations);
        if let Err(e) = walked {
            bad.push(format!("the ground-truth walk failed: {e}"));
            return;
        }
        for (tip, segs) in offers {
            if tip != v.tip.commit_seq {
                self.log.skipped.fetch_add(1, Ordering::Relaxed);
                continue;
            }
            self.log
                .checked
                .fetch_add(segs.len() as u64, Ordering::Relaxed);
            for s in segs {
                if named.contains(&s) {
                    bad.push(format!(
                        "{s:?} offered at tip {} but a retained root names it",
                        tip.0
                    ));
                }
            }
        }
    }
}

impl Collector for Oracle {
    fn step(&mut self, v: &GcView<'_>, b: GcBudget) -> Result<GcProgress> {
        self.auto_advance();
        self.check(v);
        self.inner.step(v, b)
    }
    fn urgent(&mut self, v: &GcView<'_>, w: u64) -> Result<GcProgress> {
        self.check(v);
        self.inner.urgent(v, w)
    }
    fn dry_run(&mut self, v: &GcView<'_>) -> Result<GcPassSummary> {
        self.inner.dry_run(v)
    }
    fn stats(&self) -> GcStats {
        self.inner.stats()
    }
    fn request_pass(&mut self) -> bool {
        self.inner.request_pass()
    }
    fn status(&self) -> GcStatusSnapshot {
        self.inner.status()
    }
    fn take_moved(&mut self) -> Vec<Moved> {
        self.inner.take_moved()
    }
    fn set_pressure(&mut self, raised: bool) {
        self.inner.set_pressure(raised);
    }
    fn set_tip_refs(&mut self, on: bool) {
        self.inner.set_tip_refs(on);
    }
    fn set_shadow_judge(&mut self, on: bool) {
        self.inner.set_shadow_judge(on);
    }
    fn take_mark_report(&mut self) -> Option<MarkReport> {
        self.inner.take_mark_report()
    }
    fn propose_dead(&mut self, v: &GcView<'_>, dead: &[DeadSince]) -> bool {
        self.log.proposals.fetch_add(1, Ordering::Relaxed);
        self.log
            .max_offer
            .fetch_max(dead.len() as u64, Ordering::Relaxed);
        self.check(v);
        self.inner.propose_dead(v, dead)
    }
}

fn oracle(v: &Vol, inner: Box<dyn Collector>, log: &Arc<OracleLog>) -> Box<dyn Collector> {
    Box::new(Oracle {
        inner,
        hooks: Arc::clone(&v.hooks),
        log: Arc::clone(log),
        clock: Arc::clone(&v.clock),
    })
}

#[derive(Clone, Copy, Debug)]
enum Op {
    /// A whole chunk of one of the pool's patterns.
    Write(u8, u8),
    /// One block of a chunk (a base read inside the cut).
    Part(u8, u8, u8),
    /// A chunk zeroed.
    Zero(u8),
    Cut,
    Tick,
    /// An urgent pass: a mark without a preceding offer.
    Urgent,
    /// The process dies and the volume is opened again (the table is new).
    Reopen(u8),
    /// A snapshot pin on the newest commit: the table knows the tip only, so
    /// nothing may be offered while it exists, and the retained root it adds
    /// names segments the tip does not once the tip moves on.
    Pin,
    /// The pins are removed.
    Unpin,
    /// A requested pass (`run_pass`): a mark and its deletes that no offer
    /// precedes, entered from the control plane.
    Pass,
    /// The device grows by one chunk (twice per script at most): a cut that
    /// no write asked for, with the map at a new size.
    Grow,
}

fn ops(pool: u8) -> impl Strategy<Value = Vec<Op>> {
    proptest::collection::vec(
        prop_oneof![
            10 => (0..CHUNKS as u8, 0..pool).prop_map(|(c, p)| Op::Write(c, p)),
            6 => (0..CHUNKS as u8, 0..4u8, 0..pool).prop_map(|(c, b, p)| Op::Part(c, b, p)),
            2 => (0..CHUNKS as u8).prop_map(Op::Zero),
            4 => Just(Op::Cut),
            8 => Just(Op::Tick),
            1 => Just(Op::Urgent),
            1 => any::<u8>().prop_map(Op::Reopen),
            2 => Just(Op::Pin),
            2 => Just(Op::Unpin),
            1 => Just(Op::Pass),
            1 => Just(Op::Grow),
        ],
        1..64,
    )
}

/// What a step looked like from outside: the same for every mode of the
/// table when it changes nothing.
#[derive(Clone, PartialEq, Eq, Debug)]
struct Obs {
    segments: BTreeSet<SegId>,
    stats: GcStats,
    condemned_pending: u64,
}

/// What a run exercised, summed over its engines.
#[derive(Clone, Copy, Default, Debug)]
struct Cov {
    dedup_hits: u64,
    compacted: u64,
    offered: u64,
    /// Segments proposals condemned, un-condemned by a recheck, deleted;
    /// offers withheld for a pin.
    condemned: u64,
    uncondemned: u64,
    proposal_deleted: u64,
    pin_blocked: u64,
    /// Ticks that ran with a pin present, and proposals condemned in them
    /// (must be 0).
    pinned_ticks: u64,
    pinned_condemned: u64,
    confirmed: u64,
    rebases: u64,
    reopens: u64,
    urgent: u64,
    /// Requested passes that ran, and grows that happened.
    passes: u64,
    grows: u64,
    /// Offered segments the oracle checked, and offers it could not.
    checked: u64,
    skipped: u64,
}

struct Run {
    obs: Vec<Obs>,
    refs: RefsStats,
    data: Vec<u8>,
    cov: Cov,
}

fn refs_of(r: &Running) -> RefsStats {
    r.engine
        .gc_status_handle()
        .get()
        .refs
        .map(|b| *b)
        .unwrap_or_default()
}

fn observe(r: &Running) -> Obs {
    let st = r.engine.gc_status_handle().get();
    Obs {
        segments: SegmentStore::list(&r.store)
            .expect("list")
            .iter()
            .map(|i| i.seg)
            .collect(),
        stats: st.stats,
        condemned_pending: st.condemned_pending,
    }
}

fn run(
    mk: &dyn Fn() -> Box<dyn Collector>,
    script: &[Op],
    retain: u32,
    mode: u8,
) -> std::result::Result<Run, TestCaseError> {
    let shadow = mode != OFF && mode != OFF_PLAIN;
    // The equivalence arms write data no earlier write had: a flagged
    // segment is never a dedup target, so with repeated data the two modes
    // would store different copies (never wrong data, but other files).
    let unique = mode == ON_PLAIN || mode == OFF_PLAIN;
    let mut v = Vol::create(CS, CHUNKS + 4, 2, JournalMode::Mixed(3));
    v.tweak = tweak(retain, mode);
    let log = Arc::new(OracleLog::default());
    *lock(&v.collector) = Some(oracle(&v, mk(), &log));
    let mut r = v.open().map_err(|e| e.0).expect("open");
    let mut obs = Vec::new();
    let mut cov = Cov::default();
    let mut seed = 5_000u64;
    let cs = u64::from(CS);
    let condemned_now = |r: &Running| r.engine.gc_status_handle().get().reclaim.proposal_condemned;
    let tick = |v: &Vol,
                r: &mut Running,
                obs: &mut Vec<Obs>,
                pinned: bool,
                cov: &mut Cov|
     -> Result<(), TestCaseError> {
        v.clock.advance(Duration::from_secs(11));
        let before = condemned_now(r);
        r.engine
            .commit_now(false)
            .map_err(|e| TestCaseError::fail(format!("cut: {e}")))?;
        r.engine
            .tick()
            .map_err(|e| TestCaseError::fail(format!("tick: {e}")))?;
        if pinned {
            // The pin rule: no proposal is taken while a pin exists.
            cov.pinned_ticks += 1;
            let n = condemned_now(r) - before;
            cov.pinned_condemned += n;
            prop_assert_eq!(n, 0, "a proposal was condemned while a pin existed");
        }
        if mode == ON || mode == ON_PLAIN {
            let rc = r.engine.gc_status_handle().get().reclaim;
            prop_assert_eq!(
                rc.proposal_rejected_early,
                0,
                "the engine's lag and the collector's keep_floor disagree"
            );
            prop_assert_eq!(rc.propose_errors, 0);
            prop_assert_eq!(
                rc.proposal_named,
                0,
                "a proposed segment was named by a retained root (the lag rule)"
            );
        }
        if shadow {
            let t = refs_of(r);
            prop_assert_eq!(t.drift_segments, 0, "the table differed from a mark");
            prop_assert_eq!(t.report_mismatches, 0);
            {
                let bad = lock(&log.violations);
                prop_assert!(bad.is_empty(), "the oracle: {bad:?}");
            }
            prop_assert_eq!(t.shadow_unconfirmed_named, 0, "a mark found an offer named");
        }
        obs.push(observe(r));
        Ok(())
    };
    let account = |r: &Running, cov: &mut Cov, last: Option<&Obs>| {
        let t = refs_of(r);
        cov.offered += t.shadow_would_propose;
        cov.pin_blocked += t.pin_blocked;
        let rc = r.engine.gc_status_handle().get().reclaim;
        cov.condemned += rc.proposal_condemned;
        cov.uncondemned += rc.proposal_uncondemned;
        cov.proposal_deleted += rc.proposal_deleted;
        cov.confirmed += t.shadow_confirmed;
        cov.rebases += t.rebases;
        cov.dedup_hits += r.engine.stats().dedup_hits;
        cov.compacted += last.map_or(0, |o| o.stats.segments_compacted);
    };
    let mut pinned: Vec<lunix_iface::store::PinRoot> = Vec::new();
    let mut grows = 0u32;
    for op in script {
        match *op {
            Op::Pass => {
                // The clock moves with every step of the pass (`Oracle`).
                log.auto_advance.store(true, Ordering::Relaxed);
                let res = GcControl::run_pass(&r.engine, false);
                log.auto_advance.store(false, Ordering::Relaxed);
                prop_assert!(res.is_ok(), "a requested pass failed: {:?}", res.err());
                cov.passes += 1;
                obs.push(observe(&r));
            }
            Op::Grow => {
                if grows < 2 {
                    grows += 1;
                    let to = r.engine.size_bytes() + cs;
                    prop_assert!(r.engine.grow(to).is_ok(), "grow failed");
                    cov.grows += 1;
                    obs.push(observe(&r));
                }
            }
            Op::Pin => {
                let tip = lunix_iface::store::CommitStore::list(&r.store)
                    .expect("commits")
                    .iter()
                    .map(|c| c.seq)
                    .max();
                if let Some(commit_seq) = tip {
                    pinned = vec![lunix_iface::store::PinRoot {
                        kind: lunix_iface::store::PinKind::Snapshot,
                        name: format!("snap-{}", commit_seq.0),
                        commit_seq,
                        deadline: None,
                    }];
                    r.store.set_pins(pinned.clone());
                }
                // The engine reads the pins at most once a second: a pin made
                // within the second of an offer is caught by the collector's
                // own check (and would trip the oracle, which sees the offer
                // first), so the clock moves past the cache.
                v.clock.advance(Duration::from_secs(2));
            }
            Op::Unpin => {
                pinned.clear();
                r.store.set_pins(Vec::new());
                v.clock.advance(Duration::from_secs(2));
            }
            Op::Write(c, p) => {
                seed += 1;
                let d = pattern(if unique { seed } else { u64::from(p) }, CS as usize);
                prop_assert!(r
                    .write(usize::from(c) % 2, u64::from(c) * cs, &d, false)
                    .is_ok());
            }
            Op::Part(c, b, p) => {
                seed += 1;
                // The pool of a dedup-heavy run is small: the same block
                // again; a compaction-heavy one is nearly all distinct.
                let d = pattern(
                    if p < 8 && !unique {
                        900 + u64::from(p)
                    } else {
                        seed
                    },
                    4096,
                );
                let off = u64::from(c) * cs + u64::from(b) * 4096;
                prop_assert!(r.write(usize::from(c) % 2, off, &d, false).is_ok());
            }
            Op::Zero(c) => {
                prop_assert!(r.hole(0, IoOp::WriteZeroes, u64::from(c) * cs, CS).is_ok());
            }
            Op::Cut => {
                r.engine
                    .commit_now(false)
                    .map_err(|e| TestCaseError::fail(format!("cut: {e}")))?;
                obs.push(observe(&r));
            }
            Op::Tick => tick(&v, &mut r, &mut obs, !pinned.is_empty(), &mut cov)?,
            Op::Urgent => {
                v.clock.advance(Duration::from_secs(11));
                // A mark in a call that made no offer. What it returns
                // (nothing to free is not an error here) is the same in
                // both modes.
                let _ = GcControl::run_urgent(&r.engine, 1 << 20);
                cov.urgent += 1;
                obs.push(observe(&r));
            }
            Op::Reopen(s) => {
                account(&r, &mut cov, obs.last());
                cov.reopens += 1;
                let p = r.stop();
                v.absorb(p, u64::from(s));
                *lock(&v.collector) = Some(oracle(&v, mk(), &log));
                r = v.open().map_err(|e| e.0).expect("reopen");
                r.store.set_pins(pinned.clone());
                obs.push(observe(&r));
            }
        }
    }
    // Let the collector finish whatever it started, with the pins gone: the
    // volume converges (the equivalence test compares what is left).
    pinned.clear();
    r.store.set_pins(Vec::new());
    // A recheck mark that finds a segment dead which no earlier mark
    // condemned does not condemn it (a new tip does): new data rolls the
    // retained window past everything, and both arms see the same writes.
    for _ in 0..6 {
        seed += 1;
        let d = pattern(seed, CS as usize);
        prop_assert!(r.write(0, (CHUNKS - 1) * cs, &d, false).is_ok());
        tick(&v, &mut r, &mut obs, false, &mut cov)?;
    }
    for _ in 0..14 {
        tick(&v, &mut r, &mut obs, false, &mut cov)?;
    }
    let data = r.read_all();
    let refs = refs_of(&r);
    account(&r, &mut cov, obs.last());
    cov.checked = log.checked.load(Ordering::Relaxed);
    cov.skipped = log.skipped.load(Ordering::Relaxed);
    Ok(Run {
        obs,
        refs,
        data,
        cov,
    })
}

/// One case: shadow against off.
fn case(
    script: &[Op],
    retain: u32,
    pipeline: bool,
    total: &Cell<Cov>,
) -> std::result::Result<(), TestCaseError> {
    let mk = move || -> Box<dyn Collector> { Box::new(collector(pipeline)) };
    let on = run(&mk, script, retain, SHADOW)?;
    prop_assert_eq!(on.refs.state, RefsState::Ready);
    prop_assert_eq!(on.refs.drift_segments, 0);
    prop_assert!(on.refs.rebases >= 1, "no mark ran");
    // Shadow changes nothing the collector decides.
    let off = run(&mk, script, retain, OFF)?;
    prop_assert_eq!(&on.obs, &off.obs);
    prop_assert!(on.data == off.data);
    prop_assert_eq!(off.refs, RefsStats::default());
    let mut t = total.get();
    t.dedup_hits += on.cov.dedup_hits;
    t.compacted += on.cov.compacted;
    t.offered += on.cov.offered;
    t.confirmed += on.cov.confirmed;
    t.rebases += on.cov.rebases;
    t.reopens += on.cov.reopens;
    t.urgent += on.cov.urgent;
    t.passes += on.cov.passes;
    t.grows += on.cov.grows;
    t.checked += on.cov.checked;
    t.skipped += on.cov.skipped;
    t.pin_blocked += on.cov.pin_blocked;
    t.pinned_ticks += on.cov.pinned_ticks;
    t.pinned_condemned += on.cov.pinned_condemned;
    total.set(t);
    Ok(())
}

/// One case of the active mode: `on` against off. The proposals change
/// when segments are condemned and deleted, never what stays: both runs
/// read the same data and, once the pins are gone and the collector has
/// finished, hold the same segments (nothing compacts, nothing dedups, so
/// the ids are the same). The oracle runs in the `on` arm at every offer;
/// the engine and the collector agree on the lag (no early rejection); no
/// proposal is taken in a tick that has a pin.
fn case_active(
    script: &[Op],
    retain: u32,
    pipeline: bool,
    total: &Cell<Cov>,
) -> std::result::Result<(), TestCaseError> {
    let mk = move || -> Box<dyn Collector> { Box::new(collector(pipeline)) };
    let on = run(&mk, script, retain, ON_PLAIN)?;
    prop_assert_eq!(on.refs.state, RefsState::Ready);
    prop_assert_eq!(on.refs.drift_segments, 0);
    let off = run(&mk, script, retain, OFF_PLAIN)?;
    prop_assert!(on.data == off.data, "the volumes read different data");
    let (a, b) = (on.obs.last(), off.obs.last());
    prop_assert_eq!(
        a.map(|o| (&o.segments, o.condemned_pending)),
        b.map(|o| (&o.segments, o.condemned_pending)),
        "the volumes did not converge to the same files"
    );
    let mut t = total.get();
    t.offered += on.cov.offered;
    t.condemned += on.cov.condemned;
    t.uncondemned += on.cov.uncondemned;
    t.proposal_deleted += on.cov.proposal_deleted;
    t.pin_blocked += on.cov.pin_blocked;
    t.pinned_ticks += on.cov.pinned_ticks;
    t.pinned_condemned += on.cov.pinned_condemned;
    t.rebases += on.cov.rebases;
    t.reopens += on.cov.reopens;
    t.urgent += on.cov.urgent;
    t.passes += on.cov.passes;
    t.grows += on.cov.grows;
    t.checked += on.cov.checked;
    t.skipped += on.cov.skipped;
    total.set(t);
    Ok(())
}

fn cases() -> u32 {
    std::env::var("LUNIX_REFS_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(48)
}

/// The active mode on volumes where ids move (dedup, compaction, restarts,
/// urgent passes, pins): data equal to `off`, the ground-truth oracle at every
/// offer, no early rejection, no lag-rule witness. A wrong proposal is
/// possible here (a chunk id live elsewhere than the entries hint), so
/// `proposal_uncondemned` is counted, not bounded to 0.
fn case_active_moving(
    script: &[Op],
    retain: u32,
    pipeline: bool,
    total: &Cell<Cov>,
) -> std::result::Result<(), TestCaseError> {
    let mk = move || -> Box<dyn Collector> { Box::new(collector(pipeline)) };
    let on = run(&mk, script, retain, ON)?;
    prop_assert_eq!(on.refs.state, RefsState::Ready);
    prop_assert_eq!(on.refs.drift_segments, 0);
    let off = run(&mk, script, retain, OFF)?;
    prop_assert!(on.data == off.data, "the volumes read different data");
    let mut t = total.get();
    t.condemned += on.cov.condemned;
    t.uncondemned += on.cov.uncondemned;
    t.proposal_deleted += on.cov.proposal_deleted;
    t.pin_blocked += on.cov.pin_blocked;
    t.pinned_ticks += on.cov.pinned_ticks;
    t.pinned_condemned += on.cov.pinned_condemned;
    t.dedup_hits += on.cov.dedup_hits;
    t.compacted += on.cov.compacted;
    t.reopens += on.cov.reopens;
    t.urgent += on.cov.urgent;
    t.passes += on.cov.passes;
    t.grows += on.cov.grows;
    t.checked += on.cov.checked;
    t.skipped += on.cov.skipped;
    total.set(t);
    Ok(())
}

fn drive_active_moving(pool: u8) -> Cov {
    let total = Cell::new(Cov::default());
    let mut runner = TestRunner::new(RunnerConfig {
        cases: cases(),
        ..RunnerConfig::default()
    });
    runner
        .run(
            &(ops(pool), 1u32..4, any::<bool>()),
            |(script, retain, pipeline)| case_active_moving(&script, retain, pipeline, &total),
        )
        .unwrap_or_else(|e| panic!("{e}"));
    total.get()
}

#[test]
fn the_active_mode_is_safe_where_ids_move_dedup_and_compaction() {
    let (d, c) = (drive_active_moving(3), drive_active_moving(200));
    eprintln!("active, dedup-heavy: {d:?}\nactive, compaction-heavy: {c:?}");
    assert!(d.dedup_hits > 0 && c.compacted > 0, "{d:?} {c:?}");
    assert!(d.condemned + c.condemned > 0, "{d:?} {c:?}");
    // `uncondemned` (the by-id class: a chunk id live elsewhere than the
    // entries hint) is reported, not required: 1 500 cases of either pool
    // never made one (the compaction re-hints always landed; the class needs
    // a lost re-hint, a crash with moves pending). The collector-level tests
    // and the oracle cover a wrong proposal by offering one.
    for x in [d, c] {
        assert!(x.passes > 0 && x.grows > 0, "{x:?}");
        assert_eq!(x.pinned_condemned, 0, "{x:?}");
        assert!(x.checked > 0 && x.skipped == 0, "{x:?}");
    }
}

fn drive_active() -> Cov {
    let total = Cell::new(Cov::default());
    let mut runner = TestRunner::new(RunnerConfig {
        cases: cases(),
        ..RunnerConfig::default()
    });
    runner
        .run(
            &(ops(200), 1u32..4, any::<bool>()),
            |(script, retain, pipeline)| case_active(&script, retain, pipeline, &total),
        )
        .unwrap_or_else(|e| panic!("{e}"));
    total.get()
}

/// The active mode over random workloads with pins, restarts and urgent
/// passes: the proposals condemn, the rechecks delete, nothing a root reaches
/// is lost, the volumes end with the same files as without the mode, and no
/// proposal is taken while a pin exists.
#[test]
fn the_active_mode_converges_to_the_same_files_and_obeys_the_pin_rule() {
    let c = drive_active();
    eprintln!("active coverage: {c:?}");
    assert!(c.condemned > 0, "no proposal condemned anything: {c:?}");
    assert!(
        c.proposal_deleted > 0,
        "no proposed segment was deleted: {c:?}"
    );
    assert!(c.pinned_ticks > 0, "no tick ran with a pin: {c:?}");
    assert!(c.pin_blocked > 0, "a pin never held an offer back: {c:?}");
    assert_eq!(c.pinned_condemned, 0, "{c:?}");
    assert_eq!(c.uncondemned, 0, "no id moves without compaction: {c:?}");
    assert!(c.reopens > 0 && c.urgent > 0, "{c:?}");
    assert!(c.passes > 0 && c.grows > 0, "{c:?}");
    assert!(c.checked > 0, "the oracle checked no offer: {c:?}");
    assert_eq!(c.skipped, 0, "offers the oracle could not check: {c:?}");
}

fn drive(pool: u8) -> Cov {
    let total = Cell::new(Cov::default());
    let mut runner = TestRunner::new(RunnerConfig {
        cases: cases(),
        ..RunnerConfig::default()
    });
    runner
        .run(
            &(ops(pool), 1u32..4, any::<bool>()),
            |(script, retain, pipeline)| case(&script, retain, pipeline, &total),
        )
        .unwrap_or_else(|e| panic!("{e}"));
    total.get()
}

/// Dedup-heavy: three patterns over twelve chunks, so most chunks share
/// extents and a segment's last reference moves around. Restarts and urgent
/// passes are in the scripts.
#[test]
fn the_table_matches_every_mark_on_a_dedup_heavy_workload() {
    let c = drive(3);
    eprintln!("dedup-heavy coverage: {c:?}");
    assert!(c.dedup_hits > 0, "no dedup hit in any case: {c:?}");
    assert!(c.offered > 0 && c.confirmed > 0, "{c:?}");
    assert!(c.reopens > 0 && c.urgent > 0, "{c:?}");
    assert!(c.passes > 0 && c.grows > 0, "{c:?}");
    assert!(c.checked > 0, "the oracle checked no offer: {c:?}");
    assert_eq!(c.skipped, 0, "offers the oracle could not check: {c:?}");
}

/// Compaction-heavy: mostly partial overwrites of distinct data, eager
/// compaction, so extents move between segments and re-hints land.
#[test]
fn the_table_matches_every_mark_on_a_compaction_heavy_workload() {
    let c = drive(200);
    eprintln!("compaction-heavy coverage: {c:?}");
    assert!(c.compacted > 0, "no compaction in any case: {c:?}");
    assert!(c.offered > 0 && c.confirmed > 0, "{c:?}");
    assert!(c.reopens > 0 && c.urgent > 0, "{c:?}");
    assert!(c.passes > 0 && c.grows > 0, "{c:?}");
    assert!(c.checked > 0, "the oracle checked no offer: {c:?}");
    assert_eq!(c.skipped, 0, "offers the oracle could not check: {c:?}");
}

/// A collector that reports one entry too many in the first segment of
/// every mark: the table must notice.
struct Skewed(Gc);

impl Collector for Skewed {
    fn step(&mut self, v: &GcView<'_>, b: GcBudget) -> Result<GcProgress> {
        Collector::step(&mut self.0, v, b)
    }
    fn urgent(&mut self, v: &GcView<'_>, w: u64) -> Result<GcProgress> {
        Collector::urgent(&mut self.0, v, w)
    }
    fn dry_run(&mut self, v: &GcView<'_>) -> Result<GcPassSummary> {
        Collector::dry_run(&mut self.0, v)
    }
    fn stats(&self) -> GcStats {
        Collector::stats(&self.0)
    }
    fn request_pass(&mut self) -> bool {
        Collector::request_pass(&mut self.0)
    }
    fn status(&self) -> GcStatusSnapshot {
        Collector::status(&self.0)
    }
    fn take_moved(&mut self) -> Vec<Moved> {
        Collector::take_moved(&mut self.0)
    }
    fn set_tip_refs(&mut self, on: bool) {
        Collector::set_tip_refs(&mut self.0, on);
    }
    fn set_shadow_judge(&mut self, on: bool) {
        Collector::set_shadow_judge(&mut self.0, on);
    }
    fn take_mark_report(&mut self) -> Option<MarkReport> {
        let mut r = Collector::take_mark_report(&mut self.0)?;
        for f in &mut r.finished {
            if let Some(e) = f.refs.refs.first_mut() {
                e.1 += 1;
            }
        }
        Some(r)
    }
}

#[test]
fn a_wrong_mark_report_is_found_counted_and_repaired() {
    let mut v = Vol::create(CS, CHUNKS + 4, 2, JournalMode::Mixed(3));
    v.tweak = tweak(2, SHADOW);
    *lock(&v.collector) = Some(Box::new(Skewed(collector(false))));
    let mut r = v.open().map_err(|e| e.0).expect("open");
    for c in 0..4u64 {
        let d = pattern(c, CS as usize);
        assert!(r.write(0, c * u64::from(CS), &d, false).is_ok());
    }
    let mut last = RefsStats::default();
    for i in 0..8u64 {
        // New data each round: a new tip, so a new pass and mark.
        let d = pattern(100 + i, CS as usize);
        assert!(r.write(0, (i % 4) * u64::from(CS), &d, false).is_ok());
        v.clock.advance(Duration::from_secs(11));
        r.engine.commit_now(false).expect("cut");
        r.engine.tick().expect("tick");
        last = refs_of(&r);
    }
    assert!(last.rebases >= 2, "{last:?}");
    // The first mark is the base; every later one finds the skew.
    assert!(last.drift_segments >= 1, "{last:?}");
}

/// S7: one switch. The collector is built without `tip_refs`; opening the
/// engine in shadow mode makes the table reach `Ready` anyway, and with the
/// mode off nothing exists.
#[test]
fn shadow_mode_switches_the_collectors_counting_on_by_itself() {
    let mut v = Vol::create(CS, CHUNKS + 4, 2, JournalMode::Mixed(3));
    v.tweak = tweak(2, SHADOW);
    *lock(&v.collector) = Some(Box::new(collector(false)));
    let mut r = v.open().map_err(|e| e.0).expect("open");
    assert!(r.write(0, 0, &pattern(1, CS as usize), false).is_ok());
    for _ in 0..3 {
        v.clock.advance(Duration::from_secs(11));
        r.engine.commit_now(false).expect("cut");
        r.engine.tick().expect("tick");
    }
    let t = refs_of(&r);
    assert_eq!(t.state, RefsState::Ready, "{t:?}");
    assert!(t.rebases >= 1, "{t:?}");

    let mut v = Vol::create(CS, CHUNKS + 4, 2, JournalMode::Mixed(3));
    v.tweak = tweak(2, OFF);
    *lock(&v.collector) = Some(Box::new(collector(false)));
    let mut r = v.open().map_err(|e| e.0).expect("open");
    assert!(r.write(0, 0, &pattern(1, CS as usize), false).is_ok());
    for _ in 0..3 {
        v.clock.advance(Duration::from_secs(11));
        r.engine.commit_now(false).expect("cut");
        r.engine.tick().expect("tick");
    }
    assert!(
        r.engine.gc_status_handle().get().refs.is_none(),
        "no table with the mode off"
    );
    // Shadow without a collector: nothing to compare with, nothing kept.
    let mut v = Vol::create(CS, CHUNKS + 4, 2, JournalMode::Mixed(3));
    v.tweak = tweak(2, SHADOW);
    let mut r = v.open().map_err(|e| e.0).expect("open");
    assert!(r.write(0, 0, &pattern(1, CS as usize), false).is_ok());
    r.engine.commit_now(false).expect("cut");
    assert!(r.engine.gc_status_handle().get().refs.is_none());
}

/// A fixed overwrite workload with dedup, compaction, a restart and an
/// urgent pass in it. A smoke test that the counters move and that the
/// table matches every mark; the figures say nothing about a real volume.
#[test]
fn the_shadow_check_offers_what_the_marks_condemn() {
    let mut script = Vec::new();
    for round in 0..14u8 {
        for c in 0..CHUNKS as u8 {
            script.push(Op::Write(c, (round + c) % 3));
        }
        for c in 0..4u8 {
            script.push(Op::Part(c, round % 4, 200 + round));
        }
        script.push(Op::Cut);
        script.push(Op::Tick);
        script.push(Op::Cut);
        script.push(Op::Tick);
        script.push(Op::Cut);
        script.push(Op::Tick);
        if round == 5 {
            script.push(Op::Urgent);
        }
        if round == 9 {
            script.push(Op::Reopen(3));
        }
    }
    let mk = || -> Box<dyn Collector> { Box::new(collector(true)) };
    let on = run(&mk, &script, 2, SHADOW).expect("run");
    let off = run(&mk, &script, 2, OFF).expect("run");
    assert_eq!(on.obs, off.obs);
    let t = on.refs;
    eprintln!("{t:?} {:?}", on.cov);
    assert_eq!(t.state, RefsState::Ready);
    assert_eq!(t.drift_segments, 0);
    assert!(on.cov.offered > 0 && on.cov.confirmed > 0, "{:?}", on.cov);
    assert!(
        on.cov.dedup_hits > 0 && on.cov.compacted > 0,
        "{:?}",
        on.cov
    );
    assert_eq!((on.cov.reopens, on.cov.urgent), (1, 1), "{:?}", on.cov);
    assert!(
        on.cov.checked > 0,
        "the oracle checked no offer: {:?}",
        on.cov
    );
    assert_eq!(t.shadow_unconfirmed_named, 0);
    // Bytes: every confirmed, early or missed segment has a size.
    assert!(t.shadow_confirmed_bytes > 0, "{t:?}");
    assert!(t.shadow_confirmed > 0 && t.shadow_confirmed_bytes >= t.shadow_confirmed);
    let last = on.obs.last().expect("obs");
    assert!(last.stats.bytes_freed > 0, "the collector deleted nothing");
}

/// The cut counters of the gate: cuts that drained journal bytes, their bytes
/// and their time, cumulative (the cost per journal byte is then exact, not
/// sampled). A cut that drains nothing is not counted.
#[test]
fn the_cut_counters_sum_the_cuts_that_drained_journal_bytes() {
    let mut v = Vol::create(CS, CHUNKS + 4, 2, JournalMode::Mixed(3));
    v.tweak = tweak(2, OFF);
    *lock(&v.collector) = Some(Box::new(collector(false)));
    let mut r = v.open().map_err(|e| e.0).expect("open");
    let s0 = r.engine.cut_stats();
    assert_eq!((s0.cuts, s0.cut_bytes_sum, s0.cut_us_sum), (0, 0, 0));
    let mut last = s0;
    let mut bytes = 0;
    for i in 0..3u64 {
        let d = pattern(40 + i, CS as usize);
        assert!(r.write(0, i * u64::from(CS), &d, false).is_ok());
        r.engine.commit_now(false).expect("cut");
        let s = r.engine.cut_stats();
        assert_eq!(s.cuts, last.cuts + 1, "one more cut");
        assert!(s.cut_bytes_sum > last.cut_bytes_sum);
        assert_eq!(s.cut_bytes_sum, bytes + s.last_cut_bytes);
        bytes = s.cut_bytes_sum;
        assert!(s.cut_us_sum >= last.cut_us_sum);
        last = s;
    }
    // Nothing to drain: a cut that commits without journal bytes (a forced
    // FULL) is not in the sums.
    r.engine.commit_now(true).expect("forced cut");
    let s = r.engine.cut_stats();
    assert_eq!((s.cuts, s.cut_bytes_sum), (last.cuts, last.cut_bytes_sum));
}

/// What a discard flood left: the files and the data, plus the figures the
/// report reads (`LUNIX_REFS_FLOOD` raises the size; they print then).
struct Flooded {
    files: BTreeSet<SegId>,
    data: Vec<u8>,
}

/// The number of chunks, each in a segment of its own, a flood test discards
/// in one request.
fn flood_size() -> u64 {
    std::env::var("LUNIX_REFS_FLOOD")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(2_000)
}

/// 2.4: `n` chunks in `n` segments, all named by retained commits, then one
/// request zeroes all of them; the collector is ticked between cuts until the
/// segments are gone.
///
/// `slow` is the offer phase as the daemon lives it: regular passes 30 s
/// apart and a tick every 100 ms, so only proposals act for 30 s after the
/// flood; the function then reports the cost of the proposals over that
/// window and returns.
fn flood(mode: u8, n: u64, slow: bool) -> Option<Flooded> {
    let on = mode == ON_PLAIN;
    let gc = if slow {
        collector_every(true, Duration::from_secs(30))
    } else {
        collector(true)
    };
    // A segment target below one chunk: every chunk is a segment of its own.
    let mut v = Vol::create_sized(
        CS,
        n + 4,
        2,
        JournalMode::Mixed(3),
        lunix_testkit::PlainCodec::new(),
        IdLen::Sixteen,
        1,
    );
    v.tweak = tweak(2, mode);
    let log = Arc::new(OracleLog::default());
    *lock(&v.collector) = Some(oracle(&v, Box::new(gc), &log));
    let mut r = v.open().map_err(|e| e.0).expect("open");
    let cs = u64::from(CS);
    // A forced FULL: the tip moves without a write, so the lag runs out.
    let tick_by = |v: &Vol, r: &mut Running, by: Duration| {
        v.clock.advance(by);
        r.engine.commit_now(true).expect("cut");
        r.engine.tick().expect("tick");
    };
    let tick = |v: &Vol, r: &mut Running| tick_by(v, r, Duration::from_secs(11));
    // The first mark is the table's base.
    for _ in 0..3 {
        tick(&v, &mut r);
    }
    // Sixty-four chunks per cut: a cut's journal bytes stay small, and the
    // index (which names one run per cut) stays under its limit.
    for i in 0..n {
        let d = pattern(7_000 + i, CS as usize);
        assert!(r.write(0, i * cs, &d, false).is_ok(), "write {i}");
        if i % 64 == 63 {
            r.engine.commit_now(false).expect("cut");
        }
    }
    r.engine.commit_now(false).expect("cut");
    for _ in 0..3 {
        tick(&v, &mut r);
    }
    let before = observe(&r).segments.len() as u64;
    assert!(before >= n, "{before} segments for {n} chunks");
    let t0 = refs_of(&r);
    if on {
        assert_eq!(t0.state, RefsState::Ready, "{t0:?}");
        assert!(t0.segments >= n, "{t0:?}");
    }
    let rc0 = r.engine.gc_status_handle().get().reclaim;
    let len = u32::try_from(n * cs).expect("the flood fits one request");
    assert!(r.hole(0, IoOp::WriteZeroes, 0, len).is_ok(), "the discard");
    if slow {
        let started = std::time::Instant::now();
        let mut peak = 0u64;
        for _ in 0..300 {
            tick_by(&v, &mut r, Duration::from_millis(100));
            peak = peak.max(refs_of(&r).zero_waiting);
        }
        let rc1 = r.engine.gc_status_handle().get().reclaim;
        let t1 = refs_of(&r);
        let calls = rc1.propose_calls - rc0.propose_calls;
        let us = rc1.propose_us_sum - rc0.propose_us_sum;
        eprintln!(
            "flood {n}, a tick every 100 ms and passes 30 s apart, first 30 s after the discard: \
             propose calls {calls}, propose_us_sum {us} (share of 30 s: {:.3} %), mean {:.0} us, \
             max {} us; condemned by proposals {}, deleted {}; zero_waiting peak {peak}, \
             at the end {}; the in-process wall time of the window was {:.2} s",
            us as f64 / 30e6 * 100.0,
            us as f64 / calls.max(1) as f64,
            rc1.propose_us_max,
            rc1.proposal_condemned - rc0.proposal_condemned,
            rc1.proposal_deleted - rc0.proposal_deleted,
            t1.zero_waiting,
            started.elapsed().as_secs_f64(),
        );
        assert!(
            log.max_offer.load(Ordering::Relaxed) <= 64,
            "a proposal was offered {} segments",
            log.max_offer.load(Ordering::Relaxed)
        );
        return None;
    }
    // The flood's cut, then `lag + 1` more with the collector ticked between
    // them, then until the zeros are all gone.
    let (mut ticks, mut max_waiting) = (0u32, 0u64);
    let started = std::time::Instant::now();
    loop {
        tick(&v, &mut r);
        ticks += 1;
        let t = refs_of(&r);
        max_waiting = max_waiting.max(t.zero_waiting);
        if ticks >= 3 && (!on || t.zero_waiting == 0) {
            break;
        }
        assert!(ticks < 200, "the zeros are not draining: {t:?}");
    }
    let offer_wall = started.elapsed();
    let rc1 = r.engine.gc_status_handle().get().reclaim;
    let t1 = refs_of(&r);
    if on {
        // The coverage witness: the flood existed.
        assert!(max_waiting >= n / 2, "zero_waiting peaked at {max_waiting}");
        assert_eq!(t1.zero_waiting, 0);
        assert_eq!(t1.segments, t0.segments - n, "{t0:?} {t1:?}");
        assert!(
            log.max_offer.load(Ordering::Relaxed) <= 64,
            "a proposal was offered {} segments (OFFER_MAX is 64)",
            log.max_offer.load(Ordering::Relaxed)
        );
        assert!(log.proposals.load(Ordering::Relaxed) > 0);
        let calls = rc1.propose_calls - rc0.propose_calls;
        let us = rc1.propose_us_sum - rc0.propose_us_sum;
        if std::env::var("LUNIX_REFS_FLOOD").is_ok() {
            eprintln!(
                "flood {n}: {ticks} ticks until the zeros drained, offer phase {:.2} s wall; \
                 propose calls {calls}, propose_us_sum {us}, mean {:.0} us, max {} us; \
                 condemned by proposals {}, deleted {}; zero_waiting peak {max_waiting}",
                offer_wall.as_secs_f64(),
                us as f64 / calls.max(1) as f64,
                rc1.propose_us_max,
                rc1.proposal_condemned - rc0.proposal_condemned,
                rc1.proposal_deleted - rc0.proposal_deleted,
            );
        }
    }
    // Let the collector finish: the grace of what it condemned runs out and
    // the segments go.
    let (mut same, mut last) = (0u32, observe(&r).segments);
    for _ in 0..600 {
        tick(&v, &mut r);
        let now = observe(&r);
        if now.segments == last {
            same += 1;
            if same >= 10 {
                break;
            }
        } else {
            same = 0;
            last = now.segments;
        }
    }
    let done = observe(&r);
    assert!(
        same >= 10,
        "the segments were still changing after 600 ticks"
    );
    let rc = r.engine.gc_status_handle().get().reclaim;
    let t = refs_of(&r);
    if on {
        assert_eq!(t.drift_segments, 0, "{t:?}");
        assert_eq!(t.report_mismatches, 0, "{t:?}");
        assert_eq!(rc.proposal_rejected_early, 0);
        assert_eq!(rc.propose_errors, 0);
        assert_eq!(rc.proposal_named, 0);
        let bad = lock(&log.violations);
        assert!(bad.is_empty(), "the oracle: {bad:?}");
    }
    let data = r.read_all();
    assert!(
        data.iter().all(|&b| b == 0),
        "the device does not read zero"
    );
    assert!(
        (done.segments.len() as u64) < before,
        "nothing was deleted: {} of {before}",
        done.segments.len()
    );
    Some(Flooded {
        files: done.segments,
        data,
    })
}

/// 2.4: a discard of N chunks in one request: bounded offers, the zeros are
/// condemned and drained, the table's size falls by N, nothing drifts, the
/// device reads zero and the files are those of the same script with the mode
/// off.
#[test]
fn a_discard_flood_is_offered_in_bounded_calls_drains_and_leaves_the_files_of_mode_off() {
    let n = flood_size();
    let on = flood(ON_PLAIN, n, false).expect("the run");
    let off = flood(OFF_PLAIN, n, false).expect("the run");
    assert!(on.data == off.data, "the volumes read different data");
    assert_eq!(on.files, off.files, "the volumes ended with other files");
}

/// 2.4, the cost of the offer phase as the daemon lives it (the figure the
/// report reads; set `LUNIX_REFS_FLOOD`, else it does nothing): the share of
/// the coordinator's time the proposals take in the 30 s after a flood.
#[test]
fn the_offer_phase_after_a_flood_costs_the_coordinator_little() {
    if std::env::var("LUNIX_REFS_FLOOD").is_err() {
        return;
    }
    flood(ON_PLAIN, flood_size(), true);
}
