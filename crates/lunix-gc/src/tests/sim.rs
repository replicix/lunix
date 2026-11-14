//! A small volume over the testkit fakes that drives the collector the way
//! M11's coordinator does: cuts (dedup, segments, index inserts after the
//! commit, `persist` up to the highest named segment), the C3 re-hint of
//! `moved` records with the output named by the same FULL, `chain_floor`
//! from `retain_last`, the rollback floor raised after each FULL, and a
//! crash/reopen path (orphan sweep above the high-water mark, `catch_up`).
//!
//! The model is the content of every commit that is a root (by chunk
//! number, as a data seed), plus the objects reachable from the roots,
//! computed independently of `lunix-gc`.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::{Gc, GcConfig, GcHooks};
use lunix_format::commit::{Commit, CommitKind, CommonHeader, Full, MapRoot};
use lunix_format::local::RollbackFloor;
use lunix_format::node::DataEntry;
use lunix_format::{
    ChunkId, Codec, CommitSeq, CryptoMode, Epoch, ExtentKind, Hint, IdLen, IdPrefix, Level,
    LineageIdx, ObjId, SegId, TimeMs, WriteSeq,
};
use lunix_iface::clock::{Clock, ManualClock};
use lunix_iface::crypto::{CommitCodec, EncodingDesc, ExtentCodec, OpenExpect, OpenOutput};
use lunix_iface::gc::{Collector, GcBudget, GcParams, GcProgress, GcView, Moved, TipInfo};
use lunix_iface::index::{ChunkIndex, Confirm};
use lunix_iface::map::{entry_hint, BlockMap, ChunkNo};
use lunix_iface::space::{SpaceGate, SpaceKind};
use lunix_iface::store::{
    CommitStore, PackRead, PackStore, PinKind, PinRoot, SegmentStore, SegmentWriter, SweepRequest,
    VolumeDir,
};
use lunix_iface::{ErrorKind, Result};
use lunix_testkit::{
    IndexImage, MemIndex, MemSpace, MemSpaceConfig, MemStore, MemStoreConfig, PlainCodec,
    StoreImage,
};

use super::toy::ToyCodec;

/// Which map implementation backs the volume.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum MapKind {
    /// The testkit's flat map (the content oracle).
    Flat,
    /// `lunix-map`'s radix tree (real pack ids and shared subtrees).
    Radix,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct SimCfg {
    pub chunk_size: u32,
    pub id_len: IdLen,
    pub chunk_count: u64,
    pub segment_target: u64,
    pub encrypted: bool,
    pub map: MapKind,
    pub retain_last: u32,
    pub gc: GcConfig,
}

impl SimCfg {
    pub(crate) fn new(chunk_size: u32, id_len: IdLen) -> Self {
        Self {
            chunk_size,
            id_len,
            chunk_count: 48,
            segment_target: u64::from(chunk_size) * 4,
            encrypted: false,
            map: MapKind::Flat,
            retain_last: 2,
            gc: GcConfig {
                pass_interval: Duration::from_secs(1),
                ..GcConfig::default()
            },
        }
    }
}

pub(crate) const GRACE: Duration = Duration::from_secs(60);

/// Crash-at hooks.
#[derive(Debug, Default)]
pub(crate) struct Hooks {
    target: Mutex<Option<(&'static str, u64)>>,
    hits: Mutex<BTreeMap<&'static str, u64>>,
    crashed: Mutex<Option<&'static str>>,
}

impl Hooks {
    pub(crate) fn arm(&self, point: &'static str, nth: u64) {
        let base = self
            .hits
            .lock()
            .expect("hooks")
            .get(point)
            .copied()
            .unwrap_or(0);
        *self.target.lock().expect("hooks") = Some((point, base + nth.max(1)));
    }
    pub(crate) fn crashed(&self) -> Option<&'static str> {
        *self.crashed.lock().expect("hooks")
    }
}

impl GcHooks for Hooks {
    fn crash_at(&self, point: &'static str) -> bool {
        let n = {
            let mut h = self.hits.lock().expect("hooks");
            let c = h.entry(point).or_insert(0);
            *c += 1;
            *c
        };
        let fire = *self.target.lock().expect("hooks") == Some((point, n));
        if fire {
            *self.crashed.lock().expect("hooks") = Some(point);
        }
        fire
    }
}

