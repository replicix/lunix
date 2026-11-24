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
//! * **Recheck-only** (`GcConfig::fast_recheck`, under engine pressure, once
//!   a grace has ended): `Marking → Condemning → Deleting`, then the pass
//!   finishes without compaction. The deletes are the pipelined pass's, under
//!   the same rule (`recheck_verdict`), so the pass is only a shorter way to
//!   the point where a mark began after a grace ended. It moves
//!   `last_end` but not `last_full_end`, so regular passes (and their
//!   compaction) keep their schedule.
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
    Collector, DeadSince, GcBudget, GcPassSummary, GcProgress, GcReclaimStats, GcState, GcStats,
    GcStatusSnapshot, GcView, MarkCondemned, MarkFinished, MarkReport, Moved, TipRefs,
};
use lunix_iface::space::SpaceKind;
use lunix_iface::{Error, ErrorKind, Result};

use crate::compact::{self, Compactor};
use crate::condemned::{CondemnedEntry, CondemnedList, GraceStarts, ObjectRef, MAX_ENTRIES};
use crate::config::{self, FloorSource, GcConfig, GcHooks, NoHooks};
use crate::evidence::{Action, Deferral, Evidence};
use crate::footers::CacheBytes;
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

/// The kind of the pass in progress (`GcConfig::pipeline`, `fast_recheck`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PassKind {
    /// A request, an urgent pass, or `pipeline` off: the whole sequence.
    Whole,
    /// A scheduled pass with `pipeline`: condemn, delete, compact.
    Pipelined,
    /// Under pressure, a grace ended: condemn, delete, no compaction.
    Recheck,
}

