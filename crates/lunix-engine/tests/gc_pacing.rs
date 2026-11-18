//! The coordinator's GC pacing (milestone gcrate, re-review M-2): the share
//! a busy collector gets after a cut (`gc_share`: a third of the cut, at
//! most 1 s, ended by a clock that stands still or a step with no work), and
//! the pacing of cuts made for compaction moves alone (under writes while a
//! pass is busy: at most one a second; otherwise at once).
//!
//! A scripted collector moves the test's manual clock inside its steps, so
//! the cut's length and each step's length are what the test says.

mod common;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use common::*;
use lunix_engine::CommitOutcome;
use lunix_iface::clock::ManualClock;
use lunix_iface::gc::{
    Collector, GcBudget, GcParams, GcPassSummary, GcProgress, GcState, GcStats, GcView, Moved,
};
use lunix_iface::map::{entry_hint, ChunkNo};
use lunix_iface::Result;
use lunix_testkit::JournalMode;

const CS: u32 = 16 << 10;

/// What the scripted collector does; shared with the test.
#[derive(Default)]
struct Script {
    /// Steps taken.
    steps: AtomicU64,
    /// Clock advance of the next step only (ms), then cleared.
    next_ms: AtomicU64,
    /// Clock advance of every other step (ms).
    each_ms: AtomicU64,
    /// Report work left (`Deleting`, not done) instead of an idle collector.
    busy: AtomicBool,
    /// Move chunk 0 to a new segment at the next step.
    move_next: AtomicBool,
}

struct Scripted {
    script: Arc<Script>,
    clock: Arc<ManualClock>,
}

impl Collector for Scripted {
    fn step(&mut self, view: &GcView<'_>, _b: GcBudget) -> Result<GcProgress> {
        let s = &self.script;
        s.steps.fetch_add(1, Ordering::SeqCst);
        let once = s.next_ms.swap(0, Ordering::SeqCst);
        let ms = if once > 0 {
            once
        } else {
            s.each_ms.load(Ordering::SeqCst)
        };
        self.clock.advance(Duration::from_millis(ms));
        let mut moved = Vec::new();
        if s.move_next.swap(false, Ordering::SeqCst) {
            let e = view.map.get(ChunkNo(0))?.expect("chunk 0");
            let old = entry_hint(&e);
            let mut bytes = Vec::new();
            view.segments.read_extent(&old, &mut bytes)?;
            let id = view
                .index
                .get_by_prefix(&e.prefix)?
                .iter()
                .find(|c| c.hint == old)
                .expect("indexed")
                .id;
            let mut w = view.segments.open_segment()?;
            let new = w.append_extent(&id, None, &bytes)?;
            w.seal(None)?;
            view.index.relocate(&id, &old, &new)?;
            moved.push(Moved { id, old, new });
        }
        let busy = s.busy.load(Ordering::SeqCst);
        Ok(GcProgress {
            moved,
            freed: 0,
            state: if busy {
                GcState::Deleting
            } else {
                GcState::Idle
            },
            done: !busy,
            cut_wanted: false,
        })
    }

    fn urgent(&mut self, _v: &GcView<'_>, _want: u64) -> Result<GcProgress> {
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
        max_bytes: 1 << 20,
    });
    cfg.gc_params = Some(GcParams {
        retain_last: 2,
        grace: Duration::from_secs(60),
        read_timeout: Duration::from_secs(10),
        rate_bytes_per_s: 64 << 20,
        live_threshold: 0.5,
        chunk_size: CS,
        id_len: lunix_format::IdLen::Sixteen,
    });
    cfg.commit_interval = Duration::from_secs(30);
}

fn setup() -> (Vol, Running, Arc<Script>) {
    let mut v = Vol::create(CS, 64, 1, JournalMode::Mixed(5));
    v.tweak = gc_on;
    let script = Arc::new(Script::default());
    *lock(&v.collector) = Some(Box::new(Scripted {
        script: Arc::clone(&script),
        clock: Arc::clone(&v.clock),
    }));
    let mut r = v.open().map_err(|e| e.0).expect("open");
    assert!(r.write(0, 0, &pattern(1, CS as usize), false).is_ok());
    r.engine.commit_now(false).expect("commit");
    (v, r, script)
}

