//! A small volume over the testkit fakes for the directed tests of M17.B:
//! FULL commits made like M11's coordinator does (segments sealed, the map
//! updated and sealed into a pack, the FULL, then the index inserts and a
//! run), with the collector driven between them through its public API.
//!
//! Unlike the crate's own simulation it never dedups (every write stores a
//! new extent, so a test can place a second copy of a chunk where it wants)
//! and it can hold back the collector's moves (a lagging wiring) to build
//! the states the delete-time index invariant guards.
#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;
use std::time::Duration;

use lunix_format::commit::{Commit, CommitKind, CommonHeader, Full};
use lunix_format::local::RollbackFloor;
use lunix_format::node::DataEntry;
use lunix_format::{
    ChunkId, Codec, CommitSeq, CryptoMode, Epoch, ExtentKind, Hint, IdLen, IdPrefix, Level,
    LineageIdx, ObjId, SegId, TimeMs, WriteSeq,
};
use lunix_gc::{Gc, GcConfig};
use lunix_iface::clock::{Clock, ManualClock};
use lunix_iface::crypto::{CommitCodec, EncodingDesc, ExtentCodec};
use lunix_iface::gc::{Collector, GcBudget, GcParams, GcProgress, GcState, GcView, Moved, TipInfo};
use lunix_iface::index::ChunkIndex;
use lunix_iface::map::{entry_hint, BlockMap, ChunkNo, NodeSource};
use lunix_iface::space::{SpaceGate, SpaceKind};
use lunix_iface::store::{CommitStore, PackStore, SegmentStore, SegmentWriter, VolumeDir};
use lunix_iface::Result;
use lunix_testkit::{MemIndex, MemSpace, MemSpaceConfig, MemStore, MemStoreConfig, PlainCodec};

pub const GRACE: Duration = Duration::from_secs(60);
pub const CHUNK: u32 = 16 << 10;
pub const CHUNKS: u64 = 64;

/// The bytes of a chunk with data seed `seed`.
pub fn data_of(seed: u64) -> Vec<u8> {
    let mut v = vec![0u8; CHUNK as usize];
    let mut z = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
    for c in v.chunks_mut(8).take(64) {
        z ^= z << 13;
        z ^= z >> 7;
        z ^= z << 17;
        c.copy_from_slice(&z.to_le_bytes()[..c.len()]);
    }
    v[..8].copy_from_slice(&seed.to_le_bytes());
    v
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
                .chain(&f.index_packs)
                .chain(f.map_root.pack.iter())
                .map(|id| id.raw())
                .collect::<Vec<_>>()
        })
        .max()
        .unwrap_or(0)
}

pub struct Vol {
    pub store: MemStore,
    pub index: Arc<MemIndex>,
    pub map: Arc<dyn BlockMap>,
    pub codec: Arc<dyn ExtentCodec>,
    pub commit_codec: Arc<PlainCodec>,
    pub space: Arc<MemSpace>,
    pub clock: Arc<ManualClock>,
    pub gc: Gc,
    pub cfg: GcConfig,
    pub retained: Vec<Commit>,
    pub tip: Full,
    /// Moves the collector returned that no cut applied yet.
    pub moved: Vec<Moved>,
    upto: Option<SegId>,
    runs_unnamed: Vec<ObjId>,
    pending: BTreeMap<u64, Option<u64>>,
    /// Chunks of the volume.
    pub chunks: u64,
}

impl Vol {
    /// An empty volume of [`CHUNKS`] chunks whose segments hold up to
    /// `extents_per_segment` extents.
    pub fn new(extents_per_segment: u64) -> Self {
        Self::sized(extents_per_segment, CHUNKS)
    }

