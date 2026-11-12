//! The cut and commit coordinator: the local FULL commit protocol C1-C10
//! (plan M11 step 5, DESIGN §8.1-§8.2, ADR-9).
//!
//! Durable order: C4 (segments sealed) and C5 (map pack sealed) before C6
//! (the commit object, the durability point of the cut), C6 before the
//! rollback floor (C7) and before any journal reclaim (C9). A failure before
//! C6 leaves the previous commit as the tip; what C4/C5 wrote is an orphan
//! of the open-time sweep. A failure inside C3..C6 that is not a crash also
//! stops further commits in this process (the in-memory map is ahead of the
//! durable state), and writes fail until the volume is reopened.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;
use std::time::Duration;

use lunix_format::commit::{Commit, CommitKind, CommonHeader, Full};
use lunix_format::local::{CheckpointSlot, RollbackFloor};
use lunix_format::{CommitSeq, Epoch, Hint, SegId, TimeMs, WriteSeq};
use lunix_iface::gc::{Collector, GcView, Moved, TipInfo};
use lunix_iface::journal::WriteSeqRange;
use lunix_iface::map::{entry_hint, ChunkNo};
use lunix_iface::overlay::OverlayCut;
use lunix_iface::space::SpaceKind;
use lunix_iface::{Error, ErrorKind, Result};

use crate::destage::DestageCtx;
use crate::engine::{CommitOutcome, Inner};
use crate::recover::{high_water, Recovered};
use crate::stats::add;

/// The coordinator's state, under the engine's coordinator mutex.
pub(crate) struct Coord {
    /// The durable tip.
    pub(crate) tip: Full,
    /// BLAKE3 of the tip's stored bytes (the next `parent_hash`).
    pub(crate) tip_hash: [u8; 32],
    /// The retained FULL commits, `chain_floor ..= tip`, authenticated.
    pub(crate) retained: Vec<Commit>,
    /// Monotonic time of the last commit (the `T` trigger).
    pub(crate) last_commit_at: Duration,
    /// Compaction moves to apply at the next cut's C3.
    pub(crate) moved: Vec<Moved>,
    pub(crate) collector: Option<Box<dyn Collector>>,
    /// When a tick first saw the volume dirty since the last commit (`T`
    /// counts from there, so an idle period does not fire it at once).
    pub(crate) dirty_since: Option<Duration>,
    /// Bytes charged to `Index` (the size of every run file).
    pub(crate) index_charged: u64,
    /// The highest segment a durable commit names (for `persist`).
    pub(crate) upto_seg: Option<SegId>,
}

impl Coord {
    pub(crate) fn new(
        rec: &Recovered,
        collector: Option<Box<dyn Collector>>,
        now: Duration,
    ) -> Self {
        Self {
            tip: rec.tip.clone(),
            tip_hash: rec.tip_hash,
            retained: rec.retained.clone(),
            last_commit_at: now,
            moved: Vec::new(),
            dirty_since: None,
            collector,
            index_charged: rec.index_charged,
            upto_seg: rec.upto_seg,
        }
    }
}

/// The ledger bounds held for one cut (the accounting walk-through of
/// `lunix_iface::space`, steps 2-5).
#[derive(Default)]
struct Acct {
    /// Charged to `Segments` ahead of C4 (0 if the headroom was short).
    seg_bound: u64,
    /// Still held in `Headroom` for C5, C6 and C8.
    head_left: u64,
}

const NODE_BOUND: u64 = 66 << 10;

impl Inner {
    fn reserve_bounds(&self, cut: &OverlayCut) -> Acct {
        let n = cut.chunks.len() as u64;
        let ovh = u64::from(self.extents.mode().extent_ovh());
        let cs = u64::from(self.cfg.chunk_size);
        let seg_bound = n.saturating_mul(cs + ovh + 64).saturating_add(64 << 10);
        // Leaves touched: at most one per 256 chunks (the smallest fanout),
        // plus the interior path, each node at most a full node extent.
        let leaves: HashSet<u64> = cut.chunks.iter().map(|c| c.chunk / 256).collect();
        let map_bound = (leaves.len() as u64 + 4).saturating_mul(NODE_BOUND + ovh);
        let head = map_bound + (64 << 10) + n.saturating_mul(64) + (4 << 10);
        let mut acct = Acct::default();
        if self
            .space
            .reserve_headroom(SpaceKind::Segments, seg_bound)
            .is_ok()
        {
            acct.seg_bound = seg_bound;
        }
        if self
            .space
            .reserve_headroom(SpaceKind::Headroom, head)
            .is_ok()
        {
            acct.head_left = head;
        }
        if acct.seg_bound == 0 || acct.head_left == 0 {
            tracing::warn!("commit headroom is short; charging actual sizes after the fact");
        }
        acct
    }

