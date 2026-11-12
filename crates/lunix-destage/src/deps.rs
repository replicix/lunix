//! What a cut runs against.

use lunix_iface::crypto::ExtentCodec;
use lunix_iface::index::ChunkIndex;
use lunix_iface::journal::Journal;
use lunix_iface::map::BlockMap;
use lunix_iface::space::SpaceGate;
use lunix_iface::store::SegmentStore;

use crate::config::{DestageConfig, DestageParams};

/// The collaborators of [`destage_cut`](crate::destage_cut). All are used from
/// worker threads and the calling thread, never from a queue thread.
#[derive(Clone, Copy)]
pub struct DestageDeps<'a> {
    /// Blocking reads of overlay data (`Journal::read_at`).
    pub journal: &'a dyn Journal,
    /// The map: base chunks for read-modify-write, and the cut's update.
    pub map: &'a dyn BlockMap,
    /// Dedup lookups and the stale-hint fallback. Inserts are the caller's.
    pub index: &'a dyn ChunkIndex,
    /// Extent appends and reads.
    pub segments: &'a dyn SegmentStore,
    /// Chunk ids, sealing, opening (keyed in `encrypted` and `integrity`).
    pub codec: &'a dyn ExtentCodec,
    /// Read-only check and `fatal_io`. The ledger charges for the cut
    /// (headroom at C2, the true-up at C4) are the coordinator's.
    pub space: &'a dyn SpaceGate,
    /// The volume parameters.
    pub params: DestageParams,
    /// Pool tuning.
    pub config: DestageConfig,
}
