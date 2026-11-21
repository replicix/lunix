//! The adapter from M9's pipeline (`lunix_destage::destage_cut`) to the
//! engine's [`Destager`] seam (M11 report, request 3; M9 re-review R1-R7).
//!
//! * The cut is borrowed; the engine keeps it for `Overlay::release`.
//! * `DestagedSegment::sealed` becomes `DestageOutcome::segments` (every one
//!   sealed before the map transaction committed); the segments' `entries`,
//!   flattened in order, become `index_inserts`, which the engine inserts only
//!   after C6 is durable.
//! * `DestageStats` become the engine's counters; the stage times go in the
//!   engine's order (hash, dedup, compress, seal, append).
//! * The error class is copied once, with an exhaustive match
//!   ([`retry_of`]); `Retry::classify` is never called on the error again (M9
//!   samples `dirty_leaf_count()` after the failure, the engine's table
//!   before the call: two copies would drift).
//! * `DestageCtx::reader` and `::writes` are unused: M9 does its own R1 base
//!   and confirmation reads (counted in `extent_auth_failures`) and takes the
//!   `write_seq` range from the cut.
//! * `Partial::freed_bytes` is informational (R3): the coordinator releases
//!   its C2 bound whole on failure, so nothing is released here.
//! * `AfterMapSeal` (R4) comes through as the coordinator's "cut smaller"
//!   path. It cannot occur today: every commit's C5 seals the map and a commit
//!   that fails after C3 stops the engine, so a cut never starts with dirty
//!   leaves.
//! * Back-pressure (R7): `DestageCtx` has no pressure carrier, so the adapter
//!   derives one level per cut from `Journal::fill()` with the thresholds of
//!   [`PressureThresholds`] (an open choice for the maintainer).

use lunix_destage::{
    destage_cut, DestageConfig, DestageError, DestageOutcome as M9Outcome, DestageParams,
    DestageStats, ErrorClass, Pressure,
};
use lunix_engine::{DestageCtx, DestageFailure, DestageOutcome, Destager, Retry};
use lunix_iface::overlay::OverlayCut;

/// The engine's [`Retry`] for M9's [`ErrorClass`]: one exhaustive match, so a
/// new class on either side is a compile error here.
pub fn retry_of(class: ErrorClass) -> Retry {
    match class {
        ErrorClass::Transient => Retry::Later,
        ErrorClass::AfterSpace => Retry::AfterSpace,
        ErrorClass::AfterMapSeal => Retry::AfterMapSeal,
        ErrorClass::CutTooLarge => Retry::CutTooLarge,
        ErrorClass::Permanent => Retry::Never,
        ErrorClass::Stop => Retry::Stop,
    }
}

/// Journal fill ratios from which a cut is compressed with LZ4 only, or
/// stored raw (R7: undecided by the maintainer; these are the daemon's
/// defaults until then).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct PressureThresholds {
    /// Fill at or above which the policy's codec is replaced by LZ4.
    pub lz4: f64,
    /// Fill at or above which chunks are stored raw.
    pub raw: f64,
}

impl Default for PressureThresholds {
    fn default() -> Self {
        Self {
            lz4: 0.75,
            raw: 0.9,
        }
    }
}

impl PressureThresholds {
    /// The pressure for a journal fill ratio.
    pub fn pressure(&self, fill: f64) -> Pressure {
        if fill >= self.raw {
            Pressure::Raw
        } else if fill >= self.lz4 {
            Pressure::Lz4
        } else {
            Pressure::None
        }
    }
}

/// M9's pipeline behind the engine's [`Destager`].
#[derive(Clone, Debug)]
pub struct PipelineDestager {
    /// The volume parameters (from the verified descriptor).
    pub params: DestageParams,
    /// Pool tuning; `pressure` is replaced per cut.
    pub config: DestageConfig,
    /// R7 thresholds.
    pub thresholds: PressureThresholds,
}

impl PipelineDestager {
    /// The adapter for a volume.
    pub fn new(params: DestageParams, config: DestageConfig) -> Self {
        Self {
            params,
            config,
            thresholds: PressureThresholds::default(),
        }
    }
}

