//! The index and its ledger after the GC review (request 5, N13) and M12b F2:
//! the `Index` charge is trued up against the ledger, a failed `persist`
//! blocks the next FULL, the catch-up is persisted at open, and a compaction
//! output is named even when its re-hints could not be applied.

mod common;

use std::sync::atomic::Ordering;
use std::time::Duration;

use common::*;
use lunix_engine::CommitOutcome;
use lunix_format::commit::Commit;
use lunix_iface::crypto::CommitCodec;
use lunix_iface::gc::{
    Collector, GcBudget, GcParams, GcPassSummary, GcProgress, GcState, GcStats, GcView, Moved,
};
use lunix_iface::index::ChunkIndex;
use lunix_iface::map::{entry_hint, ChunkNo};
use lunix_iface::space::{SpaceGate, SpaceKind};
use lunix_iface::store::CommitStore;
use lunix_iface::Result;
use lunix_testkit::JournalMode;

const CS: u32 = 16 << 10;

fn vol() -> Vol {
    Vol::create(CS, 64, 2, JournalMode::Mixed(13))
}

fn run_bytes(r: &Running) -> u64 {
    ChunkIndex::all_runs(&*r.index)
        .expect("runs")
        .iter()
        .map(|x| x.size_bytes)
        .sum()
}

fn index_charge(r: &Running) -> u64 {
    r.space.snapshot().by_kind[SpaceKind::Index.index()]
}

fn tip_full(v: &Vol, r: &Running) -> lunix_format::commit::Full {
    let tip = CommitStore::tip(&r.store).unwrap().unwrap();
    let bytes = CommitStore::get(&r.store, tip).unwrap();
    match v.codec.open_commit(tip, &bytes).unwrap() {
        Commit::Full(f) => f,
        _ => panic!("FULL"),
    }
}

fn commit_with_data(r: &mut Running, seed: u64) -> std::result::Result<CommitOutcome, String> {
    assert!(r
        .write(
            0,
            (seed % 32) * u64::from(CS),
            &pattern(seed, CS as usize),
            false
        )
        .is_ok());
    r.engine.commit_now(false).map_err(|e| e.to_string())
}

#[test]
fn the_index_charge_follows_the_ledger_after_gc_deleted_a_run() {
    let v = vol();
    let mut r = v.open().map_err(|e| e.0).expect("open");
    commit_with_data(&mut r, 1).expect("commit");
    commit_with_data(&mut r, 2).expect("commit");
    assert_eq!(index_charge(&r), run_bytes(&r));
    // What GC does with a run neither the manifest nor a retained FULL names
    // (iface `space` walk-through step 6): delete it, release its bytes.
    let run = ChunkIndex::all_runs(&*r.index).expect("runs")[0];
    let freed = ChunkIndex::delete_run(&*r.index, run).expect("delete");
    assert!(freed > 0);
    r.space.release(SpaceKind::Index, freed);
    assert_eq!(index_charge(&r), run_bytes(&r));
    // The next commits true up against the ledger: no second release.
    for seed in 3..6 {
        commit_with_data(&mut r, seed).expect("commit");
        assert_eq!(index_charge(&r), run_bytes(&r), "after commit {seed}");
    }
}

#[test]
fn a_failed_persist_keeps_the_commit_and_blocks_the_next_full_until_a_retry_succeeds() {
    let v = vol();
    let mut r = v.open().map_err(|e| e.0).expect("open");
    commit_with_data(&mut r, 1).expect("commit");
    let runs = run_bytes(&r);
    // C8's persist fails: the commit (C6) is durable and stays.
    v.persist_faults.store(1, Ordering::SeqCst);
    let out = commit_with_data(&mut r, 2).expect("the commit is durable");
    let CommitOutcome::Committed { commit_seq, .. } = out else {
        panic!("{out:?}")
    };
    assert_eq!(r.engine.tip().0, commit_seq);
    assert_eq!(run_bytes(&r), runs, "no run written");
    // The next cut retries first and fails, writing nothing, while it fails;
    // the tick wants it even on an idle volume.
    v.persist_faults.store(1, Ordering::SeqCst);
    let e = commit_with_data(&mut r, 3).expect_err("the retry fails: no FULL");
    assert!(e.contains("injected persist failure"), "{e}");
    assert_eq!(r.engine.tip().0, commit_seq);
    assert!(!r.engine.is_stopped(), "not fatal");
    // Once it succeeds the cut goes on and commits the pending write.
    let out = r.engine.tick().expect("retry and commit");
    assert!(matches!(out, CommitOutcome::Committed { .. }), "{out:?}");
    assert!(r.engine.tip().0 > commit_seq);
    assert!(run_bytes(&r) > runs, "the entries are in a run");
    assert_eq!(index_charge(&r), run_bytes(&r));
    assert_eq!(r.engine.tick().expect("idle"), CommitOutcome::Idle);
    assert_eq!(r.read(0, 3 * u64::from(CS), CS).1, pattern(3, CS as usize));
}

