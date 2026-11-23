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
    /// Live bytes compaction may rewrite in one pass (default 1 GiB; 256 MiB
    /// in [`GcConfig::daemon`]).
    pub compact_per_pass: u64,
    /// New leaves one mark unit walks and resolves (default 32: 16 384
    /// entries at fanout 512).
    pub leaves_per_unit: usize,
    /// Live extents one finishing unit of a mark sorts (default 262 144).
    pub finish_per_unit: usize,

    /// Memory the mark may spend on decoded segment footers, in bytes
    /// (default 32 MiB; 16 to 256 MiB is the useful range). A footer costs
    /// 40 bytes per extent of its segment, so the default holds a few
    /// hundred segments of 16 KiB chunks and thousands of 64 KiB ones; a
    /// mark that touches more segments than fit reloads the oldest.
    pub footer_cache_bytes: usize,

    /// Overlap the grace with the next passes (default off; the daemon turns
    /// it on, [`GcConfig::daemon`]). Off, a pass marks, condemns, waits out
    /// the grace, marks again for the recheck and deletes: whatever dies
    /// during that wait is condemned only by the next pass and waits a whole
    /// grace again, so under sustained overwrite the store holds about two
    /// graces plus a pass interval of writes. On, a scheduled pass does not
    /// wait: it condemns what its one mark finds newly dead, and that mark is
    /// the recheck of every object listed before it whose grace ended before
    /// the mark began (as in the urgent pass): those are deleted; then it
    /// compacts. An object waits one grace plus at
    /// most two pass periods. A pass on request (`request_pass`) always runs
    /// the whole sequence.
    pub pipeline: bool,
    /// With `pipeline`: the time between a pass that did reclaim work (a
    /// segment condemned, deleted or compacted, or 64 MiB freed) and the next
    /// one, at least three times its mark time, instead of
    /// `pass_interval` (default 5 s), so passes follow each other while
    /// garbage keeps coming.
    pub busy_interval: Duration,
    /// With `pipeline`, while the engine reports pressure (writes paced or
    /// held, [`Collector::set_pressure`](lunix_iface::gc::Collector::set_pressure)):
    /// recheck-only passes. Once the grace of a listed object has ended (and
    /// this delay with it), and this delay has passed since the end of the
    /// previous pass, a pass marks, condemns what its mark finds dead, deletes
    /// what its mark rechecks and ends without compaction. Regular passes
    /// (with compaction) keep their own schedule, measured from the end of
    /// the last one. `None`: off (default; the daemon's key `gc_fast_recheck`
    /// turns it on).
    pub fast_recheck: Option<Duration>,
    /// Tests only: recheck-only passes without pressure too.
    #[doc(hidden)]
    pub fast_recheck_always: bool,
    /// Count the map entries per segment (by hint) of the tip root during
    /// every mark and hand the counts to the engine
    /// ([`Collector::take_tip_refs`](lunix_iface::gc::Collector::take_tip_refs)):
    /// the check of the engine's reference table (the daemon's
    /// `gc_condemn_on_cut`). Default off: no extra work in the mark.
    pub tip_refs: bool,
}

impl Default for GcConfig {
    fn default() -> Self {
        Self {
            pass_interval: Duration::from_secs(30),
            compact_per_pass: 1 << 30,
            leaves_per_unit: 32,
            finish_per_unit: 1 << 18,
            footer_cache_bytes: 32 << 20,
            pipeline: false,
            busy_interval: Duration::from_secs(5),
            fast_recheck: None,
            fast_recheck_always: false,
            tip_refs: false,
        }
    }
}

impl GcConfig {
    /// The daemon's settings: the defaults with `pipeline` on, and 256 MiB
    /// compacted per pass. A pipelined pass deletes at its start what earlier
    /// passes made dead, so a long compaction phase delays those deletes: a
    /// smaller share per pass keeps the passes (and the deletes) coming while
    /// compaction is the work (milestone gcrate, the M15 root cell).
    pub fn daemon() -> Self {
        Self {
            pipeline: true,
            compact_per_pass: 256 << 20,
            ..Self::default()
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
