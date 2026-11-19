//! The size of a cut and the write path's counters (ADR-33 T3 and T4).
//!
//! A cut is bounded by journal bytes: at most `cut_target` times the drain
//! rate of the last cuts, never less than what lets reclaim free a ring file
//! of the fullest lane, never more than half the ring. The rate is
//! `bytes / duration` over the last [`HISTORY`] committed cuts, the duration
//! being C1 to C9 on the engine's clock. With no history the rate bounds
//! nothing and the half ring does.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// Cuts the drain rate is averaged over.
pub(crate) const HISTORY: usize = 4;

/// The journal bytes and duration of the last [`HISTORY`] committed cuts.
#[derive(Debug, Default)]
pub(crate) struct DrainHistory {
    cuts: VecDeque<(u64, Duration)>,
}

impl DrainHistory {
    /// Notes a committed cut of `bytes` that took `took`.
    pub(crate) fn push(&mut self, bytes: u64, took: Duration) {
        if self.cuts.len() == HISTORY {
            self.cuts.pop_front();
        }
        self.cuts.push_back((bytes, took));
    }

    /// The journal bytes the drain rate of the history moves in `target`;
    /// `None` without a history or when it took no measurable time.
    pub(crate) fn budget(&self, target: Duration) -> Option<u64> {
        let bytes: u128 = self.cuts.iter().map(|c| u128::from(c.0)).sum();
        let took: u128 = self.cuts.iter().map(|c| c.1.as_nanos()).sum();
        if took == 0 || bytes == 0 {
            return None;
        }
        let budget = bytes.saturating_mul(target.as_nanos()) / took;
        Some(u64::try_from(budget).unwrap_or(u64::MAX))
    }
}

/// The counters behind [`CutStats`], written by the coordinator and the
/// queues, read without any lock.
#[derive(Debug, Default)]
pub(crate) struct CutCounters {
    pub(crate) ring_waiters: AtomicU64,
    pub(crate) ring_wait_max_ms: AtomicU64,
    pub(crate) last_cut_bytes: AtomicU64,
    pub(crate) last_cut_ms: AtomicU64,
}

impl CutCounters {
    pub(crate) fn snapshot(&self) -> CutStats {
        CutStats {
            ring_waiters: self.ring_waiters.load(Ordering::Relaxed),
            ring_wait_longest_ms: self.ring_wait_max_ms.load(Ordering::Relaxed),
            last_cut_bytes: self.last_cut_bytes.load(Ordering::Relaxed),
            last_cut_ms: self.last_cut_ms.load(Ordering::Relaxed),
        }
    }
}

/// The write path's counters of the engine (ADR-33): ring waits and the last
/// cut. Read without the coordinator's mutex.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CutStats {
    /// Writes parked now because the journal ring has no free file.
    pub ring_waiters: u64,
    /// The longest a write has waited for the ring (one still waiting
    /// included, as of the last time it was judged), milliseconds.
    pub ring_wait_longest_ms: u64,
    /// The journal bytes of the last committed cut.
    pub last_cut_bytes: u64,
    /// Its duration, C1 to C9, milliseconds.
    pub last_cut_ms: u64,
}