/// The bytes of a chunk with data seed `seed`.
pub(crate) fn data_of(seed: u64, chunk_size: u32) -> Vec<u8> {
    let mut v = vec![0u8; chunk_size as usize];
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

pub(crate) struct Sim {
    pub cfg: SimCfg,
    pub store: MemStore,
    pub index: Arc<MemIndex>,
    pub map: Arc<dyn BlockMap>,
    pub codec: Arc<dyn ExtentCodec>,
    pub commit_codec: Arc<PlainCodec>,
    pub space: Arc<MemSpace>,
    pub clock: Arc<ManualClock>,
    pub gc: Gc,
    pub hooks: Arc<Hooks>,
    pub retained: Vec<Commit>,
    pub tip: Full,
    pub retain_last: u32,
    pub moved: Vec<Moved>,
    pub pending_named: Vec<SegId>,
    pub upto_seg: Option<SegId>,
    pub runs_unnamed: Vec<ObjId>,
    pub pending: BTreeMap<u64, Option<u64>>,
    /// Content of every committed `commit_seq` that may still be a root.
    pub model: BTreeMap<u64, BTreeMap<u64, u64>>,
    pub raise_floor: bool,
    pub pins: Vec<PinRoot>,
}

fn new_codec(cfg: &SimCfg) -> Arc<dyn ExtentCodec> {
    if cfg.encrypted {
        Arc::new(ToyCodec::default())
    } else {
        Arc::new(PlainCodec::new())
    }
}

fn store_cfg(cfg: &SimCfg) -> MemStoreConfig {
    MemStoreConfig {
        segment_target: cfg.segment_target,
        mode: if cfg.encrypted {
            CryptoMode::Encrypted
        } else {
            CryptoMode::Integrity
        },
        ..MemStoreConfig::default()
    }
}

fn open_map(
    cfg: &SimCfg,
    codec: &Arc<dyn ExtentCodec>,
    store: &MemStore,
    root: Option<&MapRoot>,
) -> Result<Arc<dyn BlockMap>> {
    let packs: Arc<dyn PackRead> = Arc::new(store.clone());
    Ok(match (cfg.map, root) {
        (MapKind::Flat, None) => Arc::new(lunix_testkit::FlatMap::new(
            codec.clone(),
            packs,
            cfg.chunk_count,
            cfg.id_len,
        )),
        (MapKind::Flat, Some(r)) => Arc::new(lunix_testkit::FlatMap::open(
            codec.clone(),
            packs,
            r,
            cfg.chunk_count,
            cfg.id_len,
        )?),
        (MapKind::Radix, r) => {
            let mc = lunix_map::MapConfig::new(256, cfg.id_len);
            let src: Arc<dyn lunix_iface::map::NodeSource> = Arc::new(store.clone());
            match r {
                None => Arc::new(lunix_map::RadixMap::new(
                    codec.clone(),
                    src,
                    cfg.chunk_count,
                    mc,
                )?),
                Some(r) => Arc::new(lunix_map::RadixMap::open(
                    codec.clone(),
                    src,
                    r,
                    cfg.chunk_count,
                    mc,
                )?),
            }
        }
    })
}

pub(crate) fn high_water(commits: &[Commit]) -> u64 {
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

impl Sim {
    pub(crate) fn new(cfg: SimCfg) -> Self {
        let store = MemStore::with_config(store_cfg(&cfg));
        let codec = new_codec(&cfg);
        let commit_codec = Arc::new(PlainCodec::new());
        let index = Arc::new(MemIndex::new(store.mem_ids()));
        let map = open_map(&cfg, &codec, &store, None).expect("map");
        let clock = Arc::new(ManualClock::new(TimeMs(1_700_000_000_000)));
        let space = MemSpace::new(MemSpaceConfig::new(1 << 40), clock.clone());
        let hooks = Arc::new(Hooks::default());
        let genesis = Full::genesis(
            [0x11; 32],
            cfg.chunk_count * u64::from(cfg.chunk_size),
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
        let gc = Gc::with_hooks(Arc::new(store.clone()), cfg.gc, hooks.clone());
        let mut model = BTreeMap::new();
        model.insert(0, BTreeMap::new());
        Self {
            retain_last: cfg.retain_last,
            cfg,
            store,
            index,
            map,
            codec,
            commit_codec,
            space,
            clock,
            gc,
            hooks,
            retained: vec![Commit::Full(genesis.clone())],
            tip: genesis,
            moved: Vec::new(),
            pending_named: Vec::new(),
            upto_seg: None,
            runs_unnamed: Vec::new(),
            pending: BTreeMap::new(),
            model,
            raise_floor: true,
            pins: Vec::new(),
        }
    }

    pub(crate) fn params(&self) -> GcParams {
        GcParams {
            retain_last: self.retain_last,
            grace: GRACE,
            read_timeout: Duration::from_secs(10),
            rate_bytes_per_s: 64 << 20,
            live_threshold: 0.5,
            chunk_size: self.cfg.chunk_size,
            id_len: self.cfg.id_len,
        }
    }

    pub(crate) fn tip_info(&self) -> TipInfo {
        TipInfo {
            commit_seq: self.tip.common.commit_seq,
            map_root: self.tip.map_root,
            chain_floor: self.tip.chain_floor,
            high_water_mark: high_water(&self.retained),
        }
    }

    /// The view the coordinator would pass.
    pub(crate) fn with_view<R>(&self, f: impl FnOnce(&GcView<'_>) -> R) -> R {
        let view = GcView {
            retained: &self.retained,
            tip: self.tip_info(),
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

    pub(crate) fn budget() -> GcBudget {
        GcBudget {
            max_duration: Duration::from_millis(50),
            max_bytes: 4 << 20,
        }
    }

    /// One collector step; moves go to the next cut.
    pub(crate) fn step(&mut self) -> Result<GcProgress> {
        self.step_with(Self::budget())
    }

    /// A step that does exactly one unit of work (budget 0).
    pub(crate) fn unit(&mut self) -> Result<GcProgress> {
        self.step_with(GcBudget {
            max_duration: Duration::ZERO,
            max_bytes: 4 << 20,
        })
    }

    pub(crate) fn step_with(&mut self, budget: GcBudget) -> Result<GcProgress> {
        let mut gc = std::mem::replace(
            &mut self.gc,
            Gc::new(Arc::new(self.store.clone()), self.cfg.gc),
        );
        let r = self.with_view(|v| gc.step(v, budget));
        self.gc = gc;
        let p = r?;
        self.moved.extend(p.moved.iter().copied());
        Ok(p)
    }

    pub(crate) fn urgent(&mut self, want: u64) -> Result<GcProgress> {
        let mut gc = std::mem::replace(
            &mut self.gc,
            Gc::new(Arc::new(self.store.clone()), self.cfg.gc),
        );
        let r = self.with_view(|v| gc.urgent(v, want));
        self.gc = gc;
        let p = r?;
        self.moved.extend(p.moved.iter().copied());
        Ok(p)
    }

    /// Steps until a pass finishes, advancing the clock past graces. Cuts
    /// happen only when `cut_between` says so.
    pub(crate) fn run_pass(
        &mut self,
        cut_between: &mut dyn FnMut(&mut Sim) -> Result<()>,
    ) -> Result<()> {
        self.run_pass_with(false, cut_between)
    }

    /// As [`run_pass`](Self::run_pass); with `unit`, every step does one unit
    /// of work, so the cuts interleave with every step of the pass.
    pub(crate) fn run_pass_with(
        &mut self,
        unit: bool,
        cut_between: &mut dyn FnMut(&mut Sim) -> Result<()>,
    ) -> Result<()> {
        for _ in 0..1_000_000 {
            let p = if unit { self.unit()? } else { self.step()? };
            // Done, or idle with nothing due.
            if p.done || p.state == lunix_iface::gc::GcState::Idle {
                return Ok(());
            }
            if p.state == lunix_iface::gc::GcState::Grace {
                self.clock.advance(Duration::from_secs(5));
                continue;
            }
            cut_between(self)?;
            self.clock.advance(if unit {
                Duration::from_millis(300)
            } else {
                Duration::from_secs(5)
            });
        }
        panic!("a pass did not finish");
    }

    pub(crate) fn pass(&mut self) -> Result<()> {
        self.run_pass(&mut |_| Ok(()))
    }

    // ------------------------------------------------------------- writes

    pub(crate) fn write(&mut self, ci: u64, seed: u64) {
        self.pending.insert(ci % self.cfg.chunk_count, Some(seed));
    }

    pub(crate) fn discard(&mut self, ci: u64) {
        self.pending.insert(ci % self.cfg.chunk_count, None);
    }

    fn content(&self) -> BTreeMap<u64, u64> {
        self.model
            .get(&self.tip.common.commit_seq.0)
            .cloned()
            .unwrap_or_default()
    }

    fn charge(&self, kind: SpaceKind, bytes: u64) {
        self.space.charge(kind, bytes).expect("charge");
    }

    /// A cut and FULL commit (C2..C8 of M11 in one call).
    pub(crate) fn commit(&mut self) -> Result<()> {
        let cs = self.cfg.chunk_size;
        let id_len = self.cfg.id_len;
        let mut content = self.content();
        // C3: destage.
        let mut writer: Option<Box<dyn SegmentWriter>> = None;
        let mut new_segments: Vec<SegId> = Vec::new();
        let mut inserts: Vec<(ChunkId, Hint)> = Vec::new();
        let mut cut_ids: HashMap<ChunkId, Hint> = HashMap::new();
        let mut sets: Vec<(u64, Option<DataEntry>)> = Vec::new();
        let pending = std::mem::take(&mut self.pending);
        for (&ci, op) in &pending {
            let Some(seed) = *op else {
                sets.push((ci, None));
                content.remove(&ci);
                continue;
            };
            content.insert(ci, seed);
            let data = data_of(seed, cs);
            let id = self.codec.chunk_id(ExtentKind::Data, &data);
            let mut hint = cut_ids.get(&id).copied();
            if hint.is_none() {
                if let Some(hit) = self.index.get_for_dedup(&id)? {
                    let ok = hit.confirm == Confirm::Verified || {
                        let mut raw = Vec::new();
                        self.store.read_extent(&hit.hint, &mut raw).is_ok()
                            && self
                                .codec
                                .open(
                                    ExtentKind::Data,
                                    &raw,
                                    &OpenExpect {
                                        chunk_size: cs,
                                        chunk_id: Some(&id),
                                        id_prefix: None,
                                        verify_id: false,
                                        output: OpenOutput::Stored,
                                    },
                                    &mut Vec::new(),
                                )
                                .is_ok()
                    };
                    if ok {
                        hint = Some(hit.hint);
                    }
                }
            }
            let hint = match hint {
                Some(h) => h,
                None => {
                    if writer.as_ref().is_some_and(|w| w.is_full()) {
                        let s = writer.take().expect("writer").seal(None)?;
                        self.charge(SpaceKind::Segments, s.size_bytes);
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
                            ulen: cs,
                            dict_id: None,
                        },
                        &data,
                        &mut ext,
                    )?;
                    let h = w.append_extent(&id, None, &ext)?;
                    inserts.push((id, h));
                    cut_ids.insert(id, h);
                    h
                }
            };
            sets.push((
                ci,
                Some(DataEntry {
                    lineage: LineageIdx(0),
                    seg: hint.seg,
                    off: hint.off,
                    clen: hint.clen,
                    prefix: IdPrefix::of(&id, id_len),
                }),
            ));
        }
        if let Some(w) = writer.take() {
            let s = w.seal(None)?;
            self.charge(SpaceKind::Segments, s.size_bytes);
            new_segments.push(s.seg);
        }
        // C3, second half: the moves become conditional re-hints.
        let mut rehints = Vec::new();
        if !self.moved.is_empty() {
            let olds: HashMap<Hint, Hint> = self.moved.iter().map(|m| (m.old, m.new)).collect();
            let root = self.tip.map_root;
            self.map.walk_leaves(&root, &mut |ci, e| {
                if let Some(&new) = olds.get(&entry_hint(e)) {
                    rehints.push((ci, entry_hint(e), new));
                }
            })?;
            for m in &self.moved {
                if !self.pending_named.contains(&m.new.seg) {
                    self.pending_named.push(m.new.seg);
                }
            }
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
        // C5: map pack.
        let mut pack = self.store.open_pack()?;
        let res = self.map.seal(&mut *pack)?;
        let mut map_packs = Vec::new();
        if !pack.is_empty() {
            let sp = pack.seal()?;
            self.charge(SpaceKind::Map, sp.size_bytes);
            map_packs.push(sp.pack);
        }
        // C6: the FULL.
        let seq = CommitSeq(self.tip.common.commit_seq.0 + 1);
        let keep = u64::from(self.retain_last.max(1)) - 1;
        let chain_floor = CommitSeq(seq.0.saturating_sub(keep)).max(self.tip.chain_floor);
        new_segments.append(&mut self.pending_named);
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
        self.charge(SpaceKind::Commits, sealed.bytes.len() as u64);
        self.moved.clear();
        self.tip = full.clone();
        self.retained.push(Commit::Full(full));
        self.retained
            .retain(|r| r.common().commit_seq >= chain_floor);
        self.model.insert(seq.0, content);
        if self.raise_floor {
            self.store.raise_rollback_floor(&RollbackFloor {
                max_commit_seq: seq,
                tip_hash: sealed.hash,
            })?;
        }
        if let Some(&m) = new_segments.iter().max() {
            self.upto_seg = Some(self.upto_seg.map_or(m, |u| u.max(m)));
        }
        // C8: index inserts (after the durable commit), then a run.
        for (id, h) in inserts {
            self.index.insert(id, h)?;
        }
        if let Some(upto) = self.upto_seg {
            for r in self.index.persist(upto)? {
                self.charge(SpaceKind::Index, r.size_bytes);
                self.runs_unnamed.push(r.id);
            }
        }
        self.prune_model();
        Ok(())
    }

    /// Keeps the model of commits that are roots or still exist.
    fn prune_model(&mut self) {
        let keep: BTreeSet<u64> = self
            .retained
            .iter()
            .map(|c| c.common().commit_seq.0)
            .chain(self.pins.iter().map(|p| p.commit_seq.0))
            .collect();
        self.model.retain(|k, _| keep.contains(k));
    }

    pub(crate) fn set_retain(&mut self, n: u32) {
        self.retain_last = n.clamp(1, 6);
    }

    /// Pins a retained commit (a snapshot pin).
    pub(crate) fn pin(&mut self, pick: u64) {
        let seqs: Vec<CommitSeq> = self
            .retained
            .iter()
            .map(|c| c.common().commit_seq)
            .collect();
        let seq = seqs[(pick % seqs.len() as u64) as usize];
        self.pins.push(PinRoot {
            kind: PinKind::Snapshot,
            name: format!("snap-{}-{}", seq.0, self.pins.len()),
            commit_seq: seq,
            deadline: None,
        });
        self.store.set_pins(self.pins.clone());
    }

    pub(crate) fn unpin(&mut self, pick: u64) {
        if self.pins.is_empty() {
            return;
        }
        let i = (pick % self.pins.len() as u64) as usize;
        self.pins.remove(i);
        self.store.set_pins(self.pins.clone());
        self.prune_model();
    }

    // ------------------------------------------------------------- crash

    /// A crash now: the stores keep what is durable; the collector, the
    /// map, the memtable and the pending moves are lost; the volume reopens
    /// like M11's recovery.
    pub(crate) fn crash_reopen(&mut self, seed: u64) -> Result<()> {
        let img: StoreImage = self.store.crash(seed);
        let idx: IndexImage = self.index.image();
        self.reopen(&img, &idx)
    }

    pub(crate) fn reopen(&mut self, img: &StoreImage, idx: &IndexImage) -> Result<()> {
        let store = MemStore::from_image(img);
        let index = Arc::new(MemIndex::from_image(store.mem_ids(), idx));
        let tip_seq = store.tip()?.expect("a tip");
        let tip = match self
            .commit_codec
            .open_commit(tip_seq, &store.get(tip_seq)?)?
        {
            Commit::Full(f) => f,
            _ => panic!("tip is not a FULL"),
        };
        self.commit_codec
            .verify_chain(&store, tip_seq, tip.chain_floor, &[0x11; 32])?;
        let mut retained = Vec::new();
        for s in tip.chain_floor.0..=tip_seq.0 {
            retained.push(
                self.commit_codec
                    .open_commit(CommitSeq(s), &store.get(CommitSeq(s))?)?,
            );
        }
        let hwm = high_water(&retained);
        let mut named_segments = HashSet::new();
        let mut named_packs = HashSet::new();
        for c in &retained {
            if let Commit::Full(f) = c {
                named_segments.extend(f.new_segments.iter().copied());
                named_packs.extend(f.map_packs.iter().copied());
                named_packs.extend(f.map_root.pack.iter().copied());
            }
        }
        store.sweep_orphans(&SweepRequest {
            high_water_mark: hwm,
            named_segments: &named_segments,
            named_packs: &named_packs,
        })?;
        let segs: Vec<SegId> = named_segments.iter().copied().collect();
        index.catch_up(&segs, &store)?;
        let map = open_map(&self.cfg, &self.codec, &store, Some(&tip.map_root))?;
        let space = MemSpace::new(MemSpaceConfig::new(1 << 40), self.clock.clone());
        let seg_bytes: u64 = SegmentStore::list(&store)?
            .iter()
            .map(|s| s.size_bytes)
            .sum();
        let pack_bytes: u64 = store.list_packs()?.iter().map(|p| p.size_bytes).sum();
        let commit_bytes: u64 = CommitStore::list(&store)?
            .iter()
            .map(|c| c.size_bytes)
            .sum();
        let run_bytes: u64 = index.all_runs()?.iter().map(|r| r.size_bytes).sum();
        for (k, b) in [
            (SpaceKind::Segments, seg_bytes),
            (SpaceKind::Map, pack_bytes),
            (SpaceKind::Commits, commit_bytes),
            (SpaceKind::Index, run_bytes),
        ] {
            space.charge(k, b)?;
        }
        store.set_pins(self.pins.clone());
        self.hooks = Arc::new(Hooks::default());
        self.gc = Gc::with_hooks(Arc::new(store.clone()), self.cfg.gc, self.hooks.clone());
        self.store = store;
        self.index = index;
        self.map = map;
        self.space = space;
        self.upto_seg = named_segments.iter().copied().max().max(self.upto_seg);
        self.tip = tip;
        self.retained = retained;
        self.moved.clear();
        self.pending_named.clear();
        self.pending.clear();
        self.runs_unnamed.clear();
        self.prune_model();
        Ok(())
    }

    // ------------------------------------------------------------- oracle

    /// Reads a map entry like the engine's R1 reader: the hint with the
    /// entry's prefix, else the index by prefix (stale hint); lean volumes
    /// read the hint as a scan.
    pub(crate) fn read_entry(&self, e: &DataEntry) -> Option<Vec<u8>> {
        self.read_entry_with(&*self.index, e)
    }

    /// [`read_entry`](Self::read_entry) through another index (a rebuilt or
    /// crash-recovered one).
    pub(crate) fn read_entry_with(&self, index: &dyn ChunkIndex, e: &DataEntry) -> Option<Vec<u8>> {
        let cs = self.cfg.chunk_size;
        let lean = self.cfg.id_len == IdLen::Zero;
        let open = |hint: &Hint, id: Option<&ChunkId>| -> Option<Vec<u8>> {
            let mut raw = Vec::new();
            self.store.read_extent(hint, &mut raw).ok()?;
            let mut out = Vec::new();
            self.codec
                .open(
                    ExtentKind::Data,
                    &raw,
                    &OpenExpect {
                        chunk_size: cs,
                        chunk_id: id,
                        id_prefix: if lean || id.is_some() {
                            None
                        } else {
                            Some(&e.prefix)
                        },
                        verify_id: lean,
                        output: OpenOutput::Plaintext,
                    },
                    &mut out,
                )
                .ok()?;
            Some(out)
        };
        if let Some(d) = open(&entry_hint(e), None) {
            return Some(d);
        }
        if lean {
            return None;
        }
        for c in index.get_by_prefix(&e.prefix).ok()? {
            if let Some(d) = open(&c.hint, Some(&c.id)) {
                return Some(d);
            }
        }
        None
    }

    /// The roots: retained FULLs and pinned commits.
    pub(crate) fn roots(&self) -> Vec<(u64, MapRoot)> {
        let mut r: Vec<(u64, MapRoot)> = self
            .retained
            .iter()
            .filter_map(|c| match c {
                Commit::Full(f) => Some((f.common.commit_seq.0, f.map_root)),
                _ => None,
            })
            .collect();
        for p in &self.pins {
            if r.iter().any(|(s, _)| *s == p.commit_seq.0) {
                continue;
            }
            let bytes = self.store.get(p.commit_seq).expect("pinned commit exists");
            if let Ok(Commit::Full(f)) = self.commit_codec.open_commit(p.commit_seq, &bytes) {
                r.push((p.commit_seq.0, f.map_root));
            }
        }
        r
    }

    /// Every block of every root reads back its model content, and every
    /// commit object a root, a pin or the rollback floor window needs exists.
    pub(crate) fn check_roots(&self) -> std::result::Result<(), String> {
        self.check_roots_with(&*self.index)?;
        self.check_commits()
    }

    /// Every block of every root reads back through `index` as fallback.
    pub(crate) fn check_roots_with(
        &self,
        index: &dyn ChunkIndex,
    ) -> std::result::Result<(), String> {
        for (seq, root) in self.roots() {
            let want = self
                .model
                .get(&seq)
                .ok_or_else(|| format!("no model for root {seq}"))?;
            let mut got = BTreeMap::new();
            self.map
                .walk_leaves(&root, &mut |ci, e| {
                    got.insert(ci.0, *e);
                })
                .map_err(|e| format!("walk of root {seq}: {e}"))?;
            if got.keys().collect::<Vec<_>>() != want.keys().collect::<Vec<_>>() {
                return Err(format!(
                    "root {seq}: populated chunks differ from the model"
                ));
            }
            for (ci, e) in &got {
                let data = self
                    .read_entry_with(index, e)
                    .ok_or_else(|| format!("root {seq} chunk {ci}: unreadable ({e:?})"))?;
                if data != data_of(want[ci], self.cfg.chunk_size) {
                    return Err(format!("root {seq} chunk {ci}: wrong data"));
                }
            }
        }
        Ok(())
    }

    /// Commit objects that roots, pins and the rollback floor window need.
    pub(crate) fn check_commits(&self) -> std::result::Result<(), String> {
        let floor = self.store.rollback_floor().map_err(|e| e.to_string())?;
        let tip = self.tip.common.commit_seq.0;
        let low = floor
            .map_or(tip, |f| f.max_commit_seq.0.min(tip))
            .min(self.keep_floor());
        for s in low..=tip {
            self.store
                .get(CommitSeq(s))
                .map_err(|_| format!("commit {s} missing (needed from {low})"))?;
        }
        for p in &self.pins {
            self.store
                .get(p.commit_seq)
                .map_err(|_| format!("pinned commit {} missing", p.commit_seq.0))?;
        }
        // The chain names nothing that is gone (fsck would flag it).
        let segs: BTreeSet<SegId> = SegmentStore::list(&self.store)
            .map_err(|e| e.to_string())?
            .iter()
            .map(|x| x.seg)
            .collect();
        let packs: BTreeSet<ObjId> = self
            .store
            .list_packs()
            .map_err(|e| e.to_string())?
            .iter()
            .map(|x| x.pack)
            .collect();
        for c in &self.retained {
            if let Commit::Full(f) = c {
                for s in &f.new_segments {
                    if !segs.contains(s) {
                        return Err(format!(
                            "commit {} names missing segment {s:?}",
                            f.common.commit_seq.0
                        ));
                    }
                }
                for p in f.map_packs.iter().chain(f.map_root.pack.iter()) {
                    if !packs.contains(p) {
                        return Err(format!(
                            "commit {} names missing pack {p:?}",
                            f.common.commit_seq.0
                        ));
                    }
                }
            }
        }
        Ok(())
    }

    /// Ledger = bytes of the files that exist, per kind.
    pub(crate) fn check_ledger(&self) -> std::result::Result<(), String> {
        let snap = self.space.snapshot();
        let seg: u64 = SegmentStore::list(&self.store)
            .map_err(|e| e.to_string())?
            .iter()
            .map(|s| s.size_bytes)
            .sum();
        let pack: u64 = self
            .store
            .list_packs()
            .map_err(|e| e.to_string())?
            .iter()
            .map(|p| p.size_bytes)
            .sum();
        let com: u64 = CommitStore::list(&self.store)
            .map_err(|e| e.to_string())?
            .iter()
            .map(|c| c.size_bytes)
            .sum();
        let run: u64 = self
            .index
            .all_runs()
            .map_err(|e| e.to_string())?
            .iter()
            .map(|r| r.size_bytes)
            .sum();
        let got = [
            snap.by_kind[SpaceKind::Segments.index()],
            snap.by_kind[SpaceKind::Map.index()],
            snap.by_kind[SpaceKind::Commits.index()],
            snap.by_kind[SpaceKind::Index.index()],
        ];
        if got != [seg, pack, com, run] {
            return Err(format!(
                "ledger {got:?} != files [segments, packs, commits, runs] {:?}",
                [seg, pack, com, run]
            ));
        }
        Ok(())
    }

    /// The model's reachable objects (independent of `lunix-gc`): segments
    /// holding the live copy of an entry of some root (the index's current
    /// copy when it exists and the footer confirms it, else the hinted
    /// extent), packs holding a node of some root, runs the manifest names.
    pub(crate) fn model_reachable(&self) -> BTreeSet<(u8, u64)> {
        let mut out = BTreeSet::new();
        let lean = self.cfg.id_len == IdLen::Zero;
        let footer_id = |h: &Hint| -> Option<ChunkId> {
            let f = self.store.footer(h.seg).ok()?;
            f.entries
                .iter()
                .find(|x| x.off == h.off && x.clen == h.clen)
                .map(|x| x.chunk_id)
        };
        for (_, root) in self.roots() {
            self.map
                .walk_nodes(&root, &mut |n| {
                    out.insert((2, n.loc.pack.raw()));
                })
                .expect("walk nodes");
            let mut entries = Vec::new();
            self.map
                .walk_leaves(&root, &mut |_, e| entries.push(*e))
                .expect("walk leaves");
            for e in entries {
                let h = entry_hint(&e);
                if lean {
                    out.insert((1, h.seg.raw()));
                    continue;
                }
                let id = footer_id(&h).filter(|id| e.prefix.matches(id));
                match id {
                    Some(id) => match self.index.get(&id).ok().flatten() {
                        Some(c) if footer_id(&c.hint) == Some(id) => {
                            out.insert((1, c.hint.seg.raw()));
                        }
                        _ => {
                            out.insert((1, h.seg.raw()));
                        }
                    },
                    None => {
                        for c in self.index.get_by_prefix(&e.prefix).unwrap_or_default() {
                            if footer_id(&c.hint) == Some(c.id) {
                                out.insert((1, c.hint.seg.raw()));
                            }
                        }
                    }
                }
            }
        }
        for r in self.index.runs().expect("runs") {
            out.insert((3, r.id.raw()));
        }
        out
    }

    /// The objects that exist: (kind, id) with kind 1 segment, 2 pack, 3 run.
    pub(crate) fn existing(&self) -> BTreeSet<(u8, u64)> {
        let mut out = BTreeSet::new();
        for s in SegmentStore::list(&self.store).expect("list") {
            out.insert((1, s.seg.raw()));
        }
        for p in self.store.list_packs().expect("packs") {
            out.insert((2, p.pack.raw()));
        }
        for r in self.index.all_runs().expect("runs") {
            out.insert((3, r.id.raw()));
        }
        out
    }

    /// Objects kept regardless of liveness: named by a retained commit or
    /// above the tip's high-water mark.
    pub(crate) fn kept_by_rule(&self) -> BTreeSet<u64> {
        let hwm = high_water(&self.retained);
        let mut s: BTreeSet<u64> = self
            .existing()
            .into_iter()
            .map(|(_, id)| id)
            .filter(|&id| id > hwm)
            .collect();
        for c in &self.retained {
            if let Commit::Full(f) = c {
                s.extend(
                    f.new_segments
                        .iter()
                        .chain(&f.map_packs)
                        .chain(&f.index_packs)
                        .chain(f.map_root.pack.iter())
                        .map(|i| i.raw()),
                );
            }
        }
        s
    }

    /// Commits a quiescent volume must hold, and may hold no more than.
    /// The lowest `chain_floor` of any retained FULL: commits from it up
    /// stay so that `--truncate-tip` can mount the predecessor (N12).
    pub(crate) fn keep_floor(&self) -> u64 {
        self.retained
            .iter()
            .filter_map(|c| match c {
                Commit::Full(f) => Some(f.chain_floor.0),
                _ => None,
            })
            .chain(std::iter::once(self.tip.chain_floor.0))
            .min()
            .unwrap_or(0)
    }

    /// One-shot non-crash faults armed only around collector calls: `Io` or
    /// `NoSpace` at the `nth` next pass of a store or index point.
    pub(crate) fn arm_fault(&self, point: &str, nth: u64, nospace: bool) {
        let fault = if nospace {
            lunix_testkit::Fault::NoSpace
        } else {
            lunix_testkit::Fault::Io
        };
        if point.starts_with("index.") {
            self.index.faults().arm(point, nth, fault);
        } else {
            self.store.faults().arm(point, nth, fault);
        }
    }

    pub(crate) fn disarm_faults(&self) {
        self.index.faults().disarm();
        self.store.faults().disarm();
    }

    pub(crate) fn expected_commits(&self) -> BTreeSet<u64> {
        let tip = self.tip.common.commit_seq.0;
        let floor = self
            .store
            .rollback_floor()
            .expect("floor")
            .map_or(u64::MAX, |f| f.max_commit_seq.0);
        let cf = self.keep_floor();
        let mut s: BTreeSet<u64> = (cf.min(floor)..=tip).collect();
        s.extend(self.pins.iter().map(|p| p.commit_seq.0));
        s
    }

    /// Drives the volume to quiescence: commits until no move is pending and
    /// retained commits name nothing new, passes until nothing changes.
    pub(crate) fn quiesce(&mut self) -> Result<()> {
        for round in 0..40 {
            // Name every pending output and roll the retained window over.
            for _ in 0..(self.retain_last as usize + 2) {
                self.commit()?;
            }
            self.clock.advance(GRACE * 2);
            self.pass()?;
            if round > 0 && self.moved.is_empty() && self.gc.condemned().is_empty() {
                return Ok(());
            }
        }
        panic!(
            "the volume did not quiesce: condemned {:?}, moved {:?}, last pass {:?}",
            self.gc.condemned(),
            self.moved,
            self.gc.status().last_pass
        );
    }

    /// The commit object kinds error helper for tests.
    pub(crate) fn commit_exists(&self, seq: u64) -> bool {
        match self.store.get(CommitSeq(seq)) {
            Ok(_) => true,
            Err(e) if e.kind() == ErrorKind::NotFound => false,
            Err(e) => panic!("{e}"),
        }
    }
}
