//! The collector: the pass state machine behind [`Collector`].
//!
//! A pass is `Marking → Condemning → Grace → Rechecking → Deleting →
//! Compacting`, each phase made of small units; [`Collector::step`] runs units
//! until its budget is spent, so a pass never stalls a cut for longer than a
//! unit past the budget. Grace, recheck and delete are skipped when nothing is
//! condemned; compaction is skipped on a lean volume and when nothing
//! qualifies.
//!
//! # Safety argument (plan M17 rules 5, 6, 7 and 12)
//!
//! * Only objects listed when a mark started are candidates, and only when
//!   no root of that mark reaches them, their id is at or below the tip's
//!   high-water mark, no **currently** retained commit names them and they
//!   are not an unnamed compaction output of this process.
//! * A condemned segment is flagged in the index before its grace starts, so
//!   `get_for_dedup` never returns it; at every pass start (and load) each
//!   segment on the list is re-flagged and, if the flag was missing, its
//!   grace restarts.
//! * The recheck is a new mark whose root set is pinned after every
//!   condemned object was flagged and its grace elapsed; an object it finds
//!   referenced (or named, or above the mark) is un-condemned.
//! * A segment that stale hints point into is deleted only while the live
//!   copy of every id they resolve to is in a segment a durable FULL named
//!   (at or below the highest id the FULLs read since start name, and not an
//!   unnamed output): the delete-time index invariant; otherwise the delete
//!   is deferred.
//! * Commit objects are deleted only below the durable tip's `chain_floor`,
//!   below the rollback floor's `max_commit_seq` while a floor exists (an
//!   unreadable floor defers every commit delete), and never when pinned.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use lunix_format::commit::Commit;
use lunix_format::local::RollbackFloor;
use lunix_format::{CommitSeq, ObjId, SegId};
use lunix_iface::gc::{
    Collector, GcBudget, GcPassSummary, GcProgress, GcState, GcStats, GcStatusSnapshot, GcView,
    Moved,
};
use lunix_iface::space::SpaceKind;
use lunix_iface::{Error, ErrorKind, Result};

use crate::compact::{self, Compactor};
use crate::condemned::{CondemnedEntry, CondemnedList, GraceStarts, ObjectRef};
use crate::config::{self, FloorSource, GcConfig, GcHooks, NoHooks};
use crate::evidence::{Action, Evidence};
use crate::mark::{Liveness, Mark};
use crate::points::{point, Points};

/// What a recheck decided for one condemned object.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Verdict {
    /// Still unreferenced: delete.
    Delete,
    /// Referenced again (or named, or above the mark): un-condemn.
    Uncondemn(&'static str),
    /// Already gone: drop from the list.
    Gone,
    /// Keep condemned, decide in a later pass.
    Defer(&'static str),
}

#[derive(Debug)]
enum Phase {
    Idle,
    Marking(Box<Mark>),
    Condemning {
        todo: Vec<(ObjectRef, u64)>,
        listed: bool,
        next: usize,
    },
    Grace {
        until: Duration,
    },
    Rechecking(Box<Mark>),
    Deleting {
        queue: VecDeque<(ObjectRef, Verdict)>,
    },
    Compacting(Option<Box<Compactor>>),
    Finishing,
}

#[derive(Debug, Default)]
struct PassCtx {
    summary: GcPassSummary,
    evidence: Evidence,
    liveness: Option<Liveness>,
}

/// What one unit asks of the step loop.
enum Flow {
    Continue,
    Yield,
    PassDone,
}

/// Objects any currently retained FULL names (rule 12 (ii)).
fn named_by_retained(view: &GcView<'_>) -> BTreeSet<u64> {
    let mut s = BTreeSet::new();
    for c in view.retained {
        if let Commit::Full(f) = c {
            s.extend(
                f.new_segments
                    .iter()
                    .chain(&f.map_packs)
                    .chain(&f.index_packs)
                    .chain(f.map_root.pack.iter())
                    .map(|id| id.raw()),
            );
        }
    }
    s
}

fn retained_seqs(view: &GcView<'_>) -> BTreeSet<CommitSeq> {
    view.retained
        .iter()
        .map(|c| c.common().commit_seq)
        .collect()
}

fn object_bytes(l: &Liveness, obj: ObjectRef) -> Option<u64> {
    match obj {
        ObjectRef::Segment(s) => l.segments.iter().find(|x| x.seg == s).map(|x| x.size_bytes),
        ObjectRef::Pack(p) => l.packs.iter().find(|x| x.pack == p).map(|x| x.size_bytes),
        ObjectRef::Run(r) => l.runs.iter().find(|x| x.id == r).map(|x| x.size_bytes),
        ObjectRef::Commit(c) => l.commits.iter().find(|x| x.seq == c).map(|x| x.size_bytes),
    }
}

/// Inputs shared by condemn, recheck and delete decisions.
struct Judge<'a> {
    l: &'a Liveness,
    named: BTreeSet<u64>,
    manifest: BTreeSet<ObjId>,
    retained: BTreeSet<CommitSeq>,
    hwm: u64,
    /// The highest id a durable FULL is known to name (see `Gc::named_high`).
    named_high: u64,
    chain_floor: CommitSeq,
    tip: CommitSeq,
    /// `None` when `state/pins` could not be read.
    floor: Option<Option<RollbackFloor>>,
    outputs: &'a BTreeSet<SegId>,
}

