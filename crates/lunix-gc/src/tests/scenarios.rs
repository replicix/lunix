//! Directed cases of the M17.A gate: the dedup race and stale mark, in-flight
//! output, the delete-time index invariant, stale hints, the four points of
//! mark staleness, the rollback-floor window, re-seal and relocation order.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use lunix_format::commit::Commit;
use lunix_format::local::RollbackFloor;
use lunix_format::{ChunkId, CommitSeq, ExtentKind, Hint, IdLen, SegId};
use lunix_iface::crypto::{CommitCodec, ExtentCodec, OpenExpect, OpenOutput};
use lunix_iface::gc::{Collector, GcState, GcView};
use lunix_iface::index::ChunkIndex;
use lunix_iface::map::ChunkNo;
use lunix_iface::store::{CommitStore, SegmentStore, VolumeDir};
use lunix_iface::{Error, Result};
use lunix_testkit::MemIndex;

use super::sim::{data_of, high_water, MapKind, Sim, SimCfg, GRACE};
use super::toy::ToyCodec;
use crate::condemned::ObjectRef;
use crate::{FloorSource, Gc};

const CS: u32 = 16 << 10;

fn sim() -> Sim {
    Sim::new(SimCfg::new(CS, IdLen::Sixteen))
}

fn segs(s: &Sim) -> BTreeSet<SegId> {
    SegmentStore::list(&s.store)
        .unwrap()
        .iter()
        .map(|x| x.seg)
        .collect()
}

fn hint_of(s: &Sim, ci: u64) -> Hint {
    let e = s.map.get(ChunkNo(ci)).unwrap().expect("populated");
    lunix_iface::map::entry_hint(&e)
}

fn id_of(seed: u64) -> ChunkId {
    lunix_testkit::PlainCodec::new().chunk_id(ExtentKind::Data, &data_of(seed, CS))
}

/// Steps one unit at a time until the state is `want` (or the pass ends).
fn units_until(s: &mut Sim, want: GcState) {
    for _ in 0..100_000 {
        let p = s.unit().unwrap();
        if p.state == want || p.done {
            return;
        }
        s.clock.advance(Duration::from_millis(100));
    }
    panic!("never reached {want:?}");
}

/// A segment that holds only chunk id X (seed 77), referenced by no root.
fn dead_segment_holding_x(s: &mut Sim) -> SegId {
    s.write(0, 77);
    s.commit().unwrap();
    let seg = hint_of(s, 0).seg;
    s.write(0, 78);
    s.commit().unwrap();
    s.commit().unwrap();
    s.commit().unwrap();
    seg
}

#[test]
fn a_cut_that_dedups_between_mark_and_condemn_is_found_by_the_recheck() {
    let mut s = sim();
    let x_seg = dead_segment_holding_x(&mut s);
    s.clock.advance(Duration::from_secs(5));
    // Mark (stale from here on), then a cut that dedups X onto the
    // not-yet-condemned segment.
    units_until(&mut s, GcState::Condemning);
    assert!(s.index.get_for_dedup(&id_of(77)).unwrap().is_some());
    s.write(5, 77);
    s.commit().unwrap();
    assert_eq!(hint_of(&s, 5).seg, x_seg, "the write deduplicated");
    // Condemn on the stale mark: X's segment is condemned and from that
    // moment dedup no longer returns it.
    units_until(&mut s, GcState::Grace);
    assert!(s.gc.condemned().contains(&ObjectRef::Segment(x_seg)));
    assert!(
        s.index.get_for_dedup(&id_of(77)).unwrap().is_none(),
        "dedup never targets a condemned segment"
    );
    s.commit().unwrap();
    // Grace, recheck: the late reference un-condemns, nothing is deleted.
    s.clock.advance(GRACE * 2);
    s.pass().unwrap();
    assert!(segs(&s).contains(&x_seg));
    assert!(!s.gc.condemned().contains(&ObjectRef::Segment(x_seg)));
    assert!(s.index.get_for_dedup(&id_of(77)).unwrap().is_some());
    s.check_roots().unwrap();
    s.check_ledger().unwrap();
}

