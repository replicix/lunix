//! The garbage-collection hooks (M17, DESIGN section 11).
//!
//! # Who calls what
//!
//! The engine's cut/commit coordinator is the **only** caller of
//! [`Collector::step`], and calls it only between cuts and only when the
//! in-memory map root equals the tip's `map_root` (no dirty leaves). M11
//! enforces it, M17 relies on it and asserts it through the view. A `step` is
//! budgeted and resumable; it never stalls a cut for longer than
//! [`GcBudget::max_duration`].
//!
//! The space ladder's *compact* step and the control socket's `Gc` request talk
//! to the collector through [`GcControl`], whose calls run the collector on the
//! calling thread under the coordinator's mutex (never during a cut, the map
//! at the tip), so the single-caller rule holds.

use std::time::Duration;

use lunix_format::commit::{Commit, MapRoot};
use lunix_format::{ChunkId, CommitSeq, Hint, IdLen, SegId};
use serde::{Deserialize, Serialize};

use crate::clock::Clock;
use crate::crypto::{CommitCodec, ExtentCodec};
use crate::error::Result;
use crate::index::ChunkIndex;
use crate::map::BlockMap;
use crate::space::SpaceGate;
use crate::store::{CommitStore, GcStateStore, PackStore, PinSource, SegmentStore};

/// A compaction move: the chunk `id` now lives at `new`; the map entry that
/// still has hint `old` is to be re-hinted at the next cut (C3), via
/// `MapTxn::rehint`. The index was already told with `relocate`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Moved {
    /// The chunk.
    pub id: ChunkId,
    /// Where it was.
    pub old: Hint,
    /// Where it is.
    pub new: Hint,
}

/// The settings a pass needs, from `volume.json` and the descriptor.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct GcParams {
    /// FULL commits kept (`last:N`), default 2.
    pub retain_last: u32,
    /// Grace between condemn and delete (floor 30 s).
    pub grace: Duration,
    /// Read timeout; the grace floor is 10 s plus twice this.
    pub read_timeout: Duration,
    /// Compaction rate cap in bytes per second.
    pub rate_bytes_per_s: u64,
    /// Compact segments whose live fraction is below this (default 0.5).
    pub live_threshold: f64,
    /// The volume chunk size.
    pub chunk_size: u32,
    /// The map id-prefix length; `Zero` refuses compaction (`Unsupported`).
    pub id_len: IdLen,
}

/// What one `step` may spend.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct GcBudget {
    /// Wall-clock budget on the view's clock (default 50 ms).
    pub max_duration: Duration,
    /// Compaction read+write bytes allowed in this step.
    pub max_bytes: u64,
}

/// The tip, as the engine verified it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TipInfo {
    /// Its `commit_seq`.
    pub commit_seq: CommitSeq,
    /// Its map root.
    pub map_root: MapRoot,
    /// Its `chain_floor`.
    pub chain_floor: CommitSeq,
    /// Raw value of the highest object id any retained commit names. GC never
    /// condemns an object above it.
    pub high_water_mark: u64,
}

/// Everything a step reads and writes, bundled by the engine for one call.
#[derive(Clone, Copy)]
pub struct GcView<'a> {
    /// The retained commits, authenticated (FULLs and any DELTAs of the tip).
    pub retained: &'a [Commit],
    /// The tip's numbers.
    pub tip: TipInfo,
    /// Snapshot, reader and hold pins.
    pub pins: &'a dyn PinSource,
    /// The map (read-only traversal of any root, plus `rehint` via the engine).
    pub map: &'a dyn BlockMap,
    /// The chunk index.
    pub index: &'a dyn ChunkIndex,
    /// Segments.
    pub segments: &'a dyn SegmentStore,
    /// Map packs.
    pub packs: &'a dyn PackStore,
    /// Commit objects.
    pub commits: &'a dyn CommitStore,
    /// The commit codec (authentication of commits).
    pub commit_codec: &'a dyn CommitCodec,
    /// The extent codec (compaction re-seals with a fresh salt).
    pub extents: &'a dyn ExtentCodec,
    /// The condemned list and evidence objects.
    pub state: &'a dyn GcStateStore,
    /// The ledger (`release` after durable deletes, headroom for output).
    pub space: &'a dyn SpaceGate,
    /// Time.
    pub clock: &'a dyn Clock,
    /// Settings.
    pub params: GcParams,
}

