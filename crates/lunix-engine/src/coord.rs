//! The cut and commit coordinator: the local FULL commit protocol C1-C10
//! (plan M11 step 5, DESIGN §8.1-§8.2, ADR-9).
//!
//! Durable order: C3 (the destager seals every segment before its map
//! transaction commits) and C5 (map pack sealed) before C6 (the commit
//! object, the durability point of the cut), C6 before the rollback floor
//! (C7) and before any journal reclaim (C9). C4 fixes `new_segments` and
//! trues up the ledger. A failure before C6 leaves the previous commit as
//! the tip; what C3/C5 wrote is an orphan of the open-time sweep.
//!
//! Failure handling: a destage failure leaves the map untouched and is
//! classified ([`Retry`]): `MapFull` classes cut smaller, `Later` and
//! `AfterSpace` fail this attempt (the next trigger retries), `Never` and
//! `Stop` stop the engine's commits (writes fail, reads continue). Compaction
//! re-hints run right after a successful destage, in their own transaction
//! (a failed destage leaves the map at the tip, so GC can still run); a
//! failure there keeps them for the next cut; their output segments are named
//! by this cut's FULL all the same (N10: naming an existing sealed segment is always
//! safe, and `persist` may then cover their relocations). A failure after the
//! destage committed its transaction (C5, C6) stops further commits in this
//! process (the in-memory map is ahead of the durable state) until the
//! volume is reopened.
//!
//! The index (N13): the entries of every segment a durable commit names are
//! persisted before another FULL is written. A failed `persist` at C8 (or of
//! the catch-up at open) leaves the commit durable and marks the index
//! unpersisted; the next cut retries it first and fails, writing nothing,
//! while it still fails, so the FULL that names those segments cannot leave
//! the retained window before their entries are in a run.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;
use std::time::Duration;

use lunix_format::commit::{Commit, CommitKind, CommonHeader, Full};
use lunix_format::local::{CheckpointSlot, RollbackFloor};
use lunix_format::{CommitSeq, Epoch, Hint, SegId, TimeMs, WriteSeq};
use lunix_iface::gc::{Collector, GcProgress, GcState, Moved};
use lunix_iface::journal::WriteSeqRange;
use lunix_iface::map::{entry_hint, ChunkNo};
use lunix_iface::overlay::OverlayCut;
use lunix_iface::space::SpaceKind;
use lunix_iface::{Error, ErrorKind, Result};

use crate::destage::{DestageCtx, Retry};
use crate::engine::{CommitOutcome, Inner};
use crate::recover::Recovered;
use crate::stats::add;

/// The shortest time between two cuts made for pending compaction moves
/// alone (`wanted`).
const MOVES_CUT_GAP: Duration = Duration::from_secs(1);

/// After a cut, a busy collector steps for up to the cut's time divided by
/// this (`gc_share`): a quarter of the coordinator.
const GC_SHARE_DIVISOR: u32 = 3;

/// The longest share after one cut (`gc_share`): a long cut does not hold the
/// coordinator (the ladder, `grow`, the window turn, ring-parked writes) for
/// a third of its time.
const GC_SHARE_MAX: Duration = Duration::from_secs(1);

/// Steps in a row without the clock moving that end a share (`gc_share`).
const GC_SHARE_STALLS: u32 = 8;

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
    /// Output segments of re-hints already applied to the in-memory map and
    /// not yet named by a durable commit (M17 rule 4).
    pub(crate) pending_named: Vec<SegId>,
    /// The last background commit error, logged once per change.
    pub(crate) last_error: Option<String>,
    pub(crate) collector: Option<Box<dyn Collector>>,
    /// When a tick first saw the volume dirty since the last commit (`T`
    /// counts from there, so an idle period does not fire it at once).
    pub(crate) dirty_since: Option<Duration>,
    /// The highest segment a durable commit names (for `persist`).
    pub(crate) upto_seg: Option<SegId>,
    /// A `persist` failed (at C8 or of the catch-up at open): the next cut
    /// retries it before anything else and fails while it fails (N13).
    pub(crate) index_unpersisted: bool,
    /// When the last urgent pass ended (a commit that failed for space, or
    /// the space ladder's compact step): `space_short` waits
    /// `space_backoff` after it.
    pub(crate) last_space_urgent: Option<Duration>,
    /// The pause between two urgent passes run because commits fail for
    /// space: back to the minimum after a successful commit or a pass that
    /// freed bytes, at most the floor grace after one that condemned, else
    /// doubling to a maximum.
    pub(crate) space_backoff: Duration,
    /// The last `apply_moved` left moves (for want of room under the map's
    /// dirty-leaf cap) or failed: moves alone then trigger a cut only once
    /// per commit interval (each such cut walks the tip's leaves).
    pub(crate) moves_deferred: bool,
    /// The collector asked for a cut (`GcProgress::cut_wanted`): a FULL below
    /// the tip must name a compaction output before its source can go.
    pub(crate) gc_cut: bool,
    /// The last GC step error, logged once per change.
    pub(crate) gc_error: Option<String>,
    /// The last error of `apply_moved`, logged once per change.
    pub(crate) rehint_error: Option<String>,
    /// The journal bytes and durations of the last committed cuts: the drain
    /// rate behind the bound of the next one (ADR-33).
    pub(crate) drain: crate::cut::DrainHistory,
    /// The journal bytes the cut in flight released at C9.
    pub(crate) cut_bytes: u64,
    /// `clock.monotonic()` when the cut in flight finished C9 (the GC step of
    /// C10 is not part of the cut's time).
    pub(crate) cut_end: Duration,
}

