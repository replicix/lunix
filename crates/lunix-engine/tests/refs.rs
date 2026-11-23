//! The engine's reference table in shadow mode (milestone refs): over random
//! workloads with the real collector, the table matches every mark exactly
//! (`drift_segments == 0`), and shadow mode changes no decision of the
//! collector (the segment set and the collector's counters after every step
//! equal those of a run with the mode off).

mod common;

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use common::*;
use lunix_engine::{CondemnOnCut, EngineConfig};
use lunix_format::{IdLen, SegId};
use lunix_gc::{FloorSource, Gc, GcConfig};
use lunix_iface::block::IoOp;
use lunix_iface::gc::{
    Collector, GcBudget, GcParams, GcPassSummary, GcProgress, GcStats, GcStatusSnapshot, GcView,
    MarkCondemned, Moved, RefsState, RefsStats, TipRefs,
};
use lunix_iface::store::SegmentStore;
use lunix_iface::Result;
use lunix_testkit::JournalMode;
use proptest::prelude::*;

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

fn collector(pipeline: bool, tip_refs: bool) -> Gc {
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
            tip_refs,
            ..GcConfig::default()
        },
    )
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
}

fn ops(pool: u8) -> impl Strategy<Value = Vec<Op>> {
    proptest::collection::vec(
        prop_oneof![
            5 => (0..CHUNKS as u8, 0..pool).prop_map(|(c, p)| Op::Write(c, p)),
            3 => (0..CHUNKS as u8, 0..4u8, 0..pool).prop_map(|(c, b, p)| Op::Part(c, b, p)),
            1 => (0..CHUNKS as u8).prop_map(Op::Zero),
            2 => Just(Op::Cut),
            3 => Just(Op::Tick),
        ],
        1..48,
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

struct Run {
    obs: Vec<Obs>,
    refs: RefsStats,
    data: Vec<u8>,
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
    c: Box<dyn Collector>,
    script: &[Op],
    retain: u32,
    shadow: bool,
) -> std::result::Result<Run, TestCaseError> {
    let mut v = Vol::create(CS, CHUNKS + 4, 2, JournalMode::Mixed(3));
    v.tweak = tweak(retain, shadow);
    *lock(&v.collector) = Some(c);
    let mut r = v.open().map_err(|e| e.0).expect("open");
    let mut obs = Vec::new();
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
            let t = r.engine.gc_status_handle().get().refs;
            prop_assert_eq!(t.drift_segments, 0, "the table differed from a mark");
            prop_assert_eq!(t.report_mismatches, 0);
        }
        obs.push(observe(r));
        Ok(())
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
        }
    }
    // Let the collector finish whatever it started.
    for _ in 0..8 {
        tick(&v, &mut r, &mut obs)?;
    }
    let data = r.read_all();
    let refs = *r.engine.gc_status_handle().get().refs;
    Ok(Run { obs, refs, data })
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 24, ..ProptestConfig::default() })]

    /// Dedup-heavy: three patterns over twelve chunks, so most chunks share
    /// extents and a segment's last reference moves around.
    #[test]
    fn the_table_matches_every_mark_on_a_dedup_heavy_workload(
        script in ops(3), retain in 1u32..4, pipeline in any::<bool>()
    ) {
        let on = run(Box::new(collector(pipeline, true)), &script, retain, true)?;
        prop_assert_eq!(on.refs.state, RefsState::Ready);
        prop_assert_eq!(on.refs.drift_segments, 0);
        prop_assert!(on.refs.rebases >= 1, "no mark ran");
        // Shadow changes nothing the collector decides.
        let off = run(Box::new(collector(pipeline, false)), &script, retain, false)?;
        prop_assert_eq!(&on.obs, &off.obs);
        prop_assert!(on.data == off.data);
        prop_assert_eq!(off.refs, RefsStats::default());
    }

    /// Compaction-heavy: mostly partial overwrites of distinct data, eager
    /// compaction, so extents move between segments and re-hints land.
    #[test]
    fn the_table_matches_every_mark_on_a_compaction_heavy_workload(
        script in ops(200), retain in 1u32..4, pipeline in any::<bool>()
    ) {
        let on = run(Box::new(collector(pipeline, true)), &script, retain, true)?;
        prop_assert_eq!(on.refs.state, RefsState::Ready);
        prop_assert_eq!(on.refs.drift_segments, 0);
        let off = run(Box::new(collector(pipeline, false)), &script, retain, false)?;
        prop_assert_eq!(&on.obs, &off.obs);
        prop_assert!(on.data == off.data);
    }
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
fn a_wrong_count_is_found_counted_and_repaired() {
    let mut v = Vol::create(CS, CHUNKS + 4, 2, JournalMode::Mixed(3));
    v.tweak = tweak(2, true);
    *lock(&v.collector) = Some(Box::new(Skewed(collector(false, true))));
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
        last = *r.engine.gc_status_handle().get().refs;
    }
    assert!(last.rebases >= 2, "{last:?}");
    // The first mark is the base; every later one finds the skew.
    assert!(last.drift_segments >= 1, "{last:?}");
}

#[test]
fn the_table_does_not_exist_with_the_mode_off_or_without_a_collector() {
    let mut v = Vol::create(CS, CHUNKS + 4, 2, JournalMode::Mixed(3));
    v.tweak = tweak(2, false);
    *lock(&v.collector) = Some(Box::new(collector(false, true)));
    let mut r = v.open().map_err(|e| e.0).expect("open");
    assert!(r.write(0, 0, &pattern(1, CS as usize), false).is_ok());
    for _ in 0..3 {
        v.clock.advance(Duration::from_secs(11));
        r.engine.commit_now(false).expect("cut");
        r.engine.tick().expect("tick");
    }
    assert_eq!(
        *r.engine.gc_status_handle().get().refs,
        RefsStats::default()
    );
    // Shadow without a collector: nothing to compare with, nothing kept.
    let mut v = Vol::create(CS, CHUNKS + 4, 2, JournalMode::Mixed(3));
    v.tweak = tweak(2, true);
    let mut r = v.open().map_err(|e| e.0).expect("open");
    assert!(r.write(0, 0, &pattern(1, CS as usize), false).is_ok());
    r.engine.commit_now(false).expect("cut");
    assert_eq!(
        *r.engine.gc_status_handle().get().refs,
        RefsStats::default()
    );
}

/// A fixed overwrite workload with dedup and compaction in it: the table
/// offers what the marks then condemn, and nothing differs.
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
    }
    let on = run(Box::new(collector(true, true)), &script, 2, true).expect("run");
    let off = run(Box::new(collector(true, false)), &script, 2, false).expect("run");
    assert_eq!(on.obs, off.obs);
    let t = on.refs;
    eprintln!("{t:?}");
    assert_eq!(t.state, RefsState::Ready);
    assert_eq!(t.drift_segments, 0);
    assert!(t.shadow_would_propose > 0, "{t:?}");
    assert!(t.shadow_confirmed > 0, "{t:?}");
    assert!(t.rebases >= 5, "{t:?}");
    let last = on.obs.last().expect("obs");
    assert!(last.stats.bytes_freed > 0, "the collector deleted nothing");
}