/// Where a pass is.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GcState {
    /// No pass in progress.
    Idle,
    /// Walking the roots.
    Marking,
    /// Choosing and condemning dead objects.
    Condemning,
    /// Waiting out the grace period.
    Grace,
    /// Re-marking before deleting.
    Rechecking,
    /// Deleting condemned objects.
    Deleting,
    /// Compacting low-live segments.
    Compacting,
}

/// The outcome of one budgeted step.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct GcProgress {
    /// Moves for the next cut to apply with `MapTxn::rehint`. Non-empty means a
    /// cut is wanted even on an idle volume (the output segment is named only
    /// by that cut).
    pub moved: Vec<Moved>,
    /// Bytes deleted in this step.
    pub freed: u64,
    /// Where the pass is now.
    pub state: GcState,
    /// True when the pass is finished (the evidence object was written).
    pub done: bool,
    /// The collector deferred a delete until a FULL **below the tip** names
    /// the live copies (a compaction output only the tip names): the engine
    /// cuts one FULL even on an idle volume, so the delete goes in the next
    /// pass instead of being deferred at every pass.
    pub cut_wanted: bool,
}

/// Counters across passes.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct GcStats {
    /// Passes completed.
    pub passes: u64,
    /// Bytes in live extents at the last mark.
    pub live_bytes: u64,
    /// Bytes in dead extents of surviving segments at the last mark.
    pub dead_bytes: u64,
    /// Bytes deleted in total.
    pub bytes_freed: u64,
    /// Segments compacted in total.
    pub segments_compacted: u64,
    /// Compaction bytes read.
    pub compaction_read: u64,
    /// Compaction bytes written (write amplification = written / freed).
    pub compaction_written: u64,
}

/// What a pass did or, for a dry run, would do. Also the `Gc` response body.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct GcPassSummary {
    /// The pass number (`gc/<pass:016x>`); 0 for a dry run.
    pub pass: u64,
    /// True when nothing was changed.
    pub dry_run: bool,
    /// Segments condemned.
    pub segments_condemned: u64,
    /// Segments deleted.
    pub segments_deleted: u64,
    /// Map packs deleted.
    pub packs_deleted: u64,
    /// Index runs deleted.
    pub runs_deleted: u64,
    /// Commit objects below `chain_floor` deleted.
    pub commits_deleted: u64,
    /// Segments compacted.
    pub segments_compacted: u64,
    /// Bytes freed (dry run: bytes that would be).
    pub bytes_freed: u64,
    /// Bytes compaction rewrote (dry run: would rewrite).
    pub bytes_moved: u64,
}

