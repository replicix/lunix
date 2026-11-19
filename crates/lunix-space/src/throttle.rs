//! The paced admission band (ADR-32, the note's section 5.3) as plain state
//! machines: the curve, the token bucket and the counters. No locks, no
//! threads and no clock of their own; [`crate::LunixSpace`] drives them under
//! its one mutex.
//!
//! ```text
//! x    = headroom / soft                    (0 < x < 1 inside the band)
//! rate = max(r_min, r_mid * x / (1 - x))    bytes per second
//! ```
//!
//! The curve is the OpenZFS delay hyperbola in rate form: unbounded at the
//! soft mark, `r_mid` in the middle of the band, `r_min` near the limit.

use std::collections::VecDeque;
use std::time::Duration;

use lunix_iface::space::{ThrottleFigures, ThrottleState};

/// The floor of `r_mid`: 16 MiB/s.
pub const MID_RATE_FLOOR: u64 = 16 << 20;

/// The largest journal record: 1 MiB of data and its 64-byte header.
const MAX_RECORD: i64 = (1 << 20) + 64;

/// The bucket's capacity in time: 50 ms of the rate.
const BUCKET_MS: u64 = 50;

/// How far back the ingest and reclaim averages look.
const METER_WINDOW: Duration = Duration::from_secs(10);

/// The throttle's settings (the daemon's `throttle*` keys of `lunixd.toml`
/// and the volume's `gc_rate`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ThrottleConfig {
    /// False turns the band off: the bounded stop and the latch stay.
    pub enabled: bool,
    /// The soft mark as a percent of the limit, 1 ..= 50.
    pub soft_percent: u8,
    /// The soft mark is at least this many bytes (and at most half the
    /// limit).
    pub soft_min: u64,
    /// The soft mark is at most this many bytes.
    pub soft_max: u64,
    /// `r_min`: the slowest rate inside the band, bytes per second.
    pub min_rate: u64,
    /// The longest a request is delayed by pacing; past it the request is
    /// admitted if it fits and the bucket carries the debt.
    pub max_delay: Duration,
    /// `r_mid`, the rate in the middle of the band: `max(gc_rate, 16 MiB/s)`.
    pub mid_rate: u64,
}

impl Default for ThrottleConfig {
    /// The ADR's defaults with the default `gc_rate` of 64 MiB/s.
    fn default() -> Self {
        Self {
            enabled: true,
            soft_percent: 15,
            soft_min: 1 << 30,
            soft_max: 64 << 30,
            min_rate: 4 << 20,
            max_delay: Duration::from_secs(10),
            mid_rate: 64 << 20,
        }
    }
}

impl ThrottleConfig {
    /// `r_mid` from the volume's `gc_rate` (bytes per second).
    #[must_use]
    pub fn with_gc_rate(mut self, gc_rate: u64) -> Self {
        self.mid_rate = gc_rate.max(MID_RATE_FLOOR);
        self
    }

    /// The soft mark for `limit`: `soft_percent` of it, clamped to
    /// `soft_min ..= soft_max` and to half the limit. 0 with the band off.
    pub fn soft_mark(&self, limit: u64) -> u64 {
        if !self.enabled {
            return 0;
        }
        let pct = u128::from(limit) * u128::from(self.soft_percent) / 100;
        let pct = u64::try_from(pct).unwrap_or(u64::MAX);
        pct.max(self.soft_min)
            .min(self.soft_max.max(self.soft_min))
            .min(limit / 2)
    }

    /// The admission rate at `headroom` below a soft mark of `soft`, bytes
    /// per second; `None` at or above the mark (no pacing) or with the band
    /// off.
    pub fn rate(&self, headroom: u64, soft: u64) -> Option<u64> {
        if !self.enabled || soft == 0 || headroom >= soft {
            return None;
        }
        let h = u128::from(headroom);
        let r = u128::from(self.mid_rate) * h / (u128::from(soft) - h);
        Some(u64::try_from(r).unwrap_or(u64::MAX).max(self.min_rate))
    }
}

/// A token bucket in bytes of journal record. It has no clock: every call is
/// told the time. Inactive while the gate is open; entering the band fills
/// it, so the first write of an episode pays nothing.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Bucket {
    tokens: i64,
    last: Duration,
    active: bool,
}