/// A segment of four chunks of which three are overwritten (u = 1/4), with
/// the old roots rolled off.
fn low_live_segment(s: &mut Sim) -> SegId {
    for i in 0..4 {
        s.write(i, 200 + i);
    }
    s.commit().unwrap();
    let seg = hint_of(s, 0).seg;
    assert!((0..4).all(|i| hint_of(s, i).seg == seg));
    for i in 1..4 {
        s.write(i, 300 + i);
    }
    s.commit().unwrap();
    // Until no FULL of GC's retained set (from the lowest chain_floor a
    // retained FULL records, N24) predates the overwrite: 2·retain_last − 2
    // commits.
    for _ in 0..(2 * s.retain_last).saturating_sub(2).max(2) {
        s.commit().unwrap();
    }
    seg
}

#[test]
fn compaction_output_is_never_condemned_before_a_cut_names_it() {
    let mut s = sim();
    let src = low_live_segment(&mut s);
    s.clock.advance(Duration::from_secs(5));
    s.pass().unwrap();
    assert!(!s.moved.is_empty(), "the low-live segment was compacted");
    let out = s.moved[0].new.seg;
    assert!(out.raw() > high_water(&s.retained));
    // An idle volume: urgent passes and dry runs see the output above the
    // mark and never condemn it.
    s.clock.advance(GRACE * 2);
    s.urgent(u64::MAX).unwrap();
    assert!(segs(&s).contains(&out));
    assert!(!s.gc.condemned().contains(&ObjectRef::Segment(out)));
    // A FULL that does not carry the moves (as if the wiring lagged) lifts
    // the high-water mark above the output: it is still protected.
    let moved = std::mem::take(&mut s.moved);
    s.write(20, 999);
    s.commit().unwrap();
    assert!(out.raw() < high_water(&s.retained));
    s.moved = moved;
    s.clock.advance(GRACE * 2);
    s.urgent(u64::MAX).unwrap();
    s.clock.advance(GRACE * 2);
    s.urgent(u64::MAX).unwrap();
    assert!(segs(&s).contains(&out));
    assert!(s.gc.unnamed_outputs().contains(&out));
    // The pending moves force the cut that names it.
    s.commit().unwrap();
    assert!(s.tip.new_segments.contains(&out));
    assert_eq!(hint_of(&s, 0).seg, out, "re-hinted at the cut");
    s.step().unwrap();
    assert!(s.gc.unnamed_outputs().is_empty());
    // And the source goes once its stale hints roll off.
    s.quiesce().unwrap();
    assert!(!segs(&s).contains(&src));
    s.check_roots().unwrap();
}

#[test]
fn a_crash_before_the_naming_cut_drops_the_output_and_keeps_the_source_indexed() {
    let mut s = sim();
    let src = low_live_segment(&mut s);
    s.clock.advance(Duration::from_secs(5));
    s.pass().unwrap();
    let out = s.moved.first().expect("compacted").new.seg;
    // Relocated in the memtable only: no persisted run names the output.
    let durable = MemIndex::from_image(s.store.mem_ids(), &s.index.image());
    let mut in_runs = false;
    durable
        .iter_entries(&mut |c| in_runs |= c.hint.seg == out)
        .unwrap();
    assert!(
        !in_runs,
        "relocate entries are persisted only once the output is named"
    );
    // The source is dead now (the index's copies are in the output), but its
    // delete waits for the output to be named: passes on the idle volume
    // condemn it and defer.
    for _ in 0..4 {
        s.clock.advance(GRACE * 2);
        s.urgent(u64::MAX).unwrap();
    }
    assert!(
        segs(&s).contains(&src),
        "the source outlives an unnamed output"
    );
    s.crash_reopen(1).unwrap();
    assert!(
        !segs(&s).contains(&out),
        "the open-time sweep removed the output"
    );
    assert!(segs(&s).contains(&src));
    assert_eq!(
        s.index.get(&id_of(200)).unwrap().map(|c| c.hint.seg),
        Some(src)
    );
    s.check_roots().unwrap();
    s.quiesce().unwrap();
    s.check_roots().unwrap();
}

