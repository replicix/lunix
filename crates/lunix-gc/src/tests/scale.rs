//! Step budgets and memory on a 4 M-chunk fake volume (gate). The fakes cost
//! nothing on a manual clock, so the stores are wrapped in a cost model that
//! advances the clock per operation (stated below); no step may then take
//! longer than twice its budget, and the collector's own structures beyond
//! the live-extent set stay under 64 MiB.
//!
//! Cost model (deliberately pessimistic for an in-memory volume): map walks
//! 0.5 µs per leaf entry, 2 µs per node, 20 µs per call plus 20 ns per entry of
//! the skip set (the traversal checks every leaf against it); an index lookup
//! 1 µs, a prefix lookup 5 µs, a condemn flag 200 µs (a manifest write); a
//! footer 50 µs plus 0.1 µs per entry, an extent read 5 µs, a delete 100 µs.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use lunix_format::commit::{Commit, CommitKind, CommonHeader, Full, MapRoot};
use lunix_format::node::DataEntry;
use lunix_format::segment::FooterBody;
use lunix_format::{
    ChunkId, Codec, CommitSeq, Epoch, ExtentKind, Hint, IdLen, IdPrefix, Level, LineageIdx, SegId,
    TimeMs, WriteSeq,
};
use lunix_iface::clock::{Clock, ManualClock};
use lunix_iface::crypto::{CommitCodec, EncodingDesc, ExtentCodec};
use lunix_iface::gc::{Collector, GcBudget, GcParams, GcView, TipInfo};
use lunix_iface::index::{CatchUpReport, ChunkIndex, DedupHit, IndexedChunk, RunRef};
use lunix_iface::map::{BlockMap, Cached, ChunkNo, MapTxn, NodeHash, NodeRef, SealResult};
use lunix_iface::store::{
    CommitStore, ExtentLocation, PackStore, PackWrite, SegmentFooters, SegmentInfo, SegmentStore,
    SegmentWriter,
};
use lunix_iface::Result;
use lunix_testkit::{MemIndex, MemSpace, MemSpaceConfig, MemStore, MemStoreConfig, PlainCodec};

use super::sim::high_water;
use crate::{Gc, GcConfig};

struct Cost(Arc<ManualClock>);

impl Cost {
    fn ns(&self, n: u64) {
        self.0.advance(Duration::from_nanos(n));
    }
}

struct CostMap<'a>(&'a dyn BlockMap, &'a Cost);

impl BlockMap for CostMap<'_> {
    fn chunk_count(&self) -> u64 {
        self.0.chunk_count()
    }
    fn get(&self, ci: ChunkNo) -> Result<Option<DataEntry>> {
        self.0.get(ci)
    }
    fn try_get_cached(&self, ci: ChunkNo) -> Cached {
        self.0.try_get_cached(ci)
    }
    fn begin_update(&self) -> Result<Box<dyn MapTxn + '_>> {
        self.0.begin_update()
    }
    fn seal(&self, packs: &mut dyn PackWrite) -> Result<SealResult> {
        self.0.seal(packs)
    }
    fn grow(&self, n: u64) -> Result<()> {
        self.0.grow(n)
    }
    fn root(&self) -> MapRoot {
        self.0.root()
    }
    fn dirty_leaf_count(&self) -> u64 {
        self.0.dirty_leaf_count()
    }
    fn walk_leaves_skipping(
        &self,
        root: &MapRoot,
        skip: &HashSet<NodeHash>,
        f: &mut dyn FnMut(ChunkNo, &DataEntry),
    ) -> Result<()> {
        // The traversal checks every leaf of the root against the skip set.
        self.1.ns(20_000 + 20 * skip.len() as u64);
        self.0.walk_leaves_skipping(root, skip, &mut |c, e| {
            self.1.ns(500);
            f(c, e);
        })
    }
    fn walk_nodes(&self, root: &MapRoot, f: &mut dyn FnMut(&NodeRef)) -> Result<()> {
        self.walk_nodes_skipping(root, &HashSet::new(), f)
    }
    fn walk_nodes_skipping(
        &self,
        root: &MapRoot,
        skip: &HashSet<NodeHash>,
        f: &mut dyn FnMut(&NodeRef),
    ) -> Result<()> {
        self.1.ns(20_000);
        self.0.walk_nodes_skipping(root, skip, &mut |n| {
            self.1.ns(2_000);
            f(n);
        })
    }
}