fn signed(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

impl Bucket {
    pub fn new() -> Self {
        Self {
            tokens: 0,
            last: Duration::ZERO,
            active: false,
        }
    }

    /// Capacity at `rate`: one largest record or 50 ms of the rate.
    fn capacity(rate: u64) -> i64 {
        let burst = u128::from(rate) * u128::from(BUCKET_MS) / 1000;
        MAX_RECORD.max(signed(u64::try_from(burst).unwrap_or(u64::MAX)))
    }

    /// Leaves the band: the bucket is dropped.
    pub fn deactivate(&mut self) {
        self.active = false;
    }

    /// Brings the tokens up to `now` at `rate` (a fresh bucket starts full).
    pub fn refill(&mut self, now: Duration, rate: u64) {
        let cap = Self::capacity(rate);
        if !self.active {
            *self = Self {
                tokens: cap,
                last: now,
                active: true,
            };
            return;
        }
        let dt = now.saturating_sub(self.last);
        let add = u128::from(rate) * dt.as_nanos() / 1_000_000_000;
        let add = signed(u64::try_from(add).unwrap_or(u64::MAX));
        // The debt is at most one second of the rate now, not of the rate
        // it was taken at (review m1).
        self.tokens = self
            .tokens
            .saturating_add(add)
            .min(cap)
            .max(signed(rate).saturating_neg());
        self.last = now;
    }

    /// True when the bucket holds `bytes`.
    pub fn has(&self, bytes: u64) -> bool {
        self.tokens >= signed(bytes)
    }

    /// Spends `bytes` the bucket holds.
    pub fn take(&mut self, bytes: u64) {
        self.tokens = self.tokens.saturating_sub(signed(bytes));
    }

    /// Spends `bytes` the bucket does not hold: the debt is at most one
    /// second of `rate` (a larger one is forgiven).
    pub fn overdraw(&mut self, bytes: u64, rate: u64) {
        let floor = signed(rate).saturating_neg();
        self.tokens = self.tokens.saturating_sub(signed(bytes)).max(floor);
    }

    /// How long until the bucket holds `bytes` at `rate` (zero if it does).
    pub fn wait_for(&self, bytes: u64, rate: u64) -> Duration {
        let need = signed(bytes).saturating_sub(self.tokens);
        if need <= 0 || rate == 0 {
            return Duration::ZERO;
        }
        let ns = u128::try_from(need).unwrap_or(0) * 1_000_000_000 / u128::from(rate);
        Duration::from_nanos(u64::try_from(ns + 1).unwrap_or(u64::MAX))
    }
}

/// Cumulative counters of the gate.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Counters {
    pub delayed_writes: u64,
    pub delayed_ms: u64,
    pub delay_max_ms: u64,
    pub delay_capped: u64,
    pub bypassed_fua: u64,
    pub enospc: u64,
    pub stops: u64,
    /// Bytes admitted by `reserve` (charged or granted).
    pub admitted: u64,
    /// Bytes of store released (segments, packs, runs, commits, cache).
    pub freed: u64,
}

/// The state and its bookkeeping: when it was entered and the time spent
/// outside `Open`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct StateClock {
    pub state: ThrottleState,
    pub since: Duration,
    /// Time of finished episodes outside `Open`.
    throttled: Duration,
}

impl StateClock {
    pub fn new(now: Duration) -> Self {
        Self {
            state: ThrottleState::Open,
            since: now,
            throttled: Duration::ZERO,
        }
    }

    /// Records the state at `now`; returns true when it changed.
    pub fn enter(&mut self, to: ThrottleState, now: Duration, c: &mut Counters) -> bool {
        if to == self.state {
            return false;
        }
        if self.state != ThrottleState::Open {
            self.throttled = self
                .throttled
                .saturating_add(now.saturating_sub(self.since));
        }
        if to == ThrottleState::Stop {
            c.stops += 1;
        }
        self.state = to;
        self.since = now;
        true
    }

    /// Time outside `Open` up to `now`.
    pub fn throttled(&self, now: Duration) -> Duration {
        if self.state == ThrottleState::Open {
            self.throttled
        } else {
            self.throttled
                .saturating_add(now.saturating_sub(self.since))
        }
    }
}

/// One-per-second samples of the cumulative counters, for the 10 s averages
/// the status shows (not control inputs).
#[derive(Debug, Default)]
pub(crate) struct Meter {
    samples: VecDeque<(Duration, u64, u64)>,
}

