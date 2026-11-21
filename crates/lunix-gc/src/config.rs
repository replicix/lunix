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

    /// Decoded segment footers kept by the mark (default 32).
    pub footer_cache: usize,

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
}

impl Default for GcConfig {
    fn default() -> Self {
        Self {
            pass_interval: Duration::from_secs(30),
            compact_per_pass: 1 << 30,
            leaves_per_unit: 32,
            finish_per_unit: 1 << 18,
            footer_cache: 32,
            pipeline: false,
            busy_interval: Duration::from_secs(5),
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

/// How many seconds of the compaction rate one pass may move at least
/// ([`per_pass_bytes`]). Passes follow each other at least `busy_interval`
/// (5 s) apart, so 4 s of the rate is what the rate allows between two.
pub const PASS_RATE_WINDOW_SECS: u64 = 4;

/// The live bytes one pass may compact: [`GcConfig::compact_per_pass`], or
/// [`PASS_RATE_WINDOW_SECS`] seconds of the compaction rate if that is more.
/// At the daemon's defaults (256 MiB, 64 MiB/s) the two are equal, so the
/// normal pass is unchanged; the rate the engine hands the collector while
/// the gate paces writes (sized to the ingest, milestone gate) lifts the cap
/// with it, which the per-pass cap alone would hold at about 256 MiB per
/// pass however high the rate. An uncapped rate (`u64::MAX`) is uncapped.
#[must_use]
pub fn per_pass_bytes(compact_per_pass: u64, rate_bytes_per_s: u64) -> u64 {
    compact_per_pass.max(rate_bytes_per_s.saturating_mul(PASS_RATE_WINDOW_SECS))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_per_pass_cap_follows_the_rate_above_the_default_and_never_falls_below_it() {
        let mib = 1u64 << 20;
        // The daemon's figures: unchanged.
        assert_eq!(per_pass_bytes(256 * mib, 64 * mib), 256 * mib);
        // Below them: the configured cap holds.
        assert_eq!(per_pass_bytes(256 * mib, mib), 256 * mib);
        // A raised or sized rate lifts it.
        assert_eq!(per_pass_bytes(256 * mib, 400 * mib), 1600 * mib);
        // Uncapped stays uncapped, no overflow.
        assert_eq!(per_pass_bytes(256 * mib, u64::MAX), u64::MAX);
    }
}
