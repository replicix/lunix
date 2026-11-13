//! Regression tests for the M11 review findings: journal-read failures,
//! deferred re-hints, classified destage failures, grow after a failed
//! destage, a write in flight at a cut and reads inside the C2-C9 window.

mod common;

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use common::*;
use lunix_engine::{CommitOutcome, Retry};
use lunix_format::commit::Commit;
use lunix_iface::block::{BlockEngine, IoOp};
use lunix_iface::crypto::CommitCodec;
use lunix_iface::gc::{
    Collector, GcBudget, GcParams, GcPassSummary, GcProgress, GcState, GcStats, GcView, Moved,
};
use lunix_iface::map::{entry_hint, ChunkNo};
use lunix_iface::store::CommitStore;
use lunix_iface::Result;
use lunix_testkit::JournalMode;

const CS: u32 = 16 << 10;

fn vol() -> Vol {
    Vol::create(CS, 16, 2, JournalMode::Mixed(13))
}

#[test]
fn a_failed_journal_read_of_an_undestaged_write_is_eio_not_stale_data() {
    let v = vol();
    let mut r = v.open().map_err(|e| e.0).expect("open");
    let old = pattern(1, CS as usize);
    assert!(r.write(0, 0, &old, false).is_ok());
    r.engine.commit_now(false).expect("commit");
    let new = pattern(2, 4096);
    assert!(r.write(1, 4096, &new, false).is_ok());
    // The record of `new` is only in the journal: its read fails.
    v.checks.fail_jread_completion.store(1, Ordering::SeqCst);
    let (st, data) = r.read(0, 4096, 4096);
    assert_eq!(st.errno(), Some(5), "never the map's older version");
    assert!(data.is_empty());
    v.checks.fail_jread_submit.store(1, Ordering::SeqCst);
    assert_eq!(r.read(1, 0, 8192).0.errno(), Some(5));
    // Without a fault the write reads back.
    assert_eq!(r.read(0, 4096, 4096).1, new);
}

