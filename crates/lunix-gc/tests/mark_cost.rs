//! N25: what one mark costs as the volume grows (M17.A re-review; gc.md
//! "Steps and their bounds"). Each leaf unit of the mark traverses the whole
//! root with the other leaves in the skip set, so a mark does about
//! `leaves² / 32` skip-set lookups. This measures it on the real map
//! (`lunix-map`, fanout 512) over the testkit's in-memory store and index,
//! with the real clock: the wall time of a whole mark (`liveness`) and the
//! longest budgeted step of a pass while it marks.
//!
//! Ignored by default (it builds the whole volume in memory: about 250 bytes
//! per chunk); `bench/phase1/gc/run.sh mark` runs it at 1, 4, 16 and 64 M
//! chunks:
//!
//! ```text
//! LUNIX_GC_MARK_CHUNKS=67108864 cargo test --release -p lunix-gc \
//!   --test mark_cost -- --ignored --nocapture
//! ```

use std::sync::Arc;
use std::time::{Duration, Instant};

use lunix_format::commit::{Commit, CommitKind, CommonHeader, Full};
use lunix_format::node::DataEntry;
use lunix_format::{
    ChunkId, Codec, CommitSeq, Epoch, ExtentKind, IdLen, IdPrefix, Level, LineageIdx, WriteSeq,
};
use lunix_gc::{Gc, GcConfig};
use lunix_iface::clock::{Clock, SystemClock};
use lunix_iface::crypto::{CommitCodec, EncodingDesc, ExtentCodec};
use lunix_iface::gc::{Collector, GcBudget, GcParams, GcState, GcView, TipInfo};
use lunix_iface::index::ChunkIndex;
use lunix_iface::map::{BlockMap, ChunkNo, NodeSource};
use lunix_iface::store::{CommitStore, PackStore, SegmentStore};
use lunix_testkit::{MemIndex, MemSpace, MemSpaceConfig, MemStore, MemStoreConfig, PlainCodec};

const CS: u32 = 16 << 10;
/// Chunks per cut: below the map's dirty-leaf cap (20 000 leaves).
const BATCH: u64 = 8 << 20;

fn id(i: u64) -> ChunkId {
    let mut b = [0u8; 32];
    b[..8].copy_from_slice(&i.to_le_bytes());
    b[16..24].copy_from_slice(&(i ^ 0x5a5a_5a5a).wrapping_mul(0x9e37_79b9).to_le_bytes());
    ChunkId(b)
}

fn high_water(commits: &[Commit]) -> u64 {
    commits
        .iter()
        .filter_map(|c| match c {
            Commit::Full(f) => Some(f),
            _ => None,
        })
        .flat_map(|f| {
            f.new_segments
                .iter()
                .chain(&f.map_packs)
                .chain(f.map_root.pack.iter())
                .map(|x| x.raw())
                .collect::<Vec<_>>()
        })
        .max()
        .unwrap_or(0)
}