/// M12b F2: a crash between C6 and C8's persist, then an idle open and
/// close: the catch-up's entries are persisted by the open itself.
#[test]
fn the_catch_up_is_persisted_at_open_so_an_idle_close_keeps_it() {
    let mut v = vol();
    let mut r = v.open().map_err(|e| e.0).expect("open");
    commit_with_data(&mut r, 1).expect("commit");
    v.hooks.arm("engine.c7.checkpoint", 1);
    assert!(commit_with_data(&mut r, 2).is_err());
    let p = r.stop();
    v.absorb(p, 2);
    let r = v.open().map_err(|e| e.0).expect("reopen");
    assert!(
        r.engine.recovery_report().catch_up.inserted > 0,
        "the commit's segments were not indexed"
    );
    // Idle: nothing to commit, then a crash (or a close that cuts nothing).
    assert_eq!(r.engine.tick().expect("tick"), CommitOutcome::Idle);
    let p = r.stop();
    v.absorb(p, 3);
    let mut r = v.open().map_err(|e| e.0).expect("reopen again");
    assert_eq!(
        r.engine.recovery_report().catch_up.inserted,
        0,
        "the first open persisted the catch-up"
    );
    assert_eq!(index_charge(&r), run_bytes(&r));
    assert_eq!(r.read(0, 2 * u64::from(CS), CS).1, pattern(2, CS as usize));
}

#[test]
fn a_catch_up_that_cannot_be_persisted_at_open_is_retried_by_the_first_cut() {
    let mut v = vol();
    let mut r = v.open().map_err(|e| e.0).expect("open");
    commit_with_data(&mut r, 1).expect("commit");
    v.hooks.arm("engine.c7.checkpoint", 1);
    assert!(commit_with_data(&mut r, 2).is_err());
    let p = r.stop();
    v.absorb(p, 2);
    // The open's persist and the first retry fail.
    v.persist_faults.store(2, Ordering::SeqCst);
    let mut r = v.open().map_err(|e| e.0).expect("the open does not fail");
    let (tip, _) = r.engine.tip();
    assert!(
        commit_with_data(&mut r, 3).is_err(),
        "no FULL before the persist"
    );
    assert_eq!(r.engine.tip().0, tip);
    commit_with_data(&mut r, 4).expect("retried, then committed");
    let p = r.stop();
    v.absorb(p, 4);
    let r = v.open().map_err(|e| e.0).expect("reopen");
    assert_eq!(r.engine.recovery_report().catch_up.inserted, 0);
}

/// Moves chunk 0's extent to a new segment at its first step.
struct Mover {
    done: bool,
}

impl Collector for Mover {
    fn step(&mut self, view: &GcView<'_>, _b: GcBudget) -> Result<GcProgress> {
        let mut moved = Vec::new();
        if !self.done {
            self.done = true;
            let e = view.map.get(ChunkNo(0))?.expect("chunk 0");
            let old = entry_hint(&e);
            let mut bytes = Vec::new();
            view.segments.read_extent(&old, &mut bytes)?;
            let id = view
                .index
                .get_by_prefix(&e.prefix)?
                .into_iter()
                .find(|c| c.hint == old)
                .expect("indexed")
                .id;
            let mut w = view.segments.open_segment()?;
            let new = w.append_extent(&id, None, &bytes)?;
            w.seal(None)?;
            view.index.relocate(&id, &old, &new)?;
            moved.push(Moved { id, old, new });
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
}

/// Review request 5 (c), N10: a failed `apply_moved` does not keep the cut
/// from naming the compaction output, so `persist` never covers a
/// relocation into an unnamed segment.
#[test]
fn a_compaction_output_is_named_by_the_next_cut_even_when_its_rehints_fail() {
    let mut v = vol();
    v.tweak = gc_on;
    *lock(&v.collector) = Some(Box::new(Mover { done: false }));
    let mut r = v.open().map_err(|e| e.0).expect("open");
    commit_with_data(&mut r, 0).expect("commit; C10 moves chunk 0");
    let before: Vec<_> = lunix_iface::store::SegmentStore::list(&r.store)
        .unwrap()
        .iter()
        .map(|s| s.seg)
        .collect();
    let out_seg = *before.iter().max().expect("the output segment");
    v.walk_faults.store(1, Ordering::SeqCst);
    let out = r
        .engine
        .tick()
        .expect("the cut commits without the re-hints");
    assert!(matches!(out, CommitOutcome::Committed { .. }));
    let f = tip_full(&v, &r);
    assert!(
        f.new_segments.contains(&out_seg),
        "the output {out_seg:?} is named although its re-hints failed: {:?}",
        f.new_segments
    );
    // A later cut with new segments persists the relocation (its segment is
    // named), and the deferred re-hints apply.
    commit_with_data(&mut r, 9).expect("commit");
    assert_eq!(r.engine.tick().expect("tick"), CommitOutcome::Idle);
    let p = r.stop();
    v.absorb(p, 5);
    let mut r = v.open().map_err(|e| e.0).expect("reopen");
    assert_eq!(r.read(0, 0, CS).1, pattern(0, CS as usize));
    assert_eq!(index_charge(&r), run_bytes(&r));
}