#[test]
fn a_failed_journal_read_of_a_destaged_write_falls_back_to_the_map() {
    let v = vol();
    let mut r = v.open().map_err(|e| e.0).expect("open");
    assert!(r.write(0, 0, &pattern(1, CS as usize), false).is_ok());
    r.engine.commit_now(false).expect("commit");
    let new = pattern(2, 4096);
    assert!(r.write(1, 4096, &new, false).is_ok());
    // Stop after C6: the commit covers `new`, the overlay still holds it.
    v.hooks.arm("engine.c7.checkpoint", 1);
    assert!(r.engine.commit_now(false).is_err());
    v.checks.fail_jread_completion.store(1, Ordering::SeqCst);
    let (st, data) = r.read(0, 4096, 4096);
    assert!(st.is_ok(), "{st:?}");
    assert_eq!(data, new);
    v.checks.fail_jread_submit.store(1, Ordering::SeqCst);
    assert_eq!(r.read(0, 4096, 4096).1, new);
}

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
        })
    }
    fn urgent(&mut self, _v: &GcView<'_>, _w: u64) -> Result<GcProgress> {
        Ok(GcProgress {
            moved: Vec::new(),
            freed: 0,
            state: GcState::Idle,
            done: true,
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

fn tip_full(v: &Vol, r: &Running) -> lunix_format::commit::Full {
    let tip = CommitStore::tip(&r.store).unwrap().unwrap();
    let bytes = CommitStore::get(&r.store, tip).unwrap();
    match v.codec.open_commit(tip, &bytes).unwrap() {
        Commit::Full(f) => f,
        _ => panic!("FULL"),
    }
}

#[test]
fn a_failed_rehint_is_deferred_to_the_next_cut_not_fatal() {
    let mut v = vol();
    v.tweak = gc_on;
    *lock(&v.collector) = Some(Box::new(Mover { done: false }));
    let mut r = v.open().map_err(|e| e.0).expect("open");
    assert!(r.write(0, 0, &pattern(7, CS as usize), false).is_ok());
    // C10 of this commit moves chunk 0.
    r.engine.commit_now(false).expect("commit");
    v.walk_faults.store(1, Ordering::SeqCst);
    let out = r
        .engine
        .tick()
        .expect("the cut commits without the re-hints");
    assert!(matches!(out, CommitOutcome::Committed { .. }));
    assert!(
        !r.engine.is_stopped(),
        "a recoverable re-hint failure is not fatal"
    );
    let out = r.engine.tick().expect("the deferred re-hints apply now");
    assert!(matches!(out, CommitOutcome::Committed { .. }));
    let f = tip_full(&v, &r);
    assert!(!f.new_segments.is_empty(), "the compaction output is named");
    assert_eq!(r.engine.tick().expect("tick"), CommitOutcome::Idle);
    assert_eq!(r.read(0, 0, CS).1, pattern(7, CS as usize));
    assert!(r.write(1, 4096, &pattern(8, 4096), false).is_ok());
}

#[test]
fn a_transient_destage_failure_is_retried_and_a_permanent_one_stops_commits() {
    let v = vol();
    let mut r = v.open().map_err(|e| e.0).expect("open");
    assert!(r.write(0, 0, &pattern(1, 4096), false).is_ok());
    *lock(&v.destage_fault) = Some(Retry::Later);
    assert!(r.engine.commit_now(false).is_err());
    assert!(!r.engine.is_stopped());
    assert!(matches!(
        r.engine.commit_now(false).expect("retried"),
        CommitOutcome::Committed { .. }
    ));
    assert!(r.write(0, 4096, &pattern(2, 4096), false).is_ok());
    *lock(&v.destage_fault) = Some(Retry::Never);
    assert!(r.engine.commit_now(false).is_err());
    assert!(r.engine.is_stopped(), "a permanent failure stops commits");
    let t = r.submit(0, IoOp::Write, false, 8192, 4096, &pattern(3, 4096));
    assert_eq!(r.wait(&[t])[0].0.errno(), Some(5), "writes fail");
    assert_eq!(r.read(0, 4096, 4096).1, pattern(2, 4096), "reads continue");
}

#[test]
fn a_damaged_base_extent_is_a_permanent_destage_failure() {
    let v = vol();
    let mut r = v.open().map_err(|e| e.0).expect("open");
    assert!(r.write(0, 0, &pattern(1, CS as usize), false).is_ok());
    r.engine.commit_now(false).expect("commit");
    // Chunk 0's extent is damaged; a partial overwrite needs it as its base.
    let (root, size) = {
        let f = tip_full(&v, &r);
        (f.map_root, f.size)
    };
    let ext: Arc<dyn lunix_iface::crypto::ExtentCodec> = Arc::new(v.codec);
    let packs: Arc<dyn lunix_iface::store::PackRead> = Arc::new(r.store.clone());
    let m =
        lunix_testkit::FlatMap::open(ext, packs, &root, size / u64::from(CS), v.id_len).unwrap();
    let hint = entry_hint(
        &lunix_iface::map::BlockMap::get(&m, ChunkNo(0))
            .unwrap()
            .unwrap(),
    );
    lock(&v.read_faults.flip).insert(hint);
    assert!(r.write(1, 4096, &pattern(2, 4096), false).is_ok());
    let before = r.engine.stats().extent_auth_failures;
    assert!(r.engine.commit_now(false).is_err());
    assert!(r.engine.is_stopped());
    assert!(r.engine.stats().extent_auth_failures > before);
    // No retry: a stopped engine refuses further commits.
    assert!(r.engine.commit_now(false).is_err());
}

#[test]
fn grow_after_a_failed_destage_keeps_the_map_at_the_committed_size() {
    let cs = u64::from(CS);
    let mut v = vol();
    let mut r = v.open().map_err(|e| e.0).expect("open");
    assert!(r.write(0, 0, &pattern(1, 4096), false).is_ok());
    *lock(&v.destage_fault) = Some(Retry::Later);
    assert!(r.engine.grow(32 * cs).is_err());
    assert_eq!(r.engine.size_bytes(), 16 * cs);
    r.engine
        .commit_now(false)
        .expect("a plain commit at the old size");
    let p = r.stop();
    v.absorb(p, 1);
    let mut r = v
        .open()
        .map_err(|e| e.0)
        .expect("reopen: the root matches the size");
    assert_eq!(r.engine.size_bytes(), 16 * cs);
    r.engine.grow(32 * cs).expect("grow");
    assert!(r.write(0, 31 * cs, &pattern(2, 4096), false).is_ok());
    r.engine.commit_now(false).expect("commit");
    let p = r.stop();
    v.absorb(p, 2);
    let mut r = v.open().map_err(|e| e.0).expect("reopen");
    assert_eq!(r.read(0, 31 * cs, 4096).1, pattern(2, 4096));
}