/// Steps the collector took after the cut a tick made, with the cut taking
/// `cut_ms` (the first step, the cut's own C10 step, moves the clock) and
/// every later step `step_ms`.
fn share_steps(cut_ms: u64, step_ms: u64, busy: bool) -> u64 {
    let (v, mut r, script) = setup();
    assert!(r
        .write(0, u64::from(CS), &pattern(2, CS as usize), false)
        .is_ok());
    // A tick sees the volume dirty (the commit interval counts from here),
    // then the interval is over: the next tick cuts.
    assert_eq!(r.engine.tick().expect("tick"), CommitOutcome::Idle);
    v.clock.advance(Duration::from_secs(31));
    script.busy.store(busy, Ordering::SeqCst);
    script.next_ms.store(cut_ms, Ordering::SeqCst);
    script.each_ms.store(step_ms, Ordering::SeqCst);
    let before = script.steps.load(Ordering::SeqCst);
    assert!(matches!(
        r.engine.tick().expect("tick"),
        CommitOutcome::Committed { .. }
    ));
    // Minus the cut's own step at C10.
    script.steps.load(Ordering::SeqCst) - before - 1
}

#[test]
fn a_busy_collector_steps_for_a_third_of_the_cut_after_it() {
    // A 3 s cut: 1 s of share, 100 ms per step.
    let n = share_steps(3000, 100, true);
    assert!((9..=11).contains(&n), "{n} steps after a 3 s cut");
    // A 600 ms cut: 200 ms.
    let n = share_steps(600, 100, true);
    assert!((1..=3).contains(&n), "{n} steps after a 600 ms cut");
}

#[test]
fn the_share_is_at_most_one_second() {
    // A 30 s cut: still 1 s of share, not 10 s.
    let n = share_steps(30_000, 100, true);
    assert!((9..=11).contains(&n), "{n} steps after a 30 s cut");
}

#[test]
fn a_clock_that_stands_still_ends_the_share() {
    // The steps take no time on the clock: the share ends after a few.
    let n = share_steps(3000, 0, true);
    assert!(
        (1..=8).contains(&n),
        "{n} steps with a clock standing still"
    );
}

#[test]
fn a_collector_with_no_work_gets_no_share() {
    assert_eq!(share_steps(3000, 100, false), 0);
}

#[test]
fn moves_cut_at_once_on_an_idle_volume() {
    let (_v, r, script) = setup();
    script.busy.store(true, Ordering::SeqCst);
    script.move_next.store(true, Ordering::SeqCst);
    // The move is made at this idle tick's step; nothing written since.
    assert_eq!(r.engine.tick().expect("tick"), CommitOutcome::Idle);
    assert!(
        matches!(
            r.engine.tick().expect("tick"),
            CommitOutcome::Committed { .. }
        ),
        "an idle volume's moves wait"
    );
}

#[test]
fn moves_under_writes_cut_once_a_second_while_a_pass_is_busy() {
    let (v, mut r, script) = setup();
    script.busy.store(true, Ordering::SeqCst);
    script.move_next.store(true, Ordering::SeqCst);
    assert_eq!(r.engine.tick().expect("tick"), CommitOutcome::Idle);
    // A write newer than the tip: the volume is being written.
    assert!(r
        .write(0, u64::from(CS), &pattern(3, CS as usize), false)
        .is_ok());
    v.clock.advance(Duration::from_millis(500));
    assert_eq!(
        r.engine.tick().expect("tick"),
        CommitOutcome::Idle,
        "a move cut within a second of the last cut, under writes"
    );
    v.clock.advance(Duration::from_millis(600));
    assert!(
        matches!(
            r.engine.tick().expect("tick"),
            CommitOutcome::Committed { .. }
        ),
        "no move cut a second after the last cut"
    );
    // With the pass done, moves cut at once again, writes or not.
    script.busy.store(false, Ordering::SeqCst);
    assert!(r
        .write(0, 2 * u64::from(CS), &pattern(4, CS as usize), false)
        .is_ok());
    script.move_next.store(true, Ordering::SeqCst);
    assert_eq!(r.engine.tick().expect("tick"), CommitOutcome::Idle);
    assert!(matches!(
        r.engine.tick().expect("tick"),
        CommitOutcome::Committed { .. }
    ));
}