impl<'a> Judge<'a> {
    fn new(
        view: &GcView<'_>,
        l: &'a Liveness,
        floor_src: &dyn FloorSource,
        outputs: &'a BTreeSet<SegId>,
        named_high: u64,
    ) -> Result<Self> {
        let floor = match floor_src.current_floor() {
            Ok(f) => Some(f),
            Err(e) => {
                tracing::warn!(error = %e, "gc: rollback floor unreadable; no commit object is deleted in this pass");
                None
            }
        };
        Ok(Self {
            l,
            named: named_by_retained(view),
            manifest: view.index.runs()?.iter().map(|r| r.id).collect(),
            retained: retained_seqs(view),
            hwm: view.tip.high_water_mark,
            named_high: named_high.max(view.tip.high_water_mark),
            chain_floor: view.tip.chain_floor,
            tip: view.tip.commit_seq,
            floor,
            outputs,
        })
    }

    /// Why a commit object must stay, if it must.
    fn commit_kept(&self, seq: CommitSeq) -> Option<&'static str> {
        if seq >= self.chain_floor || seq >= self.tip || self.retained.contains(&seq) {
            return Some("at or above chain_floor");
        }
        if self.l.pinned.contains(&seq) {
            return Some("pinned");
        }
        match self.floor {
            None => Some("rollback floor unreadable"),
            Some(Some(f)) if seq >= f.max_commit_seq => Some("at or above the rollback floor"),
            Some(_) => None,
        }
    }

    /// Why a container must stay regardless of liveness, if it must.
    fn container_kept(&self, raw: u64) -> Option<&'static str> {
        if raw > self.hwm {
            Some("above the high-water mark")
        } else if self.named.contains(&raw) {
            Some("named by a retained commit")
        } else {
            None
        }
    }

    /// The dead objects of the mark that are not on `list` yet.
    fn dead(&self, list: &CondemnedList) -> Vec<(ObjectRef, u64)> {
        let l = self.l;
        let mut out = Vec::new();
        for s in &l.segments {
            if l.live_bytes(s.seg) == 0
                && self.container_kept(s.seg.raw()).is_none()
                && !self.outputs.contains(&s.seg)
            {
                out.push((ObjectRef::Segment(s.seg), s.size_bytes));
            }
        }
        for p in &l.packs {
            if !l.live_packs.contains(&p.pack) && self.container_kept(p.pack.raw()).is_none() {
                out.push((ObjectRef::Pack(p.pack), p.size_bytes));
            }
        }
        for r in &l.runs {
            if !self.manifest.contains(&r.id) && self.container_kept(r.id.raw()).is_none() {
                out.push((ObjectRef::Run(r.id), r.size_bytes));
            }
        }
        for c in &l.commits {
            if self.commit_kept(c.seq).is_none() {
                out.push((ObjectRef::Commit(c.seq), c.size_bytes));
            }
        }
        out.retain(|(o, _)| !list.entries.contains_key(o));
        out
    }

    /// The recheck of one condemned object against this (newer) mark.
    fn verdict(&self, obj: ObjectRef) -> Verdict {
        let l = self.l;
        let Some(_) = object_bytes(l, obj) else {
            return Verdict::Gone;
        };
        match obj {
            ObjectRef::Segment(s) => {
                if l.live_bytes(s) > 0 {
                    Verdict::Uncondemn("referenced at recheck")
                } else if let Some(why) = self.container_kept(s.raw()) {
                    Verdict::Uncondemn(why)
                } else if self.outputs.contains(&s) {
                    Verdict::Uncondemn("unnamed compaction output")
                } else {
                    Verdict::Delete
                }
            }
            ObjectRef::Pack(p) => {
                if l.live_packs.contains(&p) {
                    Verdict::Uncondemn("referenced at recheck")
                } else if let Some(why) = self.container_kept(p.raw()) {
                    Verdict::Uncondemn(why)
                } else {
                    Verdict::Delete
                }
            }
            ObjectRef::Run(r) => {
                if self.manifest.contains(&r) {
                    Verdict::Uncondemn("named by the index manifest")
                } else if let Some(why) = self.container_kept(r.raw()) {
                    Verdict::Uncondemn(why)
                } else {
                    Verdict::Delete
                }
            }
            ObjectRef::Commit(c) => match self.commit_kept(c) {
                Some("rollback floor unreadable") => Verdict::Defer("rollback floor unreadable"),
                Some(why) => Verdict::Uncondemn(why),
                None => Verdict::Delete,
            },
        }
    }

    /// The delete-time index invariant for a segment: every copy that its
    /// stale hints rely on must be durable, that is named by a durable FULL:
    /// at or below a high-water mark this process has seen (a FULL named
    /// everything up to it, D3, except compaction outputs whose moves are
    /// still pending, which are excluded by name).
    fn segment_deletable(&self, s: SegId) -> Option<&'static str> {
        match self.l.deps.get(&s) {
            Some(dep) if dep.raw() > self.named_high || self.outputs.contains(dep) => {
                Some("live copies of its ids are not durable yet")
            }
            _ => None,
        }
    }
}

/// The phase-1 collector (`lunix-gc`).
pub struct Gc {
    cfg: GcConfig,
    floor: Arc<dyn FloorSource>,
    points: Points,
    loaded: bool,
    list: CondemnedList,
    grace: GraceStarts,
    outputs: BTreeSet<SegId>,
    /// The highest id named by any FULL read by `observe_named` (and the
    /// tip's mark). Every segment at or below it was named by a durable FULL,
    /// except compaction outputs still in `outputs` and segments no index
    /// entry points into (orphans): ids are allocated in order and a cut
    /// names everything it wrote, so only GC's own outputs can be passed by.
    named_high: u64,
    /// The newest commit `observe_named` has read.
    scanned: Option<CommitSeq>,
    outbox: Vec<Moved>,
    phase: Phase,
    ctx: Option<PassCtx>,
    last_end: Option<Duration>,
    last_tip: Option<CommitSeq>,
    last_pins: Option<BTreeSet<CommitSeq>>,
    stats: GcStats,
    last_pass: Option<GcPassSummary>,
    totals: GcPassSummary,
    last_finished_ms: Option<u64>,
    peak_heap: usize,
    unit_count: u64,
    last_compacted: Vec<compact::Candidate>,
}