    /// A volume of `chunks` chunks (the map has fanout 256: more than 256
    /// chunks make an interior root over several leaves).
    pub fn sized(extents_per_segment: u64, chunks: u64) -> Self {
        let store = MemStore::with_config(MemStoreConfig {
            segment_target: u64::from(CHUNK) * extents_per_segment,
            mode: CryptoMode::Integrity,
            ..MemStoreConfig::default()
        });
        let codec: Arc<dyn ExtentCodec> = Arc::new(PlainCodec::new());
        let commit_codec = Arc::new(PlainCodec::new());
        let index = Arc::new(MemIndex::new(store.mem_ids()));
        let source: Arc<dyn NodeSource> = Arc::new(store.clone());
        let map: Arc<dyn BlockMap> = Arc::new(
            lunix_map::RadixMap::new(
                codec.clone(),
                source,
                chunks,
                lunix_map::MapConfig::new(256, IdLen::Sixteen),
            )
            .expect("map"),
        );
        let clock = Arc::new(ManualClock::new(TimeMs(1_700_000_000_000)));
        let space = MemSpace::new(MemSpaceConfig::new(1 << 40), clock.clone());
        let genesis = Full::genesis(
            [0x11; 32],
            chunks * u64::from(CHUNK),
            map.root().hash,
            clock.now(),
        );
        let sealed = commit_codec
            .seal_commit(&Commit::Full(genesis.clone()))
            .expect("seal genesis");
        store.put(CommitSeq(0), &sealed.bytes).expect("genesis");
        space
            .charge(SpaceKind::Commits, sealed.bytes.len() as u64)
            .expect("charge");
        store
            .raise_rollback_floor(&RollbackFloor {
                max_commit_seq: CommitSeq(0),
                tip_hash: sealed.hash,
            })
            .expect("floor");
        let cfg = GcConfig {
            pass_interval: Duration::from_secs(1),
            ..GcConfig::default()
        };
        let gc = Gc::new(Arc::new(store.clone()), cfg);
        Self {
            store,
            index,
            map,
            codec,
            commit_codec,
            space,
            clock,
            gc,
            cfg,
            retained: vec![Commit::Full(genesis.clone())],
            tip: genesis,
            moved: Vec::new(),
            upto: None,
            runs_unnamed: Vec::new(),
            pending: BTreeMap::new(),
            chunks,
        }
    }

    pub fn params(&self) -> GcParams {
        GcParams {
            retain_last: 2,
            grace: GRACE,
            read_timeout: Duration::from_secs(10),
            rate_bytes_per_s: 64 << 20,
            live_threshold: 0.5,
            chunk_size: CHUNK,
            id_len: IdLen::Sixteen,
        }
    }

    /// A collector as a restarted process builds it (nothing in memory).
    pub fn restart_collector(&mut self) {
        self.gc = Gc::new(Arc::new(self.store.clone()), self.cfg);
    }

