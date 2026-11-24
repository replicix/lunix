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
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::*;
use lunix_engine::{CondemnOnCut, EngineConfig};
use lunix_format::{IdLen, SegId};
use lunix_gc::{FloorSource, Gc, GcConfig};
use lunix_iface::block::{BlockEngine, IoOp};
use lunix_iface::gc::{
    Collector, GcBudget, GcControl, GcParams, GcPassSummary, GcProgress, GcStats, GcStatusSnapshot,
    GcView, MarkCondemned, Moved, RefsState, RefsStats, TipRefs,
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

/// Engine settings for `RETAIN` retained FULLs; the table in shadow mode or
/// off. Eager compaction (threshold 0.9) and the smallest grace.
fn cfg<const RETAIN: u32, const SHADOW: bool>(c: &mut EngineConfig) {
    c.gc_budget = Some(GcBudget {
        max_duration: Duration::from_secs(1),
        max_bytes: 1 << 30,
    });
    c.gc_params = Some(GcParams {
        retain_last: RETAIN,
        grace: Duration::from_secs(10),
        read_timeout: Duration::ZERO,
        rate_bytes_per_s: 1 << 30,
        live_threshold: 0.9,
        chunk_size: CS,
        id_len: IdLen::Sixteen,
    });
    c.retain_last = RETAIN;
    if SHADOW {
        c.gc_condemn_on_cut = CondemnOnCut::Shadow;
    }
}

fn tweak(retain: u32, shadow: bool) -> fn(&mut EngineConfig) {
    match (retain, shadow) {
        (1, false) => cfg::<1, false>,
        (1, true) => cfg::<1, true>,
        (3, false) => cfg::<3, false>,
        (3, true) => cfg::<3, true>,
        (_, false) => cfg::<2, false>,
        (_, true) => cfg::<2, true>,
    }
}

/// The real collector. `GcConfig::tip_refs` is left off on purpose: opening
/// the engine in shadow mode switches it on (one switch), and a table that
/// stayed `Rebuilding` would fail every test here.
fn collector(pipeline: bool) -> Gc {
    Gc::new(
        Arc::new(NoFloor),
        GcConfig {
            pass_interval: Duration::ZERO,
            busy_interval: Duration::ZERO,
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
}

impl Oracle {
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
                    bad.push(format!("{s:?} offered at tip {} but a retained root names it", tip.0));
                }
            }
        }
    }
}

