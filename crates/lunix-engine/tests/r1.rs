//! R1 (cross-cutting rule 13): reads carry an expectation and a mismatch is
//! an `extent_auth_failures` event and an error, never data; a stale hint
//! falls back to the index and still confirms the full id. Also the GC hook
//! (C10) and `moved` records applied as re-hints at the next cut.

mod common;

use std::sync::{Arc, Mutex};

use common::*;
use lunix_format::commit::{Commit, MapRoot};
use lunix_format::node::DataEntry;
use lunix_format::{ChunkId, Hint, IdLen};
use lunix_iface::block::BlockEngine;
use lunix_iface::crypto::{CommitCodec, ExtentCodec};
use lunix_iface::gc::{
    Collector, GcBudget, GcParams, GcPassSummary, GcProgress, GcState, GcStats, GcView, Moved,
};
use lunix_iface::index::ChunkIndex;
use lunix_iface::map::{entry_hint, BlockMap, ChunkNo};
use lunix_iface::store::{CommitStore, PackRead, SegmentStore};
use lunix_iface::Result;
use lunix_testkit::{FlatMap, JournalMode, PlainCodec};

const CS: u32 = 16 << 10;

fn tip_root(r: &Running) -> (MapRoot, u64) {
    let tip = CommitStore::tip(&r.store).unwrap().unwrap();
    let bytes = CommitStore::get(&r.store, tip).unwrap();
    match PlainCodec::new().open_commit(tip, &bytes).unwrap() {
        Commit::Full(f) => (f.map_root, f.size),
        _ => panic!("not a FULL"),
    }
}

fn entry(r: &Running, ci: u64) -> DataEntry {
    let (root, size) = tip_root(r);
    let ext: Arc<dyn ExtentCodec> = Arc::new(PlainCodec::new());
    let packs: Arc<dyn PackRead> = Arc::new(r.store.clone());
    let m = FlatMap::open(ext, packs, &root, size / u64::from(CS), IdLen::Sixteen).unwrap();
    m.get(ChunkNo(ci)).unwrap().expect("a stored chunk")
}

/// Three chunks in the map, the overlay empty, the read cache cold.
fn setup() -> (Vol, Running) {
    let v = Vol::create(CS, 16, 2, JournalMode::Mixed(3));
    let mut r = v.open().map_err(|e| e.0).expect("open");
    for c in 0..3u64 {
        assert!(r
            .write(0, c * u64::from(CS), &pattern(100 + c, CS as usize), false)
            .is_ok());
    }
    r.engine.commit_now(false).expect("commit");
    (v, r)
}

fn auth_failures(r: &Running) -> u64 {
    r.engine.stats().extent_auth_failures
}

#[test]
fn a_swapped_valid_extent_fails_the_read() {
    let (v, mut r) = setup();
    let a = entry_hint(&entry(&r, 0));
    let b = entry_hint(&entry(&r, 1));
    lock(&v.read_faults.swap).insert(a, b);
    let before = auth_failures(&r);
    let (st, data) = r.read(0, 0, CS);
    assert_eq!(st.errno(), Some(5), "EIO, never B's bytes");
    assert!(data.is_empty());
    assert!(auth_failures(&r) > before);
}

#[test]
fn an_older_extent_for_the_same_lba_fails_the_read() {
    let (v, mut r) = setup();
    let old = entry_hint(&entry(&r, 0));
    assert!(r.write(1, 0, &pattern(999, CS as usize), false).is_ok());
    r.engine.commit_now(false).expect("commit");
    let new = entry_hint(&entry(&r, 0));
    assert_ne!(old, new);
    lock(&v.read_faults.swap).insert(new, old);
    let before = auth_failures(&r);
    let (st, _) = r.read(0, 0, CS);
    assert_eq!(st.errno(), Some(5));
    assert!(auth_failures(&r) > before);
}

#[test]
fn a_damaged_tag_or_a_wrong_offset_fails_the_read() {
    let (v, mut r) = setup();
    let a = entry_hint(&entry(&r, 2));
    lock(&v.read_faults.flip).insert(a);
    let before = auth_failures(&r);
    assert_eq!(r.read(0, 2 * u64::from(CS), 4096).0.errno(), Some(5));
    assert!(auth_failures(&r) > before);
    lock(&v.read_faults.flip).clear();
    // The extent of chunk 1 presented at chunk 0's hint (the same segment,
    // another offset).
    let h0 = entry_hint(&entry(&r, 0));
    let h1 = entry_hint(&entry(&r, 1));
    lock(&v.read_faults.swap).insert(h0, Hint { off: h1.off, ..h0 });
    assert_eq!(r.read(1, 0, 4096).0.errno(), Some(5));
}