    /// Moves up to `bound` of the held headroom to `kind` as `actual` bytes.
    fn settle(&self, acct: &mut Acct, kind: SpaceKind, bound: u64, actual: u64) {
        let from = bound.min(acct.head_left);
        acct.head_left -= from;
        if let Err(e) = self.space.transfer(SpaceKind::Headroom, kind, from, actual) {
            tracing::warn!(error = %e, "ledger transfer failed");
        }
    }

    fn abandon(&self, acct: Acct) {
        if acct.seg_bound > 0 {
            self.space.release(SpaceKind::Segments, acct.seg_bound);
        }
        if acct.head_left > 0 {
            self.space.release(SpaceKind::Headroom, acct.head_left);
        }
    }

    fn fatal(&self, step: &str, e: &Error) {
        if !self.points.is_dead() {
            tracing::error!(step, error = %e, "commit failed after the map changed: no more commits in this process");
        }
        self.failed.store(true, Ordering::Release);
    }

    /// The trigger check (plan M11 step 5): `T`, `B`, journal fill, a full
    /// ring, pending `moved` records. An idle volume cuts nothing.
    pub(crate) fn tick(&self) -> Result<CommitOutcome> {
        let mut co = crate::engine::lock(&self.coord);
        if self.wanted(&mut co) {
            self.commit(&mut co, false, None)
        } else {
            Ok(CommitOutcome::Idle)
        }
    }

    fn wanted(&self, co: &mut Coord) -> bool {
        if !co.moved.is_empty() {
            return true;
        }
        let dirty = self.journal.last_issued() > co.tip.cut_seq;
        if !dirty {
            co.dirty_since = None;
            return false;
        }
        let now = self.clock.monotonic();
        let since = *co.dirty_since.get_or_insert(now.max(co.last_commit_at));
        let elapsed = now.saturating_sub(since);
        elapsed >= self.cfg.commit_interval
            || self.overlay.dirty_bytes() >= self.cfg.dirty_bytes
            || self.journal.fill().ratio() > self.cfg.journal_fill_trigger
            || self.journal_full.load(Ordering::Acquire)
    }

    /// Cuts at the current `write_seq` and commits (C1-C10). `force`
    /// commits even when nothing changed; `new_size` grows the volume.
    pub(crate) fn commit(
        &self,
        co: &mut Coord,
        force: bool,
        new_size: Option<u64>,
    ) -> Result<CommitOutcome> {
        if self.is_failed() {
            return Err(Error::Io(std::io::Error::other(
                "the engine stopped committing after a failure",
            )));
        }
        let prev = co.tip.cut_seq.0;
        let size = new_size.unwrap_or(co.tip.size).max(co.tip.size);
        // C1: pick c and wait for the in-flight writes at or below it.
        let mut c = self.journal.last_issued().0;
        let poisoned = self.poisoned_at.load(Ordering::Acquire);
        if poisoned != u64::MAX {
            c = c.min(poisoned.saturating_sub(1));
        }
        c = c.max(prev);
        if c == prev && !force && co.moved.is_empty() && size == co.tip.size {
            return Ok(CommitOutcome::Idle);
        }
        self.wait_inflight(c)?;
        point!(self.points, "engine.c1.waited");
        // A cut too big for the map's dirty-leaf cap is retried smaller:
        // any c' in (prev, c] is a cut too.
        loop {
            match self.cut_and_commit(co, c, size) {
                Err(e) if e.kind() == ErrorKind::MapFull && c > prev + 1 && !self.is_failed() => {
                    c = prev + (c - prev) / 2;
                    tracing::info!(cut = c, "map dirty-leaf cap reached: cutting smaller");
                }
                r => return r,
            }
        }
    }