impl std::fmt::Debug for Gc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Gc")
            .field("phase", &self.state())
            .field("condemned", &self.list.entries.len())
            .field("outputs", &self.outputs)
            .field("stats", &self.stats)
            .finish_non_exhaustive()
    }
}

impl Gc {
    /// A collector. `floor` is where the rollback floor is read (the volume
    /// directory in production).
    pub fn new(floor: Arc<dyn FloorSource>, cfg: GcConfig) -> Self {
        Self::with_hooks(floor, cfg, Arc::new(NoHooks))
    }

    /// A collector with test hooks.
    pub fn with_hooks(floor: Arc<dyn FloorSource>, cfg: GcConfig, hooks: Arc<dyn GcHooks>) -> Self {
        Self {
            cfg,
            floor,
            points: Points::new(hooks),
            loaded: false,
            list: CondemnedList::default(),
            grace: GraceStarts::new(),
            outputs: BTreeSet::new(),
            named_high: 0,
            scanned: None,
            outbox: Vec::new(),
            phase: Phase::Idle,
            ctx: None,
            last_end: None,
            last_tip: None,
            last_pins: None,
            stats: GcStats::default(),
            last_pass: None,
            totals: GcPassSummary::default(),
            last_finished_ms: None,
            peak_heap: 0,
            unit_count: 0,
            last_compacted: Vec::new(),
        }
    }

    /// The state for `GcStatus` (M12's `GcControl::status`).
    pub fn status(&self) -> GcStatusSnapshot {
        GcStatusSnapshot {
            state: match self.state() {
                GcState::Idle => None,
                s => Some(s),
            },
            condemned_pending: self.list.entries.len() as u64,
            moved_pending: self.outputs.len() as u64,
            last_pass: self.last_pass.clone(),
            stats: self.stats,
            last_finished_ms: self.last_finished_ms,
        }
    }

    /// The sums of every pass summary of this process (`pass` is the count
    /// of passes that wrote an evidence object).
    pub fn totals(&self) -> GcPassSummary {
        self.totals.clone()
    }

    /// The condemned objects, as the list holds them.
    pub fn condemned(&self) -> Vec<ObjectRef> {
        self.list.entries.keys().copied().collect()
    }

    /// Compaction outputs this process sealed that no retained commit names
    /// yet (never condemned).
    pub fn unnamed_outputs(&self) -> Vec<SegId> {
        self.outputs.iter().copied().collect()
    }

    /// The largest heap the collector held beyond the live-extent set,
    /// estimated from its own structures (the plan's 64 MiB gate).
    pub fn peak_heap_beyond_live(&self) -> usize {
        self.peak_heap
    }

    /// The segments the last compaction read from, with their liveness then.
    pub fn last_compacted(&self) -> &[compact::Candidate] {
        &self.last_compacted
    }

    /// Units of work done so far (tests).
    pub fn units(&self) -> u64 {
        self.unit_count
    }

    /// Where the pass is.
    pub fn state(&self) -> GcState {
        match &self.phase {
            Phase::Idle => GcState::Idle,
            Phase::Marking(_) => GcState::Marking,
            Phase::Condemning { .. } => GcState::Condemning,
            Phase::Grace { .. } => GcState::Grace,
            Phase::Rechecking(_) => GcState::Rechecking,
            Phase::Deleting { .. } => GcState::Deleting,
            Phase::Compacting(_) | Phase::Finishing => GcState::Compacting,
        }
    }

    fn precheck(&self, view: &GcView<'_>) -> Result<()> {
        self.points.alive()?;
        config::validate(&view.params)?;
        // The hook's preconditions (M11 guarantees them): between cuts, the
        // in-memory map equals the tip's root.
        if view.map.root() != view.tip.map_root || view.map.dirty_leaf_count() != 0 {
            return Err(Error::invalid(
                "gc step outside its preconditions: the map is not the tip's",
            ));
        }
        let tip_retained = view.retained.iter().any(|c| match c {
            Commit::Full(f) => {
                f.common.commit_seq == view.tip.commit_seq && f.map_root == view.tip.map_root
            }
            _ => false,
        });
        if !tip_retained {
            return Err(Error::invalid("the view's retained commits miss the tip"));
        }
        Ok(())
    }

    /// Loads the condemned list once (read-only).
    fn load(&mut self, view: &GcView<'_>) -> Result<()> {
        if self.loaded {
            return Ok(());
        }
        let now = view.clock.monotonic();
        self.list = view
            .state
            .load_condemned()?
            .as_ref()
            .and_then(CondemnedList::from_file)
            .unwrap_or_default();
        self.grace = self.list.entries.keys().map(|o| (*o, now)).collect();
        self.loaded = true;
        Ok(())
    }

