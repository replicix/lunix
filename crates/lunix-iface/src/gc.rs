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
use lunix_format::{ChunkId, CommitSeq, Hint, IdLen};
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