    fn cut_and_commit(&self, co: &mut Coord, c: u64, size: u64) -> Result<CommitOutcome> {
        let prev = co.tip.cut_seq.0;
        let cs = u64::from(self.cfg.chunk_size);
        // C2: the MVCC cut and the ledger bounds.
        let cut = self.overlay.cut(WriteSeq(c));
        let mut acct = self.reserve_bounds(&cut);
        if let Err(e) = self.points.hit("engine.c2.cut") {
            self.abandon(acct);
            return Err(e);
        }
        lunix_iface::crashpoint!("engine.c2.cut");
        // C3: destage; the map changes in memory, index inserts held back.
        if size > co.tip.size {
            if let Err(e) = self.map.grow(size / cs) {
                self.abandon(acct);
                return Err(e);
            }
        }
        let writes = (c > prev).then(|| WriteSeqRange {
            first: WriteSeq(prev + 1),
            last: WriteSeq(c),
        });
        let ctx = DestageCtx {
            journal: &*self.journal,
            map: &*self.map,
            index: &*self.index,
            segments: &*self.tracked,
            codec: &*self.extents,
            reader: &*self.reader,
            chunk_size: self.cfg.chunk_size,
            id_len: self.cfg.id_len,
            writes,
        };
        let outcome = match self.destager.destage(&cut, &ctx) {
            Ok(o) => o,
            Err(e) => {
                // Nothing reached the map; abandoned writers are dropped.
                self.open_segs.clear();
                self.abandon(acct);
                return Err(e);
            }
        };
        let moved_segs = match self.apply_moved(co) {
            Ok(s) => s,
            Err(e) => {
                self.fatal("c3 rehint", &e);
                for w in &outcome.writers {
                    self.open_segs.remove(w.id());
                }
                return Err(e);
            }
        };
        let st = &self.stats;
        add(&st.chunks_total, outcome.chunks_total);
        add(&st.chunks_zero, outcome.chunks_zero);
        add(&st.dedup_hits, outcome.dedup_hits);
        add(&st.stored_ulen, outcome.stored_ulen);
        add(&st.stored_clen, outcome.stored_clen);
        add(&st.cold_rmw, outcome.cold_rmw);
        for (a, v) in st.stage_ns.iter().zip(outcome.stage_ns) {
            add(a, v);
        }
        let index_inserts = outcome.index_inserts;
        let mut writers = outcome.writers;
        if let Err(e) = self.points.hit("engine.c3.destaged") {
            for w in &writers {
                self.open_segs.remove(w.id());
            }
            return Err(e);
        }
        lunix_iface::crashpoint!("engine.c3.destaged");
        // C4: seal the segments (footer, trailer, fdatasync, rename, dir fsync).
        let mut new_segments = Vec::with_capacity(writers.len());
        let mut seg_actual = 0u64;
        while !writers.is_empty() {
            let w = writers.remove(0);
            let id = w.id();
            let r = w.seal(writes);
            self.open_segs.remove(id);
            match r {
                Ok(s) => {
                    seg_actual += s.size_bytes;
                    new_segments.push(s.seg);
                }
                Err(e) => {
                    for w in &writers {
                        self.open_segs.remove(w.id());
                    }
                    self.fatal("c4", &e);
                    return Err(e);
                }
            }
        }
        if let Err(e) = self.space.transfer(
            SpaceKind::Segments,
            SpaceKind::Segments,
            acct.seg_bound,
            seg_actual,
        ) {
            tracing::warn!(error = %e, "ledger transfer failed");
        }
        acct.seg_bound = 0;
        // Compaction output is named by the cut that applies its re-hints
        // (M17 rule 4); until then it is above the high-water mark.
        for s in moved_segs {
            if !new_segments.contains(&s) {
                new_segments.push(s);
            }
        }
        point!(self.points, "engine.c4.sealed");
        // C5: seal the map into a pack.
        let map_bound = acct.head_left.saturating_sub((64 << 10) + (4 << 10));
        let (root, map_packs) = match self.seal_map() {
            Ok(v) => v,
            Err(e) => {
                self.fatal("c5", &e);
                return Err(e);
            }
        };
        let map_actual = map_packs.iter().map(|(_, b)| *b).sum();
        self.settle(&mut acct, SpaceKind::Map, map_bound, map_actual);
        let map_packs: Vec<_> = map_packs.into_iter().map(|(p, _)| p).collect();
        point!(self.points, "engine.c5.map_sealed");
        // C6: the FULL commit, the durability point of the cut.
        let parent = &co.tip;
        let commit_seq = parent.common.commit_seq.next()?;
        let keep = u64::from(self.cfg.retain_last.max(1)) - 1;
        let chain_floor = CommitSeq(commit_seq.0.saturating_sub(keep)).max(parent.chain_floor);
        let index_packs = match self.index.runs() {
            Ok(r) => r.into_iter().map(|r| r.id).collect(),
            Err(e) => {
                self.fatal("c6 index runs", &e);
                return Err(e);
            }
        };
        let full = Full {
            common: CommonHeader {
                kind: CommitKind::Full,
                commit_seq,
                epoch: Epoch(0),
                parent_hash: co.tip_hash,
                // D17 writer clamp: never before the parent.
                created_at: TimeMs(self.clock.now().0.max(parent.common.created_at.0)),
            },
            cut_seq: WriteSeq(c),
            size,
            chain_floor,
            map_root: root,
            heat: None,
            map_packs,
            new_segments: new_segments.clone(),
            index_packs,
        };
        let commit = Commit::Full(full.clone());
        let sealed = match self
            .commit_codec
            .seal_commit(&commit)
            .and_then(|s| self.commits.put(commit_seq, &s.bytes).map(|()| s))
        {
            Ok(s) => s,
            Err(e) => {
                self.fatal("c6", &e);
                return Err(e);
            }
        };
        self.settle(
            &mut acct,
            SpaceKind::Commits,
            64 << 10,
            sealed.bytes.len() as u64,
        );
        co.tip = full;
        co.tip_hash = sealed.hash;
        co.retained.push(commit);
        co.retained.retain(|r| r.common().commit_seq >= chain_floor);
        co.last_commit_at = self.clock.monotonic();
        co.dirty_since = None;
        if let Some(&m) = new_segments.iter().max() {
            co.upto_seg = Some(co.upto_seg.map_or(m, |u| u.max(m)));
        }
        self.size.store(size, Ordering::Release);
        add(&self.stats.commits, 1);
        point!(self.points, "engine.c6.committed");
        // C7: the checkpoint hint, then the rollback floor (D19: only after
        // C6 is durable, so the floor never exceeds the durable tip).
        let tail = self.journal.tail_after(WriteSeq(c));
        let slot = CheckpointSlot {
            slot_seq: 0,
            volume_uuid: self.dir.uuid(),
            last_commit_seq: commit_seq.0,
            id_high: self.dir.ids().high_water(),
            lane_tail_ordinals: tail
                .as_ref()
                .map(|t| t.lane_ordinals.clone())
                .unwrap_or_default(),
            stats: {
                let mut s = self.stats.snapshot();
                s.dirty_leaves = self.map.dirty_leaf_count();
                s.to_slots()
            },
        };
        if tail.is_ok() {
            if let Err(e) = self.checkpoint.store(&slot) {
                tracing::warn!(error = %e, "checkpoint hint not written");
            }
        }
        point!(self.points, "engine.c7.checkpoint");
        if let Err(e) = self.floor.raise(&RollbackFloor {
            max_commit_seq: commit_seq,
            tip_hash: sealed.hash,
        }) {
            tracing::warn!(error = %e, "rollback floor not raised (it stays lower, which is safe)");
        }
        point!(self.points, "engine.c7.floor");
        // C8: index entries for the sealed segments, then their run (named
        // by the next FULL, D3).
        let mut index_ok = true;
        for (id, hint) in index_inserts {
            if let Err(e) = self.index.insert(id, hint) {
                tracing::warn!(error = %e, "index insert failed; catch_up restores it at the next open");
                index_ok = false;
                break;
            }
        }
        if index_ok && !new_segments.is_empty() {
            if let Some(upto) = co.upto_seg {
                if let Err(e) = self.index.persist(upto) {
                    tracing::warn!(error = %e, "index run not persisted");
                }
            }
        }
        self.true_up_index(co, &mut acct);
        point!(self.points, "engine.c8.index");
        // C9: reclaim the journal below the cut, release the overlay.
        match &tail {
            Ok(t) => match self.journal.reclaim(t) {
                Ok(rep) => {
                    let held = self.journal_charged.load(Ordering::Acquire);
                    let give = rep.bytes.min(held);
                    self.journal_charged.fetch_sub(give, Ordering::AcqRel);
                    self.space.release(SpaceKind::JournalReserved, give);
                }
                Err(e) => {
                    tracing::warn!(error = %e, "journal reclaim failed; retried at the next cut")
                }
            },
            Err(e) => tracing::warn!(error = %e, "journal tail unknown; nothing reclaimed"),
        }
        self.overlay.release(&cut);
        self.abandon(acct);
        self.journal_full.store(false, Ordering::Release);
        point!(self.points, "engine.c9.reclaimed");
        self.signal_queues();
        // C10: one budgeted GC step, only between cuts with the map at the tip.
        self.gc_step(co);
        Ok(CommitOutcome::Committed {
            commit_seq,
            cut_seq: WriteSeq(c),
        })
    }