impl Meter {
    /// Takes a sample when a second passed since the last one.
    pub fn sample(&mut self, now: Duration, c: &Counters) {
        if self
            .samples
            .back()
            .is_some_and(|(t, _, _)| now.saturating_sub(*t) < Duration::from_secs(1))
        {
            return;
        }
        self.samples.push_back((now, c.admitted, c.freed));
        while self
            .samples
            .front()
            .is_some_and(|(t, _, _)| now.saturating_sub(*t) > METER_WINDOW)
        {
            self.samples.pop_front();
        }
    }

    /// `(ingest, reclaim)` bytes per second over the window ending at `now`
    /// (the oldest sample within 10 s).
    pub fn rates(&self, now: Duration, c: &Counters) -> (u64, u64) {
        let Some(&(t, a, f)) = self.samples.front() else {
            return (0, 0);
        };
        let dt = now.saturating_sub(t).as_millis();
        if dt == 0 {
            return (0, 0);
        }
        let per_s = |d: u64| u64::try_from(u128::from(d) * 1000 / dt).unwrap_or(u64::MAX);
        (
            per_s(c.admitted.saturating_sub(a)),
            per_s(c.freed.saturating_sub(f)),
        )
    }
}

/// Fills the counter part of [`ThrottleFigures`].
pub(crate) fn figures(
    c: &Counters,
    clock: &StateClock,
    meter: &Meter,
    now: Duration,
    fulls: u64,
) -> ThrottleFigures {
    let (ingest_rate, reclaim_rate) = meter.rates(now, c);
    ThrottleFigures {
        state: clock.state,
        state_since_ms: u64::try_from(clock.since.as_millis()).unwrap_or(u64::MAX),
        delayed_writes: c.delayed_writes,
        delayed_ms: c.delayed_ms,
        delay_max_ms: c.delay_max_ms,
        delay_capped: c.delay_capped,
        bypassed_fua: c.bypassed_fua,
        enospc: c.enospc,
        stops: c.stops,
        fulls,
        throttled_ms: u64::try_from(clock.throttled(now).as_millis()).unwrap_or(u64::MAX),
        ingest_rate,
        reclaim_rate,
        ..ThrottleFigures::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIB: u64 = 1 << 20;

    #[test]
    fn the_curve_has_the_documented_values_with_the_defaults() {
        let c = ThrottleConfig::default();
        let soft = 1000 * MIB;
        let at = |x: u64| c.rate(soft * x / 100, soft).expect("in the band") / MIB;
        assert_eq!(at(90), 576);
        assert_eq!(at(75), 192);
        assert_eq!(at(50), 64);
        assert_eq!(at(25), 21);
    }

    #[test]
    fn the_curve_is_monotonic_unbounded_at_the_mark_and_minimal_at_the_limit() {
        let c = ThrottleConfig::default();
        let soft = 10_000 * MIB;
        let mut last = 0;
        for h in (0..soft).step_by(MIB as usize * 7) {
            let r = c.rate(h, soft).expect("band");
            assert!(r >= last, "the rate never falls as the headroom grows");
            last = r;
        }
        assert_eq!(c.rate(0, soft), Some(c.min_rate), "r_min at the hard mark");
        assert!(
            c.rate(soft - 1, soft).expect("band") > 1 << 40,
            "unbounded at the mark"
        );
        assert_eq!(c.rate(soft, soft), None);
        assert_eq!(c.rate(soft * 2, soft), None);
        // r_min is reached at about 6 % of the band.
        assert_eq!(c.rate(soft * 5 / 100, soft), Some(c.min_rate));
        assert!(c.rate(soft * 7 / 100, soft).expect("band") > c.min_rate);
    }

    #[test]
    fn r_mid_is_the_gc_rate_with_a_floor_of_16_mib() {
        let c = ThrottleConfig::default().with_gc_rate(1 << 20);
        assert_eq!(c.mid_rate, 16 * MIB);
        let c = ThrottleConfig::default().with_gc_rate(200 * MIB);
        assert_eq!(c.mid_rate, 200 * MIB);
        assert_eq!(c.rate(50, 100), Some(200 * MIB));
    }

    #[test]
    fn the_soft_mark_is_a_percentage_clamped_and_at_most_half_the_limit() {
        let c = ThrottleConfig::default();
        let gib = 1u64 << 30;
        assert_eq!(c.soft_mark(100 * gib), 15 * gib);
        assert_eq!(c.soft_mark(10 * gib), 3 * gib / 2);
        assert_eq!(c.soft_mark(4 * gib), gib, "the lower clamp");
        assert_eq!(c.soft_mark(1000 * gib), 64 * gib, "the upper clamp");
        assert_eq!(c.soft_mark(gib), gib / 2, "never more than half the limit");
        assert_eq!(c.soft_mark(0), 0);
        let off = ThrottleConfig {
            enabled: false,
            ..c
        };
        assert_eq!(off.soft_mark(100 * gib), 0);
        assert_eq!(off.rate(0, 15 * gib), None);
    }

    #[test]
    fn a_bucket_admits_rate_times_interval_within_one_capacity() {
        let rate = 8 * MIB;
        let mut b = Bucket::new();
        let mut now = Duration::from_secs(5);
        b.refill(now, rate);
        let rec = 64 << 10;
        let mut admitted = 0u64;
        let end = now + Duration::from_secs(20);
        while now < end {
            b.refill(now, rate);
            while b.has(rec) {
                b.take(rec);
                admitted += rec;
            }
            now += Duration::from_millis(10);
        }
        let want = rate * 20;
        let cap = (MAX_RECORD as u64).max(rate * BUCKET_MS / 1000);
        assert!(
            admitted.abs_diff(want) <= cap + rec,
            "{admitted} against {want}"
        );
    }

    #[test]
    fn the_debt_is_bounded_by_one_second_of_the_rate() {
        let rate = 4 * MIB;
        let mut b = Bucket::new();
        b.refill(Duration::ZERO, rate);
        for _ in 0..100 {
            b.overdraw(MIB, rate);
        }
        assert!(!b.has(1));
        // One second later the debt is paid and the bucket holds nothing
        // more than what accrued past it.
        b.refill(Duration::from_secs(1), rate);
        assert!(b.has(0));
        assert!(b.wait_for(MIB, rate) <= Duration::from_millis(251));
    }

    #[test]
    fn a_debt_taken_at_a_high_rate_is_clamped_when_the_rate_falls() {
        // Review m1: 300 MiB/s near the soft mark, 4 MiB/s lower down.
        let mut b = Bucket::new();
        b.refill(Duration::ZERO, 300 * MIB);
        for _ in 0..10 {
            b.overdraw(100 * MIB, 300 * MIB);
        }
        // The debt is 300 MiB; at the new rate it is clamped to one second
        // of it, so a 1 MiB request waits about 1.25 s, not 75 s.
        b.refill(Duration::from_millis(1), 4 * MIB);
        assert!(b.wait_for(MIB, 4 * MIB) <= Duration::from_millis(1300));
    }

    #[test]
    fn wait_for_is_the_time_the_deficit_takes() {
        let rate = 4 * MIB;
        let mut b = Bucket::new();
        b.refill(Duration::ZERO, rate);
        b.take(MAX_RECORD as u64);
        let w = b.wait_for(MIB, rate);
        assert!(
            w >= Duration::from_millis(249) && w <= Duration::from_millis(251),
            "{w:?}"
        );
        assert_eq!(Bucket::new().wait_for(0, rate), Duration::ZERO);
    }

    #[test]
    fn the_meter_averages_the_last_ten_seconds() {
        let mut m = Meter::default();
        let mut c = Counters::default();
        for s in 0..=20u64 {
            let now = Duration::from_secs(s);
            m.sample(now, &c);
            c.admitted += 100;
            c.freed += 40;
        }
        let (i, r) = m.rates(Duration::from_secs(21), &c);
        assert_eq!((i, r), (100, 40));
        assert_eq!(Meter::default().rates(Duration::from_secs(1), &c), (0, 0));
    }

    #[test]
    fn the_state_clock_sums_the_time_outside_open() {
        let mut k = StateClock::new(Duration::ZERO);
        let mut c = Counters::default();
        assert!(!k.enter(ThrottleState::Open, Duration::from_secs(1), &mut c));
        assert!(k.enter(ThrottleState::Slowdown, Duration::from_secs(10), &mut c));
        assert!(k.enter(ThrottleState::Stop, Duration::from_secs(14), &mut c));
        assert!(k.enter(ThrottleState::Open, Duration::from_secs(20), &mut c));
        assert_eq!(c.stops, 1);
        assert_eq!(
            k.throttled(Duration::from_secs(99)),
            Duration::from_secs(10)
        );
        assert!(k.enter(ThrottleState::Slowdown, Duration::from_secs(30), &mut c));
        assert_eq!(
            k.throttled(Duration::from_secs(35)),
            Duration::from_secs(15)
        );
    }
}