/// Checks every root through (a) a fresh index rebuilt from every segment and
/// (b) the durable index image after a crash with `catch_up` over the
/// segments the retained chain names.
fn check_index_invariant(s: &Sim) {
    let rebuilt = MemIndex::new(s.store.mem_ids());
    let all: Vec<SegId> = segs(s).into_iter().collect();
    rebuilt.rebuild(&all, &s.store).unwrap();
    s.check_roots_with(&rebuilt)
        .unwrap_or_else(|e| panic!("with a rebuilt index: {e}"));
    let crashed = MemIndex::from_image(s.store.mem_ids(), &s.index.image());
    let named: Vec<SegId> = s
        .retained
        .iter()
        .filter_map(|c| match c {
            Commit::Full(f) => Some(f.new_segments.clone()),
            _ => None,
        })
        .flatten()
        .collect();
    crashed.catch_up(&named, &s.store).unwrap();
    s.check_roots_with(&crashed)
        .unwrap_or_else(|e| panic!("with the crash-recovered index: {e}"));
}

#[test]
fn deleting_segments_with_stale_hints_keeps_every_live_id_resolvable() {
    for seed in 0..12u64 {
        let mut cfg = SimCfg::new(CS, IdLen::Sixteen);
        cfg.retain_last = 2 + (seed % 3) as u32;
        let mut s = Sim::new(cfg);
        let mut k = seed;
        let mut deletions = 0;
        for round in 0..40u64 {
            for _ in 0..6 {
                k = k
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1442695040888963407);
                s.write(k >> 58, (k >> 30) % 24);
            }
            s.commit().unwrap();
            if round % 3 == 0 {
                s.clock.advance(GRACE);
            }
            for _ in 0..6 {
                let before = segs(&s);
                let p = s.step().unwrap();
                s.clock.advance(Duration::from_secs(20));
                if segs(&s) != before || p.freed > 0 {
                    deletions += 1;
                    check_index_invariant(&s);
                }
            }
        }
        assert!(deletions > 0);
    }
}

#[test]
fn a_retained_roots_stale_hint_reads_through_the_index_before_and_after_the_delete() {
    let mut cfg = SimCfg::new(CS, IdLen::Sixteen);
    cfg.retain_last = 3;
    let mut s = Sim::new(cfg);
    let src = low_live_segment(&mut s);
    s.clock.advance(Duration::from_secs(5));
    s.pass().unwrap();
    assert!(!s.moved.is_empty());
    s.commit().unwrap();
    // The predecessor FULL still points into the source; the tip is re-hinted.
    let older = match &s.retained[s.retained.len() - 2] {
        Commit::Full(f) => f.map_root,
        _ => unreachable!(),
    };
    let mut stale = Vec::new();
    s.map
        .walk_leaves(&older, &mut |_, e| {
            if e.seg == src {
                stale.push(*e);
            }
        })
        .unwrap();
    assert!(!stale.is_empty());
    s.check_roots().unwrap();
    // Run passes until the source is deleted while that root is retained.
    for _ in 0..10 {
        s.clock.advance(GRACE * 2);
        s.commit().unwrap();
        s.pass().unwrap();
        if !segs(&s).contains(&src) {
            break;
        }
    }
    assert!(!segs(&s).contains(&src), "the compacted source was deleted");
    assert!(
        s.retained
            .iter()
            .any(|c| matches!(c, Commit::Full(f) if f.map_root == older)),
        "the root with the stale hint is still retained"
    );
    for e in &stale {
        let d = s.read_entry(e).expect("stale hint reads through the index");
        assert!((200..204).any(|seed| d == data_of(seed, CS)));
    }
    s.check_roots().unwrap();
}

#[test]
fn rule12_i_the_mark_walks_exactly_the_root_set_pinned_at_its_start() {
    let mut s = sim();
    for i in 0..30 {
        s.write(i, i);
    }
    s.commit().unwrap();
    let start: Vec<u64> = s.retained.iter().map(|c| c.common().commit_seq.0).collect();
    let mut m = crate::mark::Mark::new(s.cfg.gc);
    let mut n = 0;
    loop {
        let done = s.with_view(|v| m.unit(v)).unwrap();
        if done {
            break;
        }
        // A cut between every unit: new roots, new segments.
        s.write(40 + n, 500 + n);
        s.commit().unwrap();
        n += 1;
    }
    let l = m.into_liveness();
    let mut walked: Vec<u64> = l.roots.iter().map(|r| r.seq.0).collect();
    walked.sort_unstable();
    assert_eq!(walked, start);
    // Objects created after the mark started are not even candidates.
    let listed: BTreeSet<SegId> = l.segments.iter().map(|x| x.seg).collect();
    assert!(segs(&s).iter().any(|x| !listed.contains(x)));
}