struct CostIndex<'a>(&'a MemIndex, &'a Cost);

impl ChunkIndex for CostIndex<'_> {
    fn get_for_dedup(&self, id: &ChunkId) -> Result<Option<DedupHit>> {
        self.0.get_for_dedup(id)
    }
    fn get(&self, id: &ChunkId) -> Result<Option<IndexedChunk>> {
        self.1.ns(1_000);
        self.0.get(id)
    }
    fn get_by_prefix(&self, p: &IdPrefix) -> Result<Vec<IndexedChunk>> {
        self.1.ns(5_000);
        self.0.get_by_prefix(p)
    }
    fn insert(&self, id: ChunkId, hint: Hint) -> Result<()> {
        self.0.insert(id, hint)
    }
    fn relocate(&self, id: &ChunkId, old: &Hint, new: &Hint) -> Result<bool> {
        self.1.ns(1_000);
        self.0.relocate(id, old, new)
    }
    fn mark_condemned(&self, seg: SegId) -> Result<()> {
        self.1.ns(200_000);
        self.0.mark_condemned(seg)
    }
    fn mark_condemned_all(&self, segs: &[SegId]) -> Result<()> {
        // One manifest swap however many segments.
        self.1.ns(200_000);
        self.0.mark_condemned_all(segs)
    }
    fn unmark_condemned_all(&self, segs: &[SegId]) -> Result<()> {
        self.1.ns(200_000);
        self.0.unmark_condemned_all(segs)
    }
    fn retire_segments(&self, segs: &[SegId]) -> Result<()> {
        self.1.ns(200_000);
        self.0.retire_segments(segs)
    }
    fn unmark_condemned(&self, seg: SegId) -> Result<()> {
        self.1.ns(200_000);
        self.0.unmark_condemned(seg)
    }
    fn condemned(&self) -> Result<Vec<SegId>> {
        self.0.condemned()
    }
    fn drop_segment(&self, seg: SegId) -> Result<()> {
        self.1.ns(200_000);
        self.0.drop_segment(seg)
    }
    fn persist(&self, upto: SegId) -> Result<Vec<RunRef>> {
        self.0.persist(upto)
    }
    fn runs(&self) -> Result<Vec<RunRef>> {
        self.0.runs()
    }
    fn all_runs(&self) -> Result<Vec<RunRef>> {
        self.0.all_runs()
    }
    fn catch_up(&self, s: &[SegId], f: &dyn SegmentFooters) -> Result<CatchUpReport> {
        self.0.catch_up(s, f)
    }
    fn rebuild(&self, s: &[SegId], f: &dyn SegmentFooters) -> Result<()> {
        self.0.rebuild(s, f)
    }
    fn delete_run(&self, run: RunRef) -> Result<u64> {
        self.1.ns(100_000);
        self.0.delete_run(run)
    }
    fn iter_entries(&self, f: &mut dyn FnMut(&IndexedChunk)) -> Result<()> {
        self.0.iter_entries(f)
    }
}

struct CostSegs<'a>(&'a MemStore, &'a Cost);

impl SegmentStore for CostSegs<'_> {
    fn open_segment(&self) -> Result<Box<dyn SegmentWriter>> {
        self.0.open_segment()
    }
    fn read_extent(&self, hint: &Hint, out: &mut Vec<u8>) -> Result<()> {
        self.1.ns(5_000);
        self.0.read_extent(hint, out)
    }
    fn locate(&self, hint: &Hint) -> Result<ExtentLocation> {
        self.0.locate(hint)
    }
    fn footer(&self, seg: SegId) -> Result<FooterBody> {
        let f = self.0.footer(seg)?;
        self.1.ns(50_000 + 100 * f.entries.len() as u64);
        Ok(f)
    }
    fn list(&self) -> Result<Vec<SegmentInfo>> {
        let l = SegmentStore::list(self.0)?;
        self.1.ns(10_000 + 50 * l.len() as u64);
        Ok(l)
    }
    fn delete(&self, seg: SegId) -> Result<u64> {
        self.1.ns(100_000);
        SegmentStore::delete(self.0, seg)
    }
}

const CHUNKS: u64 = 4 << 20;

