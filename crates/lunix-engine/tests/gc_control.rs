//! The engine's GC wiring (plan M12 step 12, M12c): compaction re-hints run
//! after the destage and share the map's dirty-leaf cap with it, so they are
//! applied in batches of leaves that fit, and moves left over trigger a cut
//! on their own only once per commit interval (M12c re-review M-1).

mod common;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use common::*;
use lunix_engine::CommitOutcome;
use lunix_format::commit::Commit;
use lunix_format::Hint;
use lunix_iface::crypto::CommitCodec;
use lunix_iface::gc::{
    Collector, GcBudget, GcControl, GcParams, GcPassSummary, GcProgress, GcState, GcStats, GcView,
    Moved,
};
use lunix_iface::map::{entry_hint, BlockMap, ChunkNo};
use lunix_iface::store::CommitStore;
use lunix_iface::Result;
use lunix_testkit::JournalMode;

const CS: u32 = 16 << 10;
/// Chunks per map leaf in the test map (`lunix_testkit::map::LEAF_CHUNKS`).
const LEAF: u64 = 512;

/// Once armed, moves the given chunks into one new segment.
struct LeafMover {
    chunks: Vec<u64>,
    armed: Arc<AtomicBool>,
}

impl Collector for LeafMover {
    fn step(&mut self, view: &GcView<'_>, _b: GcBudget) -> Result<GcProgress> {
        let mut moved = Vec::new();
        if self.armed.swap(false, Ordering::SeqCst) {
            let mut w = view.segments.open_segment()?;
            let mut todo = Vec::new();
            for &c in &self.chunks {
                let e = view.map.get(ChunkNo(c))?.expect("chunk written");
                let old = entry_hint(&e);
                let mut bytes = Vec::new();
                view.segments.read_extent(&old, &mut bytes)?;
                let id = view
                    .index
                    .get_by_prefix(&e.prefix)?
                    .into_iter()
                    .find(|x| x.hint == old)
                    .expect("indexed")
                    .id;
                let new = w.append_extent(&id, None, &bytes)?;
                todo.push(Moved { id, old, new });
            }
            w.seal(None)?;
            for m in &todo {
                view.index.relocate(&m.id, &m.old, &m.new)?;
            }
            moved = todo;
        }
        Ok(GcProgress {
            moved,
            freed: 0,
            state: GcState::Idle,
            done: true,
            cut_wanted: false,
        })
    }
    fn urgent(&mut self, _v: &GcView<'_>, _w: u64) -> Result<GcProgress> {
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
    cfg.max_leaves_per_cut = 2;
}

/// The hint of chunk `c` in the tip's map.
fn tip_hint(v: &Vol, r: &Running, c: u64) -> Hint {
    let tip = CommitStore::tip(&r.store).unwrap().unwrap();
    let bytes = CommitStore::get(&r.store, tip).unwrap();
    let f = match v.codec.open_commit(tip, &bytes).unwrap() {
        Commit::Full(f) => f,
        _ => panic!("FULL"),
    };
    let ext: Arc<dyn lunix_iface::crypto::ExtentCodec> = Arc::new(v.codec);
    let packs: Arc<dyn lunix_iface::store::PackRead> = Arc::new(r.store.clone());
    let m = lunix_testkit::FlatMap::open(ext, packs, &f.map_root, f.size / u64::from(CS), v.id_len)
        .unwrap();
    entry_hint(&m.get(ChunkNo(c)).unwrap().expect("chunk"))
}

#[test]
fn rehints_that_do_not_fit_the_dirty_leaf_cap_go_in_batches_not_all_deferred() {
    let cs = u64::from(CS);
    let mut v = Vol::create(CS, 4 * LEAF, 1, JournalMode::Mixed(13));
    v.dirty_cap = Some(2);
    v.tweak = gc_on;
    let armed = Arc::new(AtomicBool::new(false));
    *lock(&v.collector) = Some(Box::new(LeafMover {
        chunks: vec![0, LEAF, 2 * LEAF],
        armed: Arc::clone(&armed),
    }));
    let mut r = v.open().map_err(|e| e.0).expect("open");
    for (i, c) in [0, LEAF, 2 * LEAF].into_iter().enumerate() {
        assert!(r
            .write(0, c * cs, &pattern(i as u64 + 1, CS as usize), false)
            .is_ok());
    }
    // Three leaves exceed the cap of 2: the cut is retried smaller.
    while r.engine.commit_now(false).expect("commit") != CommitOutcome::Idle {}
    let old: Vec<Hint> = [0, LEAF, 2 * LEAF]
        .iter()
        .map(|&c| tip_hint(&v, &r, c))
        .collect();

    // An idle tick steps the collector: three moves in three leaves.
    armed.store(true, Ordering::SeqCst);
    assert_eq!(r.engine.tick().expect("tick"), CommitOutcome::Idle);

    // A cut that dirties a fourth leaf leaves room for one more leaf: one
    // leaf of re-hints goes in (before the fix: none, all deferred).
    assert!(r
        .write(0, 3 * LEAF * cs, &pattern(9, CS as usize), false)
        .is_ok());
    assert!(matches!(
        r.engine.commit_now(false).expect("commit"),
        CommitOutcome::Committed { .. }
    ));
    let now: Vec<Hint> = [0, LEAF, 2 * LEAF]
        .iter()
        .map(|&c| tip_hint(&v, &r, c))
        .collect();
    assert_ne!(now[0], old[0], "the first leaf's move was applied");
    assert_eq!(now[1..], old[1..], "the others wait");

    // The moves left over trigger a cut on their own only once per commit
    // interval (each such cut walks the tip's leaves).
    assert_eq!(r.engine.tick().expect("tick"), CommitOutcome::Idle);
    v.clock.advance(Duration::from_secs(60));
    assert!(matches!(
        r.engine.tick().expect("tick"),
        CommitOutcome::Committed { .. }
    ));
    for (i, &c) in [0, LEAF, 2 * LEAF].iter().enumerate() {
        assert_ne!(tip_hint(&v, &r, c), old[i], "chunk {c} re-hinted");
        assert_eq!(
            r.read(0, c * cs, CS).1,
            pattern(i as u64 + 1, CS as usize),
            "chunk {c} reads back"
        );
    }
    assert_eq!(r.engine.tick().expect("tick"), CommitOutcome::Idle);
    assert!(!r.engine.is_stopped());
}

// ------------------------------------------- handover of failed calls (M-1)

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    /// `step` seals a move, then fails once.
    FailStep,
    /// `urgent` seals a move, then fails once.
    FailUrgent,
    /// `urgent` asks for one cut.
    UrgentCut,
}