    fn with_view<R>(&self, f: impl FnOnce(&GcView<'_>) -> R) -> R {
        let view = GcView {
            retained: &self.retained,
            tip: TipInfo {
                commit_seq: self.tip.common.commit_seq,
                map_root: self.tip.map_root,
                chain_floor: self.tip.chain_floor,
                high_water_mark: high_water(&self.retained),
            },
            pins: &self.store,
            map: &*self.map,
            index: &*self.index,
            segments: &self.store,
            packs: &self.store,
            commits: &self.store,
            commit_codec: &*self.commit_codec,
            extents: &*self.codec,
            state: &self.store,
            space: &*self.space,
            clock: &*self.clock,
            params: self.params(),
        };
        f(&view)
    }

    /// One collector step; its moves wait for a cut.
    pub fn step(&mut self) -> Result<GcProgress> {
        let budget = GcBudget {
            max_duration: Duration::from_millis(50),
            max_bytes: 4 << 20,
        };
        let mut gc = std::mem::replace(
            &mut self.gc,
            Gc::new(Arc::new(self.store.clone()), self.cfg),
        );
        let r = self.with_view(|v| gc.step(v, budget));
        self.gc = gc;
        let p = r?;
        self.moved.extend(p.moved.iter().copied());
        Ok(p)
    }

    /// Steps until a pass is done (or there is nothing to do), advancing the
    /// clock past every grace; no cut in between.
    pub fn pass(&mut self) -> Result<()> {
        for _ in 0..100_000 {
            let p = self.step()?;
            if p.done || p.state == GcState::Idle {
                return Ok(());
            }
            self.clock.advance(if p.state == GcState::Grace {
                GRACE
            } else {
                Duration::from_secs(5)
            });
        }
        panic!("a pass did not finish");
    }

    /// Several passes with time between them (the pass interval).
    pub fn passes(&mut self, n: usize) -> Result<()> {
        for _ in 0..n {
            self.clock.advance(GRACE * 2);
            self.pass()?;
        }
        Ok(())
    }

    pub fn write(&mut self, ci: u64, seed: u64) {
        self.pending.insert(ci % self.chunks, Some(seed));
    }

    pub fn discard(&mut self, ci: u64) {
        self.pending.insert(ci % self.chunks, None);
    }

    pub fn id_of(&self, seed: u64) -> ChunkId {
        self.codec.chunk_id(ExtentKind::Data, &data_of(seed))
    }

    /// The hint of chunk `ci` in the in-memory map (the tip after a commit).
    pub fn hint(&self, ci: u64) -> Hint {
        let e = self.map.get(ChunkNo(ci)).expect("get").expect("populated");
        entry_hint(&e)
    }

    pub fn segments(&self) -> BTreeSet<SegId> {
        SegmentStore::list(&self.store)
            .expect("list")
            .iter()
            .map(|s| s.seg)
            .collect()
    }

    /// A cut and FULL that applies the pending moves and names their outputs.
    pub fn commit(&mut self) -> Result<()> {
        self.commit_with(true)
    }

    /// A cut and FULL whose wiring lags: the pending moves stay pending, the
    /// outputs stay unnamed.
    pub fn commit_lagging(&mut self) -> Result<()> {
        self.commit_with(false)
    }

    fn commit_with(&mut self, deliver: bool) -> Result<()> {
        let mut writer: Option<Box<dyn SegmentWriter>> = None;
        let mut new_segments: Vec<SegId> = Vec::new();
        let mut inserts: Vec<(ChunkId, Hint)> = Vec::new();
        let mut sets: Vec<(u64, Option<DataEntry>)> = Vec::new();
        let pending = std::mem::take(&mut self.pending);
        for (&ci, op) in &pending {
            let Some(seed) = *op else {
                sets.push((ci, None));
                continue;
            };
            let data = data_of(seed);
            let id = self.codec.chunk_id(ExtentKind::Data, &data);
            if writer.as_ref().is_some_and(|w| w.is_full()) {
                let s = writer.take().expect("writer").seal(None)?;
                self.space.charge(SpaceKind::Segments, s.size_bytes)?;
                new_segments.push(s.seg);
            }
            let w = match &mut writer {
                Some(w) => w,
                None => writer.insert(self.store.open_segment()?),
            };
            let mut ext = Vec::new();
            self.codec.seal(
                ExtentKind::Data,
                &id,
                &EncodingDesc {
                    codec: Codec::Raw,
                    level: Level(0),
                    ulen: CHUNK,
                    dict_id: None,
                },
                &data,
                &mut ext,
            )?;
            let h = w.append_extent(&id, None, &ext)?;
            inserts.push((id, h));
            sets.push((
                ci,
                Some(DataEntry {
                    lineage: LineageIdx(0),
                    seg: h.seg,
                    off: h.off,
                    clen: h.clen,
                    prefix: IdPrefix::of(&id, IdLen::Sixteen),
                }),
            ));
        }
        if let Some(w) = writer.take() {
            let s = w.seal(None)?;
            self.space.charge(SpaceKind::Segments, s.size_bytes)?;
            new_segments.push(s.seg);
        }
        let mut rehints = Vec::new();
        if deliver && !self.moved.is_empty() {
            let olds: HashMap<Hint, Hint> = self.moved.iter().map(|m| (m.old, m.new)).collect();
            let root = self.tip.map_root;
            self.map.walk_leaves(&root, &mut |ci, e| {
                if let Some(&new) = olds.get(&entry_hint(e)) {
                    rehints.push((ci, entry_hint(e), new));
                }
            })?;
            new_segments.extend(self.moved.iter().map(|m| m.new.seg));
            self.moved.clear();
        }
        {
            let mut txn = self.map.begin_update()?;
            for (ci, e) in &sets {
                match e {
                    Some(e) => txn.set(ChunkNo(*ci), *e)?,
                    None => txn.set_hole(ChunkNo(*ci))?,
                }
            }
            for (ci, old, new) in &rehints {
                txn.rehint(*ci, *old, *new)?;
            }
            txn.commit()?;
        }
        let mut pack = self.store.open_pack()?;
        let res = self.map.seal(&mut *pack)?;
        let mut map_packs = Vec::new();
        if !pack.is_empty() {
            let sp = pack.seal()?;
            self.space.charge(SpaceKind::Map, sp.size_bytes)?;
            map_packs.push(sp.pack);
        }
        let seq = CommitSeq(self.tip.common.commit_seq.0 + 1);
        // retain_last = 2.
        let chain_floor = CommitSeq(seq.0.saturating_sub(1)).max(self.tip.chain_floor);
        new_segments.sort_unstable();
        new_segments.dedup();
        let tip_bytes = self.store.get(self.tip.common.commit_seq)?;
        let full = Full {
            common: CommonHeader {
                kind: CommitKind::Full,
                commit_seq: seq,
                epoch: Epoch(0),
                parent_hash: self.commit_codec.object_hash(&tip_bytes),
                created_at: self.clock.now(),
            },
            cut_seq: WriteSeq(seq.0),
            size: self.tip.size,
            chain_floor,
            map_root: res.root,
            heat: None,
            map_packs,
            new_segments: new_segments.clone(),
            index_packs: std::mem::take(&mut self.runs_unnamed),
        };
        let sealed = self.commit_codec.seal_commit(&Commit::Full(full.clone()))?;
        self.store.put(seq, &sealed.bytes)?;
        self.space
            .charge(SpaceKind::Commits, sealed.bytes.len() as u64)?;
        self.tip = full.clone();
        self.retained.push(Commit::Full(full));
        self.retained
            .retain(|r| r.common().commit_seq >= chain_floor);
        self.store.raise_rollback_floor(&RollbackFloor {
            max_commit_seq: seq,
            tip_hash: sealed.hash,
        })?;
        if let Some(&m) = new_segments.iter().max() {
            self.upto = Some(self.upto.map_or(m, |u| u.max(m)));
        }
        for (id, h) in inserts {
            self.index.insert(id, h)?;
        }
        if let Some(upto) = self.upto {
            for r in self.index.persist(upto)? {
                self.space.charge(SpaceKind::Index, r.size_bytes)?;
                self.runs_unnamed.push(r.id);
            }
        }
        Ok(())
    }
}

impl Vol {
    /// The FULL `seq` read back from the store.
    pub fn full(&self, seq: u64) -> Full {
        let bytes = self.store.get(CommitSeq(seq)).expect("commit");
        match self
            .commit_codec
            .open_commit(CommitSeq(seq), &bytes)
            .expect("open")
        {
            Commit::Full(f) => f,
            _ => panic!("not a FULL"),
        }
    }

    /// Every node of FULL `seq`'s map, walked from a fresh map instance (from
    /// the stored packs only, nothing resident).
    pub fn nodes_from_disk(&self, seq: u64) -> Result<Vec<lunix_iface::map::NodeRef>> {
        let f = self.full(seq);
        let source: Arc<dyn NodeSource> = Arc::new(self.store.clone());
        let map = lunix_map::RadixMap::open(
            self.codec.clone(),
            source,
            &f.map_root,
            self.chunks,
            lunix_map::MapConfig::new(256, IdLen::Sixteen),
        )?;
        let mut out = Vec::new();
        map.walk_nodes(&f.map_root, &mut |n| out.push(*n))?;
        Ok(out)
    }

    /// Sealed map packs of the store.
    pub fn packs(&self) -> BTreeSet<ObjId> {
        lunix_iface::store::PackRead::list_packs(&self.store)
            .expect("packs")
            .iter()
            .map(|p| p.pack)
            .collect()
    }
}
