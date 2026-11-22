//! The size of a cut and the write path's counters (ADR-33 T3 and T4; the
//! ring pace, the windowed wait and the latency histogram of milestone
//! jstall).
//!
//! A cut is bounded by journal bytes: at most `cut_target` times the drain
//! rate of the last cuts, never less than one ring file of journal bytes (in
//! record bytes, scaled by what the records occupy of the files in use) and
//! than what lets reclaim free a ring file of the fullest lane, never more
//! than half the ring (the same scale; the minimum wins). The rate is
//! `bytes / duration` over the last [`HISTORY`] committed cuts, the duration
//! being C1 to C9 on the engine's clock. With no history the rate bounds
//! nothing and one ring file of bytes is the budget (the first cut after an
//! attach, milestone jstall).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::time::Duration;

use lunix_iface::space::{FACTOR_INITIAL, FACTOR_MAX, FACTOR_ONE};

/// Cuts the drain rate is averaged over.
pub(crate) const HISTORY: usize = 4;

/// The journal bytes and duration of the last [`HISTORY`] committed cuts.
#[derive(Debug, Default)]
pub(crate) struct DrainHistory {
    /// Journal bytes, C1 to C9 time, and the time from the end of the cut
    /// before to the end of this one (never more than twice the cut's own).
    cuts: VecDeque<(u64, Duration, Duration)>,
}

impl DrainHistory {
    /// Notes a committed cut of `bytes` that took `took`, `since_last` after
    /// the end of the cut before it (`None` for the first).
    pub(crate) fn push(&mut self, bytes: u64, took: Duration, since_last: Option<Duration>) {
        if self.cuts.len() == HISTORY {
            self.cuts.pop_front();
        }
        // The gaps between cuts (the collector's share, the tick) are part of
        // the sustained rate, an idle spell is not: capped at twice the cut.
        let interval = since_last.map_or(took, |g| g.clamp(took, took.saturating_mul(2)));
        self.cuts.push_back((bytes, took, interval));
    }