fn id(i: u64, round: u64) -> ChunkId {
    let mut b = [0u8; 32];
    b[..8].copy_from_slice(&i.to_le_bytes());
    b[8..16].copy_from_slice(&round.to_le_bytes());
    b[16..24].copy_from_slice(&(i ^ 0x5a5a_5a5a).wrapping_mul(0x9e37_79b9).to_le_bytes());
    ChunkId(b)
}

struct Vol {
    store: MemStore,
    index: Arc<MemIndex>,
    map: Arc<dyn BlockMap>,
    codec: Arc<PlainCodec>,
    retained: Vec<Commit>,
    tip: Full,
}

impl Vol {
    /// Writes chunks `range` (fake LZ4 payloads of 16 bytes: nothing here
    /// decompresses them) as one cut and commits it.
    fn cut(&mut self, range: std::ops::Range<u64>, round: u64) {
        let mut w: Option<Box<dyn SegmentWriter>> = None;
        let mut segs = Vec::new();
        let mut txn = self.map.begin_update().unwrap();
        let mut ext = Vec::new();
        for i in range {
            let cid = id(i, round);
            ext.clear();
            self.codec
                .seal(
                    ExtentKind::Data,
                    &cid,
                    &EncodingDesc {
                        codec: Codec::Lz4,
                        level: Level(0),
                        ulen: 16 << 10,
                        dict_id: None,
                    },
                    &(i ^ round).to_le_bytes().repeat(2),
                    &mut ext,
                )
                .unwrap();
            if w.as_ref().is_some_and(|w| w.is_full()) {
                segs.push(w.take().unwrap().seal(None).unwrap().seg);
            }
            let wr = match &mut w {
                Some(w) => w,
                None => w.insert(self.store.open_segment().unwrap()),
            };
            let h = wr.append_extent(&cid, None, &ext).unwrap();
            self.index.insert(cid, h).unwrap();
            txn.set(
                ChunkNo(i),
                DataEntry {
                    lineage: LineageIdx(0),
                    seg: h.seg,
                    off: h.off,
                    clen: h.clen,
                    prefix: IdPrefix::of(&cid, IdLen::Sixteen),
                },
            )
            .unwrap();
        }
        txn.commit().unwrap();
        if let Some(w) = w {
            segs.push(w.seal(None).unwrap().seg);
        }
        self.commit(segs);
    }

    fn commit(&mut self, new_segments: Vec<SegId>) {
        let mut pack = self.store.open_pack().unwrap();
        let res = self.map.seal(&mut *pack).unwrap();
        let mut map_packs = Vec::new();
        if !pack.is_empty() {
            map_packs.push(pack.seal().unwrap().pack);
        }
        let seq = CommitSeq(self.tip.common.commit_seq.0 + 1);
        let full = Full {
            common: CommonHeader {
                kind: CommitKind::Full,
                commit_seq: seq,
                epoch: Epoch(0),
                parent_hash: self
                    .codec
                    .object_hash(&self.store.get(self.tip.common.commit_seq).unwrap()),
                created_at: TimeMs(1),
            },
            cut_seq: WriteSeq(seq.0),
            size: self.tip.size,
            chain_floor: CommitSeq(seq.0.saturating_sub(1)),
            map_root: res.root,
            heat: None,
            map_packs,
            new_segments,
            index_packs: Vec::new(),
        };
        let sealed = self.codec.seal_commit(&Commit::Full(full.clone())).unwrap();
        self.store.put(seq, &sealed.bytes).unwrap();
        self.tip = full.clone();
        self.retained.push(Commit::Full(full));
        self.retained
            .retain(|c| c.common().commit_seq.0 + 1 >= seq.0);
    }
}

