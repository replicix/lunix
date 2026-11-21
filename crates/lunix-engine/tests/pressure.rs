//! GC pressure from the engine tick (ADR-32 T4; the note §5.6 and §9.2): the
//! collector gets four times the step budget, the compaction rate times
//! `gc_pressure_rate_factor` (uncapped at the limit) and the floor grace
//! exactly while the gate is not `open`, and the normal figures again 5 s
//! after it opened. A recording collector sees what each step was handed; the
//! test gate (`MemSpace`) reports the throttle state the test scripts.

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::*;
use lunix_engine::{CommitOutcome, Pressure};
use lunix_iface::gc::{
    Collector, GcBudget, GcParams, GcPassSummary, GcProgress, GcState, GcStats, GcView,
};
use lunix_iface::space::ThrottleState;
use lunix_iface::Result;
use lunix_testkit::JournalMode;

const CS: u32 = 16 << 10;
const MIB: u64 = 1 << 20;

type Seen = Arc<Mutex<Vec<(GcBudget, GcParams)>>>;

/// Records the budget and the params of every step; never has work.
struct Recorder(Seen);

impl Collector for Recorder {
    fn step(&mut self, view: &GcView<'_>, b: GcBudget) -> Result<GcProgress> {
        self.0.lock().unwrap().push((b, view.params));
        Ok(GcProgress {
            moved: Vec::new(),
            freed: 0,
            state: GcState::Idle,
            done: true,
            cut_wanted: false,
        })
    }

    fn urgent(&mut self, view: &GcView<'_>, _want: u64) -> Result<GcProgress> {
        self.0.lock().unwrap().push((
            GcBudget {
                max_duration: Duration::ZERO,
                max_bytes: 0,
            },
            view.params,
        ));
        Ok(GcProgress {
            moved: Vec::new(),
            freed: 0,
            state: GcState::Idle,
            done: true,
            cut_wanted: false,
        })
    }

    fn dry_run(&mut self, _v: &GcView<'_>) -> Result<GcPassSummary> {
        Ok(GcPassSummary::default())
    }

    fn stats(&self) -> GcStats {
        GcStats::default()
    }
}

fn gc_on(cfg: &mut lunix_engine::EngineConfig) {
    cfg.gc_budget = Some(GcBudget {
        max_duration: Duration::from_millis(50),
        max_bytes: MIB,
    });
    cfg.gc_params = Some(GcParams {
        retain_last: 2,
        grace: Duration::from_secs(60),
        read_timeout: Duration::from_secs(10),
        rate_bytes_per_s: 64 * MIB,
        live_threshold: 0.5,
        chunk_size: CS,
        id_len: lunix_format::IdLen::Sixteen,
    });
    cfg.gc_pressure_rate_factor = 4;
}

fn factor_16(cfg: &mut lunix_engine::EngineConfig) {
    gc_on(cfg);
    cfg.gc_pressure_rate_factor = 16;
}

fn short_grace(cfg: &mut lunix_engine::EngineConfig) {
    gc_on(cfg);
    if let Some(p) = &mut cfg.gc_params {
        p.grace = Duration::from_secs(20);
    }
}

fn setup_with(tweak: fn(&mut lunix_engine::EngineConfig)) -> (Vol, Running, Seen) {
    let mut v = Vol::create(CS, 64, 1, JournalMode::Mixed(5));
    v.tweak = tweak;
    let seen: Seen = Arc::default();
    *lock(&v.collector) = Some(Box::new(Recorder(Arc::clone(&seen))));
    let r = v.open().map_err(|e| e.0).expect("open");
    (v, r, seen)
}

fn setup() -> (Vol, Running, Seen) {
    setup_with(gc_on)
}

/// Ticks once and returns what the step of that tick was handed.
fn step(r: &Running, seen: &Seen) -> (GcBudget, GcParams) {
    let before = seen.lock().unwrap().len();
    assert_eq!(r.engine.tick().expect("tick"), CommitOutcome::Idle);
    let s = seen.lock().unwrap();
    assert_eq!(s.len(), before + 1, "one collector step per idle tick");
    s[before]
}

fn normal(b: GcBudget, p: GcParams) {
    assert_eq!(b.max_duration, Duration::from_millis(50));
    assert_eq!(b.max_bytes, MIB);
    assert_eq!(p.rate_bytes_per_s, 64 * MIB);
    assert_eq!(p.grace, Duration::from_secs(60));
}

