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
//! * GC's retained set is every FULL from the lowest `chain_floor` any
//!   retained FULL records up to the tip (N12 (b)): all of them are roots and
//!   nothing they name is condemned.
//! * Commit objects are deleted only below that lowest `chain_floor`,
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
use crate::condemned::{CondemnedEntry, CondemnedList, GraceStarts, ObjectRef, MAX_ENTRIES};
use crate::config::{self, FloorSource, GcConfig, GcHooks, NoHooks};
use crate::evidence::{Action, Evidence};
use crate::mark::{Liveness, Mark};
use crate::points::{point, Points};

/// Extents a compactor queues at a time.
const COMPACT_QUEUE: usize = 1 << 16;

/// Why an object must stay.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Keep {
    /// A commit at or above the lowest `chain_floor` of a retained FULL.
    ChainFloor,
    /// A commit a pin names.
    Pinned,
    /// `state/pins` could not be read: no commit is deleted.
    FloorUnreadable,
    /// A commit in the rollback-floor window.
    RollbackFloor,
    /// Above the tip's high-water mark.
    AboveMark,
    /// Named by a currently retained commit.
    Named,
    /// A run the index manifest names.
    Manifest,
    /// Reached by a root of the mark.
    Referenced,
    /// A compaction output no FULL names yet.
    Output,
}

impl Keep {
    fn reason(self) -> &'static str {
        match self {
            Self::ChainFloor => "at or above a retained chain_floor",
            Self::Pinned => "pinned",
            Self::FloorUnreadable => "rollback floor unreadable",
            Self::RollbackFloor => "at or above the rollback floor",
            Self::AboveMark => "above the high-water mark",
            Self::Named => "named by a retained commit",
            Self::Manifest => "named by the index manifest",
            Self::Referenced => "referenced at recheck",
            Self::Output => "unnamed compaction output",
        }
    }
}

/// What a recheck decides for one condemned object.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Verdict {
    /// Still unreferenced: delete.
    Delete,
    /// Referenced again (or named, or above the mark): un-condemn.
    Uncondemn(Keep),
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
        queue: VecDeque<ObjectRef>,
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

/// Objects any FULL of GC's retained set names (rule 12 (ii), N12), and
/// that set's commit seqs.
fn named_by_retained(view: &GcView<'_>) -> Result<(BTreeSet<u64>, BTreeSet<CommitSeq>)> {
    let mut s = BTreeSet::new();
    let mut seqs = BTreeSet::new();
    for f in crate::mark::retained_fulls(view)? {
        seqs.insert(f.common.commit_seq);
        s.extend(
            f.new_segments
                .iter()
                .chain(&f.map_packs)
                .chain(&f.index_packs)
                .chain(f.map_root.pack.iter())
                .map(|id| id.raw()),
        );
    }
    Ok((s, seqs))
}

fn object_bytes(l: &Liveness, obj: ObjectRef) -> Option<u64> {
    match obj {
        ObjectRef::Segment(s) => l.segment_size(s),
        ObjectRef::Pack(p) => l.pack_size(p),
        ObjectRef::Run(r) => l.run(r).map(|x| x.size_bytes),
        ObjectRef::Commit(c) => l.commit_size(c),
    }
}

fn space_kind(obj: ObjectRef) -> SpaceKind {
    match obj {
        ObjectRef::Segment(_) => SpaceKind::Segments,
        ObjectRef::Pack(_) => SpaceKind::Map,
        ObjectRef::Run(_) => SpaceKind::Index,
        ObjectRef::Commit(_) => SpaceKind::Commits,
    }
}

/// What condemn, recheck and delete decisions read from the view and the
/// stores: built once per step (no cut happens inside a step).
#[derive(Debug)]
struct Facts {
    named: BTreeSet<u64>,
    manifest: BTreeSet<ObjId>,
    retained: BTreeSet<CommitSeq>,
    hwm: u64,
    /// The highest id a durable FULL is known to name (see `Gc::named_high`).
    named_high: u64,
    /// The lowest `chain_floor` of any retained FULL: commit objects from it
    /// up are kept, and so is what those FULLs reach and name, so that
    /// `fsck --repair --truncate-tip` gives a complete volume (N12 (b)).
    keep_floor: CommitSeq,
    tip: CommitSeq,
    /// `None` when `state/pins` could not be read.
    floor: Option<Option<RollbackFloor>>,
}