#[test]
fn a_pass_over_four_million_chunks_stays_within_its_step_budgets() {
    let chunks: u64 = std::env::var("LUNIX_GC_SCALE_CHUNKS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(CHUNKS);
    let clock = Arc::new(ManualClock::new(TimeMs(1)));
    let cost = Cost(clock.clone());
    let store = MemStore::with_config(MemStoreConfig {
        segment_target: 1 << 20,
        ..MemStoreConfig::default()
    });
    let codec = Arc::new(PlainCodec::new());
    let index = Arc::new(MemIndex::new(store.mem_ids()));
    let map: Arc<dyn BlockMap> = Arc::new(
        lunix_map::RadixMap::new(
            codec.clone(),
            Arc::new(store.clone()),
            chunks,
            lunix_map::MapConfig::new(512, IdLen::Sixteen).with_cache_max(32 << 20),
        )
        .unwrap(),
    );
    let genesis = Full::genesis([1; 32], chunks * (16 << 10), map.root().hash, TimeMs(1));
    let sealed = codec.seal_commit(&Commit::Full(genesis.clone())).unwrap();
    store.put(CommitSeq(0), &sealed.bytes).unwrap();
    let mut v = Vol {
        store: store.clone(),
        index: index.clone(),
        map: map.clone(),
        codec: codec.clone(),
        retained: vec![Commit::Full(genesis.clone())],
        tip: genesis,
    };
    v.cut(0..chunks, 0);
    // Overwrite the first quarter, then roll the first root off.
    v.cut(0..chunks / 4, 1);
    v.commit(Vec::new());

    let space = MemSpace::new(MemSpaceConfig::new(1 << 40), clock.clone());
    let seg_bytes: u64 = SegmentStore::list(&store)
        .unwrap()
        .iter()
        .map(|s| s.size_bytes)
        .sum();
    let pack_bytes: u64 = lunix_iface::store::PackRead::list_packs(&store)
        .unwrap()
        .iter()
        .map(|p| p.size_bytes)
        .sum();
    use lunix_iface::space::{SpaceGate, SpaceKind};
    space.charge(SpaceKind::Segments, seg_bytes).unwrap();
    space.charge(SpaceKind::Map, pack_bytes).unwrap();
    space
        .charge(
            SpaceKind::Commits,
            CommitStore::list(&store)
                .unwrap()
                .iter()
                .map(|c| c.size_bytes)
                .sum(),
        )
        .unwrap();

    let budget = GcBudget {
        max_duration: Duration::from_millis(50),
        max_bytes: 4 << 20,
    };
    let cmap = CostMap(&*map, &cost);
    let cindex = CostIndex(&index, &cost);
    let csegs = CostSegs(&store, &cost);
    let params = GcParams {
        retain_last: 2,
        grace: Duration::from_secs(60),
        read_timeout: Duration::from_secs(10),
        rate_bytes_per_s: 64 << 20,
        live_threshold: 0.5,
        chunk_size: 16 << 10,
        id_len: IdLen::Sixteen,
    };
    let view = GcView {
        retained: &v.retained,
        tip: TipInfo {
            commit_seq: v.tip.common.commit_seq,
            map_root: v.tip.map_root,
            chain_floor: v.tip.chain_floor,
            high_water_mark: high_water(&v.retained),
        },
        pins: &store,
        map: &cmap,
        index: &cindex,
        segments: &csegs,
        packs: &store,
        commits: &store,
        commit_codec: &*codec,
        extents: &*codec,
        state: &store,
        space: &*space,
        clock: &*clock,
        params,
    };
    let mut gc = Gc::new(Arc::new(store.clone()), GcConfig::default());
    let (mut worst, mut steps, mut freed) = (Duration::ZERO, 0u64, 0u64);
    let mut done = false;
    for _ in 0..1_000_000 {
        let t0 = clock.monotonic();
        let p = gc.step(&view, budget).unwrap();
        worst = worst.max(clock.monotonic() - t0);
        steps += 1;
        freed += p.freed;
        if p.done {
            done = true;
            break;
        }
        clock.advance(Duration::from_secs(1));
    }
    assert!(done);
    let l = crate::liveness(
        &GcView {
            map: &*map,
            index: &*index,
            segments: &store,
            ..view
        },
        GcConfig::default(),
    )
    .unwrap();
    let live_set = l.live_set_bytes();
    eprintln!(
        "scale: {chunks} chunks, {} live extents ({} MiB live set), {steps} steps, worst step {worst:?} (budget {:?}), peak heap beyond the live set {} MiB, freed {} MiB",
        l.live_count,
        live_set >> 20,
        budget.max_duration,
        gc.peak_heap_beyond_live() >> 20,
        freed >> 20,
    );
    assert!(worst <= budget.max_duration * 2, "worst step {worst:?}");
    assert!(gc.peak_heap_beyond_live() <= 64 << 20);
    assert!(freed > 0, "the overwritten quarter's segments were deleted");
}