    /// Makes the index flags and the list agree: a listed segment without a
    /// flag is flagged and its grace restarts; a flagged segment that is not
    /// listed is adopted (fresh grace) when it exists, else unflagged.
    fn reconcile(&mut self, view: &GcView<'_>) -> Result<()> {
        let now = view.clock.monotonic();
        let wall = view.clock.now();
        let flagged: BTreeSet<SegId> = view.index.condemned()?.into_iter().collect();
        let existing: BTreeMap<SegId, u64> = view
            .segments
            .list()?
            .into_iter()
            .map(|s| (s.seg, s.size_bytes))
            .collect();
        let mut changed = false;
        for obj in self.list.entries.keys().copied().collect::<Vec<_>>() {
            if let ObjectRef::Segment(s) = obj {
                if !flagged.contains(&s) && existing.contains_key(&s) {
                    view.index.mark_condemned(s)?;
                    self.grace.insert(obj, now);
                }
            }
            // An entry whose condemn was interrupted before its grace began.
            let g = self.grace.entry(obj).or_insert(now);
            if *g == Duration::MAX {
                *g = now;
            }
        }
        for s in flagged {
            let obj = ObjectRef::Segment(s);
            if self.list.entries.contains_key(&obj) {
                continue;
            }
            match existing.get(&s) {
                Some(&bytes) => {
                    self.list.entries.insert(
                        obj,
                        CondemnedEntry {
                            condemned_at: wall,
                            bytes,
                        },
                    );
                    self.grace.insert(obj, now);
                    changed = true;
                }
                None => view.index.unmark_condemned(s)?,
            }
        }
        if changed {
            self.store_list(view)?;
        }
        Ok(())
    }

    fn store_list(&self, view: &GcView<'_>) -> Result<()> {
        view.state.store_condemned(&self.list.to_file()?)
    }

    /// Reads the FULLs committed since the last call (up to the tip): every
    /// id they name raises `named_high`, and a compaction output they name
    /// is no longer pending. Commit objects are durable when they exist, and
    /// GC deletes none it has not seen here first (a commit is condemned only
    /// in a pass, after this ran), so no FULL is missed.
    fn observe_named(&mut self, view: &GcView<'_>) -> Result<()> {
        self.named_high = self.named_high.max(view.tip.high_water_mark);
        let named = named_by_retained(view);
        self.outputs.retain(|o| !named.contains(&o.raw()));
        let tip = view.tip.commit_seq;
        if self.scanned.is_some_and(|s| s >= tip) {
            return Ok(());
        }
        for c in view.commits.list()? {
            if c.seq > tip || self.scanned.is_some_and(|s| c.seq <= s) {
                continue;
            }
            let bytes = match view.commits.get(c.seq) {
                Ok(b) => b,
                Err(e) if e.kind() == ErrorKind::NotFound => continue,
                Err(e) => return Err(e),
            };
            if let Commit::Full(f) = view.commit_codec.open_commit(c.seq, &bytes)? {
                for id in f
                    .new_segments
                    .iter()
                    .chain(&f.map_packs)
                    .chain(&f.index_packs)
                    .chain(f.map_root.pack.iter())
                {
                    self.named_high = self.named_high.max(id.raw());
                }
                for s in &f.new_segments {
                    self.outputs.remove(s);
                }
            }
        }
        self.scanned = Some(tip);
        Ok(())
    }

    fn due(&mut self, view: &GcView<'_>) -> Result<bool> {
        let now = view.clock.monotonic();
        if self
            .last_end
            .is_some_and(|end| now < end.saturating_add(self.cfg.pass_interval))
        {
            return Ok(false);
        }
        if self.last_tip != Some(view.tip.commit_seq) || !self.list.entries.is_empty() {
            return Ok(true);
        }
        let pins: BTreeSet<CommitSeq> = view.pins.roots()?.iter().map(|p| p.commit_seq).collect();
        Ok(self.last_pins.as_ref() != Some(&pins))
    }

    /// Drops the pass in progress (an error, or an urgent pass). The list,
    /// the index flags and the unnamed outputs stay.
    fn abort_pass(&mut self, view: &GcView<'_>) {
        if let Phase::Compacting(Some(c)) = &mut self.phase {
            c.abandon(view);
        }
        self.phase = Phase::Idle;
        self.ctx = None;
        self.last_end = Some(view.clock.monotonic());
    }

    fn note_heap(&mut self) {
        let mut h = self.list.entries.len() * 64 + self.grace.len() * 48 + self.outputs.len() * 32;
        h += match &self.phase {
            Phase::Marking(m) | Phase::Rechecking(m) => m.heap_bytes(),
            Phase::Condemning { todo, .. } => todo.capacity() * 24,
            Phase::Deleting { queue } => queue.capacity() * 32,
            Phase::Compacting(Some(c)) => c.heap_bytes(),
            _ => 0,
        };
        if let Some(ctx) = &self.ctx {
            h += ctx.evidence.heap_bytes();
            if let Some(l) = &ctx.liveness {
                h += l.heap_bytes();
            }
        }
        // The live-extent set is accounted for separately (the gate bounds
        // what comes on top of it); at most one is held at a time.
        let live = match (&self.phase, &self.ctx) {
            (Phase::Marking(m) | Phase::Rechecking(m), _) => m.live_set_bytes(),
            (
                _,
                Some(PassCtx {
                    liveness: Some(l), ..
                }),
            ) => l.live.capacity() * std::mem::size_of::<crate::mark::LiveExtent>(),
            _ => 0,
        };
        if let (
            Phase::Marking(_) | Phase::Rechecking(_),
            Some(PassCtx {
                liveness: Some(l), ..
            }),
        ) = (&self.phase, &self.ctx)
        {
            h += l.live.capacity() * std::mem::size_of::<crate::mark::LiveExtent>();
        }
        self.peak_heap = self.peak_heap.max(h.saturating_sub(live));
    }

    fn evidence(
        &mut self,
        view: &GcView<'_>,
        action: Action,
        obj: ObjectRef,
        bytes: u64,
        why: &'static str,
    ) {
        if let Some(ctx) = &mut self.ctx {
            ctx.evidence.add(view.clock.now(), action, obj, bytes, why);
        }
    }

