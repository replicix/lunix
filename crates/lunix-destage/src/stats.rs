//! The accounting a cut returns.

use std::time::Duration;

use lunix_format::Codec;
use lunix_iface::stats::EngineStats;

/// Codec slots of the per-codec arrays: the `CCH1` codec ids 0 to 3.
pub const CODEC_SLOTS: usize = 4;

/// Index of `codec` in the per-codec arrays.
pub fn codec_slot(codec: Codec) -> usize {
    debug_assert!(usize::from(codec.id()) < CODEC_SLOTS);
    usize::from(codec.id()).min(CODEC_SLOTS - 1)
}

/// What one cut did. Every counter is exact; the times are summed over
/// workers (CPU time per stage, not wall time) except `wall_ns`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct DestageStats {
    /// Bytes the overlay held for the cut (4 KiB blocks it covered).
    pub bytes_in: u64,
    /// Dirty chunks processed.
    pub chunks_total: u64,
    /// Chunks that became map holes (all zero, or covered by hole pieces).
    pub zero_holes: u64,
    /// Of those, chunks the hole pieces covered whole (no read, no scan).
    pub hole_covered: u64,
    /// Chunks adopted from the chunk index (verified, or confirmed).
    pub dedup_hits: u64,
    /// Of those, hits from a persisted run that needed the confirmation read.
    pub dedup_confirmed: u64,
    /// Persisted-run hits whose confirmation failed: stored again (D17).
    pub dedup_rejected: u64,
    /// Chunks that equalled an extent appended earlier in the same cut.
    pub dedup_in_cut: u64,
    /// Chunks stored as a new extent.
    pub unique_stored: u64,
    /// `ulen` of the stored chunks, by codec id.
    pub ulen_by_codec: [u64; CODEC_SLOTS],
    /// `clen` of the stored chunks, by codec id.
    pub clen_by_codec: [u64; CODEC_SLOTS],
    /// Stored chunks, by codec id.
    pub chunks_by_codec: [u64; CODEC_SLOTS],
    /// Extent bytes appended to segments (header, payload and tag).
    pub extent_bytes: u64,
    /// Partial-chunk writes that read the base chunk from the store.
    pub cold_rmw: u64,
    /// Base-chunk or confirmation reads whose tag or id check failed.
    pub extent_auth_failures: u64,
    /// Base reads served through the index after a stale or wrong hint.
    pub stale_hint_fallbacks: u64,
    /// LZ4 compressions of a whole chunk.
    pub lz4_calls: u64,
    /// LZ4 probes of the `auto` policy.
    pub probe_calls: u64,
    /// Probes that decided "raw".
    pub probe_raw: u64,
    /// zstd compressions.
    pub zstd_calls: u64,
    /// Chunks compressed with LZ4 because of backpressure.
    pub degraded_lz4: u64,
    /// Chunks stored raw because of backpressure.
    pub degraded_raw: u64,
    /// Segments the cut appended to.
    pub segments: u64,
    /// Wall time of the whole cut.
    pub wall_ns: u64,
    /// Assemble stage (journal reads, base read).
    pub assemble_ns: u64,
    /// Zero scan.
    pub scan_ns: u64,
    /// Chunk id.
    pub hash_ns: u64,
    /// Index lookup and confirmation.
    pub dedup_ns: u64,
    /// Compression.
    pub compress_ns: u64,
    /// Seal.
    pub seal_ns: u64,
    /// Append to the segment (and rolling).
    pub append_ns: u64,
    /// Map transaction.
    pub map_ns: u64,
}

impl DestageStats {
    /// Adds `o` to `self`, field by field.
    pub fn add(&mut self, o: &Self) {
        macro_rules! sum {
            ($($f:ident),* $(,)?) => { $( self.$f += o.$f; )* };
        }
        sum!(
            bytes_in,
            chunks_total,
            zero_holes,
            hole_covered,
            dedup_hits,
            dedup_confirmed,
            dedup_rejected,
            dedup_in_cut,
            unique_stored,
            extent_bytes,
            cold_rmw,
            extent_auth_failures,
            stale_hint_fallbacks,
            lz4_calls,
            probe_calls,
            probe_raw,
            zstd_calls,
            degraded_lz4,
            degraded_raw,
            segments,
            wall_ns,
            assemble_ns,
            scan_ns,
            hash_ns,
            dedup_ns,
            compress_ns,
            seal_ns,
            append_ns,
            map_ns,
        );
        for i in 0..CODEC_SLOTS {
            self.ulen_by_codec[i] += o.ulen_by_codec[i];
            self.clen_by_codec[i] += o.clen_by_codec[i];
            self.chunks_by_codec[i] += o.chunks_by_codec[i];
        }
    }

    /// Chunks adopted by dedup of any kind (what `EngineStats::dedup_hits` counts).
    pub fn dedup_total(&self) -> u64 {
        self.dedup_hits + self.dedup_in_cut
    }

    /// Sum of `ulen` over the stored chunks.
    pub fn stored_ulen(&self) -> u64 {
        self.ulen_by_codec.iter().sum()
    }

    /// Sum of `clen` over the stored chunks.
    pub fn stored_clen(&self) -> u64 {
        self.clen_by_codec.iter().sum()
    }

    /// Adds this cut to the engine counters (M11 step 9). The stage slots are
    /// hash, dedup, compress, seal, append.
    pub fn add_to_engine(&self, e: &mut EngineStats) {
        e.chunks_total += self.chunks_total;
        e.chunks_zero += self.zero_holes;
        e.dedup_hits += self.dedup_total();
        e.stored_ulen += self.stored_ulen();
        e.stored_clen += self.stored_clen();
        e.cold_rmw += self.cold_rmw;
        e.extent_auth_failures += self.extent_auth_failures;
        let stages = [
            self.hash_ns,
            self.dedup_ns,
            self.compress_ns,
            self.seal_ns,
            self.append_ns,
        ];
        for (slot, ns) in e.stage_ns.iter_mut().zip(stages) {
            *slot += ns;
        }
    }

    /// Wall time as a duration.
    pub fn wall(&self) -> Duration {
        Duration::from_nanos(self.wall_ns)
    }
}

/// Nanoseconds in `d`, saturating.
pub(crate) fn ns(d: Duration) -> u64 {
    u64::try_from(d.as_nanos()).unwrap_or(u64::MAX)
}