/// The collector, driven by the coordinator.
pub trait Collector: Send {
    /// Does one budgeted slice of work of the current pass, starting a pass when
    /// idle and one is due. Coordinator thread only; blocking within `budget`.
    ///
    /// # Errors
    /// `Io`/`Corrupt`/`Auth` from the stores; a failed step leaves the pass
    /// resumable (every state between crash points is recoverable).
    fn step(&mut self, view: &GcView<'_>, budget: GcBudget) -> Result<GcProgress>;

    /// An urgent pass for the space ladder: floor grace, recheck unchanged,
    /// then compaction if still short. Runs to completion within `want_bytes`
    /// or exhaustion; coordinator thread only.
    fn urgent(&mut self, view: &GcView<'_>, want_bytes: u64) -> Result<GcProgress>;

    /// What a pass would condemn and compact, changing nothing
    /// (`lunix gc --dry-run`).
    fn dry_run(&mut self, view: &GcView<'_>) -> Result<GcPassSummary>;

    /// Counters.
    fn stats(&self) -> GcStats;

    /// Makes the next `step` start a pass even when none is due (the control
    /// socket's `Gc`: a pass now, not after the pass interval). A pass in
    /// progress is not affected; an urgent pass clears the request (it is a
    /// whole pass). Returns false when the collector does not take requests
    /// (the default): `GcControl::run_pass` then fails `Unsupported`.
    fn request_pass(&mut self) -> bool {
        false
    }

    /// The collector's state for `GcStatus`. The default carries the counters
    /// only.
    fn status(&self) -> GcStatusSnapshot {
        GcStatusSnapshot {
            stats: self.stats(),
            ..GcStatusSnapshot::default()
        }
    }

    /// Moves a failed `step` or `urgent` sealed before it failed (the step's
    /// error carries none): the engine applies them at the next cut like any
    /// other, so the output is named by that cut.
    fn take_moved(&mut self) -> Vec<Moved> {
        Vec::new()
    }

    /// The engine's pressure level is `Raised` or `Urgent` (ADR-32; the
    /// space gate paces or holds writes): the collector may run recheck-only
    /// passes. Called before every call into the collector (steps, requested
    /// and urgent passes, dry runs), with the level of the last tick. The
    /// default ignores it.
    fn set_pressure(&mut self, raised: bool) {
        let _ = raised;
    }

    /// The engine keeps a reference table (`gc_condemn_on_cut`): count the
    /// entries of the tip root per segment in every mark and report what the
    /// marks pin and condemn (`take_mark_report`). Called once when the
    /// engine opens. The default ignores it.
    fn set_tip_refs(&mut self, on: bool) {
        let _ = on;
    }

    /// What the marks did in the last call, once: the mark that began in it
    /// and is still running, and the mark that finished in it. The engine's
    /// reference table snapshots its counts at a mark's start, compares them
    /// with the counts of the finished mark and judges what it condemned. The
    /// default reports nothing.
    fn take_mark_report(&mut self) -> Option<MarkReport> {
        None
    }

    /// Offers segments the engine's reference table found dead (no tip entry
    /// has named them since the cut `since`, and the lag rule holds) for
    /// condemnation without a mark (`gc_condemn_on_cut = on`). An offer is a
    /// hint with no authority: the collector checks it against its own rules
    /// (no pin, nothing a retained FULL names, nothing above the high-water
    /// mark, no compaction output), lists, flags and starts the grace of what
    /// it accepts, and the recheck of a later mark is the only judge of a
    /// delete, exactly as for a segment its own mark condemned. Called under
    /// the coordinator mutex with the map at the tip, right before a step.
    ///
    /// Returns true when the offer was consumed (every segment condemned or
    /// rejected for good: the engine forgets it), false when the collector
    /// cannot take it now (a pass is running, nothing is loaded yet, or the
    /// crash hook stopped it): the engine offers again later. The default
    /// takes nothing and consumes everything.
    fn propose_dead(&mut self, view: &GcView<'_>, dead: &[DeadSince]) -> bool {
        let _ = (view, dead);
        true
    }
}

/// A segment no tip map entry has pointed into since the cut that committed
/// as `since` (the engine's reference table).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DeadSince {
    /// The segment.
    pub seg: SegId,
    /// The commit sequence of the cut in which its entry count reached zero.
    pub since: CommitSeq,
}

/// What the marks of one collector call did, for the engine's reference table
/// ([`Collector::take_mark_report`]).
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct MarkReport {
    /// The tip a mark pinned its roots at in this call, when that mark is
    /// still running at the end of the call.
    pub started: Option<CommitSeq>,
    /// The marks that finished in this call, in the order they finished
    /// (several when the pass schedule starts one right after another inside
    /// a call).
    pub finished: Vec<MarkFinished>,
}

/// A mark that finished.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct MarkFinished {
    /// The entry counts of its tip root.
    pub refs: TipRefs,
    /// The mark pinned its roots in the same call (the engine takes its
    /// snapshot of the counts now: the map cannot change inside a call).
    /// Otherwise the snapshot of its start is already taken.
    pub began_in_call: bool,
    /// What it chose to condemn (`None` for a recheck mark that condemns
    /// nothing).
    pub condemned: Option<MarkCondemned>,
}

/// Map entries per segment of one tip root, counted by hint (the segment a
/// map entry names, not where the chunk's id currently lives).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TipRefs {
    /// The commit sequence of the root that was counted (the mark's first
    /// root, the tip).
    pub tip: CommitSeq,
    /// Segments with at least one entry and their counts, ascending by
    /// segment.
    pub refs: Vec<(SegId, u64)>,
}

/// What one mark decided about segments, for the engine's shadow check.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct MarkCondemned {
    /// The tip the mark walked (as [`TipRefs::tip`]).
    pub tip: CommitSeq,
    /// The newly dead segments the mark condemns (at most what the list has
    /// room for), ascending, with the size of each in bytes.
    pub segs: Vec<(SegId, u64)>,
    /// Segments with no map entry hinting at them in the tip root that the
    /// mark nevertheless found live by chunk id (the index's copy of an id
    /// is there): what hint counts cannot see. Includes the ones in
    /// `named`.
    pub live: Vec<SegId>,
    /// Segments the mark found dead that are on the condemned list already.
    pub listed: Vec<SegId>,
    /// Segments a retained root other than the tip names by hint (a retained
    /// FULL or a pin), and the tip does not. An offered segment in it broke
    /// the lag rule.
    pub named: Vec<SegId>,
}