    // ------------------------------------------------------------ units

    fn unit(
        &mut self,
        view: &GcView<'_>,
        budget: &GcBudget,
        freed: &mut u64,
        step_bytes: &mut u64,
    ) -> Result<Flow> {
        self.unit_count += 1;
        let flow = match std::mem::replace(&mut self.phase, Phase::Idle) {
            Phase::Idle => {
                if !self.due(view)? {
                    return Ok(Flow::Yield);
                }
                self.reconcile(view)?;
                self.ctx = Some(PassCtx::default());
                self.last_compacted.clear();
                self.last_tip = Some(view.tip.commit_seq);
                self.phase = Phase::Marking(Box::new(Mark::new(self.cfg)));
                Flow::Continue
            }
            Phase::Marking(mut m) => {
                if m.unit(view)? {
                    point!(self.points, "gc.mark.end");
                    let l = m.into_liveness();
                    self.after_mark(view, &l);
                    let todo = Judge::new(view, &l, &*self.floor, &self.outputs, self.named_high)?
                        .dead(&self.list);
                    if let Some(ctx) = &mut self.ctx {
                        ctx.liveness = Some(l);
                    }
                    self.phase = if todo.is_empty() && self.list.entries.is_empty() {
                        Phase::Compacting(None)
                    } else {
                        Phase::Condemning {
                            todo,
                            listed: false,
                            next: 0,
                        }
                    };
                } else {
                    self.phase = Phase::Marking(m);
                }
                Flow::Continue
            }
            Phase::Condemning { todo, listed, next } => {
                self.condemn_unit(view, todo, listed, next)?;
                Flow::Continue
            }
            Phase::Grace { until } => {
                if view.clock.monotonic() >= until {
                    self.phase = Phase::Rechecking(Box::new(Mark::new(self.cfg)));
                    Flow::Continue
                } else {
                    self.phase = Phase::Grace { until };
                    Flow::Yield
                }
            }
            Phase::Rechecking(mut m) => {
                if m.unit(view)? {
                    let l = m.into_liveness();
                    self.after_mark(view, &l);
                    let queue = self.recheck(view, &l)?;
                    if let Some(ctx) = &mut self.ctx {
                        ctx.liveness = Some(l);
                    }
                    self.phase = Phase::Deleting { queue };
                } else {
                    self.phase = Phase::Rechecking(m);
                }
                Flow::Continue
            }
            Phase::Deleting { mut queue } => {
                match queue.pop_front() {
                    Some((obj, v)) => {
                        *freed += self.apply_verdict(view, obj, v)?;
                        self.phase = Phase::Deleting { queue };
                    }
                    None => {
                        self.store_list(view)?;
                        self.phase = Phase::Compacting(None);
                    }
                }
                Flow::Continue
            }
            Phase::Compacting(c) => self.compact_unit(view, budget, c, step_bytes)?,
            Phase::Finishing => {
                self.finish_pass(view)?;
                Flow::PassDone
            }
        };
        self.note_heap();
        Ok(flow)
    }

    fn after_mark(&mut self, view: &GcView<'_>, l: &Liveness) {
        self.stats.live_bytes = l.total_live();
        self.stats.dead_bytes = l
            .segments
            .iter()
            .map(|s| s.size_bytes.saturating_sub(l.live_bytes(s.seg)))
            .sum();
        self.last_pins = Some(l.pinned.clone());
        let _ = view;
    }

    fn condemn_unit(
        &mut self,
        view: &GcView<'_>,
        todo: Vec<(ObjectRef, u64)>,
        listed: bool,
        next: usize,
    ) -> Result<()> {
        if !listed {
            let wall = view.clock.now();
            for &(obj, bytes) in &todo {
                self.list.entries.insert(
                    obj,
                    CondemnedEntry {
                        condemned_at: wall,
                        bytes,
                    },
                );
                // Not yet flagged in the index: the grace starts below.
                self.grace.insert(obj, Duration::MAX);
            }
            if !todo.is_empty() {
                self.store_list(view)?;
                point!(self.points, "gc.condemn.after_list_write");
            }
            self.phase = Phase::Condemning {
                todo,
                listed: true,
                next: 0,
            };
            return Ok(());
        }
        if let Some(&(obj, _)) = todo.get(next) {
            if let ObjectRef::Segment(s) = obj {
                view.index.mark_condemned(s)?;
            }
            self.phase = Phase::Condemning {
                todo,
                listed: true,
                next: next + 1,
            };
            return Ok(());
        }
        // Everything is flagged: the graces start now.
        let now = view.clock.monotonic();
        for &(obj, bytes) in &todo {
            self.grace.insert(obj, now);
            self.evidence(view, Action::Condemn, obj, bytes, "unreferenced");
            if let Some(ctx) = &mut self.ctx {
                if matches!(obj, ObjectRef::Segment(_)) {
                    ctx.summary.segments_condemned += 1;
                }
            }
        }
        // The condemn mark is not needed again: the recheck makes its own.
        if let Some(ctx) = &mut self.ctx {
            ctx.liveness = None;
        }
        let latest = self
            .grace
            .values()
            .copied()
            .filter(|&t| t != Duration::MAX)
            .max()
            .unwrap_or(now);
        self.phase = Phase::Grace {
            until: latest.saturating_add(view.params.grace),
        };
        Ok(())
    }