#[test]
fn rule12_ii_condemn_skips_an_object_a_retained_commit_names() {
    let mut s = sim();
    low_live_segment(&mut s);
    s.clock.advance(Duration::from_secs(5));
    s.pass().unwrap();
    let out = s.moved.first().expect("compacted").new.seg;
    // The chunk is overwritten before the cut that applies the move: the
    // re-hint loses to the newer write, the FULL still names the output.
    s.write(0, 4242);
    s.commit().unwrap();
    assert!(s.tip.new_segments.contains(&out));
    s.commit().unwrap();
    // One more: the FULL before the overwrite leaves GC's retained set (N24).
    s.commit().unwrap();
    // Dead now, but a retained FULL names it.
    let l = s.with_view(|v| crate::liveness(v, s.cfg.gc)).unwrap();
    assert_eq!(l.live_bytes(out), 0);
    // The stale-mark form: mark, a cut, then condemn.
    s.clock.advance(Duration::from_secs(5));
    units_until(&mut s, GcState::Condemning);
    s.write(9, 9);
    s.commit().unwrap();
    s.clock.advance(GRACE * 2);
    s.pass().unwrap();
    assert!(!s.gc.condemned().contains(&ObjectRef::Segment(out)));
    // Once the naming commit rolls off, it goes.
    assert!(segs(&s).contains(&out));
    s.quiesce().unwrap();
    assert!(!segs(&s).contains(&out));
    s.check_roots().unwrap();
}

#[test]
fn rule12_iv_cuts_during_the_recheck_walk_add_no_reference_to_condemned_objects() {
    for seed in 0..20u64 {
        let mut s = sim();
        let mut k = seed + 1;
        for _ in 0..8 {
            for _ in 0..5 {
                k = k.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
                s.write(k >> 59, (k >> 33) % 12);
            }
            s.commit().unwrap();
        }
        s.clock.advance(Duration::from_secs(5));
        units_until(&mut s, GcState::Grace);
        s.clock.advance(GRACE * 2);
        units_until(&mut s, GcState::Rechecking);
        // Cuts with writes that would dedup onto anything condemned, between
        // every unit of the recheck and the deletes.
        for _ in 0..10_000 {
            k = k.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            s.write(k >> 59, (k >> 33) % 12);
            s.commit().unwrap();
            let p = s.unit().unwrap();
            if p.done {
                break;
            }
        }
        s.check_roots().unwrap();
        s.quiesce().unwrap();
        s.check_roots().unwrap();
    }
}

#[test]
fn the_rollback_floor_window_keeps_commits_below_chain_floor() {
    let mut s = sim();
    for i in 0..3 {
        s.write(i, i);
        s.commit().unwrap();
    }
    // The floor stays at 3 while the chain moves on (a floor raise that did
    // not happen, or a floor read back from an older attach).
    s.raise_floor = false;
    let floor = s.store.rollback_floor().unwrap().unwrap().max_commit_seq;
    assert_eq!(floor, CommitSeq(3));
    for i in 0..12 {
        s.write(i, 100 + i);
        s.commit().unwrap();
        s.clock.advance(GRACE);
        s.pass().unwrap();
    }
    assert!(s.tip.chain_floor > floor, "the floor is below chain_floor");
    s.clock.advance(GRACE * 2);
    s.pass().unwrap();
    s.clock.advance(GRACE * 2);
    s.pass().unwrap();
    let tip = s.tip.common.commit_seq;
    for seq in 0..=tip.0 {
        assert_eq!(s.commit_exists(seq), seq >= floor.0, "commit {seq}");
    }
    // The attach walk from the tip back to the floor completes.
    let fh = s.commit_codec.object_hash(&s.store.get(floor).unwrap());
    s.commit_codec
        .verify_chain(&s.store, tip, floor, &[0x11; 32])
        .unwrap();
    assert_eq!(fh, s.store.rollback_floor().unwrap().unwrap().tip_hash);
    // When the floor is raised, the window closes and the rest goes (down to
    // the lowest chain_floor of a retained FULL, N12).
    s.raise_floor = true;
    s.quiesce().unwrap();
    let cf = s.keep_floor();
    assert!(cf < s.tip.chain_floor.0);
    for seq in 0..=s.tip.common.commit_seq.0 {
        assert_eq!(s.commit_exists(seq), seq >= cf, "commit {seq}");
    }
}