impl Facts {
    fn read(
        view: &GcView<'_>,
        floor_src: &dyn FloorSource,
        named_high: u64,
        by_retained: (BTreeSet<u64>, BTreeSet<CommitSeq>),
    ) -> Result<Self> {
        let floor = match floor_src.current_floor() {
            Ok(f) => Some(f),
            Err(e) => {
                tracing::warn!(error = %e, "gc: rollback floor unreadable; no commit object is deleted in this step");
                None
            }
        };
        let keep_floor = crate::mark::keep_floor(view);
        let (named, mut retained) = by_retained;
        retained.extend(view.retained.iter().map(|c| c.common().commit_seq));
        Ok(Self {
            named,
            manifest: view.index.runs()?.iter().map(|r| r.id).collect(),
            retained,
            hwm: view.tip.high_water_mark,
            // Only FULLs below the tip count (`observe_named`): a copy the
            // tip alone names goes with the tip at `--truncate-tip` (B-1).
            named_high,
            keep_floor,
            tip: view.tip.commit_seq,
            floor,
        })
    }
}

/// Condemn, recheck and delete decisions over one mark.
struct Judge<'a> {
    f: &'a Facts,
    l: &'a Liveness,
    outputs: &'a BTreeSet<SegId>,
}

impl Judge<'_> {
    /// Why a commit object must stay, if it must.
    fn commit_kept(&self, seq: CommitSeq) -> Option<Keep> {
        let f = self.f;
        if seq >= f.keep_floor || seq >= f.tip || f.retained.contains(&seq) {
            return Some(Keep::ChainFloor);
        }
        if self.l.pinned.contains(&seq) {
            return Some(Keep::Pinned);
        }
        match f.floor {
            None => Some(Keep::FloorUnreadable),
            Some(Some(fl)) if seq >= fl.max_commit_seq => Some(Keep::RollbackFloor),
            Some(_) => None,
        }
    }

    /// Why a container must stay regardless of liveness, if it must.
    fn container_kept(&self, raw: u64) -> Option<Keep> {
        if raw > self.f.hwm {
            Some(Keep::AboveMark)
        } else if self.f.named.contains(&raw) {
            Some(Keep::Named)
        } else {
            None
        }
    }

    /// The dead objects of the mark that are not on `list` yet, at most
    /// `room` of them.
    fn dead(&self, list: &CondemnedList, room: usize) -> Vec<(ObjectRef, u64)> {
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
            if !self.f.manifest.contains(&r.id) && self.container_kept(r.id.raw()).is_none() {
                out.push((ObjectRef::Run(r.id), r.size_bytes));
            }
        }
        for c in &l.commits {
            if self.commit_kept(c.seq).is_none() {
                out.push((ObjectRef::Commit(c.seq), c.size_bytes));
            }
        }
        out.retain(|(o, _)| !list.entries.contains_key(o));
        out.truncate(room);
        out
    }

    /// The recheck of one condemned object against this (newer) mark.
    fn verdict(&self, obj: ObjectRef) -> Verdict {
        let l = self.l;
        if object_bytes(l, obj).is_none() {
            return Verdict::Gone;
        }
        let keep = match obj {
            ObjectRef::Segment(s) => {
                if l.live_bytes(s) > 0 {
                    Some(Keep::Referenced)
                } else if self.outputs.contains(&s) {
                    Some(Keep::Output)
                } else {
                    self.container_kept(s.raw())
                }
            }
            ObjectRef::Pack(p) => {
                if l.live_packs.contains(&p) {
                    Some(Keep::Referenced)
                } else {
                    self.container_kept(p.raw())
                }
            }
            ObjectRef::Run(r) => {
                if self.f.manifest.contains(&r) {
                    Some(Keep::Manifest)
                } else {
                    self.container_kept(r.raw())
                }
            }
            ObjectRef::Commit(c) => self.commit_kept(c),
        };
        match keep {
            None => match obj {
                ObjectRef::Segment(s) => match self.segment_deletable(s) {
                    Some(why) => Verdict::Defer(why),
                    None => Verdict::Delete,
                },
                _ => Verdict::Delete,
            },
            Some(Keep::FloorUnreadable) => Verdict::Defer(Keep::FloorUnreadable.reason()),
            Some(k) => Verdict::Uncondemn(k),
        }
    }

    /// The delete-time index invariant for a segment: every copy its stale
    /// hints rely on must be in a segment a durable FULL named (at or below
    /// `named_high`) that is not an unnamed compaction output.
    fn segment_deletable(&self, s: SegId) -> Option<&'static str> {
        let blocked = self.l.deps.get(&s).is_some_and(|deps| {
            deps.iter()
                .any(|d| d.raw() > self.f.named_high || self.outputs.contains(d))
        });
        blocked.then_some("live copies of its ids are not durable yet")
    }
}