    /// C5. Returns the root and the sealed packs with their sizes.
    fn seal_map(
        &self,
    ) -> Result<(
        lunix_format::commit::MapRoot,
        Vec<(lunix_format::PackId, u64)>,
    )> {
        let mut pack = self.packs.open_pack()?;
        let res = self.map.seal(&mut *pack)?;
        if pack.is_empty() {
            return Ok((res.root, Vec::new()));
        }
        let sealed = pack.seal()?;
        Ok((res.root, vec![(sealed.pack, sealed.size_bytes)]))
    }

    /// Keeps the `Index` charge equal to the bytes of every run file.
    fn true_up_index(&self, co: &mut Coord, acct: &mut Acct) {
        let Ok(runs) = self.index.all_runs() else {
            return;
        };
        let total: u64 = runs.iter().map(|r| r.size_bytes).sum();
        if total >= co.index_charged {
            let grow = total - co.index_charged;
            self.settle(acct, SpaceKind::Index, grow, grow);
        } else {
            self.space
                .release(SpaceKind::Index, co.index_charged - total);
        }
        co.index_charged = total;
    }

    /// C3, second half: compaction's `moved` records become conditional
    /// re-hints (a newer write wins). `Moved` carries no chunk number, so
    /// the chunks are found by walking the tip's leaves for the old hints.
    fn apply_moved(&self, co: &mut Coord) -> Result<Vec<SegId>> {
        if co.moved.is_empty() {
            return Ok(Vec::new());
        }
        let mut segs: Vec<SegId> = co.moved.iter().map(|m| m.new.seg).collect();
        segs.sort_unstable();
        segs.dedup();
        let olds: HashMap<Hint, Hint> = co.moved.iter().map(|m| (m.old, m.new)).collect();
        let mut hits: Vec<(ChunkNo, Hint, Hint)> = Vec::new();
        let root = co.tip.map_root;
        self.map.walk_leaves(&root, &mut |ci, e| {
            let h = entry_hint(e);
            if let Some(&new) = olds.get(&h) {
                hits.push((ci, h, new));
            }
        })?;
        let mut txn = self.map.begin_update()?;
        for (ci, old, new) in hits {
            txn.rehint(ci, old, new)?;
        }
        txn.commit()?;
        co.moved.clear();
        Ok(segs)
    }