struct BrokenFloor;

impl FloorSource for BrokenFloor {
    fn current_floor(&self) -> Result<Option<RollbackFloor>> {
        Err(Error::Corrupt {
            what: "pins",
            detail: "test",
        })
    }
}

#[test]
fn an_unreadable_rollback_floor_deletes_no_commit() {
    let mut s = sim();
    s.gc = Gc::new(Arc::new(BrokenFloor), s.cfg.gc);
    for i in 0..10 {
        s.write(i, i);
        s.commit().unwrap();
        s.clock.advance(GRACE);
        s.pass().unwrap();
    }
    for seq in 0..=s.tip.common.commit_seq.0 {
        assert!(s.commit_exists(seq), "commit {seq}");
    }
    assert!(s.gc.totals().segments_deleted + s.gc.totals().packs_deleted > 0);
    s.check_roots().unwrap();
}

#[test]
fn compaction_reseals_with_a_fresh_salt_and_relocates_after_the_seal() {
    let mut cfg = SimCfg::new(CS, IdLen::ThirtyTwo);
    cfg.encrypted = true;
    let mut s = Sim::new(cfg);
    let src = low_live_segment(&mut s);
    // A relocation must find the output sealed in the store already.
    let store = s.store.clone();
    let checker = Arc::new(SealedFirst {
        inner: s.index.clone(),
        store,
        seen: std::sync::Mutex::new(0),
    });
    let mut gc = std::mem::replace(&mut s.gc, Gc::new(Arc::new(s.store.clone()), s.cfg.gc));
    s.clock.advance(Duration::from_secs(5));
    let mut moved = Vec::new();
    for _ in 0..10_000 {
        let p = s
            .with_view(|v| {
                let view = GcView {
                    index: &*checker,
                    ..*v
                };
                gc.step(&view, Sim::budget())
            })
            .unwrap();
        moved.extend(p.moved);
        if p.done {
            break;
        }
        s.clock.advance(Duration::from_secs(5));
    }
    s.gc = gc;
    assert!(*checker.seen.lock().unwrap() > 0);
    assert!(!moved.is_empty());
    for m in &moved {
        assert_eq!(m.old.seg, src);
        let (mut a, mut b) = (Vec::new(), Vec::new());
        s.store.read_extent(&m.old, &mut a).unwrap();
        s.store.read_extent(&m.new, &mut b).unwrap();
        assert_ne!(ToyCodec::salt_of(&a), ToyCodec::salt_of(&b), "a fresh salt");
        assert_ne!(a, b);
        let open = |raw: &[u8]| {
            let mut out = Vec::new();
            let meta = s
                .codec
                .open(
                    ExtentKind::Data,
                    raw,
                    &OpenExpect {
                        chunk_size: CS,
                        chunk_id: Some(&m.id),
                        id_prefix: None,
                        verify_id: true,
                        output: OpenOutput::Plaintext,
                    },
                    &mut out,
                )
                .unwrap();
            (meta.chunk_id, out)
        };
        assert_eq!(open(&a), open(&b), "same chunk id and plaintext");
    }
    s.moved = moved;
    s.commit().unwrap();
    s.quiesce().unwrap();
    s.check_roots().unwrap();
}

/// An index that checks, at every `relocate`, that the store already lists
/// the output as sealed.
struct SealedFirst {
    inner: Arc<MemIndex>,
    store: lunix_testkit::MemStore,
    seen: std::sync::Mutex<u64>,
}