    /// Decides every listed object against the recheck mark `l`.
    fn recheck(
        &mut self,
        view: &GcView<'_>,
        l: &Liveness,
    ) -> Result<VecDeque<(ObjectRef, Verdict)>> {
        let judge = Judge::new(view, l, &*self.floor, &self.outputs, self.named_high)?;
        let mut queue = VecDeque::new();
        for &obj in self.list.entries.keys() {
            // Only objects whose grace ended before this mark started.
            let elapsed = self.grace.get(&obj).is_some_and(|&t| {
                t != Duration::MAX && t.saturating_add(view.params.grace) <= l.started
            });
            let v = if elapsed {
                judge.verdict(obj)
            } else {
                Verdict::Defer("grace not over")
            };
            queue.push_back((obj, v));
        }
        Ok(queue)
    }

    /// Applies one verdict; returns the bytes freed.
    fn apply_verdict(&mut self, view: &GcView<'_>, obj: ObjectRef, v: Verdict) -> Result<u64> {
        match v {
            Verdict::Gone => {
                self.forget(view, obj)?;
                Ok(0)
            }
            Verdict::Defer(why) => {
                self.evidence(view, Action::Defer, obj, 0, why);
                Ok(0)
            }
            Verdict::Uncondemn(why) => {
                if let ObjectRef::Segment(s) = obj {
                    view.index.unmark_condemned(s)?;
                }
                self.list.entries.remove(&obj);
                self.grace.remove(&obj);
                self.evidence(view, Action::Uncondemn, obj, 0, why);
                Ok(0)
            }
            Verdict::Delete => {
                // Re-judged against the view of this step (cuts may have
                // happened since the recheck).
                let l = self
                    .ctx
                    .as_ref()
                    .and_then(|c| c.liveness.as_ref())
                    .ok_or(Error::invalid("delete without a recheck"))?;
                let judge = Judge::new(view, l, &*self.floor, &self.outputs, self.named_high)?;
                let again = judge.verdict(obj);
                let blocked = match obj {
                    ObjectRef::Segment(s) => judge.segment_deletable(s),
                    _ => None,
                };
                drop(judge);
                match (again, blocked) {
                    (Verdict::Delete, Some(why)) => {
                        self.apply_verdict(view, obj, Verdict::Defer(why))
                    }
                    (Verdict::Delete, None) => self.delete(view, obj),
                    (other, _) => self.apply_verdict(view, obj, other),
                }
            }
        }
    }

    /// Removes a listed object that no longer exists.
    fn forget(&mut self, view: &GcView<'_>, obj: ObjectRef) -> Result<()> {
        if let ObjectRef::Segment(s) = obj {
            view.index.unmark_condemned(s)?;
        }
        self.list.entries.remove(&obj);
        self.grace.remove(&obj);
        Ok(())
    }

    /// Deletes one object (unlink, directory fsync inside the store), then
    /// the index follow-up and the ledger release.
    fn delete(&mut self, view: &GcView<'_>, obj: ObjectRef) -> Result<u64> {
        let freed = match obj {
            ObjectRef::Segment(s) => {
                point!(self.points, "gc.delete.before_unlink");
                let n = view.segments.delete(s)?;
                point!(self.points, "gc.delete.after_unlink");
                view.index.drop_segment(s)?;
                view.index.unmark_condemned(s)?;
                if n > 0 {
                    view.space.release(SpaceKind::Segments, n);
                }
                point!(self.points, "gc.delete.after_dirsync");
                n
            }
            ObjectRef::Pack(p) => {
                point!(self.points, "gc.delete.before_unlink");
                let n = view.packs.delete_pack(p)?;
                point!(self.points, "gc.delete.after_unlink");
                if n > 0 {
                    view.space.release(SpaceKind::Map, n);
                }
                point!(self.points, "gc.delete.after_dirsync");
                n
            }
            ObjectRef::Run(r) => {
                let run = self
                    .ctx
                    .as_ref()
                    .and_then(|c| c.liveness.as_ref())
                    .and_then(|l| l.runs.iter().find(|x| x.id == r).copied());
                let Some(run) = run else {
                    self.forget(view, obj)?;
                    return Ok(0);
                };
                point!(self.points, "gc.delete.before_unlink");
                let n = view.index.delete_run(run)?;
                point!(self.points, "gc.delete.after_unlink");
                if n > 0 {
                    view.space.release(SpaceKind::Index, n);
                }
                point!(self.points, "gc.delete.after_dirsync");
                n
            }
            ObjectRef::Commit(c) => {
                point!(self.points, "gc.commit_delete.before_unlink");
                match view.commits.delete(c, view.tip.chain_floor) {
                    Ok(n) => {
                        if n > 0 {
                            view.space.release(SpaceKind::Commits, n);
                        }
                        n
                    }
                    Err(e) if e.kind() == ErrorKind::Refused => {
                        self.evidence(view, Action::Defer, obj, 0, "refused by the commit store");
                        return Ok(0);
                    }
                    Err(e) => return Err(e),
                }
            }
        };
        self.list.entries.remove(&obj);
        self.grace.remove(&obj);
        self.evidence(
            view,
            Action::Delete,
            obj,
            freed,
            "unreferenced after grace and recheck",
        );
        self.stats.bytes_freed += freed;
        if let Some(ctx) = &mut self.ctx {
            let s = &mut ctx.summary;
            s.bytes_freed += freed;
            match obj {
                ObjectRef::Segment(_) => s.segments_deleted += 1,
                ObjectRef::Pack(_) => s.packs_deleted += 1,
                ObjectRef::Run(_) => s.runs_deleted += 1,
                ObjectRef::Commit(_) => s.commits_deleted += 1,
            }
        }
        Ok(freed)
    }

