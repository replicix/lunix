//! The collector: the pass state machine behind [`Collector`].
//!
//! A pass is made of small units; [`Collector::step`] runs units until its
//! budget is spent, so a pass never stalls a cut for longer than a unit past
//! the budget. Two orders:
//!
//! * **The whole sequence** (a requested pass, or `GcConfig::pipeline` off):
//!   `Marking → Condemning → Grace → Rechecking → Deleting → Compacting`.
//!   Grace, recheck and delete are skipped when nothing is condemned.
//! * **Pipelined** (a scheduled pass with `pipeline`, the daemon's):
//!   `Marking → Condemning → Deleting → Compacting`. The one mark condemns
//!   the newly dead and is the recheck of the objects listed before it whose
//!   grace ended before it began; the others stay listed for a later pass.
//!
//! Compaction is skipped on a lean volume and when nothing qualifies.
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
//! * The recheck is a mark whose root set is pinned after the object was
//!   flagged and its grace elapsed: `grace start + grace <= mark start`, both
//!   on the view's monotonic clock, with the mark's start read in the unit
//!   that pins its roots (`recheck_verdict`; never the time of the delete
//!   unit). In the whole sequence it is a new mark after the grace; in a
//!   pipelined pass it is the pass's own mark, which began after the grace of
//!   every object it may delete; the urgent pass uses its own mark the same
//!   way with the floor grace. Nothing gains a reference to a flagged segment
//!   (dedup skips flagged segments and runs only inside a cut, and a step
//!   runs only with the map at the tip), so what such a mark finds dead stays
//!   dead; an object it finds referenced (or named, or above the mark) is
//!   un-condemned. A dedup hit between a mark and the flag is in the tip when
//!   the recheck mark pins its roots.
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
use crate::evidence::{Action, Deferral, Evidence};
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
    Defer(&'static str, Deferral),
}

#[derive(Debug)]
enum Phase {
    Idle,
    Marking(Box<Mark>),
    Condemning {
        todo: Vec<(ObjectRef, u64)>,
        listed: bool,
        next: usize,
        /// What follows the condemn.
        then: AfterCondemn,
    },
    Grace {
        until: Duration,
    },
    Rechecking(Box<Mark>),
    Deleting {
        queue: VecDeque<ObjectRef>,
        /// A pipelined pass: an object whose grace has not ended is skipped
        /// without a line.
        pipelined: bool,
        /// The list's length when the deletes began: the list is stored at
        /// the end only when an entry went (a pipelined pass).
        start_len: usize,
    },
    Compacting(Option<Box<Compactor>>),
    Finishing,
}

/// What follows the condemn units of a pass.
#[derive(Debug)]
enum AfterCondemn {
    /// The whole sequence: wait out the grace, then the recheck mark.
    Grace,
    /// A pipelined pass: the objects listed before this pass condemned
    /// anything, judged against this pass's mark (those whose grace ended
    /// before it began go), then compaction.
    Delete(VecDeque<ObjectRef>),
}

impl Phase {
    /// The profile slot of the phase (`None` between passes).
    fn index(&self) -> Option<usize> {
        Some(match self {
            Self::Idle => return None,
            Self::Marking(_) => slot::MARK,
            Self::Condemning { .. } => slot::CONDEMN,
            Self::Grace { .. } => slot::GRACE,
            Self::Rechecking(_) => slot::RECHECK,
            Self::Deleting { .. } => slot::DELETE,
            Self::Compacting(_) => slot::COMPACT,
            Self::Finishing => slot::FINISH,
        })
    }

    /// True in a mark phase: the condemn mark or the recheck (what the duty
    /// cap measures).
    fn is_mark(&self) -> bool {
        matches!(self, Self::Marking(_) | Self::Rechecking(_))
    }
}

#[derive(Debug, Default)]
struct PassCtx {
    summary: GcPassSummary,
    evidence: Evidence,
    liveness: Option<Liveness>,
}

/// Where one pass spent its time (diagnostics: the `gc pass done` log line).
/// Measured on the process's real clock, whatever clock the view carries.
#[derive(Debug, Default)]
struct PassProfile {
    /// When the pass started.
    started: Option<std::time::Instant>,
    /// Time inside units, per phase (`Phase::index`).
    active: [Duration; PHASES],
    /// Units run, per phase.
    units: [u64; PHASES],
    /// When the pass first entered each phase, from `started`.
    entered: [Option<Duration>; PHASES],
    /// `step` calls that ran a unit of this pass.
    steps: u64,
    /// The current `step` call is counted in `steps`.
    step_counted: bool,
    /// Time in mark units on the view's clock.
    mark_view: Duration,
}