#[test]
fn the_collector_is_boosted_exactly_while_the_gate_is_not_open() {
    let (v, r, seen) = setup();
    assert_eq!(r.engine.gc_pressure(), Pressure::Normal);
    let (b, p) = step(&r, &seen);
    normal(b, p);

    // Slowdown: 4 x the budget, 4 x the rate, the floor grace (10 s + 2 x
    // read_timeout = 30 s). Nothing else of the params changes.
    r.space.set_throttle_state(ThrottleState::Slowdown);
    let (b, p) = step(&r, &seen);
    assert_eq!(r.engine.gc_pressure(), Pressure::Raised);
    assert_eq!(b.max_duration, Duration::from_millis(200));
    assert_eq!(b.max_bytes, 4 * MIB);
    assert_eq!(p.rate_bytes_per_s, 256 * MIB);
    assert_eq!(p.grace, Duration::from_secs(30));
    assert_eq!(p.retain_last, 2);
    assert_eq!(p.read_timeout, Duration::from_secs(10));
    assert!((p.live_threshold - 0.5).abs() < f64::EPSILON);

    // Stop and full: the same budget and grace, the rate uncapped.
    for s in [ThrottleState::Stop, ThrottleState::Full] {
        r.space.set_throttle_state(s);
        let (b, p) = step(&r, &seen);
        assert_eq!(r.engine.gc_pressure(), Pressure::Urgent, "{s:?}");
        assert_eq!(b.max_duration, Duration::from_millis(200));
        assert_eq!(p.rate_bytes_per_s, u64::MAX);
        assert_eq!(p.grace, Duration::from_secs(30));
    }

    // Open again: the boost holds for 5 s, then the normal figures.
    r.space.set_throttle_state(ThrottleState::Open);
    let (b, p) = step(&r, &seen);
    assert_eq!(
        r.engine.gc_pressure(),
        Pressure::Urgent,
        "held after the stop"
    );
    assert_eq!(b.max_duration, Duration::from_millis(200));
    assert_eq!(p.grace, Duration::from_secs(30));
    v.clock.advance(Duration::from_millis(4999));
    let (b, _) = step(&r, &seen);
    assert_eq!(
        b.max_duration,
        Duration::from_millis(200),
        "4.999 s: still held"
    );
    v.clock.advance(Duration::from_millis(1));
    let (b, p) = step(&r, &seen);
    assert_eq!(
        r.engine.gc_pressure(),
        Pressure::Normal,
        "5 s after it opened"
    );
    normal(b, p);
}

#[test]
fn a_slowdown_inside_the_hold_starts_it_again() {
    let (v, r, seen) = setup();
    r.space.set_throttle_state(ThrottleState::Slowdown);
    step(&r, &seen);
    r.space.set_throttle_state(ThrottleState::Open);
    step(&r, &seen);
    v.clock.advance(Duration::from_secs(4));
    r.space.set_throttle_state(ThrottleState::Slowdown);
    step(&r, &seen);
    r.space.set_throttle_state(ThrottleState::Open);
    v.clock.advance(Duration::from_secs(1));
    step(&r, &seen);
    v.clock.advance(Duration::from_secs(4));
    let (b, _) = step(&r, &seen);
    assert_eq!(
        b.max_duration,
        Duration::from_millis(200),
        "4 s into the second hold"
    );
    v.clock.advance(Duration::from_secs(1));
    let (b, p) = step(&r, &seen);
    normal(b, p);
}

#[test]
fn the_rate_factor_is_the_daemon_key_and_the_grace_never_grows() {
    let (_v, r, seen) = setup_with(factor_16);
    r.space.set_throttle_state(ThrottleState::Slowdown);
    let (_, p) = step(&r, &seen);
    assert_eq!(p.rate_bytes_per_s, 1024 * MIB);

    // A configured grace shorter than the floor is kept.
    let (_v, r, seen) = setup_with(short_grace);
    r.space.set_throttle_state(ThrottleState::Slowdown);
    let (_, p) = step(&r, &seen);
    assert_eq!(p.grace, Duration::from_secs(20));
}

#[test]
fn the_urgent_pass_is_handed_the_boosted_params_under_pressure() {
    let (_v, r, seen) = setup();
    r.space.set_throttle_state(ThrottleState::Stop);
    step(&r, &seen);
    let n = seen.lock().unwrap().len();
    // The ladder's urgent pass (what the gate's compact step calls).
    let _ = lunix_iface::gc::GcControl::run_urgent(&r.engine, 1 << 20);
    let s = seen.lock().unwrap();
    assert!(s.len() > n, "the urgent pass ran");
    let (_, p) = s[n];
    assert_eq!(p.rate_bytes_per_s, u64::MAX);
    assert_eq!(p.grace, Duration::from_secs(30));
}