    fn compaction_sources(
        &self,
        view: &GcView<'_>,
        l: &Liveness,
        budget: u64,
    ) -> Vec<compact::Candidate> {
        if config::compaction_refused(&view.params) {
            return Vec::new();
        }
        let young = compact::tip_segments(view);
        let exclude = |s: SegId| {
            young.contains(&s)
                || self.outputs.contains(&s)
                || self.list.entries.contains_key(&ObjectRef::Segment(s))
        };
        let cands = compact::candidates(
            l,
            view.params.live_threshold,
            view.tip.high_water_mark,
            &exclude,
        );
        compact::pick(&cands, budget)
    }

    fn compact_unit(
        &mut self,
        view: &GcView<'_>,
        budget: &GcBudget,
        c: Option<Box<Compactor>>,
        step_bytes: &mut u64,
    ) -> Result<Flow> {
        let mut c = match c {
            Some(c) => c,
            None => {
                let sources = match self.ctx.as_ref().and_then(|x| x.liveness.as_ref()) {
                    Some(l) => {
                        let s = self.compaction_sources(view, l, self.cfg.compact_per_pass);
                        (!s.is_empty()).then(|| Compactor::new(l, s, view.clock.monotonic()))
                    }
                    None => None,
                };
                match sources {
                    Some(c) => Box::new(c),
                    None => {
                        self.phase = Phase::Finishing;
                        return Ok(Flow::Continue);
                    }
                }
            }
        };
        if c.is_done() {
            self.compaction_done(view, &c);
            self.phase = Phase::Finishing;
            return Ok(Flow::Continue);
        }
        // Rate cap: bytes read so far may not run ahead of `gc_rate` by more
        // than one step's worth; a step moves at most `max_bytes`.
        let elapsed = view.clock.monotonic().saturating_sub(c.started);
        let allowed =
            (view.params.rate_bytes_per_s as f64 * elapsed.as_secs_f64()) as u64 + budget.max_bytes;
        if c.bytes_read >= allowed || *step_bytes >= budget.max_bytes {
            self.phase = Phase::Compacting(Some(c));
            return Ok(Flow::Yield);
        }
        let before = c.bytes_read;
        let res = c.unit(view, &self.points, &mut self.outbox, &mut self.outputs);
        *step_bytes += c.bytes_read - before;
        match res {
            Ok(Some((seg, bytes))) => {
                self.evidence(
                    view,
                    Action::Output,
                    ObjectRef::Segment(seg),
                    bytes,
                    "compaction output",
                );
            }
            Ok(None) => {}
            Err(e) => {
                c.abandon(view);
                return Err(e);
            }
        }
        self.phase = Phase::Compacting(Some(c));
        Ok(Flow::Continue)
    }

    fn compaction_done(&mut self, view: &GcView<'_>, c: &Compactor) {
        for src in &c.sources {
            self.evidence(
                view,
                Action::Compact,
                ObjectRef::Segment(src.seg),
                src.live,
                "live fraction below the threshold",
            );
        }
        self.stats.segments_compacted += c.sources.len() as u64;
        self.last_compacted.clone_from(&c.sources);
        self.stats.compaction_read += c.bytes_read;
        self.stats.compaction_written += c.bytes_written;
        if let Some(ctx) = &mut self.ctx {
            ctx.summary.segments_compacted += c.sources.len() as u64;
            ctx.summary.bytes_moved += c.sources.iter().map(|x| x.live).sum::<u64>();
        }
    }

    /// Writes the evidence object (when the pass did anything) and closes the
    /// pass.
    fn finish_pass(&mut self, view: &GcView<'_>) -> Result<()> {
        let mut ctx = self.ctx.take().unwrap_or_default();
        if !ctx.evidence.is_empty() {
            let pass = view.state.evidence_passes()?.last().copied().unwrap_or(0) + 1;
            view.state
                .put_evidence(pass, &ctx.evidence.finish(view.clock.now()))?;
            point!(self.points, "gc.evidence.after_put");
            ctx.summary.pass = pass;
        }
        self.stats.passes += 1;
        let (t, p) = (&mut self.totals, &ctx.summary);
        t.pass += u64::from(p.pass != 0);
        t.segments_condemned += p.segments_condemned;
        t.segments_deleted += p.segments_deleted;
        t.packs_deleted += p.packs_deleted;
        t.runs_deleted += p.runs_deleted;
        t.commits_deleted += p.commits_deleted;
        t.segments_compacted += p.segments_compacted;
        t.bytes_freed += p.bytes_freed;
        t.bytes_moved += p.bytes_moved;
        self.last_pass = Some(ctx.summary);
        self.last_finished_ms = Some(view.clock.now().0);
        self.last_end = Some(view.clock.monotonic());
        self.phase = Phase::Idle;
        Ok(())
    }

    fn take_progress(&mut self, freed: u64, done: bool) -> GcProgress {
        GcProgress {
            moved: std::mem::take(&mut self.outbox),
            freed,
            state: self.state(),
            done,
        }
    }

    // ------------------------------------------------------------ urgent