/// Phases a pass profile tracks.
const PHASES: usize = 7;

/// The profile slots of the phases (`Phase::index`).
mod slot {
    pub const MARK: usize = 0;
    pub const CONDEMN: usize = 1;
    pub const GRACE: usize = 2;
    pub const RECHECK: usize = 3;
    pub const DELETE: usize = 4;
    pub const COMPACT: usize = 5;
    pub const FINISH: usize = 6;
}

/// Phase names, by `Phase::index`.
const PHASE_NAMES: [&str; PHASES] = [
    "mark", "condemn", "grace", "recheck", "delete", "compact", "finish",
];

impl PassProfile {
    fn ms(d: Duration) -> u64 {
        u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
    }

    /// `name=active_ms/units@entered_ms` per phase, space separated.
    fn phases(&self) -> String {
        use std::fmt::Write as _;
        let mut out = String::new();
        for (i, name) in PHASE_NAMES.iter().enumerate() {
            if let Some(at) = self.entered[i] {
                let _ = write!(
                    out,
                    "{}{name}={}ms/{}u@{}ms",
                    if out.is_empty() { "" } else { " " },
                    Self::ms(self.active[i]),
                    self.units[i],
                    Self::ms(at)
                );
            }
        }
        out
    }
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
    /// Segments the tip FULL names: a cut makes their names count.
    tip_named: BTreeSet<SegId>,
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
            tip_named: crate::compact::tip_segments(view),
            floor,
        })
    }
}

/// The reason of a delete deferred until a FULL below the tip names the live
/// copies of a segment's ids (the delete-time index invariant).
const NOT_DURABLE_YET: &str = "live copies of its ids are not durable yet";
/// The same, when no cut would help: the copies are in no unnamed output and
/// not named by the tip, so the engine is not asked for a cut (it would cut
/// an empty FULL at every pass; M17.B stack re-review M-1).
const NOT_DURABLE_NO_CUT: &str = "live copies of its ids are not durable yet; a cut does not help";
/// A pipelined pass is followed at the earliest after this many times its
/// own work (time inside units): see [`Gc::min_gap`].
const DUTY_FACTOR: u32 = 3;

/// Bytes freed by a pass that make it busy without a segment (commit objects
/// and map packs alone do not hurry the next pass).
const BUSY_FREED: u64 = 64 << 20;

/// A pass did reclaim work worth hurrying for: it condemned, deleted or
/// compacted a segment, or freed at least [`BUSY_FREED`]. Each cut leaves a
/// commit object and map packs dead, so any evidence line would make every
/// written volume busy for ever (gcrate review M-4).
fn reclaim_busy(p: &GcPassSummary) -> bool {
    p.segments_condemned + p.segments_deleted + p.segments_compacted > 0
        || p.bytes_freed >= BUSY_FREED
}

/// Evidence objects pruned per pass at most (one unlink and directory sync
/// each, under the coordinator's mutex).
const EVIDENCE_PRUNE_PER_PASS: usize = 16;