impl ChunkIndex for SealedFirst {
    fn get_for_dedup(&self, id: &ChunkId) -> Result<Option<lunix_iface::index::DedupHit>> {
        self.inner.get_for_dedup(id)
    }
    fn get(&self, id: &ChunkId) -> Result<Option<lunix_iface::index::IndexedChunk>> {
        self.inner.get(id)
    }
    fn get_by_prefix(
        &self,
        prefix: &lunix_format::IdPrefix,
    ) -> Result<Vec<lunix_iface::index::IndexedChunk>> {
        self.inner.get_by_prefix(prefix)
    }
    fn insert(&self, id: ChunkId, hint: Hint) -> Result<()> {
        self.inner.insert(id, hint)
    }
    fn relocate(&self, id: &ChunkId, old: &Hint, new: &Hint) -> Result<bool> {
        let sealed = SegmentStore::list(&self.store)?
            .iter()
            .any(|x| x.seg == new.seg);
        assert!(sealed, "relocate before the output segment was sealed");
        *self.seen.lock().unwrap() += 1;
        self.inner.relocate(id, old, new)
    }
    fn mark_condemned(&self, seg: SegId) -> Result<()> {
        self.inner.mark_condemned(seg)
    }
    fn mark_condemned_all(&self, segs: &[SegId]) -> Result<()> {
        self.inner.mark_condemned_all(segs)
    }
    fn unmark_condemned_all(&self, segs: &[SegId]) -> Result<()> {
        self.inner.unmark_condemned_all(segs)
    }
    fn retire_segments(&self, segs: &[SegId]) -> Result<()> {
        self.inner.retire_segments(segs)
    }
    fn unmark_condemned(&self, seg: SegId) -> Result<()> {
        self.inner.unmark_condemned(seg)
    }
    fn condemned(&self) -> Result<Vec<SegId>> {
        self.inner.condemned()
    }
    fn drop_segment(&self, seg: SegId) -> Result<()> {
        self.inner.drop_segment(seg)
    }
    fn persist(&self, upto_seg: SegId) -> Result<Vec<lunix_iface::index::RunRef>> {
        self.inner.persist(upto_seg)
    }
    fn runs(&self) -> Result<Vec<lunix_iface::index::RunRef>> {
        self.inner.runs()
    }
    fn all_runs(&self) -> Result<Vec<lunix_iface::index::RunRef>> {
        self.inner.all_runs()
    }
    fn catch_up(
        &self,
        segments: &[SegId],
        footers: &dyn lunix_iface::store::SegmentFooters,
    ) -> Result<lunix_iface::index::CatchUpReport> {
        self.inner.catch_up(segments, footers)
    }
    fn rebuild(
        &self,
        segments: &[SegId],
        footers: &dyn lunix_iface::store::SegmentFooters,
    ) -> Result<()> {
        self.inner.rebuild(segments, footers)
    }
    fn delete_run(&self, run: lunix_iface::index::RunRef) -> Result<u64> {
        self.inner.delete_run(run)
    }
    fn iter_entries(&self, f: &mut dyn FnMut(&lunix_iface::index::IndexedChunk)) -> Result<()> {
        self.inner.iter_entries(f)
    }
}

#[test]
fn radix_map_packs_shared_between_roots_stay() {
    let mut cfg = SimCfg::new(CS, IdLen::Sixteen);
    cfg.map = MapKind::Radix;
    cfg.chunk_count = 4096;
    let mut s = Sim::new(cfg);
    for round in 0..8u64 {
        for i in 0..40u64 {
            s.write((i * 97 + round * 13) % 4096, round * 100 + i);
        }
        s.commit().unwrap();
        s.clock.advance(GRACE);
        s.pass().unwrap();
        s.check_roots().unwrap();
    }
    s.quiesce().unwrap();
    s.check_roots().unwrap();
    assert!(s.gc.totals().packs_deleted > 0);
}