/// The state of the engine's reference table (`gc_condemn_on_cut`).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefsState {
    /// The mode is `off`: no table.
    #[default]
    Off,
    /// Recording deltas; no mark has given it a base yet.
    Rebuilding,
    /// Rebased on a mark and consistent with it.
    Ready,
    /// A map transaction did not do what the table predicted (or could not
    /// be predicted); it offers nothing until a mark that began after the
    /// failure repairs it.
    Drifted,
}

/// The engine's reference table and its shadow check (since the process
/// started). Memory only; nothing here is persisted.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct RefsStats {
    /// Where the table is.
    pub state: RefsState,
    /// Segments with at least one map entry.
    pub segments: u64,
    /// Segments with no entry since a cut that have not been offered yet:
    /// the lag (`2 x (retain_last - 1)` cuts) is not over, or they were left
    /// by the 64-per-step cap, a pending re-hint, or a table that is not
    /// `Ready`.
    pub zero_waiting: u64,
    /// Segments offered at the last collector step (at most 64; shadow:
    /// counted, never condemned).
    pub ready: u64,
    /// Segments that differed between the table and a mark, summed over the
    /// marks (0 when the table is right).
    pub drift_segments: u64,
    /// Marks that rebased the table.
    pub rebases: u64,
    /// Map transactions whose report differed from the prediction.
    pub report_mismatches: u64,
    /// Segments the shadow check would have offered for condemnation (an
    /// upper bound of what the active stage proposes: it only takes offers
    /// that meet an idle collector).
    pub shadow_would_propose: u64,
    /// Offers made while the collector was between passes.
    pub shadow_offered_idle: u64,
    /// Offers made while a pass was running.
    pub shadow_offered_busy: u64,
    /// Offered segments a later mark (that began at or after the offer)
    /// found dead as well.
    pub shadow_confirmed: u64,
    /// Of those, the ones whose offer met an idle collector before the mark:
    /// the offer would have started the grace earlier (`shadow_lead_ms_*`).
    pub shadow_confirmed_lead: u64,
    /// Sum, over those, of the time between the offer meeting an idle
    /// collector and the mark's condemn, milliseconds: how much earlier the
    /// grace would have started.
    pub shadow_lead_ms_sum: u64,
    /// The largest such lead.
    pub shadow_lead_ms_max: u64,
    /// Confirmed offers that never met an idle collector before the mark
    /// condemned the segment itself: no gain.
    pub shadow_confirmed_no_lead: u64,
    /// Offers such a mark did not condemn because a chunk id in the segment
    /// is live elsewhere than the entries hint: the only class that becomes a
    /// wrong proposal (the recheck stops it). Not counted here when a
    /// retained root names the segment (`shadow_unconfirmed_named`).
    pub shadow_unconfirmed_live: u64,
    /// Offers such a mark did not condemn that fit no other class: the
    /// segment is above the high-water mark, named by a pending re-hint, an
    /// unnamed compaction output, or gone from the listing. A segment a
    /// retained root names is never here (it is counted as named), and
    /// neither is one live by chunk id (`shadow_unconfirmed_live`).
    pub shadow_unconfirmed_kept: u64,
    /// Offers such a mark found still named by a retained root other than
    /// the tip (an older FULL or a pin): the lag rule was broken. Always 0 on a
    /// volume without pins and with a pin that is more than a second old (the
    /// engine withholds offers while a pin exists, from a cache of one second:
    /// in `shadow` an offer in the second after a pin appeared is recorded and
    /// can raise it); anything else is a bug in the table and blocks the active
    /// stage.
    pub shadow_unconfirmed_named: u64,
    /// Offers such a mark did not condemn because the segment was on the
    /// list already.
    pub shadow_unconfirmed_listed: u64,
    /// Offered segments a new map entry pointed into again before a mark
    /// judged them (condemn-on-cut would have flagged them first).
    pub shadow_revived: u64,
    /// Segments a mark condemned while the table knew them as zero but had
    /// not offered them yet (the lag was not over, the 64-per-step cap, a
    /// pending re-hint, or the mark ran in a call without an offer).
    pub shadow_early_by_mark: u64,
    /// Segments the marks condemned that the table did not know (never had
    /// an entry, died by compaction with stale hints, ...).
    pub shadow_missed: u64,
    /// The sizes in bytes of the segments `shadow_confirmed` counts.
    pub shadow_confirmed_bytes: u64,
    /// The sizes in bytes of the segments `shadow_early_by_mark` counts.
    pub shadow_early_bytes: u64,
    /// The sizes in bytes of the segments `shadow_missed` counts.
    pub shadow_missed_bytes: u64,
    /// Over the confirmed offers that had a lead: the segment's size in bytes
    /// times its lead in milliseconds, summed (saturating): the space-time
    /// the grace would have started earlier for.
    pub shadow_lead_byte_ms: u64,
    /// Milliseconds from the table's creation to the first mark that
    /// condemned a segment, if one has.
    pub first_mark_condemn_ms: Option<u64>,
    /// Sum of the time between a segment's count reaching zero and its offer,
    /// milliseconds.
    pub condemn_lag_ms_sum: u64,
    /// The largest such time.
    pub condemn_lag_ms_max: u64,
    /// Offers the lag figures cover.
    pub condemn_lag_count: u64,
    /// For each segment offered while a pass was running, the time between
    /// its offer and the first moment the collector was idle (a proposal is
    /// only taken then): the sum, milliseconds.
    pub idle_wait_ms_sum: u64,
    /// The largest such wait.
    pub idle_wait_ms_max: u64,
    /// Offers that waited and met an idle collector (the figures' count).
    pub idle_wait_count: u64,
    /// Offers made while a pass ran that no idle collector has met yet (and
    /// no mark has judged, no new entry revived).
    pub idle_waiting: u64,
    /// Milliseconds from the table's creation (attach) to the first call
    /// that found a segment ready, if one has.
    pub first_ready_ms: Option<u64>,
    /// Milliseconds from the table's creation to the first segment a
    /// proposal condemned (mode `on`), if one has.
    pub first_proposal_ms: Option<u64>,
    /// Milliseconds from the table's creation to the first delete of a
    /// segment a proposal condemned, if there has been one.
    pub first_proposal_delete_ms: Option<u64>,
    /// Offers the collector consumed (mode `on`): each segment was condemned
    /// or rejected (`GcReclaimStats::proposal_*` say which).
    pub proposals_taken: u64,
    /// Segments left unoffered because a pin exists (the table knows the tip
    /// only): counted once per call that had segments ready.
    pub pin_blocked: u64,
}