#[test]
fn a_stale_hint_falls_back_to_the_index_and_confirms_the_full_id() {
    let (_v, mut r) = setup();
    let e = entry(&r, 1);
    let old = entry_hint(&e);
    // Compaction by hand: copy the extent to a new segment, relocate the
    // index entry, delete the old segment.
    let mut bytes = Vec::new();
    SegmentStore::read_extent(&r.store, &old, &mut bytes).unwrap();
    let data = pattern(101, CS as usize);
    let id: ChunkId = PlainCodec::new().chunk_id(lunix_format::ExtentKind::Data, &data);
    assert!(e.prefix.matches(&id));
    let mut w = r.store.open_segment().unwrap();
    let new = w.append_extent(&id, None, &bytes).unwrap();
    w.seal(None).unwrap();
    assert!(r.index.relocate(&id, &old, &new).unwrap());
    SegmentStore::delete(&r.store, old.seg).unwrap();
    let before = auth_failures(&r);
    let (st, got) = r.read(0, u64::from(CS), CS);
    assert!(st.is_ok(), "{st:?}");
    assert_eq!(got, data);
    assert_eq!(
        auth_failures(&r),
        before,
        "a missing segment is not an auth failure"
    );
}

#[test]
fn a_stale_hint_whose_index_candidate_has_another_id_is_refused() {
    let (_v, mut r) = setup();
    // Chunk 3 in a second segment.
    assert!(r
        .write(0, 3 * u64::from(CS), &pattern(103, CS as usize), false)
        .is_ok());
    r.engine.commit_now(false).expect("commit");
    let e0 = entry(&r, 0);
    let e3 = entry(&r, 3);
    assert_ne!(e0.seg, e3.seg);
    // The index claims chunk 0's id now lives where chunk 3 is, and chunk
    // 0's segment is gone.
    let id0 =
        PlainCodec::new().chunk_id(lunix_format::ExtentKind::Data, &pattern(100, CS as usize));
    assert!(r
        .index
        .relocate(&id0, &entry_hint(&e0), &entry_hint(&e3))
        .unwrap());
    SegmentStore::delete(&r.store, e0.seg).unwrap();
    let before = auth_failures(&r);
    let (st, data) = r.read(0, 0, 4096);
    assert_eq!(
        st.errno(),
        Some(5),
        "the candidate's header id is not the full id"
    );
    assert!(data.is_empty());
    assert!(auth_failures(&r) > before);
}

/// A collector that, at its first step, moves chunk 0's extent to a new
/// segment and reports it; it records whether its preconditions held.
struct MovingCollector {
    target: ChunkNo,
    done: bool,
    log: Arc<Mutex<Vec<String>>>,
}

impl Collector for MovingCollector {
    fn step(&mut self, view: &GcView<'_>, _budget: GcBudget) -> Result<GcProgress> {
        if view.map.root() != view.tip.map_root || view.map.dirty_leaf_count() != 0 {
            lock(&self.log).push("collector called with the map off the tip".into());
        }
        lock(&self.log).push("step".into());
        let mut moved = Vec::new();
        if !self.done {
            self.done = true;
            let e = view.map.get(self.target)?.expect("chunk");
            let old = entry_hint(&e);
            let mut bytes = Vec::new();
            view.segments.read_extent(&old, &mut bytes)?;
            let cands = view.index.get_by_prefix(&e.prefix)?;
            let id = cands.iter().find(|c| c.hint == old).expect("indexed").id;
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

    fn urgent(&mut self, _view: &GcView<'_>, _want: u64) -> Result<GcProgress> {
        Ok(GcProgress {
            moved: Vec::new(),
            freed: 0,
            state: GcState::Idle,
            done: true,
        })
    }

    fn dry_run(&mut self, _view: &GcView<'_>) -> Result<GcPassSummary> {
        Ok(GcPassSummary::default())
    }

    fn stats(&self) -> GcStats {
        GcStats::default()
    }
}

fn gc_on(cfg: &mut lunix_engine::EngineConfig) {
    cfg.gc_budget = Some(GcBudget {
        max_duration: std::time::Duration::from_millis(50),
        max_bytes: 1 << 20,
    });
    cfg.gc_params = Some(GcParams {
        retain_last: 2,
        grace: std::time::Duration::from_secs(60),
        read_timeout: std::time::Duration::from_secs(10),
        rate_bytes_per_s: 64 << 20,
        live_threshold: 0.5,
        chunk_size: CS,
        id_len: IdLen::Sixteen,
    });
}

#[test]
fn moved_records_from_the_collector_become_rehints_at_an_idle_cut() {
    let mut v = Vol::create(CS, 16, 2, JournalMode::Mixed(3));
    v.tweak = gc_on;
    let log = Arc::new(Mutex::new(Vec::new()));
    *lock(&v.collector) = Some(Box::new(MovingCollector {
        target: ChunkNo(0),
        done: false,
        log: Arc::clone(&log),
    }));
    let mut r = v.open().map_err(|e| e.0).expect("open");
    assert!(r.write(0, 0, &pattern(7, CS as usize), false).is_ok());
    // C10 of this commit runs the collector, which moves chunk 0.
    r.engine.commit_now(false).expect("commit");
    let before = entry_hint(&entry(&r, 0));
    // No write since: the volume is idle, yet the pending moved record must
    // be named by a cut (its output segment is unnamed until then).
    let out = r.engine.tick().expect("tick");
    assert!(
        matches!(out, lunix_engine::CommitOutcome::Committed { .. }),
        "{out:?}"
    );
    let after = entry_hint(&entry(&r, 0));
    assert_ne!(before, after, "re-hinted at the cut");
    assert_eq!(r.read(0, 0, CS).1, pattern(7, CS as usize));
    let l = lock(&log).clone();
    assert!(l.iter().all(|s| s == "step"), "{l:?}");
    assert!(!l.is_empty());
}