impl Collector for Oracle {
    fn step(&mut self, v: &GcView<'_>, b: GcBudget) -> Result<GcProgress> {
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
    fn take_mark_started(&mut self) -> Option<lunix_format::CommitSeq> {
        self.inner.take_mark_started()
    }
    fn take_tip_refs(&mut self) -> Option<TipRefs> {
        self.inner.take_tip_refs()
    }
    fn take_mark_condemned(&mut self) -> Option<MarkCondemned> {
        self.inner.take_mark_condemned()
    }
}

fn oracle(v: &Vol, inner: Box<dyn Collector>, log: &Arc<OracleLog>) -> Box<dyn Collector> {
    Box::new(Oracle {
        inner,
        hooks: Arc::clone(&v.hooks),
        log: Arc::clone(log),
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
    confirmed: u64,
    rebases: u64,
    reopens: u64,
    urgent: u64,
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
    shadow: bool,
) -> std::result::Result<Run, TestCaseError> {
    let mut v = Vol::create(CS, CHUNKS + 4, 2, JournalMode::Mixed(3));
    v.tweak = tweak(retain, shadow);
    let log = Arc::new(OracleLog::default());
    *lock(&v.collector) = Some(oracle(&v, mk(), &log));
    let mut r = v.open().map_err(|e| e.0).expect("open");
    let mut obs = Vec::new();
    let mut cov = Cov::default();
    let mut seed = 5_000u64;
    let cs = u64::from(CS);
    let tick = |v: &Vol, r: &mut Running, obs: &mut Vec<Obs>| -> Result<(), TestCaseError> {
        v.clock.advance(Duration::from_secs(11));
        r.engine
            .commit_now(false)
            .map_err(|e| TestCaseError::fail(format!("cut: {e}")))?;
        r.engine
            .tick()
            .map_err(|e| TestCaseError::fail(format!("tick: {e}")))?;
        if shadow {
            let t = refs_of(r);
            prop_assert_eq!(t.drift_segments, 0, "the table differed from a mark");
            prop_assert_eq!(t.report_mismatches, 0);
            prop_assert_eq!(t.shadow_unconfirmed_named, 0, "a mark found an offer named");
            let bad = lock(&log.violations);
            prop_assert!(bad.is_empty(), "the oracle: {bad:?}");
        }
        obs.push(observe(r));
        Ok(())
    };
    let account = |r: &Running, cov: &mut Cov, last: Option<&Obs>| {
        let t = refs_of(r);
        cov.offered += t.shadow_would_propose;
        cov.confirmed += t.shadow_confirmed;
        cov.rebases += t.rebases;
        cov.dedup_hits += r.engine.stats().dedup_hits;
        cov.compacted += last.map_or(0, |o| o.stats.segments_compacted);
    };
    for op in script {
        match *op {
            Op::Write(c, p) => {
                let d = pattern(u64::from(p), CS as usize);
                prop_assert!(r
                    .write(usize::from(c) % 2, u64::from(c) * cs, &d, false)
                    .is_ok());
            }
            Op::Part(c, b, p) => {
                seed += 1;
                // The pool of a dedup-heavy run is small: the same block
                // again; a compaction-heavy one is nearly all distinct.
                let d = pattern(if p < 8 { 900 + u64::from(p) } else { seed }, 4096);
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
            Op::Tick => tick(&v, &mut r, &mut obs)?,
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
                obs.push(observe(&r));
            }
        }
    }
    // Let the collector finish whatever it started.
    for _ in 0..8 {
        tick(&v, &mut r, &mut obs)?;
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
    let on = run(&mk, script, retain, true)?;
    prop_assert_eq!(on.refs.state, RefsState::Ready);
    prop_assert_eq!(on.refs.drift_segments, 0);
    prop_assert!(on.refs.rebases >= 1, "no mark ran");
    // Shadow changes nothing the collector decides.
    let off = run(&mk, script, retain, false)?;
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
    fn take_mark_started(&mut self) -> Option<lunix_format::CommitSeq> {
        Collector::take_mark_started(&mut self.0)
    }
    fn take_tip_refs(&mut self) -> Option<TipRefs> {
        let mut t = Collector::take_tip_refs(&mut self.0)?;
        if let Some(e) = t.refs.first_mut() {
            e.1 += 1;
        }
        Some(t)
    }
    fn take_mark_condemned(&mut self) -> Option<MarkCondemned> {
        Collector::take_mark_condemned(&mut self.0)
    }
}

#[test]
fn a_wrong_mark_report_is_found_counted_and_repaired() {
    let mut v = Vol::create(CS, CHUNKS + 4, 2, JournalMode::Mixed(3));
    v.tweak = tweak(2, true);
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
    v.tweak = tweak(2, true);
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
    v.tweak = tweak(2, false);
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
    v.tweak = tweak(2, true);
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
    let on = run(&mk, &script, 2, true).expect("run");
    let off = run(&mk, &script, 2, false).expect("run");
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
    assert!(on.cov.checked > 0, "the oracle checked no offer: {:?}", on.cov);
    assert_eq!(t.shadow_unconfirmed_named, 0);
    // Bytes: every confirmed, early or missed segment has a size.
    assert!(t.shadow_confirmed_bytes > 0, "{t:?}");
    assert!(t.shadow_confirmed > 0 && t.shadow_confirmed_bytes >= t.shadow_confirmed);
    let last = on.obs.last().expect("obs");
    assert!(last.stats.bytes_freed > 0, "the collector deleted nothing");
}
