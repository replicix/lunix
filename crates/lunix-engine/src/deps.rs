//! What the engine is built from: one handle per trait, plus three seams the
//! frozen `lunix-iface` does not provide (the map constructor, the rollback
//! floor and the test hooks).

use std::sync::Arc;

use lunix_format::commit::MapRoot;
use lunix_format::local::RollbackFloor;
use lunix_format::{CommitSeq, SegId, WriteSeq};
use lunix_iface::block::{QueueId, Tag};
use lunix_iface::clock::Clock;
use lunix_iface::crypto::{CommitCodec, ExtentCodec};
use lunix_iface::gc::Collector;
use lunix_iface::index::ChunkIndex;
use lunix_iface::journal::Journal;
use lunix_iface::map::BlockMap;
use lunix_iface::overlay::Overlay;
use lunix_iface::space::SpaceGate;
use lunix_iface::store::{
    CheckpointStore, CommitStore, GcStateStore, PackStore, PinSource, SegmentStore, VolumeDir,
};
use lunix_iface::Result;

use crate::destage::Destager;

/// Opens the block map at a root. `lunix-map`'s `open(root)` is its
/// constructor, not a trait method, so the wiring passes it in.
pub trait MapOpener: Send + Sync {
    /// The map whose last sealed root is `root`, of `chunk_count` chunks.
    ///
    /// # Errors
    /// Whatever the map reports for an unreadable or unauthenticated root.
    fn open(&self, root: &MapRoot, chunk_count: u64) -> Result<Arc<dyn BlockMap>>;
}

/// The rollback floor `{max_commit_seq, tip_hash}` of `state/pins` version 2
/// (D19, ADR-31, R4).
///
/// `lunix_iface::store::VolumeDir` reads and writes `LocalPins`, which drops
/// the floor (format amendment report, deviation 1), so the engine takes the
/// floor through this seam until the iface gains it (see the M11 report,
/// "Requests to the lead"). Raise-only: the engine never passes a lower floor.
pub trait RollbackFloorStore: Send + Sync {
    /// The recorded floor, `None` on a host that never recorded one.
    ///
    /// # Errors
    /// `Io`, `Corrupt`.
    fn load(&self) -> Result<Option<RollbackFloor>>;

    /// Records `floor` atomically. The engine calls it only after the commit
    /// it names is durable, and never with a lower `max_commit_seq`.
    ///
    /// # Errors
    /// `Io`.
    fn raise(&self, floor: &RollbackFloor) -> Result<()>;
}

/// Observation and fault-injection points for tests; production passes
/// [`NoHooks`].
pub trait EngineHooks: Send + Sync {
    /// Asked at every engine crash point (the names in
    /// [`CRASH_POINTS`](crate::CRASH_POINTS)); `true` makes the engine stop
    /// there as a kill would: the step fails and every later operation fails.
    fn crash_at(&self, point: &'static str) -> bool {
        let _ = point;
        false
    }

    /// A write, discard or write-zeroes record of `tag` on `queue` received
    /// `write_seq` (called right after `try_reserve`, so calls arrive in
    /// `write_seq` order per queue thread).
    fn sequenced(&self, queue: QueueId, tag: Tag, write_seq: WriteSeq) {
        let _ = (queue, tag, write_seq);
    }

    /// Called on a queue thread on entry to `on_request` (`true`) and on
    /// exit (`false`), and the same around `poll`. Tests use it to mark the
    /// thread and catch a blocking call.
    fn queue_thread(&self, entering: bool) {
        let _ = entering;
    }

    /// The reference table (mode `shadow` or `on`) offered `offered` for
    /// condemnation at the tip `tip`, under the coordinator mutex and inside
    /// the collector call that follows (so the collector sees the same
    /// retained roots), before the proposal and the step. Not called for an
    /// empty offer, nor while a pin exists (nothing is offered then).
    fn shadow_offered(&self, tip: CommitSeq, offered: &[SegId]) {
        let _ = (tip, offered);
    }
}

/// The hooks of production: none.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoHooks;

impl EngineHooks for NoHooks {}

/// Every component the engine is built from. The stores may all be one
/// handle (`lunix-store`'s `LocalVolume`) cloned behind each trait.
pub struct EngineDeps {
    /// The volume directory (descriptor bytes, uuid, sweep, allocator).
    pub dir: Arc<dyn VolumeDir>,
    /// Segments.
    pub segments: Arc<dyn SegmentStore>,
    /// Map packs.
    pub packs: Arc<dyn PackStore>,
    /// Commit objects.
    pub commits: Arc<dyn CommitStore>,
    /// The checkpoint hint.
    pub checkpoint: Arc<dyn CheckpointStore>,
    /// Snapshot, reader and hold pins (GC roots).
    pub pins: Arc<dyn PinSource>,
    /// GC state.
    pub gc_state: Arc<dyn GcStateStore>,
    /// The rollback floor.
    pub floor: Arc<dyn RollbackFloorStore>,
    /// The journal (not yet recovered: `open` recovers it).
    pub journal: Arc<dyn Journal>,
    /// An empty overlay (`open` rebuilds it from the journal).
    pub overlay: Arc<dyn Overlay>,
    /// The chunk index.
    pub index: Arc<dyn ChunkIndex>,
    /// The extent codec (data and nodes).
    pub extents: Arc<dyn ExtentCodec>,
    /// The commit codec.
    pub commit_codec: Arc<dyn CommitCodec>,
    /// The space gate.
    pub space: Arc<dyn SpaceGate>,
    /// Time.
    pub clock: Arc<dyn Clock>,
    /// The map constructor.
    pub maps: Arc<dyn MapOpener>,
    /// The destage pipeline (M9, or [`SimpleDestager`](crate::SimpleDestager)).
    pub destager: Arc<dyn Destager>,
    /// The garbage collector, called at C10 (M17); `None` for no GC.
    pub collector: Option<Box<dyn Collector>>,
    /// Test hooks.
    pub hooks: Arc<dyn EngineHooks>,
}