    /// The sustained drain rate of the history in journal bytes per second:
    /// bytes over the time from cut end to cut end, so it is below the rate
    /// of the cuts themselves by the gaps between them (milestone jstall).
    /// `None` without a history.
    pub(crate) fn sustained_rate(&self) -> Option<u64> {
        let bytes: u128 = self.cuts.iter().map(|c| u128::from(c.0)).sum();
        let span: u128 = self.cuts.iter().map(|c| c.2.as_nanos()).sum();
        if span == 0 || bytes == 0 {
            return None;
        }
        Some(u64::try_from(bytes.saturating_mul(1_000_000_000) / span).unwrap_or(u64::MAX))
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

/// Cuts the store factor is averaged over (P1').
pub(crate) const FACTOR_HISTORY: usize = 8;

/// A cut counts as the "last" factor only with at least this many journal
/// bytes (see [`StoreFactor`]).
pub(crate) const LAST_MIN_BYTES: u64 = 1 << 20;

/// The store factor `A_hat` (P1'): how many store bytes (segments, map packs
/// and the commit) one journal byte becomes at the cut. The space gate
/// paces journal bytes against headroom, and the headroom falls by this
/// factor times what was admitted, a cut at a time.
///
/// `A_hat = max(byte-weighted mean of the last [`FACTOR_HISTORY`] cuts, the
/// last cut's own factor)`, clamped to `[FACTOR_ONE, FACTOR_MAX]` (1 to 16),
/// in thousandths (fixed point: the gate compares and divides it, and no
/// float crosses the trait); [`FACTOR_INITIAL`] (2.0) before the first
/// measured cut.
///
/// The last cut's own factor counts only when the cut was not tiny: at least
/// [`LAST_MIN_BYTES`] of journal and a quarter of the mean cut of the window.
/// A small cut divides the fixed costs of a cut (a segment's footer, the
/// map's path to the root, the commit) by few journal bytes and would pin
/// `A_hat` at the clamp until the next big cut (it still weighs in the mean,
/// by its bytes). Only cuts that wrote segments are measured: a cut of hole
/// records only has no store bytes of its own to divide. Not in the figure:
/// the index run written at C8 (a few percent of the cut's store bytes) and
/// the map leaves a compaction re-hint dirtied (they do count, as the map
/// packs): the two are small and of opposite signs.
#[derive(Debug, Default)]
pub(crate) struct StoreFactor {
    /// Journal bytes and settled store bytes of the last cuts.
    cuts: VecDeque<(u64, u64)>,
}

impl StoreFactor {
    /// Notes a committed cut of `journal` bytes that settled `store` bytes.
    /// Returns the cut's own factor (unclamped), `None` for a cut without
    /// journal bytes.
    pub(crate) fn push(&mut self, journal: u64, store: u64) -> Option<u64> {
        if journal == 0 {
            return None;
        }
        if self.cuts.len() == FACTOR_HISTORY {
            self.cuts.pop_front();
        }
        self.cuts.push_back((journal, store));
        Some(Self::ratio(journal, store))
    }

    /// `store / journal` in thousandths.
    fn ratio(journal: u64, store: u64) -> u64 {
        let r = u128::from(store) * u128::from(FACTOR_ONE) / u128::from(journal.max(1));
        u64::try_from(r).unwrap_or(u64::MAX)
    }

    /// `A_hat` in thousandths.
    pub(crate) fn a_hat(&self) -> u64 {
        let Some(&(last_j, last_s)) = self.cuts.back() else {
            return FACTOR_INITIAL;
        };
        let journal: u128 = self.cuts.iter().map(|c| u128::from(c.0)).sum();
        let store: u128 = self.cuts.iter().map(|c| u128::from(c.1)).sum();
        let mean = Self::ratio(
            u64::try_from(journal).unwrap_or(u64::MAX),
            u64::try_from(store).unwrap_or(u64::MAX),
        );
        let n = self.cuts.len() as u128;
        let big = last_j >= LAST_MIN_BYTES && u128::from(last_j) * 4 * n >= journal;
        let a = if big {
            mean.max(Self::ratio(last_j, last_s))
        } else {
            mean
        };
        a.clamp(FACTOR_ONE, FACTOR_MAX)
    }
}

/// Buckets of the write latency histogram (`2^(b-1) <= us < 2^b`).
pub const LATENCY_BUCKETS: usize = 32;

/// The fill ratio at which the ring pace starts (milestone jstall).
pub(crate) const PACE_ON_RATIO: f64 = 0.75;

/// The fill ratio below which the ring pace may end.
pub(crate) const PACE_OFF_RATIO: f64 = 0.5;

/// How long after the last write that waited for the ring the pace stays on
/// whatever the fill ratio says: a lane can be full while the whole ring is
/// not (the load maps to some queues only).
pub(crate) const PACE_HOLD: Duration = Duration::from_secs(5);

/// The ring pace (milestone jstall): while the journal ring is nearly full,
/// or a write waited for it a moment ago, writes are admitted at the drain
/// rate of the last cuts, so a write sees a short smooth wait in the band
/// instead of the stop of two cuts. A plain state machine: no clock of its
/// own, no lock.
#[derive(Debug, Default)]
pub(crate) struct RingPacer {
    on: bool,
    waited_at: Option<Duration>,
}

impl RingPacer {
    /// The pace at `now`: `rate` (journal bytes per second of the drain
    /// history, `None` without one) while the ring is nearly full
    /// (`ratio` of the journal's bytes in use), or a write waited for it in
    /// the last [`PACE_HOLD`]; `None` otherwise. It starts at
    /// [`PACE_ON_RATIO`] and ends below [`PACE_OFF_RATIO`] with nothing
    /// waiting lately.
    pub(crate) fn observe(
        &mut self,
        ratio: f64,
        waiting: bool,
        rate: Option<u64>,
        now: Duration,
    ) -> Option<u64> {
        if waiting {
            self.waited_at = Some(now);
        }
        let lately = self
            .waited_at
            .is_some_and(|t| now.saturating_sub(t) < PACE_HOLD);
        if self.on {
            if ratio < PACE_OFF_RATIO && !lately {
                self.on = false;
            }
        } else if ratio >= PACE_ON_RATIO || lately {
            self.on = true;
        }
        if self.on {
            rate
        } else {
            None
        }
    }
}

/// Seconds of one step of [`RecentMax`].
const STEP_SECS: u64 = 5;
/// Steps of [`RecentMax`]: 60 s.
const STEPS: u64 = 12;

/// The maximum of what was noted in the last 60 s, in five-second steps, with
/// no lock: each step is one atomic holding its step number (the high 32
/// bits) and the maximum noted in it, milliseconds (the low 32 bits).
#[derive(Debug, Default)]
pub(crate) struct RecentMax {
    steps: [AtomicU64; STEPS as usize],
}

impl RecentMax {
    fn step_of(now: Duration) -> u64 {
        now.as_secs() / STEP_SECS
    }

    /// Notes `ms` at `now`.
    pub(crate) fn note(&self, now: Duration, ms: u64) {
        let step = Self::step_of(now);
        let ms = ms.min(u64::from(u32::MAX));
        let idx = usize::try_from(step % STEPS).unwrap_or(0);
        let slot = &self.steps[idx];
        let _ = slot.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |cur| {
            let (cs, cm) = (cur >> 32, cur & 0xffff_ffff);
            if cs > step {
                // A later step already owns the slot.
                None
            } else if cs == step {
                (ms > cm).then_some((cur & !0xffff_ffff) | ms)
            } else {
                Some((step << 32) | ms)
            }
        });
    }

    /// The maximum noted in the last 60 s as of `now`.
    pub(crate) fn max(&self, now: Duration) -> u64 {
        let step = Self::step_of(now);
        self.steps
            .iter()
            .map(|a| a.load(Ordering::Relaxed))
            .filter(|v| {
                let s = v >> 32;
                s <= step && step - s < STEPS
            })
            .map(|v| v & 0xffff_ffff)
            .max()
            .unwrap_or(0)
    }
}

/// One queue's histogram, alone on its cache lines.
#[derive(Debug)]
#[repr(align(64))]
struct LatencyLane([AtomicU64; LATENCY_BUCKETS]);

/// Arrival-to-acknowledgement time of the writes, in log2 microsecond buckets
/// (milestone jstall): one histogram per queue, so the queue threads never
/// share a line; the status sums them.
#[derive(Debug)]
pub(crate) struct LatencyHist {
    lanes: Vec<LatencyLane>,
}

impl LatencyHist {
    pub(crate) fn new(lanes: usize) -> Self {
        Self {
            lanes: (0..lanes.max(1))
                .map(|_| LatencyLane(std::array::from_fn(|_| AtomicU64::new(0))))
                .collect(),
        }
    }