/// A collector for the handover paths of `GcControl`: moves of a failed
/// call (`take_moved`) and a cut asked for (`cut_wanted`).
struct Handover {
    mode: Mode,
    done: bool,
    /// A pass was asked for: only then does `FailStep` fail (the background
    /// steps of the setup's commits do nothing).
    requested: bool,
    outbox: Vec<Moved>,
}

fn idle_progress(cut_wanted: bool) -> GcProgress {
    GcProgress {
        moved: Vec::new(),
        freed: 0,
        state: GcState::Idle,
        done: true,
        cut_wanted,
    }
}

impl Handover {
    /// Copies chunk 0 into a new segment and relocates it; the move goes to
    /// the outbox, as a collector holds it until its step returns.
    fn move_chunk0(&mut self, view: &GcView<'_>) -> Result<()> {
        let e = view.map.get(ChunkNo(0))?.expect("chunk 0");
        let old = entry_hint(&e);
        let mut bytes = Vec::new();
        view.segments.read_extent(&old, &mut bytes)?;
        let id = view
            .index
            .get_by_prefix(&e.prefix)?
            .into_iter()
            .find(|x| x.hint == old)
            .expect("indexed")
            .id;
        let mut w = view.segments.open_segment()?;
        let new = w.append_extent(&id, None, &bytes)?;
        w.seal(None)?;
        view.index.relocate(&id, &old, &new)?;
        self.outbox.push(Moved { id, old, new });
        Ok(())
    }

    fn fail_once(&mut self, view: &GcView<'_>, mode: Mode) -> Result<GcProgress> {
        if self.mode == mode && !self.done {
            self.done = true;
            self.move_chunk0(view)?;
            return Err(lunix_iface::Error::Io(std::io::Error::other(
                "the call failed after sealing its output",
            )));
        }
        Ok(idle_progress(false))
    }
}