#[test]
#[ignore = "a benchmark: bench/phase1/gc/run.sh mark"]
fn mark_cost() {
    let chunks: u64 = std::env::var("LUNIX_GC_MARK_CHUNKS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1 << 20);
    let built = Instant::now();
    let store = MemStore::with_config(MemStoreConfig {
        segment_target: 1 << 20,
        ..MemStoreConfig::default()
    });
    let codec = Arc::new(PlainCodec::new());
    let index = Arc::new(MemIndex::new(store.mem_ids()));
    let source: Arc<dyn NodeSource> = Arc::new(store.clone());
    let map = lunix_map::RadixMap::new(
        codec.clone(),
        source,
        chunks,
        lunix_map::MapConfig::new(512, IdLen::Sixteen).with_cache_max(64 << 20),
    )
    .expect("map");
    let clock = SystemClock;
    let genesis = Full::genesis(
        [1; 32],
        chunks * u64::from(CS),
        map.root().hash,
        clock.now(),
    );
    let sealed = codec
        .seal_commit(&Commit::Full(genesis.clone()))
        .expect("seal");
    store.put(CommitSeq(0), &sealed.bytes).expect("put");
    let mut retained = vec![Commit::Full(genesis.clone())];
    let mut tip = genesis;
    let mut ext = Vec::new();
    let mut start = 0;
    while start < chunks {
        let end = (start + BATCH).min(chunks);
        let mut w = store.open_segment().expect("segment");
        let mut segs = Vec::new();
        let mut txn = map.begin_update().expect("txn");
        for i in start..end {
            let cid = id(i);
            ext.clear();
            codec
                .seal(
                    ExtentKind::Data,
                    &cid,
                    &EncodingDesc {
                        codec: Codec::Lz4,
                        level: Level(0),
                        ulen: CS,
                        dict_id: None,
                    },
                    &i.to_le_bytes(),
                    &mut ext,
                )
                .expect("extent");
            if w.is_full() {
                segs.push(w.seal(None).expect("seal").seg);
                w = store.open_segment().expect("segment");
            }
            let h = w.append_extent(&cid, None, &ext).expect("append");
            index.insert(cid, h).expect("index");
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
            .expect("set");
        }
        txn.commit().expect("commit txn");
        segs.push(w.seal(None).expect("seal").seg);
        let mut pack = store.open_pack().expect("pack");
        let res = map.seal(&mut *pack).expect("map seal");
        let map_packs = if pack.is_empty() {
            Vec::new()
        } else {
            vec![pack.seal().expect("pack seal").pack]
        };
        let seq = CommitSeq(tip.common.commit_seq.0 + 1);
        let full = Full {
            common: CommonHeader {
                kind: CommitKind::Full,
                commit_seq: seq,
                epoch: Epoch(0),
                parent_hash: codec.object_hash(&store.get(tip.common.commit_seq).expect("get")),
                created_at: clock.now(),
            },
            cut_seq: WriteSeq(seq.0),
            size: tip.size,
            chain_floor: CommitSeq(seq.0.saturating_sub(1)),
            map_root: res.root,
            heat: None,
            map_packs,
            new_segments: segs,
            index_packs: Vec::new(),
        };
        let sealed = codec
            .seal_commit(&Commit::Full(full.clone()))
            .expect("seal");
        store.put(seq, &sealed.bytes).expect("put");
        tip = full.clone();
        retained.push(Commit::Full(full));
        retained.retain(|c| c.common().commit_seq.0 + 1 >= seq.0);
        start = end;
    }
    // Two FULLs that change nothing: the retained window holds the whole
    // volume in every root, as on a quiet volume.
    for _ in 0..2 {
        let seq = CommitSeq(tip.common.commit_seq.0 + 1);
        let mut full = tip.clone();
        full.common.commit_seq = seq;
        full.common.parent_hash =
            codec.object_hash(&store.get(tip.common.commit_seq).expect("get"));
        full.chain_floor = CommitSeq(seq.0 - 1);
        full.map_packs = Vec::new();
        full.new_segments = Vec::new();
        let sealed = codec
            .seal_commit(&Commit::Full(full.clone()))
            .expect("seal");
        store.put(seq, &sealed.bytes).expect("put");
        tip = full.clone();
        retained.push(Commit::Full(full));
        retained.retain(|c| c.common().commit_seq.0 + 1 >= seq.0);
    }
    let build = built.elapsed();
    let space = MemSpace::new(
        MemSpaceConfig::new(1 << 50),
        Arc::new(SystemClock) as Arc<dyn Clock>,
    );
    let params = GcParams {
        retain_last: 2,
        grace: Duration::from_secs(60),
        read_timeout: Duration::from_secs(10),
        rate_bytes_per_s: 64 << 20,
        live_threshold: 0.5,
        chunk_size: CS,
        id_len: IdLen::Sixteen,
    };
    let view = GcView {
        retained: &retained,
        tip: TipInfo {
            commit_seq: tip.common.commit_seq,
            map_root: tip.map_root,
            chain_floor: tip.chain_floor,
            high_water_mark: high_water(&retained),
        },
        pins: &store,
        map: &map,
        index: &*index,
        segments: &store,
        packs: &store,
        commits: &store,
        commit_codec: &*codec,
        extents: &*codec,
        state: &store,
        space: &*space,
        clock: &clock,
        params,
    };
    // A whole mark.
    let t0 = Instant::now();
    let l = lunix_gc::liveness(&view, GcConfig::default()).expect("mark");
    let mark = t0.elapsed();
    // The mark of a pass in budgeted steps: the longest step.
    let mut gc = Gc::new(Arc::new(store.clone()), GcConfig::default());
    let budget = GcBudget {
        max_duration: Duration::from_millis(50),
        max_bytes: 4 << 20,
    };
    let (mut worst, mut steps) = (Duration::ZERO, 0u64);
    let t1 = Instant::now();
    loop {
        let s0 = Instant::now();
        let p = gc.step(&view, budget).expect("step");
        worst = worst.max(s0.elapsed());
        steps += 1;
        if !matches!(p.state, GcState::Marking) && steps > 1 {
            break;
        }
    }
    let leaves = chunks.div_ceil(512);
    eprintln!(
        "mark cost: {chunks} chunks ({} GiB at 16 KiB), {leaves} leaves, {} roots, {} live extents; \
         build {build:.1?}; whole mark {mark:.2?}; pass mark in {steps} steps over {:.2?}, \
         longest step {worst:.1?} (budget 50 ms); about {} skip-set lookups per mark (leaves^2/32)",
        (chunks * u64::from(CS)) >> 30,
        l.roots.len(),
        l.live_count,
        t1.elapsed(),
        leaves * leaves / 32
    );
}