    /// The bucket of `d`: 0 below 1 us, else `b` with `2^(b-1) <= us < 2^b`,
    /// the last bucket taking everything longer.
    pub(crate) fn bucket(d: Duration) -> usize {
        let us = u64::try_from(d.as_micros()).unwrap_or(u64::MAX);
        usize::try_from(u64::BITS - us.leading_zeros())
            .unwrap_or(LATENCY_BUCKETS - 1)
            .min(LATENCY_BUCKETS - 1)
    }

    /// Notes a write of queue `lane` that took `d`.
    pub(crate) fn note(&self, lane: usize, d: Duration) {
        let l = self.lanes.get(lane).or_else(|| self.lanes.first());
        if let Some(l) = l {
            l.0[Self::bucket(d)].fetch_add(1, Ordering::Relaxed);
        }
    }

    pub(crate) fn snapshot(&self) -> [u64; LATENCY_BUCKETS] {
        let mut out = [0u64; LATENCY_BUCKETS];
        for l in &self.lanes {
            for (o, b) in out.iter_mut().zip(&l.0) {
                *o += b.load(Ordering::Relaxed);
            }
        }
        out
    }
}

/// The counters behind [`CutStats`], written by the coordinator and the
/// queues, read without any lock.
#[derive(Debug, Default)]
pub(crate) struct CutCounters {
    pub(crate) ring_waiters: AtomicU64,
    /// Every request parked in a queue that waits for the ring: signed, since
    /// the queues add and subtract their own changes.
    pub(crate) ring_parked_writes: AtomicI64,
    /// The longest ring wait since the attach, from arrival, milliseconds.
    pub(crate) ring_wait_max_ms: AtomicU64,
    /// The same over the last 60 s.
    pub(crate) ring_wait_recent: RecentMax,
    /// The ring pace now, bytes per second (0: none).
    pub(crate) ring_pace_rate: AtomicU64,
    /// The collector yields to the cuts until this `clock.monotonic()` in ms.
    pub(crate) yield_until_ms: AtomicU64,
    pub(crate) last_cut_bytes: AtomicU64,
    pub(crate) last_cut_ms: AtomicU64,
    /// `A_hat` when the last cut began and the cut's measured factor,
    /// thousandths (0: no cut yet).
    pub(crate) store_pred_milli: AtomicU64,
    pub(crate) store_real_milli: AtomicU64,
    /// Segment seals, all cuts (ADR-33 T8).
    pub(crate) seals: AtomicU64,
    /// The most seals in flight at once in any cut since the attach.
    pub(crate) seal_in_flight_max: AtomicU64,
    pub(crate) seal_busy_ns: AtomicU64,
    pub(crate) seal_wait_ns: AtomicU64,
    pub(crate) seal_stall_ns: AtomicU64,
}

impl CutCounters {
    pub(crate) fn snapshot(&self, now: Duration) -> CutStats {
        CutStats {
            ring_waiters: self.ring_waiters.load(Ordering::Relaxed),
            ring_parked_writes: u64::try_from(self.ring_parked_writes.load(Ordering::Relaxed))
                .unwrap_or(0),
            ring_wait_max_since_attach_ms: self.ring_wait_max_ms.load(Ordering::Relaxed),
            ring_wait_max_60s_ms: self.ring_wait_recent.max(now),
            ring_pace_rate: self.ring_pace_rate.load(Ordering::Relaxed),
            last_cut_bytes: self.last_cut_bytes.load(Ordering::Relaxed),
            last_cut_ms: self.last_cut_ms.load(Ordering::Relaxed),
            store_pred_milli: self.store_pred_milli.load(Ordering::Relaxed),
            store_real_milli: self.store_real_milli.load(Ordering::Relaxed),
            seals: self.seals.load(Ordering::Relaxed),
            seal_in_flight_max: self.seal_in_flight_max.load(Ordering::Relaxed),
            seal_busy_ms: self.seal_busy_ns.load(Ordering::Relaxed) / 1_000_000,
            seal_wait_ms: self.seal_wait_ns.load(Ordering::Relaxed) / 1_000_000,
            seal_stall_ms: self.seal_stall_ns.load(Ordering::Relaxed) / 1_000_000,
        }
    }
}

/// The write path's counters of the engine (ADR-33, milestone jstall): ring
/// waits, the ring pace and the last cut. Read without the coordinator's
/// mutex.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CutStats {
    /// Writes waiting now because the journal ring has no free file (the
    /// ones refused a ring slot).
    pub ring_waiters: u64,
    /// Every request parked now in a queue that waits for the ring: the
    /// waiters and what stands behind them.
    pub ring_parked_writes: u64,
    /// The longest a write has waited for the ring since the attach, from its
    /// arrival (one still waiting included, as of the last time it was
    /// judged), milliseconds.
    pub ring_wait_max_since_attach_ms: u64,
    /// The same over the last 60 s (five-second steps).
    pub ring_wait_max_60s_ms: u64,
    /// The rate writes are paced to because the ring is nearly full, bytes
    /// per second (0: no pace).
    pub ring_pace_rate: u64,
    /// The journal bytes of the last committed cut.
    pub last_cut_bytes: u64,
    /// Its duration, C1 to C9, milliseconds.
    pub last_cut_ms: u64,
    /// `A_hat` as it stood when the last cut began, thousandths (0: no cut
    /// yet).
    pub store_pred_milli: u64,
    /// The last cut's measured store bytes per journal byte, thousandths.
    pub store_real_milli: u64,
    /// Segment seals run since the attach.
    pub seals: u64,
    /// The most seals in flight at once in one cut since the attach.
    pub seal_in_flight_max: u64,
    /// Summed duration of the seals (they overlap packing), milliseconds.
    pub seal_busy_ms: u64,
    /// Time cuts waited at the end of packing for seals still running,
    /// milliseconds.
    pub seal_wait_ms: u64,
    /// Time packers waited for a free seal slot, milliseconds.
    pub seal_stall_ms: u64,
}

