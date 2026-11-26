//! The engine's counters (M11 step 9): relaxed atomics, snapshotted as
//! [`EngineStats`].

use std::sync::atomic::{AtomicU64, Ordering};

use lunix_iface::stats::EngineStats;

macro_rules! counters {
    ($($field:ident),* $(,)?) => {
        /// One atomic per `EngineStats` field.
        #[derive(Debug, Default)]
        pub(crate) struct Counters {
            $(pub(crate) $field: AtomicU64,)*
            pub(crate) stage_ns: [AtomicU64; 5],
            /// Least age (ms) plus one of a hint that went stale under a
            /// read; 0 = no sample. Not a checkpoint slot.
            stale_age_min_p1: AtomicU64,
            /// Greatest age (ms) of such a hint. Not a checkpoint slot.
            stale_age_max: AtomicU64,
        }

        impl Counters {
            pub(crate) fn snapshot(&self) -> EngineStats {
                EngineStats {
                    $($field: self.$field.load(Ordering::Relaxed),)*
                    stage_ns: [
                        self.stage_ns[0].load(Ordering::Relaxed),
                        self.stage_ns[1].load(Ordering::Relaxed),
                        self.stage_ns[2].load(Ordering::Relaxed),
                        self.stage_ns[3].load(Ordering::Relaxed),
                        self.stage_ns[4].load(Ordering::Relaxed),
                    ],
                    read_stale_age_min_ms: self
                        .stale_age_min_p1
                        .load(Ordering::Relaxed)
                        .saturating_sub(1),
                    read_stale_age_max_ms: self.stale_age_max.load(Ordering::Relaxed),
                }
            }

            /// Restores the counters persisted in a checkpoint.
            pub(crate) fn restore(&self, s: &EngineStats) {
                $(self.$field.store(s.$field, Ordering::Relaxed);)*
                for (a, v) in self.stage_ns.iter().zip(s.stage_ns) {
                    a.store(v, Ordering::Relaxed);
                }
            }
        }
    };
}

counters!(
    bytes_written,
    bytes_read,
    ops_read,
    ops_write,
    ops_flush,
    ops_discard,
    ops_write_zeroes,
    flush_groups,
    flush_group_waiters,
    flush_group_max,
    chunks_total,
    chunks_zero,
    dedup_hits,
    stored_ulen,
    stored_clen,
    cold_rmw,
    dirty_leaves,
    commits,
    extent_auth_failures,
    read_revalidated,
    read_revalidate_exhausted,
);

impl Counters {
    /// Records the age, in milliseconds, of a hint that went stale under a
    /// read (a `NotFound` after the block's entry changed). Cold path.
    pub(crate) fn note_stale_age(&self, ms: u64) {
        self.stale_age_max.fetch_max(ms, Ordering::Relaxed);
        let p1 = ms.saturating_add(1);
        let _ = self
            .stale_age_min_p1
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |cur| {
                (cur == 0 || p1 < cur).then_some(p1)
            });
    }
}

/// Adds to a counter.
pub(crate) fn add(c: &AtomicU64, n: u64) {
    c.fetch_add(n, Ordering::Relaxed);
}

/// Raises a counter to at least `n`.
pub(crate) fn max(c: &AtomicU64, n: u64) {
    c.fetch_max(n, Ordering::Relaxed);
}