/// A snapshot of the collector for `GcStatus`.
#[derive(Clone, PartialEq, Debug, Default, Serialize, Deserialize)]
pub struct GcStatusSnapshot {
    /// Where the current pass is (`None` is idle).
    pub state: Option<GcState>,
    /// Objects waiting out the grace period.
    pub condemned_pending: u64,
    /// Segments with pending `moved` records.
    pub moved_pending: u64,
    /// The last completed pass.
    pub last_pass: Option<GcPassSummary>,
    /// Counters.
    pub stats: GcStats,
    /// When the last pass finished, milliseconds since the epoch.
    pub last_finished_ms: Option<u64>,
    /// Control calls waiting for a pass on request (`GcControl::run_pass`);
    /// set by the engine wiring, 0 from a collector.
    pub requests: u64,
    /// Reclaim latency and pass-kind counters of this process.
    pub reclaim: Box<GcReclaimStats>,
    /// The engine's reference table (set by the engine wiring; `None` from a
    /// collector and when the mode is off).
    pub refs: Option<Box<RefsStats>>,
}

/// How fast reclaim is and which passes did it (since the process started).
/// The recheck lag is the time an object waited after its grace ended until
/// it was deleted: the figure the recheck-only pass shortens.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct GcReclaimStats {
    /// Recheck-only passes completed (`GcConfig::fast_recheck`).
    pub fast_passes: u64,
    /// Passes completed that were not recheck-only (whole, pipelined,
    /// urgent): all passes minus `fast_passes`.
    pub regular_passes: u64,
    /// Segments recheck-only passes deleted.
    pub fast_segments_deleted: u64,
    /// Bytes recheck-only passes freed.
    pub fast_bytes_freed: u64,
    /// Sum, over the objects deleted by any pass kind, of the time between
    /// the end of their grace (the grace their verdict used, and not before
    /// the grace last changed) and their delete, milliseconds.
    pub recheck_lag_ms_sum: u64,
    /// The largest such time, milliseconds.
    pub recheck_lag_ms_max: u64,
    /// Objects the lag figures cover.
    pub recheck_lag_count: u64,
    /// Segments an engine proposal offered ([`Collector::propose_dead`]).
    pub proposals_offered: u64,
    /// Offered segments that were condemned (listed, flagged, grace started).
    pub proposal_condemned: u64,
    /// The sizes in bytes of those.
    pub proposal_condemned_bytes: u64,
    /// Offered segments rejected because a pin exists.
    pub proposal_rejected_pins: u64,
    /// Offered segments rejected because a retained FULL below the lag may
    /// still name them (`since` above the lowest `chain_floor`). The engine's
    /// lag and the collector's rule agree: expected 0; non-zero is a bug.
    pub proposal_rejected_early: u64,
    /// Offered segments rejected for another rule of the collector: named
    /// by a retained commit, above the high-water mark, an unnamed
    /// compaction output, already listed, or gone.
    pub proposal_rejected_kept: u64,
    /// Offered segments left for a mark because the list or the per-call cap
    /// had no room.
    pub proposal_rejected_room: u64,
    /// Calls that failed after they began (the segments were left to the
    /// marks).
    pub propose_errors: u64,
    /// Calls the collector refused because a pass was running or it was not
    /// ready: the engine offered the segments again later. A count of calls
    /// (one per step of a busy collector), not of segments.
    pub proposals_deferred: u64,
    /// Proposed segments a recheck un-condemned (a wrong proposal; it only
    /// costs a list write and a grace).
    pub proposal_uncondemned: u64,
    /// Proposed segments deleted.
    pub proposal_deleted: u64,
    /// The sizes in bytes of the deleted ones.
    pub proposal_deleted_bytes: u64,
    /// Segments the marks themselves condemned (what no proposal had
    /// condemned first).
    pub mark_condemned_segments: u64,
    /// The sizes in bytes of those (the marks' side of the share of proposals
    /// in the condemned bytes).
    pub mark_condemned_bytes: u64,
    /// Proposed segments (still in their grace) that a later mark found named
    /// by a retained root other than the tip: the lag rule broken. Always 0;
    /// the witness in mode `on`, where the `shadow_*` figures are not kept. A
    /// pin made after the proposal can raise it (the pin is such a root).
    pub proposal_named: u64,
    /// Proposal calls that did work (something was offered to a collector that
    /// was idle and the call ran), their summed and longest time in
    /// microseconds (real time, the whole call under the coordinator mutex).
    pub propose_calls: u64,
    /// See `propose_calls`.
    pub propose_us_sum: u64,
    /// See `propose_calls`.
    pub propose_us_max: u64,
    /// The same three figures for the objects recheck-only passes deleted.
    /// The difference to the figures above is every other pass kind,
    /// urgent passes included (an urgent pass in the `open` state judges
    /// with the grace floor, which can be below the grace in force, so its
    /// lag is measured from the floor).
    pub fast_lag_ms_sum: u64,
    /// See `fast_lag_ms_sum`.
    pub fast_lag_ms_max: u64,
    /// See `fast_lag_ms_sum`.
    pub fast_lag_count: u64,
    /// Segment delete verdicts the orphan rule deferred: the segment holds
    /// the id of a copy an entry with a gone hint relies on, and no FULL
    /// below the tip names that copy yet. The evidence reason is distinct from
    /// the `deps` deferral. Counted per verdict (a segment waiting over
    /// several passes counts in each).
    pub orphan_deferrals: u64,
    /// Cuts the collector asked the engine for (`cut_wanted`: a deferral a
    /// cut ends), at most one per tip.
    pub cuts_requested: u64,
}

/// How the rest of the daemon asks the coordinator for GC work. Implemented by
/// the engine wiring; `Send + Sync`. Never called on a queue thread.
pub trait GcControl: Send + Sync {
    /// Runs a pass (or a dry run) under the coordinator's mutex and returns
    /// its summary. A pass releases the mutex between its steps and waits out
    /// its grace. Blocking; used by the control socket's `Gc`.
    fn run_pass(&self, dry_run: bool) -> Result<GcPassSummary>;

    /// Runs an urgent pass wanting `want_bytes` under the coordinator's mutex;
    /// returns the bytes freed. Blocking; used by the ladder's compact step.
    fn run_urgent(&self, want_bytes: u64) -> Result<u64>;

    /// The collector's state. Non-blocking.
    fn status(&self) -> GcStatusSnapshot;
}