/// The phase-1 collector (`lunix-gc`).
///
/// The collector must live as long as the engine: the unnamed compaction
/// outputs and the moves not yet handed over are memory only, and a wiring
/// that rebuilt it while moves were pending would lose that protection.
pub struct Gc {
    cfg: GcConfig,
    floor: Arc<dyn FloorSource>,
    points: Points,
    loaded: bool,
    list: CondemnedList,
    grace: GraceStarts,
    outputs: BTreeSet<SegId>,
    /// The highest id named by a FULL read by `observe_named` that has a
    /// successor FULL; the tip's own names never count. Every segment at or
    /// below it was named by a durable FULL below the tip, except compaction
    /// outputs still in `outputs` and segments no index entry points into
    /// (orphans): ids are allocated in order and a cut names everything it
    /// wrote. A copy only the tip names is not durable for this rule: after
    /// `fsck --repair --truncate-tip` it is above the new chain's mark and the
    /// mount sweep deletes it.
    named_high: u64,
    /// The newest commit `observe_named` has read.
    scanned: Option<CommitSeq>,
    /// [`named_by_retained`] for one tip: the FULLs below the tip's
    /// `chain_floor` are read from the store once per tip, not every step.
    retained_cache: Option<(CommitSeq, BTreeSet<u64>, BTreeSet<CommitSeq>)>,
    /// Facts of the current step (`None` between steps).
    facts: Option<Facts>,
    /// Objects whose delete call failed: the file may be gone without its
    /// bytes released. Released when a later recheck finds it gone.
    uncertain: BTreeMap<ObjectRef, u64>,
    /// Compaction sources with an extent that does not open.
    bad_sources: BTreeSet<SegId>,
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
            retained_cache: None,
            facts: None,
            uncertain: BTreeMap::new(),
            bad_sources: BTreeSet::new(),
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

    /// Compaction outputs this process sealed that no FULL names yet (never
    /// condemned).
    pub fn unnamed_outputs(&self) -> Vec<SegId> {
        self.outputs.iter().copied().collect()
    }