impl Coord {
    /// Takes what a collector call handed over: its moves for the next cut,
    /// and its request for a cut (`GcProgress::cut_wanted`).
    pub(crate) fn absorb(&mut self, p: GcProgress) {
        self.moved.extend(p.moved);
        self.gc_cut |= p.cut_wanted;
    }

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
            pending_named: Vec::new(),
            last_error: None,
            dirty_since: None,
            collector,
            upto_seg: rec.upto_seg,
            index_unpersisted: rec.index_unpersisted,
            last_space_urgent: None,
            space_backoff: crate::gc::SPACE_URGENT_MIN,
            moves_deferred: false,
            gc_cut: false,
            gc_error: None,
            rehint_error: None,
            drain: crate::cut::DrainHistory::default(),
            cut_bytes: 0,
            cut_end: Duration::ZERO,
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

/// Why one cut attempt failed.
enum CutFail {
    /// The cut does not fit the map's dirty-leaf cap: try a smaller one.
    Smaller(Error),
    /// Anything else.
    Other(Error),
}

impl From<Error> for CutFail {
    fn from(e: Error) -> Self {
        Self::Other(e)
    }
}

impl Inner {
    fn reserve_bounds(&self, cut: &OverlayCut) -> Acct {
        let n = cut.chunks.len() as u64;
        let ovh = u64::from(self.extents.mode().extent_ovh());
        let cs = u64::from(self.cfg.chunk_size);
        let seg_bound = n.saturating_mul(cs + ovh + 64).saturating_add(64 << 10);
        // Leaves touched, each with its path to the root (one path per leaf
        // at worst), each node at most a full node extent.
        let fanout = u64::from(self.cfg.map_fanout.max(2));
        let leaves: HashSet<u64> = cut.chunks.iter().map(|c| c.chunk / fanout).collect();
        let mut depth = 1u64;
        let mut span = fanout;
        while span < self.map.chunk_count() {
            span = span.saturating_mul(fanout);
            depth += 1;
        }
        let map_bound = (leaves.len() as u64)
            .saturating_mul(depth)
            .saturating_add(2)
            .saturating_mul(NODE_BOUND + ovh);
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

    /// Acts on a classified destage failure (the map is untouched).
    fn destage_failed(&self, cut: &OverlayCut, f: crate::destage::DestageFailure) -> CutFail {
        match f.retry {
            Retry::AfterMapSeal | Retry::CutTooLarge => CutFail::Smaller(f.error),
            Retry::Later | Retry::AfterSpace => CutFail::Other(f.error),
            Retry::Never | Retry::Stop => {
                let cb = self.cfg.chunk_blocks();
                let lbas = f.chunk.map(|c| (c * cb, (c + 1) * cb));
                tracing::error!(
                    error = %f.error,
                    retry = ?f.retry,
                    chunk = ?f.chunk,
                    lba_range = ?lbas,
                    cut = %cut.cut_seq,
                    "destage cannot complete this cut: no more commits in this process; writes fail, reads continue"
                );
                self.failed.store(true, Ordering::Release);
                self.signal_queues();
                CutFail::Other(f.error)
            }
        }
    }

    fn fatal(&self, step: &str, e: &Error) {
        if !self.points.is_dead() {
            tracing::error!(step, error = %e, "commit failed after the map changed: no more commits in this process");
        }
        self.failed.store(true, Ordering::Release);
        self.signal_queues();
    }

    /// The trigger check (plan M11 step 5): `T`, `B`, journal fill, a full
    /// ring, pending `moved` records. An idle volume cuts nothing.
    pub(crate) fn tick(&self) -> Result<CommitOutcome> {
        // ADR-32 T4: the collector's pressure follows the gate's state.
        self.update_pressure();
        let mut co = crate::engine::lock(&self.coord);
        let wanted = self.wanted(&mut co);
        // ADR-32 T2: a full volume cuts its discards and turns the window.
        let space_cut = self.space_cut(&co);
        if wanted || space_cut.is_some() {
            let cut_started = self.clock.monotonic();
            let res = self.commit(&mut co, space_cut == Some(true), None, true);
            self.after_space_cut(&co, space_cut, &res);
            // A commit that failed before it changed the map (no space for
            // the cut above all, an index run still unpersisted, writes in
            // flight) still lets the collector work: a volume that cannot
            // commit must still free what its condemned objects hold (M17.B
            // stack review S-2, M12c review S-3). So does a commit that had
            // nothing to cut after all (`Idle`, e.g. a cut held below a
            // poisoned write; M12c merge review M-3). One rule: `gc_step`
            // runs when the map is the tip's and does nothing otherwise; a
            // committed cut stepped it at C10 already. After a failure in
            // `retry_persist` the collector runs while index entries of named
            // segments are in memory only (N13): safe, since the tip's names
            // never count for `named_high` and a FULL is written only after
            // the persist before it succeeded (M17.B stack re-review M-6).
            let committed = matches!(res, Ok(CommitOutcome::Committed { .. }));
            if !committed && !self.is_failed() {
                self.gc_step(&mut co);
            } else if committed {
                self.gc_share(&mut co, cut_started);
            }
            res
        } else {
            // An idle volume still steps the collector (M17.A review minor
            // 4): a pass waiting in its grace finishes without a cut. Its
            // moves (or a cut it asks for) make the next tick cut.
            if !self.is_failed() {
                self.gc_step(&mut co);
            }
            Ok(CommitOutcome::Idle)
        }
    }

    /// The background thread's tick: logs a failure once per distinct error
    /// instead of every period.
    pub(crate) fn background_tick(&self) {
        let res = self.tick();
        // A commit is followed at once while the ring stays above the fill
        // trigger (`coordinator_wait`); anything else, an error above all,
        // waits the period.
        self.back_to_back.store(
            matches!(res, Ok(CommitOutcome::Committed { .. })),
            Ordering::Release,
        );
        let mut co = crate::engine::lock(&self.coord);
        match res {
            Ok(_) => co.last_error = None,
            Err(e) => {
                let msg = e.to_string();
                if co.last_error.as_deref() != Some(msg.as_str()) {
                    tracing::warn!(error = %msg, "background commit failed; retrying at the next trigger");
                    co.last_error = Some(msg);
                }
            }
        }
    }

    fn wanted(&self, co: &mut Coord) -> bool {
        if !co.pending_named.is_empty() || co.index_unpersisted || co.gc_cut {
            return true;
        }
        let now = self.clock.monotonic();
        // Under writes (new writes since the tip) while a pass is still
        // compacting (`gc_busy`), pending moves cut at most once per
        // `MOVES_CUT_GAP`: with outputs sealed back to back a cut per tick
        // would leave the coordinator to the cuts, and each FULL leaves a
        // commit object and packs to collect (milestone gcrate). Otherwise,
        // an idle volume above all, they cut at once, as before.
        let since = now.saturating_sub(co.last_commit_at);
        let writing = self.journal.last_issued() > co.tip.cut_seq;
        let paced = !writing
            || !self.gc_busy.load(Ordering::Acquire)
            || since >= MOVES_CUT_GAP.min(self.cfg.commit_interval);
        if !co.moved.is_empty()
            && paced
            && (!co.moves_deferred || since >= self.cfg.commit_interval)
        {
            return true;
        }
        let dirty = self.journal.last_issued() > co.tip.cut_seq;
        if !dirty {
            co.dirty_since = None;
            return false;
        }
        let since = *co.dirty_since.get_or_insert(now.max(co.last_commit_at));
        let elapsed = now.saturating_sub(since);
        elapsed >= self.cfg.commit_interval
            || self.overlay.dirty_bytes() >= self.cfg.dirty_bytes
            || self.journal.fill().ratio() > self.cfg.journal_fill_trigger
            || self.journal_full.load(Ordering::Acquire)
            || self.cuts.ring_waiters.load(Ordering::Relaxed) > 0
    }

    /// Cuts at the current `write_seq` and commits (C1-C10). `force`
    /// commits even when nothing changed; `new_size` grows the volume.
    /// `bounded` cuts a prefix bounded by journal bytes (`bound_cut_bytes`)
    /// instead of everything written so far. A commit that fails for space, at
    /// any step (the destage, the index run's retry), runs GC's urgent pass
    /// (`space_short`).
    pub(crate) fn commit(
        &self,
        co: &mut Coord,
        force: bool,
        new_size: Option<u64>,
        bounded: bool,
    ) -> Result<CommitOutcome> {
        let res = self.commit_once(co, force, new_size, bounded);
        self.full.note_commit(&res, self.clock.monotonic());
        match &res {
            Err(e) if e.kind() == ErrorKind::NoSpace => self.space_short(co),
            Err(_) => {}
            Ok(_) => co.space_backoff = crate::gc::SPACE_URGENT_MIN,
        }
        res
    }

    fn commit_once(
        &self,
        co: &mut Coord,
        force: bool,
        new_size: Option<u64>,
        bounded: bool,
    ) -> Result<CommitOutcome> {
        if self.is_failed() {
            return Err(crate::engine::stopped());
        }
        // N13: no new FULL while index entries of named segments are only
        // in memory.
        self.retry_persist(co)?;
        let prev = co.tip.cut_seq.0;
        let size = new_size.unwrap_or(co.tip.size).max(co.tip.size);
        // C1: pick c and wait for the in-flight writes at or below it.
        let mut c = self.journal.last_issued().0;
        let poisoned = self.poisoned_at.load(Ordering::Acquire);
        if poisoned != u64::MAX {
            c = c.min(poisoned.saturating_sub(1));
        }
        c = c.max(prev);
        let idle = co.moved.is_empty() && co.pending_named.is_empty() && !co.gc_cut;
        if c == prev && !force && idle && size == co.tip.size {
            return Ok(CommitOutcome::Idle);
        }
        self.wait_inflight(c)?;
        // A write that failed during the wait left a hole at its number: the
        // cut stays below it (it would claim a prefix it does not hold).
        let poisoned = self.poisoned_at.load(Ordering::Acquire);
        if poisoned != u64::MAX {
            c = c.min(poisoned.saturating_sub(1)).max(prev);
        }
        if bounded {
            c = self.bound_cut_bytes(co, prev, c);
        }
        c = self.bound_cut(prev, c);
        point!(self.points, "engine.c1.waited");
        let cut_started = self.clock.monotonic();
        // A cut too big for the map's dirty-leaf cap is retried smaller:
        // any c' in (prev, c] is a cut too.
        loop {
            match self.cut_and_commit(co, c, size) {
                Err(CutFail::Smaller(e)) => {
                    if c <= prev + 1 || self.is_failed() {
                        return Err(e);
                    }
                    c = prev + (c - prev) / 2;
                    tracing::info!(cut = c, "map dirty-leaf cap reached: cutting smaller");
                }
                Err(CutFail::Other(e)) => return Err(e),
                Ok(o) => {
                    if matches!(o, CommitOutcome::Committed { .. }) {
                        self.note_cut(co, cut_started);
                    }
                    return Ok(o);
                }
            }
        }
    }

    /// ADR-33: lowers `c` to a cut of at most `cut_target` times the drain
    /// rate of the last cuts in journal bytes (the charge lists), at most half
    /// the ring (in record bytes, see below), at least one ring file of bytes
    /// and at least what lets reclaim free a ring file of the fullest lane
    /// (`Journal::cut_freeing_file`; the floor wins over the half ring, so
    /// every commit under pressure frees something). With no history the rate
    /// bounds nothing. Every `c'` in `(prev, c]` with all writes `<= c` in the
    /// overlay is a prefix cut, as `bound_cut` relies on. Under a light load
    /// the writes since the last cut are fewer than the bound and `c` stays.
    fn bound_cut_bytes(&self, co: &Coord, prev: u64, c: u64) -> u64 {
        if c <= prev + 1 {
            return c;
        }
        let fill = self.journal.fill();
        // The ceiling is half the ring, in the units of the charges: a ring
        // file holds fewer record bytes than its length (batch padding, file
        // headers), so half the ring's bytes is scaled by what the charged
        // records occupy of the files in use.
        let held = self.journal_charged.load(Ordering::Acquire);
        let scale = |bytes: u64| {
            if fill.used_bytes == 0 {
                bytes
            } else {
                let n = u128::from(held.min(fill.used_bytes));
                u64::try_from(u128::from(bytes) * n / u128::from(fill.used_bytes))
                    .unwrap_or(u64::MAX)
            }
        };
        let half = scale(fill.capacity_bytes / 2);
        // The rate bounds the cut, the half ring caps it, and one ring file
        // (in the same record-byte units) is the least: a fixed commit cost at or above `cut_target`
        // cannot shrink the cuts below it.
        let budget = co
            .drain
            .budget(self.cfg.cut_target)
            .map_or(half, |b| b.min(half))
            .max(
                self.cfg
                    .cut_min_bytes
                    .unwrap_or_else(|| scale(fill.file_bytes)),
            );
        let floor = self
            .journal
            .cut_freeing_file()
            .map_or(0, |w| w.0)
            .clamp(prev + 1, c);
        let bounded = self.jcharges.cut_for(budget, floor, c).clamp(prev + 1, c);
        if bounded < c {
            tracing::debug!(
                cut = c,
                bounded,
                budget,
                floor,
                "cut bounded by journal bytes"
            );
        }
        bounded
    }

    /// Notes a committed cut: its journal bytes and duration feed the next
    /// bound and the status.
    fn note_cut(&self, co: &mut Coord, started: Duration) {
        let took = co.cut_end.saturating_sub(started);
        // A cut without journal bytes (a forced FULL, GC moves) says nothing
        // about the drain rate.
        if co.cut_bytes > 0 {
            co.drain.push(co.cut_bytes, took);
        }
        let c = &self.cuts;
        c.last_cut_bytes.store(co.cut_bytes, Ordering::Relaxed);
        c.last_cut_ms.store(
            u64::try_from(took.as_millis()).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
    }

    /// Lowers `c` so that the cut dirties at most `max_leaves_per_cut` map
    /// leaves (`chunk / map_fanout`): the chunks in `cut(c')` are those with
    /// a version `<= c'`, so taking chunks in the order of their oldest
    /// visible write, `c'` stops just before the first chunk whose leaf
    /// would exceed the cap. Best effort (a superseded older version can add
    /// a chunk); a `MapFull` that still happens cuts smaller again. Any
    /// `c' > prev` with every write `<= c` in the overlay is a cut. The probe
    /// is a second MVCC view, taken only when the overlay holds more blocks
    /// than the cap could possibly need.
    fn bound_cut(&self, prev: u64, c: u64) -> u64 {
        let cap = self.cfg.max_leaves_per_cut.max(1);
        if c <= prev || self.overlay.dirty_bytes() / 4096 <= cap {
            return c;
        }
        let fanout = u64::from(self.cfg.map_fanout.max(2));
        let probe = self.overlay.cut(WriteSeq(c));
        let mut firsts: Vec<(u64, u64)> = probe
            .chunks
            .iter()
            .map(|ch| {
                let first = ch.pieces.iter().map(|p| p.write_seq.0).min().unwrap_or(c);
                (first, ch.chunk / fanout)
            })
            .collect();
        firsts.sort_unstable();
        let mut leaves = HashSet::new();
        for (first, leaf) in firsts {
            leaves.insert(leaf);
            if leaves.len() as u64 > cap {
                let bounded = first.saturating_sub(1).clamp(prev + 1, c);
                tracing::debug!(cut = c, bounded, "cut bounded by max_leaves_per_cut");
                return bounded;
            }
        }
        c
    }

    fn cut_and_commit(
        &self,
        co: &mut Coord,
        c: u64,
        size: u64,
    ) -> std::result::Result<CommitOutcome, CutFail> {
        let prev = co.tip.cut_seq.0;
        let cs = u64::from(self.cfg.chunk_size);
        // C2: the MVCC cut and the ledger bounds.
        let cut = self.overlay.cut(WriteSeq(c));
        let mut acct = self.reserve_bounds(&cut);
        if let Err(e) = self.points.hit("engine.c2.cut") {
            self.abandon(acct);
            return Err(CutFail::Other(e));
        }
        lunix_iface::crashpoint!("engine.c2.cut");
        // C3: destage; every segment sealed before the map transaction
        // commits, index inserts held back.
        let writes = (c > prev).then(|| WriteSeqRange {
            first: WriteSeq(prev + 1),
            last: WriteSeq(c),
        });
        let ctx = DestageCtx {
            journal: &*self.journal,
            map: &*self.map,
            index: &*self.index,
            segments: &*self.segments,
            codec: &*self.extents,
            reader: &*self.reader,
            space: &*self.space,
            chunk_size: self.cfg.chunk_size,
            id_len: self.cfg.id_len,
            writes,
        };
        let outcome = match self.destager.destage(&cut, &ctx) {
            Ok(o) => o,
            Err(f) => {
                // Nothing reached the map; the destager removed its segments.
                self.abandon(acct);
                return Err(self.destage_failed(&cut, f));
            }
        };
        let st = &self.stats;
        add(&st.chunks_total, outcome.chunks_total);
        add(&st.chunks_zero, outcome.chunks_zero);
        add(&st.dedup_hits, outcome.dedup_hits);
        add(&st.stored_ulen, outcome.stored_ulen);
        add(&st.stored_clen, outcome.stored_clen);
        add(&st.cold_rmw, outcome.cold_rmw);
        add(&st.extent_auth_failures, outcome.extent_auth_failures);
        for (a, v) in st.stage_ns.iter().zip(outcome.stage_ns) {
            add(a, v);
        }
        let index_inserts = outcome.index_inserts;
        let segments = outcome.segments;
        // C3, second: compaction re-hints, in their own transaction, only
        // after the destage succeeded (M12c review B-1): a destage that fails
        // (for space, typically) then leaves the map at the tip, so GC (the
        // urgent pass that frees the space) can still run. A re-hint is
        // conditional on the old hint, so a chunk this cut rewrote keeps its
        // new entry. A failure keeps them for the next cut (nothing was
        // applied); a later failure of this cut leaves them applied in
        // memory, pointing at sealed segments, still to be named
        // (`pending_named`).
        match self.apply_moved(co) {
            Ok(()) => co.rehint_error = None,
            Err(e) => {
                // Whatever failed (the leaf walk, the transaction), the moves
                // wait: retried once per commit interval, not at every tick
                // (M12c merge review M-2); logged once per distinct error.
                co.moves_deferred = true;
                let msg = e.to_string();
                if co.rehint_error.as_deref() != Some(msg.as_str()) {
                    tracing::warn!(error = %msg, "compaction re-hints deferred to a later cut; their outputs are named by this cut");
                    co.rehint_error = Some(msg);
                }
            }
        }
        self.points
            .hit("engine.c3.destaged")
            .map_err(CutFail::Other)?;
        lunix_iface::crashpoint!("engine.c3.destaged");
        // C4: the segments are sealed (footer, trailer, fdatasync, rename, dir
        // fsync) by the destager before its map transaction committed, so no
        // map entry ever names an unsealed segment; here `new_segments` is
        // fixed and the ledger trued up.
        let mut new_segments: Vec<SegId> = segments.iter().map(|s| s.seg).collect();
        let seg_actual: u64 = segments.iter().map(|s| s.size_bytes).sum();
        if let Err(e) = self.space.transfer(
            SpaceKind::Segments,
            SpaceKind::Segments,
            acct.seg_bound,
            seg_actual,
        ) {
            tracing::warn!(error = %e, "ledger transfer failed");
        }
        acct.seg_bound = 0;
        // Compaction output is named by the first cut after its seal (M17
        // rule 4, N10): the outputs of re-hints applied now or earlier
        // (`pending_named`) and of those deferred by a failed `apply_moved`
        // (still in `moved`). Naming an existing sealed segment is always
        // safe; a deferred output is named again by the cut that applies it.
        let outputs = co
            .pending_named
            .iter()
            .copied()
            .chain(co.moved.iter().map(|m| m.new.seg));
        new_segments.extend(outputs);
        // The commit format wants the ids strictly ascending; an output is
        // older than this cut's own segments.
        new_segments.sort_unstable();
        new_segments.dedup();
        self.points.hit("engine.c4.named").map_err(CutFail::Other)?;
        lunix_iface::crashpoint!("engine.c4.named");
        // C5: grow the map now (a failed destage above leaves it at the
        // committed size; this cut holds no chunk past the old size, because
        // the new size is published only after C6), then seal it into a pack.
        if size > co.tip.size {
            if let Err(e) = self.map.grow(size / cs) {
                self.fatal("c5 grow", &e);
                return Err(CutFail::Other(e));
            }
        }
        let map_bound = acct.head_left.saturating_sub((64 << 10) + (4 << 10));
        let (root, map_packs) = match self.seal_map() {
            Ok(v) => v,
            Err(e) => {
                self.fatal("c5", &e);
                return Err(e.into());
            }
        };
        let map_actual = map_packs.iter().map(|(_, b)| *b).sum();
        self.settle(&mut acct, SpaceKind::Map, map_bound, map_actual);
        let map_packs: Vec<_> = map_packs.into_iter().map(|(p, _)| p).collect();
        point!(self.points, "engine.c5.map_sealed");
        // C6: the FULL commit, the durability point of the cut.
        let parent = &co.tip;
        let commit_seq = parent.common.commit_seq.next().map_err(Error::from)?;
        let keep = u64::from(self.cfg.retain_last.max(1)) - 1;
        let chain_floor = CommitSeq(commit_seq.0.saturating_sub(keep)).max(parent.chain_floor);
        let index_packs = match self.index.runs() {
            Ok(r) => r.into_iter().map(|r| r.id).collect(),
            Err(e) => {
                self.fatal("c6 index runs", &e);
                return Err(e.into());
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
                return Err(e.into());
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
        self.full.note_cut(prev, c, commit_seq.0);
        self.publish_tip(co);
        co.retained.push(commit);
        co.retained.retain(|r| r.common().commit_seq >= chain_floor);
        co.last_commit_at = self.clock.monotonic();
        co.dirty_since = None;
        if let Some(&m) = new_segments.iter().max() {
            co.upto_seg = Some(co.upto_seg.map_or(m, |u| u.max(m)));
        }
        co.pending_named.clear();
        co.gc_cut = false;
        self.size.store(size, Ordering::Release);
        self.durable_cut.store(c, Ordering::Release);
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
        let mut failed_inserts = 0u64;
        let mut first_error = None;
        for (id, hint) in index_inserts {
            if let Err(e) = self.index.insert(id, hint) {
                failed_inserts += 1;
                first_error.get_or_insert(e);
            }
        }
        if let Some(e) = first_error {
            tracing::warn!(error = %e, failed_inserts, "index inserts failed: those chunks miss dedup until a rebuild");
        }
        // Every segment this FULL names gets its entries into a run before
        // the next FULL (N13); the catch-up at open persisted its own. A
        // failure keeps this commit (it is durable) and blocks the next cut
        // until a retry succeeds.
        if !new_segments.is_empty() || co.index_unpersisted {
            if let Some(upto) = co.upto_seg {
                match self.index.persist(upto) {
                    Ok(_) => co.index_unpersisted = false,
                    Err(e) => {
                        tracing::error!(error = %e, commit = commit_seq.0, "index run not persisted: no further commit until a retry succeeds");
                        co.index_unpersisted = true;
                    }
                }
            }
        }
        self.true_up_index(Some(&mut acct));
        point!(self.points, "engine.c8.index");
        // C9: reclaim the journal below the cut, release the overlay.
        // The ring files are the journal's to recycle; the charge of every
        // record at or below the cut goes now, whichever files were reused
        // (ADR-32 review M4): the ledger then equals what a restart charges.
        // Cleared before the reclaim: a write that parks again while the
        // reclaim goes on (it wakes lane by lane) keeps the flag.
        self.journal_full.store(false, Ordering::Release);
        match &tail {
            Ok(t) => {
                if let Err(e) = self.journal.reclaim(t) {
                    tracing::warn!(error = %e, "journal reclaim failed; retried at the next cut");
                }
            }
            Err(e) => tracing::warn!(error = %e, "journal tail unknown; nothing reclaimed"),
        }
        let held = self.journal_charged.load(Ordering::Acquire);
        let released = self.jcharges.take_upto(c);
        co.cut_bytes = released;
        let give = released.min(held);
        self.journal_charged.fetch_sub(give, Ordering::AcqRel);
        self.space.release(SpaceKind::JournalReserved, give);
        self.overlay.release(&cut);
        self.abandon(acct);
        co.cut_end = self.clock.monotonic();
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

    /// Keeps the `Index` charge equal to the bytes of every run file,
    /// against the ledger's own `Index` value: GC releases the bytes of each
    /// run it deletes (iface `space` walk-through step 6), so a private
    /// counter would release them a second time. Growth comes out of the
    /// cut's held headroom when there is one.
    fn true_up_index(&self, acct: Option<&mut Acct>) {
        let runs = match self.index.all_runs() {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, "index runs not listed; the Index charge is trued up later");
                return;
            }
        };
        let total: u64 = runs.iter().map(|r| r.size_bytes).sum();
        let charged = self.space.snapshot().by_kind[SpaceKind::Index.index()];
        match acct {
            Some(acct) if total > charged => {
                let grow = total - charged;
                self.settle(acct, SpaceKind::Index, grow, grow);
            }
            _ if total != charged => {
                if let Err(e) =
                    self.space
                        .transfer(SpaceKind::Index, SpaceKind::Index, charged, total)
                {
                    tracing::warn!(error = %e, "ledger transfer failed");
                }
            }
            _ => {}
        }
    }

    /// Retries a `persist` that failed (N13). While it fails the cut fails
    /// before it writes anything.
    fn retry_persist(&self, co: &mut Coord) -> Result<()> {
        if !co.index_unpersisted {
            return Ok(());
        }
        if let Some(upto) = co.upto_seg {
            if let Err(e) = self.index.persist(upto) {
                tracing::warn!(error = %e, "index run still not persisted: the cut waits for it");
                return Err(e);
            }
        }
        co.index_unpersisted = false;
        self.true_up_index(None);
        tracing::info!("index run persisted on retry");
        Ok(())
    }

    /// C3, second half: compaction's `moved` records become conditional
    /// re-hints (a newer write wins). `Moved` carries no chunk number, so
    /// the chunks are found by walking the tip's leaves for the old hints.
    ///
    /// The re-hints share the map's dirty-leaf cap with this cut's destage:
    /// they go in leaf order, in one transaction of at most
    /// `max_leaves_per_cut` leaves, halved on `MapFull` down to one leaf; the
    /// moves of the leaves left over stay in `moved` for a later cut
    /// (`moves_deferred`). Moves whose chunk was rewritten since are done.
    fn apply_moved(&self, co: &mut Coord) -> Result<()> {
        if co.moved.is_empty() {
            co.moves_deferred = false;
            return Ok(());
        }
        let olds: HashMap<Hint, Hint> = co.moved.iter().map(|m| (m.old, m.new)).collect();
        let mut hits: Vec<(ChunkNo, Hint, Hint)> = Vec::new();
        let root = co.tip.map_root;
        self.map.walk_leaves(&root, &mut |ci, e| {
            let h = entry_hint(e);
            if let Some(&new) = olds.get(&h) {
                hits.push((ci, h, new));
            }
        })?;
        hits.sort_unstable_by_key(|(ci, _, _)| ci.0);
        let fanout = u64::from(self.cfg.map_fanout.max(1));
        let mut leaves: Vec<u64> = hits.iter().map(|(ci, _, _)| ci.0 / fanout).collect();
        leaves.dedup();
        let mut n = leaves
            .len()
            .min(usize::try_from(self.cfg.max_leaves_per_cut.max(1)).unwrap_or(usize::MAX));
        let applied = loop {
            let Some(&last) = leaves.get(n.saturating_sub(1)) else {
                break 0;
            };
            let take = hits.partition_point(|(ci, _, _)| ci.0 / fanout <= last);
            match self.rehint(&hits[..take]) {
                Ok(()) => break take,
                Err(e) if e.kind() == ErrorKind::MapFull && n > 1 => n /= 2,
                Err(e) => {
                    co.moves_deferred = true;
                    return Err(e);
                }
            }
        };
        // Moves of the leaves not applied stay; every other one is done
        // (applied, or its chunk was rewritten: the move is moot).
        let left: HashSet<Hint> = hits[applied..].iter().map(|(_, old, _)| *old).collect();
        let (keep, done): (Vec<Moved>, Vec<Moved>) =
            co.moved.drain(..).partition(|m| left.contains(&m.old));
        co.moved = keep;
        co.moves_deferred = !co.moved.is_empty();
        let mut segs: Vec<SegId> = done.iter().map(|m| m.new.seg).collect();
        segs.sort_unstable();
        segs.dedup();
        for seg in segs {
            if !co.pending_named.contains(&seg) {
                co.pending_named.push(seg);
            }
        }
        if co.moves_deferred {
            tracing::info!(
                left = co.moved.len(),
                "compaction re-hints: the map's dirty-leaf cap left some for a later cut"
            );
        }
        Ok(())
    }

    /// One re-hint transaction; `MapFull` leaves the map untouched.
    fn rehint(&self, hits: &[(ChunkNo, Hint, Hint)]) -> Result<()> {
        if hits.is_empty() {
            return Ok(());
        }
        let mut txn = self.map.begin_update()?;
        for &(ci, old, new) in hits {
            txn.rehint(ci, old, new)?;
        }
        txn.commit().map(|_| ())
    }

    /// After a cut, while a pass has work it can do now (`gc_busy`): more
    /// steps, for up to a third of the time the cut took and at most
    /// [`GC_SHARE_MAX`], so the collector gets about a quarter of the
    /// coordinator under writes that keep it cutting back to back (one step
    /// per cut gave it a few percent, and a 4 GiB working set outran
    /// compaction; milestone gcrate). The next cut waits that long; each step
    /// still ends one unit past its budget, and a clock that does not move
    /// over [`GC_SHARE_STALLS`] steps (a manual one) ends the share.
    fn gc_share(&self, co: &mut Coord, cut_started: Duration) {
        let now = self.clock.monotonic();
        let share = (now.saturating_sub(cut_started) / GC_SHARE_DIVISOR).min(GC_SHARE_MAX);
        let until = now.saturating_add(share);
        let mut last = now;
        let mut stalled = 0u32;
        while stalled < GC_SHARE_STALLS
            && self.gc_busy.load(Ordering::Acquire)
            && !self.is_failed()
            && !self.closing.load(Ordering::Acquire)
            && last < until
        {
            self.gc_step(co);
            let t = self.clock.monotonic();
            stalled = if t == last { stalled + 1 } else { 0 };
            last = t;
        }
    }

    fn gc_step(&self, co: &mut Coord) {
        let Some(budget) = self.gc_budget_now() else {
            return;
        };
        // No collector call starts once a quiesce began (the engine is
        // about to be dropped; moves made now would be lost with it).
        if self.closing.load(Ordering::Acquire) {
            self.gc_busy.store(false, Ordering::Release);
            return;
        }
        let read_before = co.collector.as_ref().map(|c| c.stats().compaction_read);
        match self.with_collector(co, |c, v| c.step(v, budget)) {
            Ok(p) => {
                // Work left that a step can do now: the background thread
                // ticks again soon (`GC_BUSY_PERIOD`). Not while the pass
                // waits out a grace, nor while compaction sits at its rate
                // cap (it read nothing, moved nothing, freed nothing): the
                // same rule as `run_pass`'s pause.
                let read_now = co.collector.as_ref().map(|c| c.stats().compaction_read);
                let capped = p.state == GcState::Compacting
                    && read_now == read_before
                    && p.moved.is_empty()
                    && p.freed == 0;
                let busy = !p.done && !capped && !matches!(p.state, GcState::Idle | GcState::Grace);
                self.gc_busy.store(busy, Ordering::Release);
                co.absorb(p);
                co.gc_error = None;
            }
            // No collector, or the map ahead of the tip (the hook's
            // precondition, M17): nothing to step now.
            Err(e) if matches!(e.kind(), ErrorKind::Unsupported | ErrorKind::Busy) => {
                self.gc_busy.store(false, Ordering::Release);
            }
            // Moves sealed before the failure were taken by `with_collector`
            // (M17.B stack review M-2). Once per distinct error, not at every
            // tick (M-3).
            Err(e) => {
                self.gc_busy.store(false, Ordering::Release);
                let msg = e.to_string();
                if co.gc_error.as_deref() != Some(msg.as_str()) {
                    tracing::warn!(error = %msg, "gc step failed; the pass resumes later");
                    co.gc_error = Some(msg);
                }
            }
        }
    }
}