/// `gc/` objects kept locally: the newest this many; older ones are deleted
/// when a pass writes a new one (gc.md "the evidence object").
const EVIDENCE_KEEP: usize = 1024;

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
                    Some(why) => Verdict::Defer(why, Deferral::ForFull),
                    None => Verdict::Delete,
                },
                _ => Verdict::Delete,
            },
            Some(Keep::FloorUnreadable) => {
                Verdict::Defer(Keep::FloorUnreadable.reason(), Deferral::Retry)
            }
            Some(k) => Verdict::Uncondemn(k),
        }
    }

    /// The delete-time index invariant for a segment: every copy its stale
    /// hints rely on must be in a segment a durable FULL named (at or below
    /// `named_high`) that is not an unnamed compaction output.
    fn segment_deletable(&self, s: SegId) -> Option<&'static str> {
        let deps = self.l.deps.get(&s)?;
        let blocking: Vec<&SegId> = deps
            .iter()
            .filter(|d| d.raw() > self.f.named_high || self.outputs.contains(*d))
            .collect();
        if blocking.is_empty() {
            return None;
        }
        // A cut helps when it names an unnamed output, or gives the tip that
        // names a copy a successor.
        let helps = blocking
            .iter()
            .any(|d| self.outputs.contains(*d) || self.f.tip_named.contains(*d));
        Some(if helps {
            NOT_DURABLE_YET
        } else {
            NOT_DURABLE_NO_CUT
        })
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
    /// A pass was asked for (`request_pass`): the next idle step starts one
    /// whether or not it is due.
    forced: bool,
    phase: Phase,
    ctx: Option<PassCtx>,
    last_end: Option<Duration>,
    last_tip: Option<CommitSeq>,
    last_pins: Option<BTreeSet<CommitSeq>>,
    /// The last pass changed something (a line other than `defer`). A pass
    /// that changed nothing is not repeated until the tip or the pins change
    /// (M17.B stack review S-1).
    last_pass_changed: bool,
    /// A delete was deferred until a FULL below the tip names the live copies
    /// (`GcProgress::cut_wanted`).
    cut_wanted: bool,
    /// The tip a cut was last asked for: one request per tip.
    cut_asked_at: Option<CommitSeq>,
    stats: GcStats,
    last_pass: Option<GcPassSummary>,
    totals: GcPassSummary,
    last_finished_ms: Option<u64>,
    peak_heap: usize,
    unit_count: u64,
    last_compacted: Vec<compact::Candidate>,
    /// Where the current pass spends its time.
    prof: PassProfile,
    /// The current pass runs the whole sequence (a request, or `pipeline`
    /// off).
    whole: bool,
    /// The last pass did reclaim work worth hurrying for ([`reclaim_busy`]):
    /// with `pipeline`, the next is due after [`Gc::min_gap`].
    last_pass_busy: bool,
    /// Time the last finished pass spent in its marks, on the view's clock
    /// (the duty-cycle cap, [`Gc::min_gap`]). Tests set it directly.
    pub(crate) last_mark: Duration,
    /// The last pass was abandoned: the grace trigger waits `pass_interval`.
    last_aborted: bool,
    /// When the last mark of a scheduled pass began: a listed object whose
    /// grace ends after it waits for the next mark.
    last_mark_start: Option<Duration>,
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
            forced: false,
            phase: Phase::Idle,
            ctx: None,
            last_end: None,
            last_tip: None,
            last_pins: None,
            last_pass_changed: true,
            cut_wanted: false,
            cut_asked_at: None,
            stats: GcStats::default(),
            last_pass: None,
            totals: GcPassSummary::default(),
            last_finished_ms: None,
            peak_heap: 0,
            unit_count: 0,
            last_compacted: Vec::new(),
            prof: PassProfile::default(),
            whole: true,
            last_pass_busy: false,
            last_mark: Duration::ZERO,
            last_aborted: false,
            last_mark_start: None,
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
            requests: 0,
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

    /// A listed object's grace ended at or before `now` and after the last
    /// mark began: the next mark can delete it.
    fn grace_ended_since_mark(&self, grace: Duration, now: Duration) -> bool {
        self.grace
            .values()
            .filter(|&&t| t != Duration::MAX)
            .map(|t| t.saturating_add(grace))
            .any(|end| end <= now && self.last_mark_start.is_none_or(|m| m < end))
    }

    fn due(&mut self, view: &GcView<'_>) -> Result<bool> {
        if self.forced {
            return Ok(true);
        }
        let now = view.clock.monotonic();
        let waited = |d: Duration| self.last_end.is_none_or(|end| now >= end.saturating_add(d));
        let gap = self.min_gap();
        // Pipelined: a grace that ended since the last mark began makes its
        // object deletable by the next mark; it does not wait for
        // `pass_interval` (after an abandoned pass it does).
        if self.cfg.pipeline
            && waited(if self.last_aborted {
                self.cfg.pass_interval.max(gap)
            } else {
                gap
            })
            && self.grace_ended_since_mark(view.params.grace, now)
        {
            return Ok(true);
        }
        let interval = if self.cfg.pipeline && self.last_pass_busy {
            gap
        } else {
            self.cfg.pass_interval.max(gap)
        };
        if !waited(interval) {
            return Ok(false);
        }
        if self.last_tip != Some(view.tip.commit_seq)
            || (!self.list.entries.is_empty() && self.last_pass_changed)
            || !self.uncertain.is_empty()
        {
            return Ok(true);
        }
        let pins: BTreeSet<CommitSeq> = view.pins.roots()?.iter().map(|p| p.commit_seq).collect();
        if self.last_pins.as_ref() != Some(&pins) {
            return Ok(true);
        }
        // Nothing changed: look again (the pins too) one interval from now,
        // not at every idle tick; the volume is idle now.
        self.last_end = Some(now);
        self.last_pass_busy = false;
        Ok(false)
    }

    /// The shortest time between two pipelined passes: `busy_interval`, and
    /// at least [`DUTY_FACTOR`] times the last pass's mark time, so that
    /// marking takes at most about a quarter of the coordinator however large
    /// the map is. Only the mark counts: deletes and compaction are the
    /// reclaim itself, and capping them too held the collector to about an
    /// eighth of the coordinator (gcrate re-review m-1).
    pub(crate) fn min_gap(&self) -> Duration {
        self.cfg
            .busy_interval
            .min(self.cfg.pass_interval)
            .max(self.last_mark.saturating_mul(DUTY_FACTOR))
    }

    /// Writes the pass's evidence object when it has lines; returns its
    /// number (0 when nothing was written).
    fn write_evidence(&self, view: &GcView<'_>, ctx: &PassCtx) -> Result<u64> {
        if ctx.evidence.is_empty() || ctx.evidence.only_defers() {
            return Ok(0);
        }
        let passes = view.state.evidence_passes()?;
        let pass = passes.last().copied().unwrap_or(0) + 1;
        view.state
            .put_evidence(pass, &ctx.evidence.finish(view.clock.now()))?;
        // The local bound: the newest EVIDENCE_KEEP objects stay; at most
        // EVIDENCE_PRUNE_PER_PASS older ones go per pass.
        let keep = EVIDENCE_KEEP.saturating_sub(1);
        if passes.len() > keep {
            let excess = &passes[..passes.len() - keep];
            for &old in excess.iter().take(EVIDENCE_PRUNE_PER_PASS) {
                if let Err(e) = view.state.delete_evidence(old) {
                    tracing::warn!(error = %e, pass = old, "gc: an old evidence object was not pruned");
                    break;
                }
            }
        }
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
        // An abandoned pass may have done part of its work (a delete whose
        // call failed) or none (an error in its first mark): the next one
        // runs even if nothing else changed (re-review M-2).
        self.last_pass_changed = true;
        self.last_tip = None;
        // Retried one interval later, not at the busy pace (review m-2).
        self.last_pass_busy = false;
        self.last_aborted = true;
    }

    fn note_heap(&mut self) {
        let mut h = self.list.entries.len() * 64
            + self.grace.len() * 48
            + self.outputs.len() * 32
            + self.uncertain.len() * 48;
        h += match &self.phase {
            Phase::Marking(m) | Phase::Rechecking(m) => m.heap_bytes(),
            Phase::Condemning { todo, .. } => todo.capacity() * 24,
            Phase::Deleting { queue, .. } => queue.capacity() * 16,
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

    fn defer(&mut self, view: &GcView<'_>, obj: ObjectRef, why: &'static str, kind: Deferral) {
        if let Some(ctx) = &mut self.ctx {
            ctx.evidence.defer(view.clock.now(), obj, why, kind);
        }
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

    /// One unit, timed into the pass profile; logs the profile when the
    /// pass ends.
    fn unit(
        &mut self,
        view: &GcView<'_>,
        budget: &GcBudget,
        freed: &mut u64,
        step_bytes: &mut u64,
    ) -> Result<Flow> {
        let before = self.phase.index();
        // By variant, before the unit moves the phase on.
        let marking = self.phase.is_mark();
        let t = std::time::Instant::now();
        let v0 = view.clock.monotonic();
        let res = self.unit_inner(view, budget, freed, step_bytes);
        if marking {
            // A mark unit (the condemn mark or the recheck), on the view's
            // clock: the duty cap's measure.
            self.prof.mark_view += view.clock.monotonic().saturating_sub(v0);
        }
        if before.is_none() && self.phase.index().is_some() {
            self.prof = PassProfile {
                started: Some(t),
                ..PassProfile::default()
            };
        }
        if let Some(i) = before {
            self.prof.active[i] += t.elapsed();
            self.prof.units[i] += 1;
        }
        if (before.is_some() || self.phase.index().is_some()) && !self.prof.step_counted {
            self.prof.steps += 1;
            self.prof.step_counted = true;
        }
        if let (Some(j), Some(st)) = (self.phase.index(), self.prof.started) {
            self.prof.entered[j].get_or_insert_with(|| st.elapsed());
        }
        if matches!(res, Ok(Flow::PassDone)) {
            self.last_mark = self.prof.mark_view;
            self.log_pass("step");
        }
        res
    }

    /// The `gc pass done` line: where the pass spent its time.
    fn log_pass(&self, how: &'static str) {
        let Some(p) = &self.last_pass else {
            return;
        };
        let wall = self.prof.started.map_or(Duration::ZERO, |s| s.elapsed());
        // INFO for a pass that wrote evidence; on a busy volume passes that
        // changed nothing come every few seconds.
        macro_rules! pass_line {
            ($level:ident) => {
                tracing::$level!(
                    how,
                    pass = p.pass,
                    wall_ms = PassProfile::ms(wall),
                    steps = self.prof.steps,
                    phases = %self.prof.phases(),
                    condemned = p.segments_condemned,
                    segments_deleted = p.segments_deleted,
                    packs_deleted = p.packs_deleted,
                    runs_deleted = p.runs_deleted,
                    commits_deleted = p.commits_deleted,
                    compacted = p.segments_compacted,
                    bytes_freed = p.bytes_freed,
                    bytes_moved = p.bytes_moved,
                    pending = self.list.entries.len(),
                    "gc pass done"
                )
            };
        }
        if p.pass != 0 {
            pass_line!(info);
        } else {
            pass_line!(debug);
        }
    }

    fn unit_inner(
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
                self.whole = !self.cfg.pipeline || self.forced;
                self.forced = false;
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
                    let started = l.started;
                    if let Some(ctx) = &mut self.ctx {
                        ctx.liveness = Some(l);
                    }
                    self.phase = if self.whole {
                        if todo.is_empty() && self.list.entries.is_empty() {
                            Phase::Compacting(None)
                        } else {
                            Phase::Condemning {
                                todo,
                                listed: false,
                                next: 0,
                                then: AfterCondemn::Grace,
                            }
                        }
                    } else {
                        // Pipelined: the newly dead are condemned first (the
                        // flags follow the mark as closely as they can), then
                        // this mark is the recheck of the objects listed
                        // before it whose grace ended before it began.
                        self.last_mark_start = Some(started);
                        let queue: VecDeque<ObjectRef> =
                            self.list.entries.keys().copied().collect();
                        if !todo.is_empty() {
                            Phase::Condemning {
                                todo,
                                listed: false,
                                next: 0,
                                then: AfterCondemn::Delete(queue),
                            }
                        } else {
                            self.deleting_or_compacting(queue)
                        }
                    };
                } else {
                    self.phase = Phase::Marking(m);
                }
                Flow::Continue
            }
            Phase::Condemning {
                todo,
                listed,
                next,
                then,
            } => {
                if !listed {
                    self.list_condemned(view, &todo)?;
                    self.phase = Phase::Condemning {
                        todo,
                        listed: true,
                        next: 0,
                        then,
                    };
                } else if let Some(&(obj, _)) = todo.get(next) {
                    if let ObjectRef::Segment(s) = obj {
                        view.index.mark_condemned(s)?;
                    }
                    self.phase = Phase::Condemning {
                        todo,
                        listed: true,
                        next: next + 1,
                        then,
                    };
                } else {
                    let until = self.start_graces(view, &todo);
                    self.phase = match then {
                        AfterCondemn::Grace => {
                            // The condemn mark is not needed again: the
                            // recheck makes its own.
                            if let Some(ctx) = &mut self.ctx {
                                ctx.liveness = None;
                            }
                            Phase::Grace { until }
                        }
                        // Pipelined: the new graces run into the next
                        // passes; the deletes and compaction use this mark.
                        AfterCondemn::Delete(queue) => self.deleting_or_compacting(queue),
                    };
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
                    self.last_mark_start = Some(l.started);
                    if let Some(ctx) = &mut self.ctx {
                        ctx.liveness = Some(l);
                    }
                    let queue = self.list.entries.keys().copied().collect();
                    self.phase = Phase::Deleting {
                        start_len: self.list.entries.len(),
                        queue,
                        pipelined: false,
                    };
                } else {
                    self.phase = Phase::Rechecking(m);
                }
                Flow::Continue
            }
            Phase::Deleting {
                mut queue,
                pipelined,
                start_len,
            } => {
                match queue.pop_front() {
                    Some(obj) => {
                        let v = self.recheck_verdict(view, obj, view.params.grace)?;
                        // Pipelined: a grace still running is no news; a
                        // later pass decides (`due` starts it when the grace
                        // ends).
                        let skip = pipelined && matches!(v, Verdict::Defer(_, Deferral::Grace));
                        if !skip {
                            *freed += self.apply_verdict(view, obj, v)?;
                        }
                        self.phase = Phase::Deleting {
                            queue,
                            pipelined,
                            start_len,
                        };
                    }
                    None => {
                        // Deletes only remove entries: an unchanged length
                        // is an unchanged list (un-condemns stored theirs).
                        if !pipelined || self.list.entries.len() != start_len {
                            self.store_list(view)?;
                        }
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

    /// A pipelined pass's deletes over `queue`, or compaction when it is
    /// empty.
    fn deleting_or_compacting(&self, queue: VecDeque<ObjectRef>) -> Phase {
        if queue.is_empty() {
            Phase::Compacting(None)
        } else {
            Phase::Deleting {
                queue,
                pipelined: true,
                start_len: self.list.entries.len(),
            }
        }
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
            return Ok(Verdict::Defer("grace not over", Deferral::Grace));
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
            Verdict::Defer(why, kind) => {
                if why == NOT_DURABLE_YET && self.cut_asked_at != Some(view.tip.commit_seq) {
                    self.cut_wanted = true;
                    self.cut_asked_at = Some(view.tip.commit_seq);
                }
                self.defer(view, obj, why, kind);
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
                self.defer(view, obj, "refused by the store", Deferral::Retry);
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
        // No compaction without an id prefix, nor at a rate of 0 (it would
        // read one step's bytes and wait for ever; M17.B stack review M-1).
        if config::compaction_refused(&view.params) || view.params.rate_bytes_per_s == 0 {
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
        let (window, over) = compact::rate_cap(
            c.window,
            view.params.rate_bytes_per_s,
            view.clock.monotonic(),
            c.started,
            c.bytes_read,
            budget.max_bytes,
        );
        c.window = Some(window);
        if over || *step_bytes >= budget.max_bytes {
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
        self.last_pass_changed = ctx.evidence.wants_retry();
        self.last_pass_busy = reclaim_busy(&ctx.summary);
        self.last_aborted = false;
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
            cut_wanted: std::mem::take(&mut self.cut_wanted),
        }
    }

    fn run_units(&mut self, view: &GcView<'_>, budget: &GcBudget, freed: &mut u64) -> Result<bool> {
        let deadline = view.clock.monotonic().saturating_add(budget.max_duration);
        let mut step_bytes = 0u64;
        self.load(view)?;
        self.observe_named(view)?;
        self.prof.step_counted = false;
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
        self.prof = PassProfile {
            started: Some(std::time::Instant::now()),
            steps: 1,
            ..PassProfile::default()
        };
        self.load(view)?;
        self.observe_named(view)?;
        self.reconcile(view)?;
        self.ctx = Some(PassCtx::default());
        self.last_compacted.clear();
        let mut m = Mark::new(self.cfg);
        let m0 = view.clock.monotonic();
        m.run(view)?;
        let mark_time = view.clock.monotonic().saturating_sub(m0);
        point!(self.points, "gc.urgent.after_mark");
        let l = m.into_liveness();
        self.after_mark(&l);
        self.last_mark_start = Some(l.started);
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
        self.finish_pass(view)?;
        self.last_mark = mark_time;
        self.log_pass("urgent");
        Ok(())
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
        // An urgent pass is a whole pass: it answers a pending request too.
        self.forced = false;
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

    fn request_pass(&mut self) -> bool {
        self.forced = true;
        true
    }

    fn status(&self) -> GcStatusSnapshot {
        Gc::status(self)
    }

    fn take_moved(&mut self) -> Vec<Moved> {
        std::mem::take(&mut self.outbox)
    }
}