/// Sums the per-codec array.
fn sum4(v: &[u64; lunix_destage::stats::CODEC_SLOTS]) -> u64 {
    v.iter().fold(0u64, |a, b| a.saturating_add(*b))
}

/// The engine's outcome for M9's, in the documented mapping.
pub fn map_outcome(o: M9Outcome) -> DestageOutcome {
    let s: &DestageStats = &o.stats;
    let mut index_inserts = Vec::with_capacity(o.segments.iter().map(|g| g.entries.len()).sum());
    let mut segments = Vec::with_capacity(o.segments.len());
    for seg in &o.segments {
        segments.push(seg.sealed);
        index_inserts.extend(seg.entries.iter().copied());
    }
    DestageOutcome {
        segments,
        index_inserts,
        bytes_in: s.bytes_in,
        chunks_total: s.chunks_total,
        chunks_zero: s.zero_holes,
        dedup_hits: s.dedup_hits.saturating_add(s.dedup_in_cut),
        stored_ulen: sum4(&s.ulen_by_codec),
        stored_clen: sum4(&s.clen_by_codec),
        cold_rmw: s.cold_rmw,
        stage_ns: [s.hash_ns, s.dedup_ns, s.compress_ns, s.seal_ns, s.append_ns],
        seals: s.seals,
        seal_in_flight_max: s.seal_in_flight_max,
        seal_busy_ns: s.seal_busy_ns,
        seal_wait_ns: s.seal_wait_ns,
        seal_stall_ns: s.seal_stall_ns,
        extent_auth_failures: s.extent_auth_failures,
    }
}

/// The engine's failure for M9's error.
pub fn map_error(e: DestageError) -> DestageFailure {
    // R2: M9 deleted the failed cut's segments and lists the deletes that
    // failed. Those files are outside the space ledger until the open-time
    // sweep removes them (only above the high-water mark) or GC does.
    if !e.partial.undeleted.is_empty() {
        let ids: Vec<String> = e
            .partial
            .undeleted
            .iter()
            .map(ToString::to_string)
            .collect();
        tracing::warn!(
            segments = %ids.join(","),
            class = ?e.class,
            "segments of a failed cut could not be deleted; GC (or the open-time sweep, above the high-water mark) removes them"
        );
    }
    DestageFailure {
        retry: retry_of(e.class),
        chunk: e.chunk,
        error: e.error,
    }
}

impl Destager for PipelineDestager {
    fn destage(
        &self,
        cut: &OverlayCut,
        ctx: &DestageCtx<'_>,
    ) -> Result<DestageOutcome, DestageFailure> {
        let mut config = self.config;
        config.pressure = self.thresholds.pressure(ctx.journal.fill().ratio());
        let deps = lunix_destage::DestageDeps {
            journal: ctx.journal,
            map: ctx.map,
            index: ctx.index,
            segments: ctx.segments,
            codec: ctx.codec,
            space: ctx.space,
            params: self.params,
            config,
        };
        destage_cut(cut, &deps).map(map_outcome).map_err(map_error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_error_class_maps_to_its_retry() {
        let table = [
            (ErrorClass::Transient, Retry::Later),
            (ErrorClass::AfterSpace, Retry::AfterSpace),
            (ErrorClass::AfterMapSeal, Retry::AfterMapSeal),
            (ErrorClass::CutTooLarge, Retry::CutTooLarge),
            (ErrorClass::Permanent, Retry::Never),
            (ErrorClass::Stop, Retry::Stop),
        ];
        for (class, retry) in table {
            assert_eq!(retry_of(class), retry, "{class:?}");
        }
    }

    #[test]
    fn pressure_follows_the_journal_fill() {
        let t = PressureThresholds::default();
        assert_eq!(t.pressure(0.0), Pressure::None);
        assert_eq!(t.pressure(0.74), Pressure::None);
        assert_eq!(t.pressure(0.75), Pressure::Lz4);
        assert_eq!(t.pressure(0.95), Pressure::Raw);
    }
}