    fn urgent_inner(&mut self, view: &GcView<'_>, want: u64, freed: &mut u64) -> Result<()> {
        self.load(view)?;
        self.observe_named(view)?;
        self.reconcile(view)?;
        self.ctx = Some(PassCtx::default());
        self.last_compacted.clear();
        let mut m = Mark::new(self.cfg);
        m.run(view)?;
        point!(self.points, "gc.mark.end");
        let l = m.into_liveness();
        self.after_mark(view, &l);
        let floor_grace = config::grace_floor(&view.params);
        // Old entries whose floor grace ended before this mark began: this
        // mark is their recheck.
        let verdicts: Vec<(ObjectRef, Verdict)> = {
            let judge = Judge::new(view, &l, &*self.floor, &self.outputs, self.named_high)?;
            self.list
                .entries
                .keys()
                .map(|&o| {
                    let ok = self.grace.get(&o).is_some_and(|&t| {
                        t != Duration::MAX && t.saturating_add(floor_grace) <= l.started
                    });
                    (
                        o,
                        if ok {
                            judge.verdict(o)
                        } else {
                            Verdict::Defer("floor grace not over")
                        },
                    )
                })
                .collect()
        };
        let todo =
            Judge::new(view, &l, &*self.floor, &self.outputs, self.named_high)?.dead(&self.list);
        if let Some(ctx) = &mut self.ctx {
            ctx.liveness = Some(l);
        }
        for (obj, v) in verdicts {
            if *freed >= want && v == Verdict::Delete {
                continue;
            }
            *freed += self.apply_verdict(view, obj, v)?;
        }
        // New dead objects start their (floor) grace now.
        if !todo.is_empty() {
            let l = self.ctx.as_mut().and_then(|c| c.liveness.take());
            self.condemn_unit(view, todo.clone(), false, 0)?;
            for i in 0..=todo.len() {
                self.condemn_unit(view, todo.clone(), true, i)?;
            }
            if let Some(ctx) = &mut self.ctx {
                ctx.liveness = l;
            }
        }
        self.store_list(view)?;
        if *freed < want {
            let sources = {
                let l = self
                    .ctx
                    .as_ref()
                    .and_then(|c| c.liveness.as_ref())
                    .ok_or(Error::invalid("urgent pass without a mark"))?;
                let s = self.compaction_sources(view, l, want - *freed);
                (!s.is_empty()).then(|| Compactor::new(l, s, view.clock.monotonic()))
            };
            if let Some(mut c) = sources {
                while !c.is_done() {
                    match c.unit(view, &self.points, &mut self.outbox, &mut self.outputs) {
                        Ok(Some((seg, bytes))) => self.evidence(
                            view,
                            Action::Output,
                            ObjectRef::Segment(seg),
                            bytes,
                            "compaction output",
                        ),
                        Ok(None) => {}
                        Err(e) => {
                            c.abandon(view);
                            return Err(e);
                        }
                    }
                }
                self.compaction_done(view, &c);
            }
        }
        self.finish_pass(view)
    }
}

impl Collector for Gc {
    fn step(&mut self, view: &GcView<'_>, budget: GcBudget) -> Result<GcProgress> {
        self.precheck(view)?;
        let deadline = view.clock.monotonic().saturating_add(budget.max_duration);
        let mut freed = 0u64;
        let mut step_bytes = 0u64;
        let mut done = false;
        let res = (|| -> Result<()> {
            self.load(view)?;
            self.observe_named(view)?;
            loop {
                match self.unit(view, &budget, &mut freed, &mut step_bytes)? {
                    Flow::Continue => {}
                    Flow::Yield => return Ok(()),
                    Flow::PassDone => {
                        done = true;
                        return Ok(());
                    }
                }
                if view.clock.monotonic() >= deadline {
                    return Ok(());
                }
            }
        })();
        match res {
            Ok(()) => Ok(self.take_progress(freed, done)),
            Err(e) => {
                if self.points.alive().is_ok() {
                    tracing::warn!(error = %e, "gc pass abandoned; the next pass starts over");
                    self.abort_pass(view);
                }
                Err(e)
            }
        }
    }

    fn urgent(&mut self, view: &GcView<'_>, want_bytes: u64) -> Result<GcProgress> {
        self.precheck(view)?;
        self.abort_pass(view);
        let mut freed = 0u64;
        match self.urgent_inner(view, want_bytes, &mut freed) {
            Ok(()) => Ok(self.take_progress(freed, true)),
            Err(e) => {
                if self.points.alive().is_ok() {
                    self.abort_pass(view);
                }
                Err(e)
            }
        }
    }

    fn dry_run(&mut self, view: &GcView<'_>) -> Result<GcPassSummary> {
        self.precheck(view)?;
        self.load(view)?;
        let l = crate::mark::liveness(view, self.cfg)?;
        let judge = Judge::new(view, &l, &*self.floor, &self.outputs, self.named_high)?;
        let mut s = GcPassSummary {
            dry_run: true,
            ..GcPassSummary::default()
        };
        let mut count = |obj: ObjectRef, bytes: u64| {
            s.bytes_freed += bytes;
            match obj {
                ObjectRef::Segment(_) => s.segments_deleted += 1,
                ObjectRef::Pack(_) => s.packs_deleted += 1,
                ObjectRef::Run(_) => s.runs_deleted += 1,
                ObjectRef::Commit(_) => s.commits_deleted += 1,
            }
        };
        let dead = judge.dead(&self.list);
        for &(obj, bytes) in &dead {
            count(obj, bytes);
        }
        for &obj in self.list.entries.keys() {
            if judge.verdict(obj) == Verdict::Delete {
                count(obj, object_bytes(&l, obj).unwrap_or(0));
            }
        }
        drop(judge);
        s.segments_condemned = dead
            .iter()
            .filter(|(o, _)| matches!(o, ObjectRef::Segment(_)))
            .count() as u64;
        let dead_set: BTreeSet<ObjectRef> = dead.iter().map(|(o, _)| *o).collect();
        let sources: Vec<_> = self
            .compaction_sources(view, &l, self.cfg.compact_per_pass)
            .into_iter()
            .filter(|c| !dead_set.contains(&ObjectRef::Segment(c.seg)))
            .collect();
        s.segments_compacted = sources.len() as u64;
        s.bytes_moved = sources.iter().map(|c| c.live).sum();
        Ok(s)
    }

    fn stats(&self) -> GcStats {
        self.stats
    }
}
