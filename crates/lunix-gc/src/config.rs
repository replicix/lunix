//! Construction-time inputs of the collector that the per-step
//! [`GcView`](lunix_iface::gc::GcView) does not carry: the rollback floor
//! (ADR-31), test hooks and tuning knobs.

use std::time::Duration;

use lunix_format::local::RollbackFloor;
use lunix_format::IdLen;
use lunix_iface::gc::GcParams;
use lunix_iface::store::VolumeDir;
use lunix_iface::{Error, Result};

/// Where the collector reads the local rollback floor of `state/pins`
/// (ADR-31). While a floor exists, no commit object at or above its
/// `max_commit_seq` is condemned, also below `chain_floor` (plan M17 rule 6),
/// because an attach whose tip is above the floor walks the chain back to it.
///
/// Implemented for every [`VolumeDir`]; there is deliberately no "no floor"
/// implementation, so the wiring cannot forget it.
pub trait FloorSource: Send + Sync {
    /// The recorded floor; `None` when none was ever recorded.
    ///
    /// # Errors
    /// `Io`/`Corrupt` for an unreadable `state/pins`. The collector then
    /// deletes no commit object in that pass (fail closed).
    fn current_floor(&self) -> Result<Option<RollbackFloor>>;
}

impl<T: VolumeDir + ?Sized> FloorSource for T {
    fn current_floor(&self) -> Result<Option<RollbackFloor>> {
        VolumeDir::rollback_floor(self)
    }
}

/// Test hooks. Production passes [`NoHooks`].
pub trait GcHooks: Send + Sync {
    /// Called at every crash point (the names are in
    /// [`CRASH_POINTS`](crate::CRASH_POINTS)). Returning `true` stops the
    /// collector there as a `SIGKILL` would: the call fails with `Io` and every
    /// later call fails too, so the test can take the stores' crash images.
    fn crash_at(&self, point: &'static str) -> bool {
        let _ = point;
        false
    }
}

/// The production hooks: none.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoHooks;

impl GcHooks for NoHooks {}

/// Tuning of the collector that is not a `volume.json` setting.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GcConfig {
    /// Minimum time between the end of one pass and the start of the next
    /// (default 30 s). A pass also needs a reason to run: a new tip, a
    /// non-empty condemned list or changed pins.
    pub pass_interval: Duration,
    /// Live bytes compaction may rewrite in one pass (default 1 GiB).
    pub compact_per_pass: u64,
    /// The mark walks each root in at most this many leaf batches (default
    /// 64), so a batch is about `1/64` of a root's leaves.
    pub max_leaf_batches: usize,
    /// Leaves per batch at least (default 16).
    pub min_leaf_batch: usize,
    /// Decoded segment footers kept by the mark (default 32).
    pub footer_cache: usize,
}

impl Default for GcConfig {
    fn default() -> Self {
        Self {
            pass_interval: Duration::from_secs(30),
            compact_per_pass: 1 << 30,
            max_leaf_batches: 64,
            min_leaf_batch: 16,
            footer_cache: 32,
        }
    }
}

/// The grace floor: 10 s plus twice `read_timeout` (30 s with the default
/// `read_timeout` of 10 s). The urgent pass uses it instead of `grace`.
pub fn grace_floor(params: &GcParams) -> Duration {
    Duration::from_secs(10).saturating_add(params.read_timeout.saturating_mul(2))
}

/// Checks the settings a pass depends on.
///
/// # Errors
/// `Invalid` for a grace below [`grace_floor`], a live threshold outside
/// `(0, 1)`, `retain_last` of 0 or a chunk size that is not a power of two
/// from 4 KiB to 1 MiB.
pub fn validate(params: &GcParams) -> Result<()> {
    if params.grace < grace_floor(params) {
        return Err(Error::invalid(
            "gc_grace is below its floor of 10 s plus twice read_timeout",
        ));
    }
    if !(params.live_threshold > 0.0 && params.live_threshold < 1.0) {
        return Err(Error::invalid("gc_live_threshold must be in (0, 1)"));
    }
    if params.retain_last == 0 {
        return Err(Error::invalid("retain_last must be at least 1"));
    }
    if !params.chunk_size.is_power_of_two() || !(4096..=1 << 20).contains(&params.chunk_size) {
        return Err(Error::invalid("chunk size out of range"));
    }
    Ok(())
}

/// True when compaction is refused for this volume (`id_len = 0`: a stale
/// hint could not be resolved).
pub(crate) fn compaction_refused(params: &GcParams) -> bool {
    params.id_len == IdLen::Zero
}