#[test]
fn an_unnamed_sealed_segment_above_the_high_water_mark_is_never_condemned() {
    let mut s = sim();
    for i in 0..6 {
        s.write(i, i);
    }
    s.commit().unwrap();
    // A segment sealed by a cut whose FULL is not durable yet, or an orphan
    // the open-time sweep has not seen: nothing names or references it.
    let mut w = s.store.open_segment().unwrap();
    let id = id_of(4711);
    let mut ext = Vec::new();
    s.codec
        .seal(
            ExtentKind::Data,
            &id,
            &lunix_iface::crypto::EncodingDesc {
                codec: lunix_format::Codec::Raw,
                level: lunix_format::Level(0),
                ulen: CS,
                dict_id: None,
            },
            &data_of(4711, CS),
            &mut ext,
        )
        .unwrap();
    w.append_extent(&id, None, &ext).unwrap();
    let sealed = w.seal(None).unwrap();
    lunix_iface::space::SpaceGate::charge(
        &*s.space,
        lunix_iface::space::SpaceKind::Segments,
        sealed.size_bytes,
    )
    .unwrap();
    let orphan = sealed.seg;
    assert!(orphan.raw() > high_water(&s.retained));
    for _ in 0..3 {
        s.clock.advance(GRACE * 2);
        s.urgent(u64::MAX).unwrap();
    }
    assert!(segs(&s).contains(&orphan));
    assert!(!s.gc.condemned().contains(&ObjectRef::Segment(orphan)));
    // Once a FULL lifts the mark above it, it is an ordinary orphan.
    s.write(30, 30);
    s.commit().unwrap();
    s.quiesce().unwrap();
    assert!(!segs(&s).contains(&orphan));
    s.check_roots().unwrap();
    s.check_ledger().unwrap();
}

#[test]
fn a_dead_output_is_not_deleted_before_a_cut_names_it() {
    let mut s = sim();
    low_live_segment(&mut s);
    s.clock.advance(Duration::from_secs(5));
    s.pass().unwrap();
    let out = s.moved.first().expect("compacted").new.seg;
    // Cuts that do not carry the moves (a lagging wiring) overwrite the
    // moved chunk and roll its old roots off: the output is dead, unnamed
    // and below the high-water mark.
    let moved = std::mem::take(&mut s.moved);
    s.write(0, 5150);
    for _ in 0..4 {
        s.commit().unwrap();
    }
    assert!(out.raw() < high_water(&s.retained));
    let l = s.with_view(|v| crate::liveness(v, s.cfg.gc)).unwrap();
    assert_eq!(l.live_bytes(out), 0);
    for _ in 0..3 {
        s.clock.advance(GRACE * 2);
        s.urgent(u64::MAX).unwrap();
    }
    assert!(
        segs(&s).contains(&out),
        "an unnamed output stays until it is named"
    );
    // The cut that applies the moves names it; the chain never names a
    // missing segment, and afterwards it is an ordinary dead segment.
    s.moved = moved;
    s.commit().unwrap();
    assert!(s.tip.new_segments.contains(&out));
    s.check_roots().unwrap();
    s.quiesce().unwrap();
    assert!(!segs(&s).contains(&out));
    s.check_roots().unwrap();
}

#[test]
fn a_copy_named_by_a_full_that_rolled_off_before_any_step_still_counts_as_durable() {
    let mut s = sim();
    for i in 0..4 {
        s.write(i, 600 + i);
    }
    s.commit().unwrap();
    let src = hint_of(&s, 0).seg;
    for i in 1..4 {
        s.write(i, 700 + i);
    }
    s.commit().unwrap();
    // Only this pinned root still reaches the source (chunk 0).
    s.pin(u64::MAX);
    let pinned = s.pins[0].commit_seq;
    assert_eq!(pinned, s.tip.common.commit_seq);
    s.write(0, 800);
    for _ in 0..3 {
        s.commit().unwrap();
    }
    s.clock.advance(Duration::from_secs(5));
    s.pass().unwrap();
    let out = s.moved.first().expect("the source was compacted").new.seg;
    // The FULL that applies the move re-hints nothing (the tip no longer has
    // the chunk), so the map does not change; it names the output, then rolls
    // off before the collector runs again: the mark falls below the output.
    for _ in 0..5 {
        s.commit().unwrap();
    }
    assert!(out.raw() > high_water(&s.retained));
    for _ in 0..4 {
        s.clock.advance(GRACE * 2);
        s.commit().unwrap();
        s.pass().unwrap();
    }
    assert!(
        !segs(&s).contains(&src),
        "the source goes: its live copy is durable"
    );
    check_index_invariant(&s);
    s.check_roots().unwrap();
}