/// The write path's per-lane and histogram figures (milestone jstall).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WriteStats {
    /// Ring files freed so far, by lane number.
    pub freed_files_by_lane: Vec<u64>,
    /// Writes by arrival-to-acknowledgement time in the daemon: bucket `b`
    /// holds `2^(b-1) <= us < 2^b` (bucket 0: under a microsecond).
    pub write_latency_log2_us: [u64; LATENCY_BUCKETS],
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    #[test]
    fn the_store_factor_is_two_before_a_cut_and_a_cut_without_journal_bytes_says_nothing() {
        let mut f = StoreFactor::default();
        assert_eq!(f.a_hat(), 2000);
        assert_eq!(f.push(0, 5 << 20), None);
        assert_eq!(f.a_hat(), 2000);
    }

    #[test]
    fn the_store_factor_is_the_larger_of_the_weighted_mean_and_the_last_cut() {
        let mut f = StoreFactor::default();
        // 100 MiB of journal became 250 MiB, then 10 MiB became 20.
        assert_eq!(f.push(100 << 20, 250 << 20), Some(2500));
        assert_eq!(f.a_hat(), 2500);
        assert_eq!(f.push(10 << 20, 20 << 20), Some(2000));
        // The mean is 270 / 110 = 2.454; the last cut's 2.0 is below it.
        assert_eq!(f.a_hat(), 2454);
        // A last cut above the mean wins: the factor rises at once (50 MiB
        // is a big cut against a mean of 55 MiB).
        assert_eq!(f.push(50 << 20, 200 << 20), Some(4000));
        assert_eq!(f.a_hat(), 4000);
    }

    #[test]
    fn a_tiny_cut_after_large_ones_does_not_pin_the_factor() {
        let mut f = StoreFactor::default();
        for _ in 0..4 {
            f.push(200 << 20, 500 << 20);
        }
        assert_eq!(f.a_hat(), 2500);
        // 40 KiB of journal that wrote a 64 KiB chunk and three map nodes.
        f.push(40 << 10, 300 << 10);
        assert!(f.a_hat() <= 2501, "{}", f.a_hat());
        assert!(f.a_hat() >= 2499, "{}", f.a_hat());
        // Even 4 MiB, under a quarter of the 200 MiB mean, does not.
        f.push(4 << 20, 60 << 20);
        assert!(f.a_hat() < 2600, "{}", f.a_hat());
        // But a light steady load of small cuts is its own mean and last.
        let mut g = StoreFactor::default();
        for _ in 0..8 {
            g.push(2 << 20, 9 << 20);
        }
        assert_eq!(g.a_hat(), 4500);
        g.push(2 << 20, 12 << 20);
        assert_eq!(g.a_hat(), 6000, "a 2 MiB cut of a 2 MiB mean is big");
    }

    #[test]
    fn the_store_factor_forgets_cuts_older_than_eight_and_stays_within_one_and_sixteen() {
        let mut f = StoreFactor::default();
        f.push(8 << 20, 56 << 20);
        for _ in 0..FACTOR_HISTORY {
            f.push(8 << 20, 16 << 20);
        }
        assert_eq!(f.a_hat(), 2000, "the first cut left the window");
        // Below one (a cut that deduplicated or compressed): one.
        let mut f = StoreFactor::default();
        f.push(10 << 20, 3 << 20);
        assert_eq!(f.a_hat(), 1000);
        // Above sixteen: sixteen.
        f.push(100 << 20, 5000 << 20);
        assert_eq!(f.a_hat(), 16_000);
    }

    #[test]
    fn the_ring_pace_starts_at_three_quarters_ends_below_half_and_keeps_its_state_between() {
        let mut p = RingPacer::default();
        let rate = Some(1000);
        assert_eq!(p.observe(0.74, false, rate, secs(1)), None);
        assert_eq!(p.observe(0.75, false, rate, secs(2)), Some(1000));
        // Between the marks it stays on.
        assert_eq!(p.observe(0.6, false, rate, secs(3)), Some(1000));
        assert_eq!(p.observe(0.5, false, rate, secs(4)), Some(1000));
        assert_eq!(p.observe(0.49, false, rate, secs(5)), None);
        // ... and off.
        assert_eq!(p.observe(0.6, false, rate, secs(6)), None);
    }

    #[test]
    fn without_a_drain_history_there_is_no_rate_and_no_pace() {
        let mut p = RingPacer::default();
        assert_eq!(p.observe(0.99, false, None, secs(1)), None);
        // The pace is on: a history appearing gives it a rate at once.
        assert_eq!(p.observe(0.99, false, Some(7), secs(2)), Some(7));
    }

    #[test]
    fn a_waiting_write_turns_the_pace_on_and_it_stays_for_the_hold() {
        let mut p = RingPacer::default();
        let rate = Some(500);
        assert_eq!(p.observe(0.1, true, rate, secs(10)), Some(500));
        // Nobody waits now, the ring is nearly empty: still on for 5 s.
        assert_eq!(p.observe(0.1, false, rate, secs(14)), Some(500));
        assert_eq!(p.observe(0.1, false, rate, secs(15)), None);
        // Waiting again renews it.
        assert_eq!(p.observe(0.1, true, rate, secs(20)), Some(500));
        assert_eq!(p.observe(0.1, true, rate, secs(30)), Some(500));
        assert_eq!(p.observe(0.1, false, rate, secs(34)), Some(500));
    }

    #[test]
    fn the_recent_maximum_covers_sixty_seconds_in_five_second_steps() {
        let m = RecentMax::default();
        assert_eq!(m.max(secs(1_000)), 0);
        m.note(secs(1_000), 40);
        m.note(secs(1_001), 90);
        m.note(secs(1_002), 70);
        assert_eq!(m.max(secs(1_002)), 90);
        // A later, smaller figure does not hide it, a later larger one wins.
        m.note(secs(1_030), 10);
        assert_eq!(m.max(secs(1_030)), 90);
        m.note(secs(1_031), 120);
        assert_eq!(m.max(secs(1_031)), 120);
        // Gone after 60 s (the step of the note is 1_000 / 5 = 200).
        assert_eq!(m.max(secs(1_059)), 120);
        assert_eq!(m.max(secs(1_091)), 0, "both steps are out");
        // A slot reused by a later step starts again (200 and 212 share one).
        m.note(secs(1_060), 5);
        assert_eq!(m.max(secs(1_060)), 120);
    }

    #[test]
    fn the_recent_maximum_ignores_a_stale_writer() {
        let m = RecentMax::default();
        m.note(secs(500), 9);
        // The same slot, twelve steps later, then a late note of the old step.
        m.note(secs(560), 3);
        m.note(secs(500), 8);
        assert_eq!(m.max(secs(560)), 3);
    }

    #[test]
    fn latency_buckets_are_log2_of_microseconds() {
        let b = |us: u64| LatencyHist::bucket(Duration::from_micros(us));
        assert_eq!(b(0), 0);
        assert_eq!(b(1), 1);
        assert_eq!(b(2), 2);
        assert_eq!(b(3), 2);
        assert_eq!(b(4), 3);
        assert_eq!(b(1023), 10);
        assert_eq!(b(1024), 11);
        assert_eq!(b(3_000_000), 22);
        assert_eq!(LatencyHist::bucket(Duration::from_secs(10_000_000)), 31);
    }

    #[test]
    fn the_histogram_sums_its_queues() {
        let h = LatencyHist::new(2);
        h.note(0, Duration::from_micros(5));
        h.note(1, Duration::from_micros(5));
        h.note(1, Duration::from_micros(100));
        // An unknown queue lands on the first.
        h.note(9, Duration::ZERO);
        let s = h.snapshot();
        assert_eq!((s[0], s[3], s[7]), (1, 2, 1));
        assert_eq!(s.iter().sum::<u64>(), 4);
    }

    #[test]
    fn the_sustained_rate_counts_the_gaps_between_cuts_up_to_twice_the_cut() {
        let mut h = DrainHistory::default();
        h.push(1000, secs(1), None);
        // 0.5 s after the last cut ended; then an idle night, capped at 2 s.
        h.push(1000, secs(1), Some(Duration::from_millis(1500)));
        h.push(1000, secs(1), Some(secs(36_000)));
        // 3000 bytes over 1 + 1.5 + 2 s; the cuts alone moved 1000 per second.
        assert_eq!(h.sustained_rate(), Some(666));
        assert_eq!(h.budget(secs(1)), Some(1000));
        // A gap below the cut's own time is the cut's time.
        let mut g = DrainHistory::default();
        g.push(500, secs(2), Some(secs(1)));
        assert_eq!(g.sustained_rate(), Some(250));
        assert_eq!(DrainHistory::default().sustained_rate(), None);
    }
}