    fn gc_step(&self, co: &mut Coord) {
        let (Some(budget), Some(params)) = (self.cfg.gc_budget, self.cfg.gc_params) else {
            return;
        };
        if co.collector.is_none() {
            return;
        }
        // The hook's preconditions (M17): between cuts, map root = tip root.
        if self.map.root() != co.tip.map_root || self.map.dirty_leaf_count() != 0 {
            return;
        }
        let tip = TipInfo {
            commit_seq: co.tip.common.commit_seq,
            map_root: co.tip.map_root,
            chain_floor: co.tip.chain_floor,
            high_water_mark: high_water(&co.retained),
        };
        let retained = co.retained.clone();
        let view = GcView {
            retained: &retained,
            tip,
            pins: &*self.pins,
            map: &*self.map,
            index: &*self.index,
            segments: &*self.segments,
            packs: &*self.packs,
            commits: &*self.commits,
            commit_codec: &*self.commit_codec,
            extents: &*self.extents,
            state: &*self.gc_state,
            space: &*self.space,
            clock: &*self.clock,
            params,
        };
        let Some(collector) = co.collector.as_mut() else {
            return;
        };
        match collector.step(&view, budget) {
            Ok(p) => co.moved.extend(p.moved),
            Err(e) => tracing::warn!(error = %e, "gc step failed; the pass resumes later"),
        }
    }
}