impl Collector for Handover {
    fn step(&mut self, view: &GcView<'_>, _b: GcBudget) -> Result<GcProgress> {
        if !self.requested {
            return Ok(idle_progress(false));
        }
        self.fail_once(view, Mode::FailStep)
    }
    fn urgent(&mut self, view: &GcView<'_>, _w: u64) -> Result<GcProgress> {
        if self.mode == Mode::UrgentCut {
            return Ok(idle_progress(!std::mem::replace(&mut self.done, true)));
        }
        self.fail_once(view, Mode::FailUrgent)
    }
    fn dry_run(&mut self, _v: &GcView<'_>) -> Result<GcPassSummary> {
        Ok(GcPassSummary::default())
    }
    fn stats(&self) -> GcStats {
        GcStats::default()
    }
    fn request_pass(&mut self) -> bool {
        self.requested = true;
        true
    }
    fn take_moved(&mut self) -> Vec<Moved> {
        std::mem::take(&mut self.outbox)
    }
}

fn handover_vol(mode: Mode) -> (Vol, Running) {
    let mut v = Vol::create(CS, 64, 1, JournalMode::Mixed(13));
    v.tweak = gc_on;
    *lock(&v.collector) = Some(Box::new(Handover {
        mode,
        done: false,
        requested: false,
        outbox: Vec::new(),
    }));
    let mut r = v.open().map_err(|e| e.0).expect("open");
    assert!(r.write(0, 0, &pattern(5, CS as usize), false).is_ok());
    assert!(matches!(
        r.engine.commit_now(false).expect("commit"),
        CommitOutcome::Committed { .. }
    ));
    assert_eq!(
        r.engine.commit_now(false).expect("idle"),
        CommitOutcome::Idle
    );
    (v, r)
}

fn tip_seq(r: &Running) -> lunix_format::CommitSeq {
    CommitStore::tip(&r.store).unwrap().unwrap()
}

fn tip_names(v: &Vol, r: &Running) -> Vec<lunix_format::SegId> {
    let tip = tip_seq(r);
    let bytes = CommitStore::get(&r.store, tip).unwrap();
    match v.codec.open_commit(tip, &bytes).unwrap() {
        Commit::Full(f) => f.new_segments,
        _ => panic!("FULL"),
    }
}

/// The move a failed call sealed reaches the next cut: that cut names the
/// output and re-hints chunk 0, once; the next commit has nothing to do.
fn assert_moved_once(v: &Vol, r: &mut Running) {
    let old = tip_hint(v, r, 0);
    assert!(matches!(
        r.engine
            .commit_now(false)
            .expect("the cut the move asks for"),
        CommitOutcome::Committed { .. }
    ));
    let new = tip_hint(v, r, 0);
    assert_ne!(new, old, "chunk 0 re-hinted");
    assert!(tip_names(v, r).contains(&new.seg), "the output is named");
    assert_eq!(
        r.engine.commit_now(false).expect("idle"),
        CommitOutcome::Idle
    );
    assert_eq!(r.read(0, 0, CS).1, pattern(5, CS as usize));
}

#[test]
fn moves_of_a_failed_urgent_pass_reach_the_next_cut() {
    let (v, mut r) = handover_vol(Mode::FailUrgent);
    assert!(GcControl::run_urgent(&r.engine, 1 << 20).is_err());
    assert_moved_once(&v, &mut r);
}

#[test]
fn moves_of_a_failed_step_of_a_requested_pass_reach_the_next_cut() {
    let (v, mut r) = handover_vol(Mode::FailStep);
    assert!(GcControl::run_pass(&r.engine, false).is_err());
    assert_moved_once(&v, &mut r);
}

#[test]
fn a_cut_asked_for_by_an_urgent_pass_is_one_full_on_an_idle_volume() {
    let (_v, r) = handover_vol(Mode::UrgentCut);
    let before = tip_seq(&r);
    assert_eq!(
        GcControl::run_urgent(&r.engine, 1 << 20).expect("urgent"),
        0
    );
    assert!(matches!(
        r.engine.tick().expect("tick"),
        CommitOutcome::Committed { .. }
    ));
    assert_eq!(tip_seq(&r).0, before.0 + 1, "exactly one FULL");
    assert_eq!(r.engine.tick().expect("tick"), CommitOutcome::Idle);
    assert_eq!(
        GcControl::run_urgent(&r.engine, 1 << 20).expect("urgent"),
        0
    );
    assert_eq!(r.engine.tick().expect("tick"), CommitOutcome::Idle);
    assert_eq!(tip_seq(&r).0, before.0 + 1, "no FULL without a request");
}