    /// Compaction sources skipped because an extent did not open.
    pub fn skipped_sources(&self) -> Vec<SegId> {
        self.bad_sources.iter().copied().collect()
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

    /// Where the pass is (the evidence write at the end reports as
    /// `Compacting`).
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

    /// The facts of this step.
    /// The objects and commit seqs of GC's retained set, cached per tip.
    fn retained_names(
        &mut self,
        view: &GcView<'_>,
    ) -> Result<(BTreeSet<u64>, BTreeSet<CommitSeq>)> {
        if let Some((tip, named, seqs)) = &self.retained_cache {
            if *tip == view.tip.commit_seq {
                return Ok((named.clone(), seqs.clone()));
            }
        }
        let (named, seqs) = named_by_retained(view)?;
        self.retained_cache = Some((view.tip.commit_seq, named.clone(), seqs.clone()));
        Ok((named, seqs))
    }

    fn facts(&mut self, view: &GcView<'_>) -> Result<&Facts> {
        if self.facts.is_none() {
            let by = self.retained_names(view)?;
            self.facts = Some(Facts::read(view, &*self.floor, self.named_high, by)?);
        }
        self.facts
            .as_ref()
            .ok_or(Error::invalid("gc facts missing"))
    }

    /// Makes the index flags and the list agree: a listed segment without a
    /// flag is flagged and its grace restarts; a flagged segment that is not
    /// listed is adopted (fresh grace) when it exists; one that is gone has
    /// its index entries dropped before its flag is cleared.
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
                None => {
                    // Never clear the flag while entries may still point in.
                    view.index.drop_segment(s)?;
                    view.index.unmark_condemned(s)?;
                }
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

    /// Reads the FULLs committed since the last call that have a successor
    /// (the tip is read once the next FULL exists): every id they name
    /// raises `named_high`. Requiring a successor keeps a copy that only the
    /// tip names from counting as durable: `--truncate-tip` may drop the tip,
    /// and the mount sweep then deletes everything above the new chain's mark
    /// (M17.B review B-1); it also covers an engine that has not persisted a
    /// FULL's index entries at its own C8 (N13). A
    /// compaction output a retained or read FULL names is no longer pending.
    /// Commit objects are durable when they exist, and GC deletes none it
    /// has not seen here first (a commit is condemned only in a pass, after
    /// this ran), so no FULL is missed.
    fn observe_named(&mut self, view: &GcView<'_>) -> Result<()> {
        let (named, _) = self.retained_names(view)?;
        self.outputs.retain(|o| !named.contains(&o.raw()));
        let Some(last) = view.tip.commit_seq.0.checked_sub(1).map(CommitSeq) else {
            return Ok(());
        };
        if self.scanned.is_some_and(|s| s >= last) {
            return Ok(());
        }
        for c in view.commits.list()? {
            if c.seq > last || self.scanned.is_some_and(|s| c.seq <= s) {
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
        self.scanned = Some(last);
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

    /// Writes the pass's evidence object when it has lines; returns its
    /// number (0 when nothing was written).
    fn write_evidence(&self, view: &GcView<'_>, ctx: &PassCtx) -> Result<u64> {
        if ctx.evidence.is_empty() {
            return Ok(0);
        }
        let pass = view.state.evidence_passes()?.last().copied().unwrap_or(0) + 1;
        view.state
            .put_evidence(pass, &ctx.evidence.finish(view.clock.now()))?;
        Ok(pass)
    }

    /// Drops the pass in progress (an error, or an urgent pass). The list,
    /// the index flags and the unnamed outputs stay; the evidence of what the
    /// pass already did is written, best effort.
    fn abort_pass(&mut self, view: &GcView<'_>) {
        if let Phase::Compacting(Some(c)) = &mut self.phase {
            c.abandon(view);
        }
        if let Some(ctx) = self.ctx.take() {
            match self.write_evidence(view, &ctx) {
                Ok(_) => self.add_totals(&ctx.summary),
                Err(e) => {
                    tracing::warn!(error = %e, "gc: the evidence of an abandoned pass was not written");
                }
            }
        }
        self.phase = Phase::Idle;
        self.last_end = Some(view.clock.monotonic());
    }

    fn note_heap(&mut self) {
        let mut h = self.list.entries.len() * 64
            + self.grace.len() * 48
            + self.outputs.len() * 32
            + self.uncertain.len() * 48;
        h += match &self.phase {
            Phase::Marking(m) | Phase::Rechecking(m) => m.heap_bytes(),
            Phase::Condemning { todo, .. } => todo.capacity() * 24,
            Phase::Deleting { queue } => queue.capacity() * 16,
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
        // what comes on top of it); two exist only while a recheck is built
        // next to the condemn mark, and then the older one counts here.
        if let (
            Phase::Marking(_) | Phase::Rechecking(_),
            Some(PassCtx {
                liveness: Some(l), ..
            }),
        ) = (&self.phase, &self.ctx)
        {
            h += l.live_set_bytes();
        }
        self.peak_heap = self.peak_heap.max(h);
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
                    self.after_mark(&l);
                    let room = (MAX_ENTRIES as usize).saturating_sub(self.list.entries.len());
                    self.facts(view)?;
                    let todo = match &self.facts {
                        Some(f) => Judge {
                            f,
                            l: &l,
                            outputs: &self.outputs,
                        }
                        .dead(&self.list, room),
                        None => Vec::new(),
                    };
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
                if !listed {
                    self.list_condemned(view, &todo)?;
                    self.phase = Phase::Condemning {
                        todo,
                        listed: true,
                        next: 0,
                    };
                } else if let Some(&(obj, _)) = todo.get(next) {
                    if let ObjectRef::Segment(s) = obj {
                        view.index.mark_condemned(s)?;
                    }
                    self.phase = Phase::Condemning {
                        todo,
                        listed: true,
                        next: next + 1,
                    };
                } else {
                    let until = self.start_graces(view, &todo);
                    // The condemn mark is not needed again: the recheck
                    // makes its own.
                    if let Some(ctx) = &mut self.ctx {
                        ctx.liveness = None;
                    }
                    self.phase = Phase::Grace { until };
                }
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
                    self.after_mark(&l);
                    if let Some(ctx) = &mut self.ctx {
                        ctx.liveness = Some(l);
                    }
                    let queue = self.list.entries.keys().copied().collect();
                    self.phase = Phase::Deleting { queue };
                } else {
                    self.phase = Phase::Rechecking(m);
                }
                Flow::Continue
            }
            Phase::Deleting { mut queue } => {
                match queue.pop_front() {
                    Some(obj) => {
                        let v = self.recheck_verdict(view, obj, view.params.grace)?;
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
            Phase::Compacting(c) => self.compact_unit(view, budget, c, step_bytes),
            Phase::Finishing => {
                self.finish_pass(view)?;
                Flow::PassDone
            }
        };
        self.note_heap();
        Ok(flow)
    }

    fn after_mark(&mut self, l: &Liveness) {
        self.stats.live_bytes = l.total_live();
        self.stats.dead_bytes = l
            .segments
            .iter()
            .map(|s| s.size_bytes.saturating_sub(l.live_bytes(s.seg)))
            .sum();
        self.last_pins = Some(l.pinned.clone());
    }

    /// Adds `todo` to the list (grace not started: the flags come next) and
    /// stores it.
    fn list_condemned(&mut self, view: &GcView<'_>, todo: &[(ObjectRef, u64)]) -> Result<()> {
        if todo.is_empty() {
            return Ok(());
        }
        let wall = view.clock.now();
        for &(obj, bytes) in todo {
            self.list.entries.insert(
                obj,
                CondemnedEntry {
                    condemned_at: wall,
                    bytes,
                },
            );
            self.grace.insert(obj, Duration::MAX);
        }
        self.store_list(view)?;
        point!(self.points, "gc.condemn.after_list_write");
        Ok(())
    }

    /// Everything in `todo` is flagged: the graces start now. Returns when the
    /// latest grace of the list ends.
    fn start_graces(&mut self, view: &GcView<'_>, todo: &[(ObjectRef, u64)]) -> Duration {
        let now = view.clock.monotonic();
        for &(obj, bytes) in todo {
            self.grace.insert(obj, now);
            self.evidence(view, Action::Condemn, obj, bytes, "unreferenced");
            if let Some(ctx) = &mut self.ctx {
                if matches!(obj, ObjectRef::Segment(_)) {
                    ctx.summary.segments_condemned += 1;
                }
            }
        }
        let latest = self
            .grace
            .values()
            .copied()
            .filter(|&t| t != Duration::MAX)
            .max()
            .unwrap_or(now);
        latest.saturating_add(view.params.grace)
    }

    /// The verdict for one listed object against the pass's recheck mark: a
    /// grace of `grace` must have ended before that mark started.
    fn recheck_verdict(
        &mut self,
        view: &GcView<'_>,
        obj: ObjectRef,
        grace: Duration,
    ) -> Result<Verdict> {
        self.facts(view)?;
        let (Some(f), Some(l)) = (
            self.facts.as_ref(),
            self.ctx.as_ref().and_then(|c| c.liveness.as_ref()),
        ) else {
            return Err(Error::invalid("recheck without a mark"));
        };
        let elapsed = self
            .grace
            .get(&obj)
            .is_some_and(|&t| t != Duration::MAX && t.saturating_add(grace) <= l.started);
        if !elapsed {
            return Ok(Verdict::Defer("grace not over"));
        }
        Ok(Judge {
            f,
            l,
            outputs: &self.outputs,
        }
        .verdict(obj))
    }

    /// Applies one verdict; returns the bytes freed.
    fn apply_verdict(&mut self, view: &GcView<'_>, obj: ObjectRef, v: Verdict) -> Result<u64> {
        match v {
            Verdict::Gone => self.forget(view, obj),
            Verdict::Defer(why) => {
                self.evidence(view, Action::Defer, obj, 0, why);
                Ok(0)
            }
            Verdict::Uncondemn(keep) => {
                if let ObjectRef::Segment(s) = obj {
                    view.index.unmark_condemned(s)?;
                }
                self.list.entries.remove(&obj);
                self.grace.remove(&obj);
                // Durable at once: a crash must not re-flag a live segment.
                self.store_list(view)?;
                self.evidence(view, Action::Uncondemn, obj, 0, keep.reason());
                Ok(0)
            }
            Verdict::Delete => self.delete(view, obj),
        }
    }

    /// Removes a listed object that no longer exists. A segment's index
    /// entries are dropped before its flag is cleared, and an error leaves
    /// both for the next pass. Bytes of an earlier failed delete call are
    /// released now.
    fn forget(&mut self, view: &GcView<'_>, obj: ObjectRef) -> Result<u64> {
        if let ObjectRef::Segment(s) = obj {
            view.index.drop_segment(s)?;
            view.index.unmark_condemned(s)?;
        }
        self.list.entries.remove(&obj);
        self.grace.remove(&obj);
        let freed = self.uncertain.remove(&obj).unwrap_or(0);
        if freed > 0 {
            view.space.release(space_kind(obj), freed);
            self.count_delete(obj, freed);
        }
        self.evidence(view, Action::Delete, obj, freed, "already gone");
        Ok(freed)
    }

    fn count_delete(&mut self, obj: ObjectRef, freed: u64) {
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
    }

    /// Deletes one object (unlink, directory fsync inside the store) and
    /// releases its bytes, then the index follow-up for a segment. A failed
    /// call leaves the object in `uncertain` with its listed size.
    fn delete(&mut self, view: &GcView<'_>, obj: ObjectRef) -> Result<u64> {
        let listed = self
            .ctx
            .as_ref()
            .and_then(|c| c.liveness.as_ref())
            .and_then(|l| object_bytes(l, obj))
            .unwrap_or(0);
        let run = match obj {
            ObjectRef::Run(r) => self
                .ctx
                .as_ref()
                .and_then(|c| c.liveness.as_ref())
                .and_then(|l| l.run(r)),
            _ => None,
        };
        if matches!(obj, ObjectRef::Commit(_)) {
            point!(self.points, "gc.commit_delete.before_unlink");
        } else {
            point!(self.points, "gc.delete.before_unlink");
        }
        self.uncertain.insert(obj, listed);
        let res = match obj {
            ObjectRef::Segment(s) => view.segments.delete(s),
            ObjectRef::Pack(p) => view.packs.delete_pack(p),
            ObjectRef::Run(_) => match run {
                Some(run) => view.index.delete_run(run),
                None => Ok(0),
            },
            ObjectRef::Commit(c) => view.commits.delete(c, view.tip.chain_floor),
        };
        let n = match res {
            Ok(n) => n,
            Err(e) if e.kind() == ErrorKind::Refused => {
                // Nothing changed (the commit store's own floor check).
                self.uncertain.remove(&obj);
                self.evidence(view, Action::Defer, obj, 0, "refused by the store");
                return Ok(0);
            }
            Err(e) => return Err(e),
        };
        self.uncertain.remove(&obj);
        if n > 0 {
            view.space.release(space_kind(obj), n);
        }
        if !matches!(obj, ObjectRef::Commit(_)) {
            point!(self.points, "gc.delete.after_unlink");
        }
        if let ObjectRef::Segment(s) = obj {
            // The flag stays until the entries are gone (B1).
            view.index.drop_segment(s)?;
            view.index.unmark_condemned(s)?;
        }
        if !matches!(obj, ObjectRef::Commit(_)) {
            point!(self.points, "gc.delete.after_dirsync");
        }
        self.list.entries.remove(&obj);
        self.grace.remove(&obj);
        self.evidence(
            view,
            Action::Delete,
            obj,
            n,
            "unreferenced after grace and recheck",
        );
        self.count_delete(obj, n);
        Ok(n)
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
                || self.bad_sources.contains(&s)
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

    /// Moves the compactor's skipped sources into `bad_sources`.
    fn note_bad(&mut self, view: &GcView<'_>, c: &mut Compactor) {
        for seg in std::mem::take(&mut c.bad) {
            if self.bad_sources.insert(seg) {
                self.evidence(
                    view,
                    Action::Skip,
                    ObjectRef::Segment(seg),
                    0,
                    "an extent does not open",
                );
            }
        }
    }

    /// One compaction unit. An error ends compaction (the pass still writes
    /// its evidence and finishes); it never fails the step.
    fn compact_unit(
        &mut self,
        view: &GcView<'_>,
        budget: &GcBudget,
        c: Option<Box<Compactor>>,
        step_bytes: &mut u64,
    ) -> Flow {
        let mut c = match c {
            Some(c) => c,
            None => {
                let sources = match self.ctx.as_ref().and_then(|x| x.liveness.as_ref()) {
                    Some(l) => {
                        let s = self.compaction_sources(view, l, self.cfg.compact_per_pass);
                        (!s.is_empty())
                            .then(|| Compactor::new(s, view.clock.monotonic(), COMPACT_QUEUE))
                    }
                    None => None,
                };
                match sources {
                    Some(c) => Box::new(c),
                    None => {
                        self.phase = Phase::Finishing;
                        return Flow::Continue;
                    }
                }
            }
        };
        if c.is_done() {
            self.compaction_done(view, &c);
            self.phase = Phase::Finishing;
            return Flow::Continue;
        }
        // Rate cap: bytes read so far may not run ahead of `gc_rate` by more
        // than one step's worth; a step moves at most `max_bytes`.
        let elapsed_ms = view.clock.monotonic().saturating_sub(c.started).as_millis();
        let allowed = u128::from(view.params.rate_bytes_per_s)
            .saturating_mul(elapsed_ms)
            .checked_div(1000)
            .unwrap_or(0)
            .saturating_add(u128::from(budget.max_bytes));
        if u128::from(c.bytes_read) >= allowed || *step_bytes >= budget.max_bytes {
            self.phase = Phase::Compacting(Some(c));
            return Flow::Yield;
        }
        let before = c.bytes_read;
        let res = {
            let Gc {
                ctx,
                points,
                outbox,
                outputs,
                ..
            } = self;
            match ctx.as_ref().and_then(|x| x.liveness.as_ref()) {
                Some(l) => c.unit(view, l, points, outbox, outputs),
                None => Err(Error::invalid("compaction without a mark")),
            }
        };
        *step_bytes += c.bytes_read - before;
        self.note_bad(view, &mut c);
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
                tracing::warn!(error = %e, "gc compaction stopped for this pass");
                c.abandon(view);
                self.compaction_done(view, &c);
                self.phase = Phase::Finishing;
                return Flow::Continue;
            }
        }
        self.phase = Phase::Compacting(Some(c));
        Flow::Continue
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

    fn add_totals(&mut self, p: &GcPassSummary) {
        let t = &mut self.totals;
        t.pass += u64::from(p.pass != 0);
        t.segments_condemned += p.segments_condemned;
        t.segments_deleted += p.segments_deleted;
        t.packs_deleted += p.packs_deleted;
        t.runs_deleted += p.runs_deleted;
        t.commits_deleted += p.commits_deleted;
        t.segments_compacted += p.segments_compacted;
        t.bytes_freed += p.bytes_freed;
        t.bytes_moved += p.bytes_moved;
    }

    /// Writes the evidence object (when the pass did anything) and closes the
    /// pass.
    fn finish_pass(&mut self, view: &GcView<'_>) -> Result<()> {
        let mut ctx = self.ctx.take().unwrap_or_default();
        let written = self.write_evidence(view, &ctx);
        let pass = match written {
            Ok(p) => p,
            Err(e) => {
                // Keep the lines for `abort_pass` to retry.
                self.ctx = Some(ctx);
                return Err(e);
            }
        };
        if pass != 0 {
            point!(self.points, "gc.evidence.after_put");
        }
        ctx.summary.pass = pass;
        self.stats.passes += 1;
        self.add_totals(&ctx.summary);
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

    fn run_units(&mut self, view: &GcView<'_>, budget: &GcBudget, freed: &mut u64) -> Result<bool> {
        let deadline = view.clock.monotonic().saturating_add(budget.max_duration);
        let mut step_bytes = 0u64;
        self.load(view)?;
        self.observe_named(view)?;
        loop {
            match self.unit(view, budget, freed, &mut step_bytes)? {
                Flow::Continue => {}
                Flow::Yield => return Ok(false),
                Flow::PassDone => return Ok(true),
            }
            if view.clock.monotonic() >= deadline {
                return Ok(false);
            }
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
        self.after_mark(&l);
        let room = (MAX_ENTRIES as usize).saturating_sub(self.list.entries.len());
        self.facts(view)?;
        let todo = match &self.facts {
            Some(f) => Judge {
                f,
                l: &l,
                outputs: &self.outputs,
            }
            .dead(&self.list, room),
            None => Vec::new(),
        };
        if let Some(ctx) = &mut self.ctx {
            ctx.liveness = Some(l);
        }
        // Old entries whose floor grace ended before this mark began: this
        // mark is their recheck.
        let floor_grace = config::grace_floor(&view.params);
        for obj in self.list.entries.keys().copied().collect::<Vec<_>>() {
            let v = self.recheck_verdict(view, obj, floor_grace)?;
            if *freed >= want && v == Verdict::Delete {
                continue;
            }
            *freed += self.apply_verdict(view, obj, v)?;
        }
        // New dead objects start their (floor) grace now.
        if !todo.is_empty() {
            self.list_condemned(view, &todo)?;
            for &(obj, _) in &todo {
                if let ObjectRef::Segment(s) = obj {
                    view.index.mark_condemned(s)?;
                }
            }
            self.start_graces(view, &todo);
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
                (!s.is_empty()).then(|| Compactor::new(s, view.clock.monotonic(), COMPACT_QUEUE))
            };
            if let Some(mut c) = sources {
                while !c.is_done() {
                    let r = {
                        let Gc {
                            ctx,
                            points,
                            outbox,
                            outputs,
                            ..
                        } = &mut *self;
                        match ctx.as_ref().and_then(|x| x.liveness.as_ref()) {
                            Some(l) => c.unit(view, l, points, outbox, outputs),
                            None => Err(Error::invalid("compaction without a mark")),
                        }
                    };
                    self.note_bad(view, &mut c);
                    match r {
                        Ok(Some((seg, bytes))) => self.evidence(
                            view,
                            Action::Output,
                            ObjectRef::Segment(seg),
                            bytes,
                            "compaction output",
                        ),
                        Ok(None) => {}
                        Err(e) => {
                            self.points.alive()?;
                            tracing::warn!(error = %e, "gc urgent compaction stopped");
                            c.abandon(view);
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
        self.facts = None;
        let mut freed = 0u64;
        let res = self.run_units(view, &budget, &mut freed);
        self.facts = None;
        match res {
            Ok(done) => Ok(self.take_progress(freed, done)),
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
        self.facts = None;
        let mut freed = 0u64;
        let res = self.urgent_inner(view, want_bytes, &mut freed);
        self.facts = None;
        match res {
            Ok(()) => Ok(self.take_progress(freed, true)),
            Err(e) => {
                if self.points.alive().is_err() {
                    return Err(e);
                }
                tracing::warn!(error = %e, freed, "gc urgent pass stopped early");
                self.abort_pass(view);
                if freed > 0 {
                    // The ladder needs to know what was freed.
                    Ok(self.take_progress(freed, true))
                } else {
                    Err(e)
                }
            }
        }
    }

    fn dry_run(&mut self, view: &GcView<'_>) -> Result<GcPassSummary> {
        self.precheck(view)?;
        self.load(view)?;
        let l = crate::mark::liveness(view, self.cfg)?;
        let by = self.retained_names(view)?;
        let facts = Facts::read(view, &*self.floor, self.named_high, by)?;
        let judge = Judge {
            f: &facts,
            l: &l,
            outputs: &self.outputs,
        };
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
        let dead = judge.dead(&self.list, usize::MAX);
        for &(obj, bytes) in &dead {
            count(obj, bytes);
        }
        for &obj in self.list.entries.keys() {
            if judge.verdict(obj) == Verdict::Delete {
                count(obj, object_bytes(&l, obj).unwrap_or(0));
            }
        }
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