#[derive(Debug)]
enum Phase {
    Idle,
    Marking(Box<Mark>),
    Condemning {
        todo: Vec<(ObjectRef, u64)>,
        /// The list is written.
        listed: bool,
        /// The index flags are set.
        flagged: bool,
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
    /// Segments whose file is gone and whose index follow-up (drop the
    /// entries, clear the flag) is still to do: one index update for a
    /// delete phase, flushed at its end, at the end of the step that made
    /// them (so nothing is pending while the collector is not running: a
    /// clean close leaves none) and when `RETIRE_BATCH` are queued. Until
    /// then the flag stays, so dedup never reaches them; a crash or an
    /// abandoned pass loses nothing (`reconcile` and the verdict `Gone` redo
    /// it from the list and the flags).
    retired: Vec<SegId>,
    /// Segments a recheck un-condemned: their flags are cleared, and the list
    /// (already changed in memory) stored, by the same flush.
    uncondemned: Vec<SegId>,
    /// The list differs from the stored one (an un-condemn is queued).
    list_dirty: bool,
    /// Compaction sources with an extent that does not open.
    bad_sources: BTreeSet<SegId>,
    outbox: Vec<Moved>,
    /// A pass was asked for (`request_pass`): the next idle step starts one
    /// whether or not it is due.
    forced: bool,
    phase: Phase,
    ctx: Option<PassCtx>,
    /// When the last pass of any kind ended (the duty cap and the
    /// recheck-only delay are measured from it).
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
    /// The kind of the current (or last) pass.
    kind: PassKind,
    /// The engine reports pressure ([`Collector::set_pressure`]).
    pressure: bool,
    /// When the last pass that was not recheck-only ended: the regular
    /// schedule is measured from it, so recheck-only passes cannot starve
    /// compaction.
    last_full_end: Option<Duration>,
    /// Reclaim counters ([`GcReclaimStats`]).
    reclaim: GcReclaimStats,
    /// The tip a mark pinned its roots at in this call and that has not
    /// finished, until the engine takes it ([`Collector::take_mark_report`]).
    mark_started: Option<CommitSeq>,
    /// The marks that finished in this call (their tip roots' entry counts
    /// and what they condemned), until the engine takes them.
    mark_finished: Vec<MarkFinished>,
    /// Segments condemned through [`Collector::propose_dead`] and not yet
    /// deleted, un-condemned or forgotten, with the cut they went dead in
    /// (memory only; it names a verdict a proposal got).
    proposed: BTreeMap<SegId, CommitSeq>,
    /// The condemn lines of proposals, written into the next pass's evidence
    /// object (no object per proposal: one per cut would turn the kept
    /// history over in minutes). A crash loses them: the list file still has
    /// `condemned_at`, and the delete line is written by the pass that deletes.
    carried: Evidence,
    /// The last error a proposal logged (once per distinct error).
    propose_error: Option<String>,
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
    /// As `last_mark_start`, for marks of passes that were not recheck-only:
    /// the regular grace trigger reads it, so a recheck-only pass does not
    /// consume that trigger.
    last_full_mark_start: Option<Duration>,
    /// The grace the last mark judged with: a grace that shrank since (the
    /// engine's pressure floor) makes objects deletable that the last mark
    /// deferred, whatever the time of that mark.
    last_mark_grace: Duration,
    /// The grace of the verdict being applied (lag figures).
    used_grace: Duration,
    /// The grace `params.grace` had at the last call, and since when.
    seen_grace: Option<Duration>,
    grace_changed: Duration,
    /// The lag figures of the pass in progress: sum, max and count of
    /// milliseconds, and the same total of the recheck-only passes.
    pass_lag: (u64, u64, u64),
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

/// Flags the segments among `todo` in the index, in one durable update.
fn flag_segments(view: &GcView<'_>, todo: &[(ObjectRef, u64)]) -> Result<()> {
    let segs: Vec<SegId> = todo
        .iter()
        .filter_map(|&(obj, _)| match obj {
            ObjectRef::Segment(s) => Some(s),
            _ => None,
        })
        .collect();
    if segs.is_empty() {
        return Ok(());
    }
    view.index.mark_condemned_all(&segs)
}

/// Segments whose index follow-up one update covers at most (a delete phase
/// and a step flush theirs at their end too).
pub const RETIRE_BATCH: usize = 256;

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
            retired: Vec::new(),
            uncondemned: Vec::new(),
            list_dirty: false,
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
            kind: PassKind::Whole,
            pressure: false,
            last_full_end: None,
            reclaim: GcReclaimStats::default(),
            mark_started: None,
            mark_finished: Vec::new(),
            proposed: BTreeMap::new(),
            carried: Evidence::default(),
            propose_error: None,
            last_pass_busy: false,
            last_mark: Duration::ZERO,
            last_aborted: false,
            last_mark_start: None,
            last_full_mark_start: None,
            last_mark_grace: Duration::ZERO,
            used_grace: Duration::ZERO,
            seen_grace: None,
            grace_changed: Duration::ZERO,
            pass_lag: (0, 0, 0),
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
            reclaim: Box::new(self.reclaim),
            refs: None,
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

    /// The condemn lines of proposals no pass has written yet (tests).
    #[cfg(test)]
    pub(crate) fn carried_text(&self) -> String {
        self.carried.text()
    }

    /// Segments condemned by a proposal and not yet decided (tests).
    #[cfg(test)]
    pub(crate) fn proposed(&self) -> Vec<SegId> {
        self.proposed.keys().copied().collect()
    }

    /// Units of work done so far (tests).
    pub fn units(&self) -> u64 {
        self.unit_count
    }

    /// The kind of the pass in progress, or of the last one (tests).
    #[cfg(test)]
    pub(crate) fn pass_kind(&self) -> &'static str {
        match self.kind {
            PassKind::Whole => "whole",
            PassKind::Pipelined => "pipelined",
            PassKind::Recheck => "recheck",
        }
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
        let mut unflagged = Vec::new();
        for obj in self.list.entries.keys().copied().collect::<Vec<_>>() {
            if let ObjectRef::Segment(s) = obj {
                if !flagged.contains(&s) && existing.contains_key(&s) {
                    unflagged.push(s);
                    self.grace.insert(obj, now);
                }
            }
            // An entry whose condemn was interrupted before its grace began.
            let g = self.grace.entry(obj).or_insert(now);
            if *g == Duration::MAX {
                *g = now;
            }
        }
        if !unflagged.is_empty() {
            view.index.mark_condemned_all(&unflagged)?;
        }
        let mut gone = Vec::new();
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
                    gone.push(s);
                }
            }
        }
        // Never clear a flag while entries may still point in: the update
        // drops the entries and clears the flags together.
        if !gone.is_empty() {
            view.index.retire_segments(&gone)?;
        }
        if changed {
            self.store_list(view)?;
        }
        Ok(())
    }

    /// Stores the list; what a queued un-condemn still had to store is
    /// stored with it.
    fn store_list(&mut self, view: &GcView<'_>) -> Result<()> {
        view.state.store_condemned(&self.list.to_file()?)?;
        self.list_dirty = false;
        Ok(())
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
    ///
    /// Measured against the last mark of a pass that was not recheck-only
    /// (a recheck-only pass does not consume this trigger). A grace that
    /// shrank since that mark judged (pressure) counts every object whose
    /// shorter grace ended.
    fn grace_ended_since_mark(&self, grace: Duration, now: Duration) -> bool {
        let shrunk = grace < self.last_mark_grace;
        self.grace
            .values()
            .filter(|&&t| t != Duration::MAX)
            .map(|t| t.saturating_add(grace))
            .any(|end| end <= now && (shrunk || self.last_full_mark_start.is_none_or(|m| m < end)))
    }

    /// The kind of pass to start now, if one is due.
    fn due(&mut self, view: &GcView<'_>) -> Result<Option<PassKind>> {
        if self.forced {
            return Ok(Some(PassKind::Whole));
        }
        let now = view.clock.monotonic();
        // The regular pass wins when both are due: it contains the recheck.
        if self.regular_due(view, now)? {
            return Ok(Some(if self.cfg.pipeline {
                PassKind::Pipelined
            } else {
                PassKind::Whole
            }));
        }
        Ok(self.fast_due(view, now).then_some(PassKind::Recheck))
    }

    /// A regular pass (with compaction) is due. Measured from the end of the
    /// last regular pass; a regular pass never follows any pass closer than
    /// the duty cap.
    fn regular_due(&mut self, view: &GcView<'_>, now: Duration) -> Result<bool> {
        let waited = |d: Duration| {
            self.last_full_end
                .is_none_or(|end| now >= end.saturating_add(d))
        };
        let gap = self.min_gap();
        if !self
            .last_end
            .is_none_or(|e| now >= e.saturating_add(self.last_mark.saturating_mul(DUTY_FACTOR)))
        {
            return Ok(false);
        }
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
        self.last_full_end = Some(now);
        self.last_pass_busy = false;
        Ok(false)
    }

    /// A recheck-only pass is due: under pressure (or `fast_recheck_always`),
    /// at least three times the last mark (the duty cap) has passed since the
    /// end of the last pass, and a listed object's grace ended at least the
    /// delay ago, after the last mark began (so its delete is waiting for a
    /// mark; a grace that shrank at the onset of pressure counts at once).
    /// The delay is counted from the end of the grace, not from the end of
    /// the last pass.
    fn fast_due(&self, view: &GcView<'_>, now: Duration) -> bool {
        let Some(delay) = self.cfg.fast_recheck else {
            return false;
        };
        if !self.cfg.pipeline
            || !(self.pressure || self.cfg.fast_recheck_always)
            || self.last_aborted
        {
            return false;
        }
        // The duty cap counts from the end of the last pass; the delay only
        // from the end of the grace (it batches grace ends, which a pass in
        // progress has done already). This departs from the design text and
        // the simulator, which count the delay from the end of the pass too.
        let duty = self.last_mark.saturating_mul(DUTY_FACTOR);
        if !self.last_end.is_none_or(|e| now >= e.saturating_add(duty)) {
            return false;
        }
        let grace = view.params.grace;
        let shrunk = grace < self.last_mark_grace;
        self.grace
            .values()
            .filter(|&&t| t != Duration::MAX)
            .map(|t| t.saturating_add(grace))
            .any(|end| {
                end.saturating_add(delay) <= now
                    && (shrunk || self.last_mark_start.is_none_or(|m| m < end))
            })
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
        // The queued index follow-up gets one try, so an error in a delete
        // step does not leave the segments it already unlinked flagged until
        // the next pass; what still fails is found by `reconcile` (flagged,
        // no file).
        if self.points.alive().is_ok() {
            if let Err(e) = self.flush_index(view) {
                tracing::warn!(error = %e, "gc: the index follow-up of deleted segments is left to the next pass");
            }
        }
        self.retired.clear();
        self.uncondemned.clear();
        self.phase = Phase::Idle;
        let now = view.clock.monotonic();
        self.last_end = Some(now);
        self.last_full_end = Some(now);
        // An abandoned pass may have done part of its work (a delete whose
        // call failed) or none (an error in its first mark): the next one
        // runs even if nothing else changed (re-review M-2).
        self.last_pass_changed = true;
        self.last_tip = None;
        // Retried one interval later, not at the busy pace (review m-2).
        self.last_pass_busy = false;
        self.last_aborted = true;
    }

    /// The context of a pass that starts now: the condemn lines of the
    /// proposals made since the last pass are the first lines of its evidence.
    fn new_ctx(&mut self) -> PassCtx {
        PassCtx {
            evidence: std::mem::take(&mut self.carried),
            ..PassCtx::default()
        }
    }

    fn note_heap(&mut self) {
        let mut h = self.list.entries.len() * 64
            + self.carried.heap_bytes()
            + self.proposed.len() * 48
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
                    kind = ?self.kind,
                    pass = p.pass,
                    mark_ms = PassProfile::ms(self.prof.mark_view),
                    lag_count = self.pass_lag.2,
                    lag_mean_ms = self.pass_lag.0.checked_div(self.pass_lag.2).unwrap_or(0),
                    lag_max_ms = self.pass_lag.1,
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
                let Some(kind) = self.due(view)? else {
                    return Ok(Flow::Yield);
                };
                self.kind = kind;
                self.pass_lag = (0, 0, 0);
                self.forced = false;
                self.reconcile(view)?;
                self.ctx = Some(self.new_ctx());
                self.last_compacted.clear();
                // A recheck-only pass leaves the regular triggers alone: the
                // new-tip trigger must still fire for the regular pass.
                if kind != PassKind::Recheck {
                    self.last_tip = Some(view.tip.commit_seq);
                }
                self.phase = Phase::Marking(Box::new(Mark::new(self.cfg)));
                Flow::Continue
            }
            Phase::Marking(mut m) => {
                let done = m.unit(view)?;
                let t = m.take_init_tip();
                self.note_started(t);
                if done {
                    point!(self.points, "gc.mark.end");
                    let l = m.into_liveness();
                    self.note_mark(&l);
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
                    self.note_dead(&l, &todo);
                    let started = l.started;
                    if let Some(ctx) = &mut self.ctx {
                        ctx.liveness = Some(l);
                    }
                    self.phase = if self.kind == PassKind::Whole {
                        if todo.is_empty() && self.list.entries.is_empty() {
                            Phase::Compacting(None)
                        } else {
                            Phase::Condemning {
                                todo,
                                listed: false,
                                flagged: false,
                                then: AfterCondemn::Grace,
                            }
                        }
                    } else {
                        // Pipelined: the newly dead are condemned first (the
                        // flags follow the mark as closely as they can), then
                        // this mark is the recheck of the objects listed
                        // before it whose grace ended before it began.
                        self.last_mark_start = Some(started);
                        self.last_mark_grace = view.params.grace;
                        if self.kind != PassKind::Recheck {
                            self.last_full_mark_start = Some(started);
                        }
                        let queue: VecDeque<ObjectRef> =
                            self.list.entries.keys().copied().collect();
                        if !todo.is_empty() {
                            Phase::Condemning {
                                todo,
                                listed: false,
                                flagged: false,
                                then: AfterCondemn::Delete(queue),
                            }
                        } else {
                            self.start_deletes(queue)?
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
                flagged,
                then,
            } => {
                if !listed {
                    self.list_condemned(view, &todo)?;
                    self.phase = Phase::Condemning {
                        todo,
                        listed: true,
                        flagged: false,
                        then,
                    };
                } else if !flagged {
                    // One durable index update flags every segment of the
                    // pass; the list written first is authoritative.
                    flag_segments(view, &todo)?;
                    self.phase = Phase::Condemning {
                        todo,
                        listed: true,
                        flagged: true,
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
                        AfterCondemn::Delete(queue) => self.start_deletes(queue)?,
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
                let done = m.unit(view)?;
                let t = m.take_init_tip();
                self.note_started(t);
                if done {
                    let l = m.into_liveness();
                    self.note_mark(&l);
                    self.after_mark(&l);
                    self.last_mark_start = Some(l.started);
                    self.last_full_mark_start = Some(l.started);
                    self.last_mark_grace = view.params.grace;
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
                        // A queued un-condemn makes the flush store the list
                        // (it holds every delete of the phase too).
                        let stored_by_flush = self.list_dirty;
                        self.flush_index(view)?;
                        // Deletes only remove entries: an unchanged length
                        // is an unchanged list.
                        if !stored_by_flush && (!pipelined || self.list.entries.len() != start_len)
                        {
                            self.store_list(view)?;
                        }
                        self.phase = self.after_deletes();
                    }
                }
                Flow::Continue
            }
            Phase::Compacting(c) => self.compact_unit(view, budget, c, step_bytes),
            Phase::Finishing => {
                // Nothing is queued unless an earlier abort left the list
                // unstored: no-op then.
                self.flush_index(view)?;
                self.finish_pass(view)?;
                Flow::PassDone
            }
        };
        self.note_heap();
        Ok(flow)
    }

    /// What follows the deletes: compaction, except in a recheck-only pass.
    fn after_deletes(&self) -> Phase {
        if self.kind == PassKind::Recheck {
            Phase::Finishing
        } else {
            Phase::Compacting(None)
        }
    }

    /// A pipelined or recheck-only pass's deletes over `queue`, or what
    /// follows them when it is empty.
    fn start_deletes(&self, queue: VecDeque<ObjectRef>) -> Result<Phase> {
        if self.kind == PassKind::Recheck {
            point!(self.points, "gc.recheck.before_delete");
        }
        Ok(self.deleting_or_after(queue))
    }

    fn deleting_or_after(&self, queue: VecDeque<ObjectRef>) -> Phase {
        if queue.is_empty() {
            self.after_deletes()
        } else {
            Phase::Deleting {
                queue,
                pipelined: true,
                start_len: self.list.entries.len(),
            }
        }
    }

    /// Keeps the tip root's entry counts of a finished mark for the engine
    /// (`GcConfig::tip_refs`).
    fn note_mark(&mut self, l: &Liveness) {
        if !self.cfg.tip_refs {
            return;
        }
        // Only one mark runs at a time: the one that began in this call (its
        // start not taken yet) is this one.
        let began_in_call = self.mark_started.take().is_some();
        if let Some(r) = l.roots.first() {
            self.mark_finished.push(MarkFinished {
                refs: TipRefs {
                    tip: r.seq,
                    refs: l.tip_refs.iter().map(|(&s, &n)| (s, n)).collect(),
                },
                began_in_call,
                condemned: None,
            });
        }
    }

    /// Notes the tip a mark pinned its roots at, for the engine's table.
    fn note_started(&mut self, tip: Option<CommitSeq>) {
        if self.cfg.tip_refs {
            self.mark_started = tip.or(self.mark_started);
        }
    }

    /// Keeps what a mark decided about segments for the engine's shadow check
    /// (`GcConfig::tip_refs`): the newly dead ones it condemns (with their
    /// sizes), the ones live by id that no entry of the tip hints at, the
    /// dead ones already listed, and the ones an older retained root names.
    fn note_dead(&mut self, l: &Liveness, todo: &[(ObjectRef, u64)]) {
        self.reclaim.mark_condemned_segments += todo
            .iter()
            .filter(|(o, _)| matches!(o, ObjectRef::Segment(_)))
            .count() as u64;
        if !self.cfg.tip_refs {
            return;
        }
        let segs = todo
            .iter()
            .filter_map(|(o, bytes)| match o {
                ObjectRef::Segment(s) => Some((*s, *bytes)),
                _ => None,
            })
            .collect();
        // Named by a retained root other than the tip (and not by the tip).
        let named = l
            .older_refs
            .iter()
            .filter(|s| !l.tip_refs.contains_key(*s))
            .copied()
            .collect();
        let live = l
            .seg_live
            .keys()
            .filter(|s| !l.tip_refs.contains_key(*s))
            .copied()
            .collect();
        let listed = self
            .list
            .entries
            .keys()
            .filter_map(|o| match o {
                ObjectRef::Segment(s) if l.live_bytes(*s) == 0 => Some(*s),
                _ => None,
            })
            .collect();
        let condemned = l.roots.first().map(|r| MarkCondemned {
            tip: r.seq,
            segs,
            live,
            listed,
            named,
        });
        if let Some(f) = self.mark_finished.last_mut() {
            f.condemned = condemned;
        }
    }

    fn after_mark(&mut self, l: &Liveness) {
        self.stats.live_bytes = l.total_live();
        self.stats.dead_bytes = l
            .segments
            .iter()
            .map(|s| s.size_bytes.saturating_sub(l.live_bytes(s.seg)))
            .sum();
        if self.kind != PassKind::Recheck {
            self.last_pins = Some(l.pinned.clone());
        }
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
        self.used_grace = grace;
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
                // The flag and the list change together at the end of the
                // step (`flush_index`): the flag first, then the list, so a
                // crash in between re-flags a segment the next recheck
                // un-condemns again, never deletes it.
                if let ObjectRef::Segment(s) = obj {
                    self.uncondemned.push(s);
                    if self.proposed.remove(&s).is_some() {
                        // A wrong proposal (a chunk id in the segment is live
                        // elsewhere than the entries hint, or a root the
                        // table did not see): it cost a list write and a
                        // grace, nothing else.
                        self.reclaim.proposal_uncondemned += 1;
                        tracing::info!(
                            seg = s.raw(),
                            reason = keep.reason(),
                            "gc: a proposed segment was un-condemned by the recheck"
                        );
                    }
                }
                self.list.entries.remove(&obj);
                self.grace.remove(&obj);
                self.list_dirty = true;
                if self.uncondemned.len() >= RETIRE_BATCH {
                    self.flush_index(view)?;
                }
                self.evidence(view, Action::Uncondemn, obj, 0, keep.reason());
                Ok(0)
            }
            Verdict::Delete => self.delete(view, obj),
        }
    }

    /// Queues the index follow-up of a deleted segment; a full batch is
    /// flushed at once so the window of unlinked-but-unrecorded segments
    /// stays bounded.
    fn retire(&mut self, view: &GcView<'_>, seg: SegId) -> Result<()> {
        self.retired.push(seg);
        if self.retired.len() >= RETIRE_BATCH {
            self.flush_index(view)?;
        }
        Ok(())
    }

    /// The queued index work, in one durable update each: the follow-up of
    /// every segment deleted since the last call (entries dropped and flags
    /// cleared together), then the flags of the segments un-condemned, then
    /// the list when an un-condemn changed it. On an error what was not done
    /// stays queued.
    fn flush_index(&mut self, view: &GcView<'_>) -> Result<()> {
        if !self.retired.is_empty() {
            view.index.retire_segments(&self.retired)?;
            self.retired.clear();
        }
        if !self.uncondemned.is_empty() {
            view.index.unmark_condemned_all(&self.uncondemned)?;
            self.uncondemned.clear();
        }
        if self.list_dirty {
            self.store_list(view)?;
        }
        Ok(())
    }

    /// Removes a listed object that no longer exists. A segment's index
    /// entries are dropped before its flag is cleared, and an error leaves
    /// both for the next pass. Bytes of an earlier failed delete call are
    /// released now.
    fn forget(&mut self, view: &GcView<'_>, obj: ObjectRef) -> Result<u64> {
        if let ObjectRef::Segment(s) = obj {
            self.retire(view, s)?;
            self.proposed.remove(&s);
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

    /// Records how long `obj` waited after its grace ended until its delete:
    /// from the end of the grace the verdict used, but not before the grace
    /// last changed (a shrunk grace makes the backlog deletable at once; that
    /// wait is not recheck lag).
    fn note_lag(&mut self, view: &GcView<'_>, obj: ObjectRef) {
        let Some(&start) = self.grace.get(&obj).filter(|&&t| t != Duration::MAX) else {
            return;
        };
        let from = start
            .saturating_add(self.used_grace)
            .max(self.grace_changed);
        let lag = view.clock.monotonic().saturating_sub(from);
        let ms = u64::try_from(lag.as_millis()).unwrap_or(u64::MAX);
        let r = &mut self.reclaim;
        r.recheck_lag_ms_sum = r.recheck_lag_ms_sum.saturating_add(ms);
        r.recheck_lag_ms_max = r.recheck_lag_ms_max.max(ms);
        r.recheck_lag_count += 1;
        if self.kind == PassKind::Recheck {
            r.fast_lag_ms_sum = r.fast_lag_ms_sum.saturating_add(ms);
            r.fast_lag_ms_max = r.fast_lag_ms_max.max(ms);
            r.fast_lag_count += 1;
        }
        let p = &mut self.pass_lag;
        p.0 = p.0.saturating_add(ms);
        p.1 = p.1.max(ms);
        p.2 += 1;
    }

    /// Notes when `params.grace` changed (the lag figures start there).
    fn note_grace(&mut self, view: &GcView<'_>) {
        if self.seen_grace != Some(view.params.grace) {
            if let Some(old) = self.seen_grace {
                self.grace_changed = view.clock.monotonic();
                // Not coalesced: the pressure onset the gate's timings read
                // from comes with the grace the engine switches to.
                tracing::info!(
                    from_ms = u64::try_from(old.as_millis()).unwrap_or(u64::MAX),
                    to_ms = u64::try_from(view.params.grace.as_millis()).unwrap_or(u64::MAX),
                    "gc: the grace in force changed"
                );
            }
            self.seen_grace = Some(view.params.grace);
        }
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
        self.note_lag(view, obj);
        if !matches!(obj, ObjectRef::Commit(_)) {
            point!(self.points, "gc.delete.after_unlink");
        }
        if let ObjectRef::Segment(s) = obj {
            // The flag stays until the entries are gone (B1): both follow in
            // one update when the phase ends.
            self.retire(view, s)?;
            if self.proposed.remove(&s).is_some() {
                self.reclaim.proposal_deleted += 1;
                self.reclaim.proposal_deleted_bytes =
                    self.reclaim.proposal_deleted_bytes.saturating_add(n);
            }
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
                        (!s.is_empty()).then(|| {
                            Compactor::new(
                                s,
                                view.clock.monotonic(),
                                COMPACT_QUEUE,
                                CacheBytes::new(self.cfg.footer_cache_bytes),
                            )
                        })
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
        // Nothing is queued when a pass ends: the delete phase (or the urgent
        // pass) flushed, and a recheck-only pass ends right after its deletes.
        debug_assert!(
            self.retired.is_empty() && self.uncondemned.is_empty() && !self.list_dirty,
            "index work is still queued at the end of a pass"
        );
        let now = view.clock.monotonic();
        ctx.summary.pass = pass;
        if self.kind == PassKind::Recheck {
            // The regular triggers (a new tip, changes, the pins, the full
            // end) are left to the regular pass; only a busy pass hurries it.
            self.last_pass_busy |= reclaim_busy(&ctx.summary);
            let r = &mut self.reclaim;
            r.fast_passes += 1;
            r.fast_segments_deleted += ctx.summary.segments_deleted;
            r.fast_bytes_freed += ctx.summary.bytes_freed;
        } else {
            self.last_pass_changed = ctx.evidence.wants_retry();
            self.last_pass_busy = reclaim_busy(&ctx.summary);
            self.last_full_end = Some(now);
            self.reclaim.regular_passes += 1;
        }
        self.last_aborted = false;
        self.stats.passes += 1;
        self.add_totals(&ctx.summary);
        self.last_pass = Some(ctx.summary);
        self.last_finished_ms = Some(view.clock.now().0);
        self.last_end = Some(now);
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
        self.note_grace(view);
        self.prof.step_counted = false;
        loop {
            match self.unit(view, budget, freed, &mut step_bytes)? {
                Flow::Continue => {}
                Flow::Yield => return self.end_step(view, false),
                Flow::PassDone => return Ok(true),
            }
            if view.clock.monotonic() >= deadline {
                return self.end_step(view, false);
            }
        }
    }

    /// Ends a step that leaves the pass in progress: nothing stays queued
    /// between steps (a clean close then leaves none). Not after a stop at a
    /// crash point (a killed process does no more). A finished pass has
    /// flushed already (the delete phase does), and an error ends the pass
    /// (`abort_pass` tries once more).
    fn end_step(&mut self, view: &GcView<'_>, done: bool) -> Result<bool> {
        if self.points.alive().is_ok() {
            self.flush_index(view)?;
        }
        Ok(done)
    }

    // ---------------------------------------------------------- proposals

    /// The work of [`Collector::propose_dead`]: filters the offer by the
    /// collector's own rules, then lists, flags and starts the grace of what
    /// is left, in that order, through the functions a pass uses
    /// (`list_condemned`, `flag_segments`). The condemn lines are carried to
    /// the next pass's evidence object.
    ///
    /// Safety: an offer is a hint. Nothing here deletes; the segment is
    /// listed and flagged like one a mark found dead, and only a later mark
    /// that began after its grace ended (`recheck_verdict`, unchanged) can
    /// delete it, whatever the offer was.
    fn propose_inner(&mut self, view: &GcView<'_>, dead: &[DeadSince]) -> Result<()> {
        self.precheck(view)?;
        let offered: BTreeMap<SegId, CommitSeq> = dead.iter().map(|d| (d.seg, d.since)).collect();
        self.reclaim.proposals_offered += offered.len() as u64;
        // A pin names a root the engine's table does not know: no proposal
        // is taken while one exists.
        let pins = view.pins.roots()?.len();
        if pins > 0 {
            self.reclaim.proposal_rejected_pins += offered.len() as u64;
            return Ok(());
        }
        let sizes: BTreeMap<SegId, u64> = view
            .segments
            .list()?
            .into_iter()
            .map(|s| (s.seg, s.size_bytes))
            .collect();
        // What the rules below read: not the whole of `Facts` (the rollback
        // floor and the index manifest are for deletes, and a proposal does
        // not delete).
        let (named, _) = self.retained_names(view)?;
        let (hwm, keep_floor) = (view.tip.high_water_mark, crate::mark::keep_floor(view));
        let room = (MAX_ENTRIES as usize)
            .saturating_sub(self.list.entries.len())
            .min(self.cfg.propose_max);
        let (mut early, mut kept, mut full) = (0u64, 0u64, 0u64);
        let mut todo: Vec<(ObjectRef, u64)> = Vec::new();
        let mut since: Vec<(SegId, CommitSeq)> = Vec::new();
        for (&seg, &k) in &offered {
            let obj = ObjectRef::Segment(seg);
            let Some(&bytes) = sizes.get(&seg) else {
                kept += 1;
                continue;
            };
            // Every root of a mark is a FULL at or above `keep_floor`: one
            // of them can still name a segment whose count reached zero in a
            // later cut only if `k` is above it.
            if k > keep_floor {
                early += 1;
                continue;
            }
            // The mark's own keeps (`Judge::container_kept`): above the
            // high-water mark, named by a retained commit; and an unnamed
            // compaction output, an object already listed.
            if seg.raw() > hwm
                || named.contains(&seg.raw())
                || self.outputs.contains(&seg)
                || self.list.entries.contains_key(&obj)
            {
                kept += 1;
                continue;
            }
            if todo.len() >= room {
                full += 1;
                continue;
            }
            todo.push((obj, bytes));
            since.push((seg, k));
        }
        let r = &mut self.reclaim;
        r.proposal_rejected_early += early;
        r.proposal_rejected_kept += kept;
        r.proposal_rejected_room += full;
        if todo.is_empty() {
            return Ok(());
        }
        point!(self.points, "gc.propose.before_list_write");
        self.list_condemned(view, &todo)?;
        flag_segments(view, &todo)?;
        point!(self.points, "gc.propose.after_flags");
        let now = view.clock.monotonic();
        let wall = view.clock.now();
        let mut bytes_sum = 0u64;
        for &(obj, bytes) in &todo {
            self.grace.insert(obj, now);
            self.carried
                .add(wall, Action::Condemn, obj, bytes, "unreferenced");
            bytes_sum = bytes_sum.saturating_add(bytes);
        }
        self.proposed.extend(since);
        self.reclaim.proposal_condemned += todo.len() as u64;
        self.reclaim.proposal_condemned_bytes = self
            .reclaim
            .proposal_condemned_bytes
            .saturating_add(bytes_sum);
        Ok(())
    }

    // ------------------------------------------------------------ urgent

    fn urgent_inner(&mut self, view: &GcView<'_>, want: u64, freed: &mut u64) -> Result<()> {
        self.prof = PassProfile {
            started: Some(std::time::Instant::now()),
            steps: 1,
            ..PassProfile::default()
        };
        self.kind = PassKind::Whole;
        self.pass_lag = (0, 0, 0);
        self.load(view)?;
        self.note_grace(view);
        self.observe_named(view)?;
        self.reconcile(view)?;
        self.ctx = Some(self.new_ctx());
        self.last_compacted.clear();
        let mut m = Mark::new(self.cfg);
        let m0 = view.clock.monotonic();
        m.run(view)?;
        let t = m.take_init_tip();
        self.note_started(t);
        let mark_time = view.clock.monotonic().saturating_sub(m0);
        point!(self.points, "gc.urgent.after_mark");
        let l = m.into_liveness();
        self.note_mark(&l);
        self.after_mark(&l);
        self.last_mark_start = Some(l.started);
        self.last_full_mark_start = Some(l.started);
        self.last_mark_grace = config::grace_floor(&view.params);
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
        self.note_dead(&l, &todo);
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
        self.flush_index(view)?;
        // New dead objects start their (floor) grace now.
        if !todo.is_empty() {
            self.list_condemned(view, &todo)?;
            flag_segments(view, &todo)?;
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
                (!s.is_empty()).then(|| {
                    Compactor::new(
                        s,
                        view.clock.monotonic(),
                        COMPACT_QUEUE,
                        CacheBytes::new(self.cfg.footer_cache_bytes),
                    )
                })
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

    fn set_pressure(&mut self, raised: bool) {
        if raised != self.pressure {
            // Not coalesced (the engine's throttle lines are): the gate's
            // onset times read from it.
            tracing::info!(raised, "gc: the engine's pressure level changed");
        }
        self.pressure = raised;
    }

    fn set_tip_refs(&mut self, on: bool) {
        self.cfg.tip_refs = on;
    }

    fn take_mark_report(&mut self) -> Option<MarkReport> {
        if self.mark_started.is_none() && self.mark_finished.is_empty() {
            return None;
        }
        Some(MarkReport {
            started: self.mark_started.take(),
            finished: std::mem::take(&mut self.mark_finished),
        })
    }

    fn propose_dead(&mut self, view: &GcView<'_>, dead: &[DeadSince]) -> bool {
        // Between passes only: a proposal is never mixed into a running pass
        // (its deletes compare list lengths, its `dead()` was computed from
        // an older list), and the list must be loaded.
        if self.cfg.propose_max == 0 {
            return true;
        }
        if !matches!(self.phase, Phase::Idle)
            || self.ctx.is_some()
            || !self.loaded
            || self.points.alive().is_err()
        {
            self.reclaim.proposals_deferred += 1;
            return false;
        }
        let res = self.propose_inner(view, dead);
        match res {
            Ok(()) => true,
            Err(e) => {
                // Stopped at a crash point: the engine offers again.
                if self.points.alive().is_err() {
                    return false;
                }
                // Whatever was listed and not flagged is flagged by the next
                // pass's `reconcile`; the marks find the rest.
                self.reclaim.propose_errors += 1;
                let msg = e.to_string();
                if self.propose_error.as_deref() != Some(msg.as_str()) {
                    tracing::warn!(error = %msg, "gc: a proposal failed; the marks find those segments");
                    self.propose_error = Some(msg);
                }
                true
            }
        }
    }
}
