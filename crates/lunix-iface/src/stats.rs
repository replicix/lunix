//! The engine's counter snapshot (M11 step 9), shared by `lunix status`, the
//! control protocol and the checkpoint hint.

use serde::{Deserialize, Serialize};

/// Counters since the volume was created, persisted in the checkpoint's
/// `stats` slots (most recent write wins; a lost checkpoint resets them, they
/// are never authoritative).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct EngineStats {
    /// Bytes the guest wrote.
    pub bytes_written: u64,
    /// Bytes the guest read.
    pub bytes_read: u64,
    /// READ requests.
    pub ops_read: u64,
    /// WRITE requests.
    pub ops_write: u64,
    /// FLUSH requests.
    pub ops_flush: u64,
    /// DISCARD requests.
    pub ops_discard: u64,
    /// WRITE_ZEROES requests.
    pub ops_write_zeroes: u64,
    /// Flush groups issued.
    pub flush_groups: u64,
    /// Sum of waiters over all flush groups (mean size = sum / groups).
    pub flush_group_waiters: u64,
    /// Largest flush group.
    pub flush_group_max: u64,
    /// Chunks destaged.
    pub chunks_total: u64,
    /// Chunks that were all zero (map holes).
    pub chunks_zero: u64,
    /// Chunks adopted by dedup.
    pub dedup_hits: u64,
    /// Sum of `ulen` of stored (unique) chunks.
    pub stored_ulen: u64,
    /// Sum of `clen` of stored (unique) chunks.
    pub stored_clen: u64,
    /// Partial-chunk writes that had to read the base chunk.
    pub cold_rmw: u64,
    /// Dirty map leaves at the last sample.
    pub dirty_leaves: u64,
    /// Commits made.
    pub commits: u64,
    /// Extents whose tag or AEAD failed on read.
    pub extent_auth_failures: u64,
    /// Nanoseconds spent per destage stage: hash, dedup, compress, seal, append.
    pub stage_ns: [u64; 5],
}

impl EngineStats {
    /// Number of checkpoint slots this layout uses (`<= 192`).
    pub const SLOTS: usize = 24;

    /// The counters as checkpoint slots, in a fixed order.
    pub fn to_slots(&self) -> Vec<u64> {
        let mut v = vec![
            self.bytes_written,
            self.bytes_read,
            self.ops_read,
            self.ops_write,
            self.ops_flush,
            self.ops_discard,
            self.ops_write_zeroes,
            self.flush_groups,
            self.flush_group_waiters,
            self.flush_group_max,
            self.chunks_total,
            self.chunks_zero,
            self.dedup_hits,
            self.stored_ulen,
            self.stored_clen,
            self.cold_rmw,
            self.dirty_leaves,
            self.commits,
            self.extent_auth_failures,
        ];
        v.extend_from_slice(&self.stage_ns);
        debug_assert_eq!(v.len(), Self::SLOTS);
        v
    }

    /// Rebuilds the counters from checkpoint slots; missing slots read as 0
    /// (an older layout) and extra slots are ignored (a newer one).
    pub fn from_slots(slots: &[u64]) -> Self {
        let g = |i: usize| slots.get(i).copied().unwrap_or(0);
        Self {
            bytes_written: g(0),
            bytes_read: g(1),
            ops_read: g(2),
            ops_write: g(3),
            ops_flush: g(4),
            ops_discard: g(5),
            ops_write_zeroes: g(6),
            flush_groups: g(7),
            flush_group_waiters: g(8),
            flush_group_max: g(9),
            chunks_total: g(10),
            chunks_zero: g(11),
            dedup_hits: g(12),
            stored_ulen: g(13),
            stored_clen: g(14),
            cold_rmw: g(15),
            dirty_leaves: g(16),
            commits: g(17),
            extent_auth_failures: g(18),
            stage_ns: [g(19), g(20), g(21), g(22), g(23)],
        }
    }
}