#[test]
fn a_volume_resting_in_the_band_with_no_writer_loses_its_pressure() {
    // Review m4: the state is a function of the headroom, so a volume whose
    // data sits above the soft mark stays `slowdown` for ever; with no
    // writer the boost ends after the hold all the same.
    let (v, r, seen) = setup();
    r.space.set_throttle_state(ThrottleState::Slowdown);
    let (b, _) = step(&r, &seen);
    assert_eq!(b.max_duration, Duration::from_millis(200));
    r.space.set_throttle_idle(true);
    let (b, _) = step(&r, &seen);
    assert_eq!(r.engine.gc_pressure(), Pressure::Raised, "held");
    assert_eq!(b.max_duration, Duration::from_millis(200));
    v.clock.advance(Duration::from_millis(4999));
    step(&r, &seen);
    assert_eq!(r.engine.gc_pressure(), Pressure::Raised);
    v.clock.advance(Duration::from_millis(1));
    let (b, p) = step(&r, &seen);
    assert_eq!(r.engine.gc_pressure(), Pressure::Normal);
    normal(b, p);
    // A writer again: raised at once.
    r.space.set_throttle_idle(false);
    step(&r, &seen);
    assert_eq!(r.engine.gc_pressure(), Pressure::Raised);
}

#[test]
fn in_the_band_the_collector_is_sized_to_the_ingest_and_grows_through_the_band() {
    // Milestone gate. 100 MiB/s of ingest, soft mark 1000 MiB, write
    // amplification 2.0 by default: the compaction rate is the ingest times
    // that at the soft mark (200 MiB/s, under the 4 x 64 = 256 MiB/s floor),
    // 1.5 times at the middle of the band and twice at the limit; the step
    // budget follows (never below the floors).
    let (_v, r, seen) = setup();
    r.space.set_throttle_state(ThrottleState::Slowdown);
    // The floor still rules at the soft mark: 200 < 256.
    r.space
        .set_throttle_figures(100 * MIB, 999 * MIB, 1000 * MIB);
    let (b, p) = step(&r, &seen);
    assert_eq!(p.rate_bytes_per_s, 256 * MIB);
    assert_eq!(b.max_bytes, 4 * MIB);
    // 400 MiB/s of ingest: 800 MiB/s at the soft mark, 1200 in the middle,
    // 1600 at the limit: the rate and the budget are sized, not the floors.
    r.space
        .set_throttle_figures(400 * MIB, 999 * MIB, 1000 * MIB);
    let (b, p) = step(&r, &seen);
    assert!(p.rate_bytes_per_s.abs_diff(800 * MIB) <= 2 * MIB, "{p:?}");
    assert_eq!(b.max_bytes, 13 * MIB, "ceil(800 / 64) x the 1 MiB base");
    assert_eq!(b.max_duration, Duration::from_millis(50 * 8), "time capped");
    r.space
        .set_throttle_figures(400 * MIB, 500 * MIB, 1000 * MIB);
    let (b, p) = step(&r, &seen);
    assert_eq!(p.rate_bytes_per_s, 1200 * MIB);
    assert_eq!(b.max_bytes, 19 * MIB);
    r.space.set_throttle_figures(400 * MIB, 0, 1000 * MIB);
    let (b, p) = step(&r, &seen);
    assert_eq!(p.rate_bytes_per_s, 1600 * MIB);
    assert_eq!(b.max_bytes, 25 * MIB);
    // Stop: uncapped rate, budget still sized.
    r.space.set_throttle_state(ThrottleState::Stop);
    let (b, p) = step(&r, &seen);
    assert_eq!(p.rate_bytes_per_s, u64::MAX);
    assert_eq!(b.max_bytes, 25 * MIB);
}

#[test]
fn the_write_amplification_is_the_tunable_of_the_sizing() {
    fn amp_100(cfg: &mut lunix_engine::EngineConfig) {
        gc_on(cfg);
        cfg.gc_write_amp_percent = 100;
    }
    let (_v, r, seen) = setup_with(amp_100);
    r.space.set_throttle_state(ThrottleState::Slowdown);
    r.space.set_throttle_figures(400 * MIB, 0, 1000 * MIB);
    let (_, p) = step(&r, &seen);
    // 400 x 1.0 x 2 at the limit.
    assert_eq!(p.rate_bytes_per_s, 800 * MIB);
}

#[test]
fn the_sizing_ends_with_the_pressure() {
    let (v, r, seen) = setup();
    r.space.set_throttle_state(ThrottleState::Slowdown);
    r.space.set_throttle_figures(400 * MIB, 0, 1000 * MIB);
    step(&r, &seen);
    r.space.set_throttle_state(ThrottleState::Open);
    r.space
        .set_throttle_figures(400 * MIB, 5000 * MIB, 1000 * MIB);
    v.clock.advance(Duration::from_secs(5));
    step(&r, &seen);
    v.clock.advance(Duration::from_secs(5));
    let (b, p) = step(&r, &seen);
    normal(b, p);
}
