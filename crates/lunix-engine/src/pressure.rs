//! GC pressure and the throttle's log lines (ADR-32, the note's sections
//! 5.6 and 7): what the engine does with the gate's state at each tick.
//!
//! The collector gets more while writes are slowed or held: four times the
//! step budget, `gc_rate` times `gc_pressure_rate_factor` (uncapped at the
//! limit) and the floor grace instead of `gc_grace_ms`, which is the urgent
//! pass's concession made per step (`GcBudget` and `GcParams` are built per
//! step already, so the `Collector` trait does not change). Pressure ends
//! with hysteresis: the engine returns to the normal figures after the gate
//! has been open for [`HOLD`].
//!
//! **Sizing (milestone gate).** The factors above are floors. Inside the band
//! the collector is sized to the writers: the compaction work wanted is the
//! ingest rate times the write amplification (`gc_write_amp_percent`, 200 by
//! default; 1.5 to 1.7 was measured), raised from 1 to 2 times as the
//! headroom falls from the soft mark to the limit ([`gc_demand`]). The step
//! budget and the compaction rate cap are the larger of the factors and
//! that demand, so in steady overload the reclaim rate meets the admitted
//! rate and the band reaches an equilibrium instead of ending in `stop`.
//!
//! Both pieces here are plain state machines: the engine feeds them the
//! gate's snapshot and the clock and acts on what they return.

use std::time::Duration;

use std::sync::atomic::Ordering;

use lunix_iface::gc::{GcBudget, GcParams};
use lunix_iface::space::{SpaceSnapshot, ThrottleState};

use crate::engine::{lock, Inner};

/// How long the gate must stay open before the collector goes back to its
/// normal figures.
pub const HOLD: Duration = Duration::from_secs(5);

/// The step budget's multiple under pressure (the floor; see [`gc_demand`]).
pub const BUDGET_FACTOR: u32 = 4;

/// The largest multiple the sizing may give the step budget's bytes.
pub const MAX_BUDGET_FACTOR: u64 = 64;

/// The largest multiple of a step's time budget (a step holds the
/// coordinator; the bytes of a larger budget are got from more steps, see
/// [`SHARE_UNDER_PRESSURE`]).
pub const MAX_TIME_FACTOR: u64 = 8;

/// The collector stops yielding to ring-parked writes (milestone jstall) when
/// the headroom is below this multiple of the soft mark.
pub const NEAR_BAND_MULTIPLE: u64 = 2;

/// The default write amplification, percent: compaction bytes (read and
/// write) per byte ingested.
pub const DEFAULT_WRITE_AMP_PERCENT: u64 = 200;

/// The share of a cut's time the collector gets after it while a gate
/// pressure holds (an outstanding third otherwise): equal to the cut, so the
/// collector gets about half of the coordinator.
pub const SHARE_UNDER_PRESSURE: u32 = 1;

/// Transitions closer than this are coalesced into one log line.
pub const LOG_COALESCE: Duration = Duration::from_secs(5);

/// How often a throttled volume logs a status line.
pub const LOG_PERIOD: Duration = Duration::from_secs(30);

/// The compaction rate while writes wait for the journal ring, as a divisor
/// of the configured rate (milestone jstall).
pub const YIELD_RATE_DIVISOR: u64 = 8;

/// The least compaction rate while writes wait for the ring: the collector
/// is slowed, never stopped (a rate of 0 stays 0).
pub const YIELD_RATE_FLOOR: u64 = 4 << 20;

/// The collector's settings while it yields the device to the cuts: the
/// compaction rate is an eighth of the configured one, at least 4 MiB/s and
/// never above the configured rate; 0 (no compaction) stays 0.
#[must_use]
pub fn yielding(base: GcParams) -> GcParams {
    let rate = base.rate_bytes_per_s;
    GcParams {
        rate_bytes_per_s: if rate == 0 {
            0
        } else {
            rate.min((rate / YIELD_RATE_DIVISOR).max(YIELD_RATE_FLOOR))
        },
        ..base
    }
}

/// How long the collector keeps yielding after the last write that waited
/// for the ring, milliseconds (milestone jstall: no flip-flop of the rate).
pub const YIELD_HOLD_MS: u64 = 2_000;

/// What the collector is getting.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum Pressure {
    /// The configured budget, rate and grace.
    #[default]
    Normal,
    /// Writes are paced (`slowdown`): 4 x the budget, the rate times the
    /// factor, the floor grace.
    Raised,
    /// Writes wait or fail for space (`stop`, `full`): as raised, with the
    /// compaction rate uncapped, and the ladder's urgent pass as well.
    Urgent,
}

impl Pressure {
    /// The name the status prints.
    pub fn name(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Raised => "raised",
            Self::Urgent => "urgent",
        }
    }

    /// The step budget at this pressure: four times the base, or more when
    /// `demand` (the compaction rate the writers call for, [`gc_demand`])
    /// is more than four times `base_rate`. The bytes scale with the whole
    /// multiple, the time with at most [`MAX_TIME_FACTOR`].
    #[must_use]
    pub fn budget(self, base: GcBudget, base_rate: u64, demand: u64) -> GcBudget {
        if self == Self::Normal {
            return base;
        }
        let want = if base_rate == 0 {
            0
        } else {
            demand.div_ceil(base_rate)
        };
        let f = want.clamp(u64::from(BUDGET_FACTOR), MAX_BUDGET_FACTOR);
        GcBudget {
            max_duration: base
                .max_duration
                .saturating_mul(u32::try_from(f.min(MAX_TIME_FACTOR)).unwrap_or(u32::MAX)),
            max_bytes: base.max_bytes.saturating_mul(f),
        }
    }

    /// The collector's settings at this pressure: the compaction rate the
    /// larger of `rate_factor` times the base and `demand` (uncapped when
    /// urgent) and the grace at the floor (10 s plus twice `read_timeout`,
    /// the urgent pass's rule), never longer than configured.
    #[must_use]
    pub fn params(self, base: GcParams, rate_factor: u64, demand: u64) -> GcParams {
        if self == Self::Normal {
            return base;
        }
        let floor = Duration::from_secs(10).saturating_add(base.read_timeout.saturating_mul(2));
        GcParams {
            grace: base.grace.min(floor),
            // A rate of 0 means "no compaction" and stays 0 (the volume's
            // choice outranks the pressure).
            rate_bytes_per_s: if base.rate_bytes_per_s == 0 {
                0
            } else if self == Self::Urgent {
                u64::MAX
            } else {
                base.rate_bytes_per_s
                    .saturating_mul(rate_factor.max(1))
                    .max(demand)
            },
            ..base
        }
    }
}

/// The compaction work the writers call for, bytes per second (read plus
/// write): the ingest rate times `amp_percent` / 100, times a factor that
/// rises from 1 at the soft mark to 2 at the limit (the backlog is worked
/// off as the band narrows). 0 with the band off, or outside it (nothing is
/// being paced, so nothing is asked beyond the base figures).
#[must_use]
pub fn gc_demand(snap: &SpaceSnapshot, amp_percent: u64) -> u64 {
    let t = &snap.throttle;
    if t.soft == 0 || t.headroom >= t.soft || t.state == ThrottleState::Open {
        return 0;
    }
    // x = headroom / soft in 0..1; the factor is 2 - x.
    let pos = u128::from(t.headroom.min(t.soft)) * 1000 / u128::from(t.soft);
    let milli = 2000 - pos;
    let d = u128::from(t.ingest_rate) * u128::from(amp_percent) * milli / 100_000;
    u64::try_from(d).unwrap_or(u64::MAX)
}

/// The pressure from the gate's state, with the hold.
#[derive(Debug, Default)]
pub struct PressureTracker {
    level: Pressure,
    open_since: Option<Duration>,
}

impl PressureTracker {
    /// The pressure at `now` given the gate's `state`: it rises at once and
    /// falls to `Normal` once the gate has been open for [`HOLD`]. A volume
    /// that rests inside the band with no writer (`active` false: nothing
    /// admitted in the last 10 s, nothing waiting) counts as open, so the
    /// pressure ends there too (review m4); `stop` and `full` stay urgent,
    /// since writers are waiting or failing.
    pub fn observe(&mut self, state: ThrottleState, active: bool, now: Duration) -> Pressure {
        let want = match state {
            ThrottleState::Open => Pressure::Normal,
            ThrottleState::Slowdown if !active => Pressure::Normal,
            ThrottleState::Slowdown => Pressure::Raised,
            ThrottleState::Stop | ThrottleState::Full => Pressure::Urgent,
        };
        if want != Pressure::Normal {
            self.level = want;
            self.open_since = None;
        } else if self.level != Pressure::Normal {
            let since = *self.open_since.get_or_insert(now);
            if now.saturating_sub(since) >= HOLD {
                self.level = Pressure::Normal;
                self.open_since = None;
            }
        }
        self.level
    }

    /// The level as of the last observation.
    pub fn level(&self) -> Pressure {
        self.level
    }
}

/// One log line the throttle owes (the engine renders it with `tracing`).
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum LogLine {
    /// INFO `write throttle on`.
    On {
        headroom: u64,
        soft: u64,
        limit: u64,
        rate: u64,
        ingest_rate: u64,
        reclaim_rate: u64,
    },
    /// INFO `write throttle`, every 30 s outside `open`.
    Periodic {
        state: ThrottleState,
        rate: u64,
        headroom: u64,
        delayed_writes: u64,
        delay_max_ms: u64,
        gc_pressure: Pressure,
    },
    /// INFO `write throttle off`.
    Off {
        throttled_ms: u64,
        delayed_writes: u64,
        delayed_ms: u64,
        delay_max_ms: u64,
    },
    /// WARN `writes wait for space`.
    Stop { waiters: u64, limit: u64, used: u64 },
    /// ERROR `volume full`.
    Full { used: u64, limit: u64 },
    /// INFO `volume no longer full`.
    NotFull {
        headroom: u64,
        full_ms: u64,
        enospc: u64,
    },
}

/// True while writers are active: bytes admitted in the last 10 s, or
/// requests waiting.
pub(crate) fn active(snap: &SpaceSnapshot) -> bool {
    snap.throttle.ingest_rate > 0 || snap.waiters > 0
}

/// Decides which lines a sequence of snapshots owes: one per state change,
/// changes closer than [`LOG_COALESCE`] to the last line merged into the
/// next one that is due, and a status line every [`LOG_PERIOD`] outside
/// `open`.
#[derive(Debug, Default)]
pub struct ThrottleWatch {
    /// The state the last transition line was written for.
    logged: ThrottleState,
    last_line: Option<Duration>,
    last_periodic: Option<Duration>,
    /// When the volume was last seen latched full, for `full_ms`.
    full_since: Option<Duration>,
    /// Latches already written a line for (the gate's `fulls`).
    fulls_logged: u64,
}

impl ThrottleWatch {
    /// The lines owed at `now` for `snap` under `pressure`.
    pub fn observe(
        &mut self,
        snap: &SpaceSnapshot,
        pressure: Pressure,
        now: Duration,
    ) -> Vec<LogLine> {
        let t = &snap.throttle;
        let to = t.state;
        let mut out = Vec::new();
        // The error line of a latch is never merged away: not by a line
        // written a moment before, nor by a latch that closes again before
        // the next transition line is due (review m5).
        if t.fulls > self.fulls_logged {
            self.fulls_logged = t.fulls;
            out.push(LogLine::Full {
                used: snap.used(),
                limit: snap.limit,
            });
            self.full_since = Some(now);
            self.logged = ThrottleState::Full;
            self.last_line = Some(now);
            self.last_periodic = Some(now);
            if to == ThrottleState::Full {
                return out;
            }
        }
        if to == ThrottleState::Full && self.full_since.is_none() {
            self.full_since = Some(now);
        }
        let due = self
            .last_line
            .is_none_or(|l| now.saturating_sub(l) >= LOG_COALESCE);
        if to != self.logged && due {
            let from = self.logged;
            if to == ThrottleState::Open {
                if from == ThrottleState::Full {
                    out.push(self.not_full(snap, now));
                }
                out.push(LogLine::Off {
                    throttled_ms: t.throttled_ms,
                    delayed_writes: t.delayed_writes,
                    delayed_ms: t.delayed_ms,
                    delay_max_ms: t.delay_max_ms,
                });
            } else {
                if from == ThrottleState::Full {
                    out.push(self.not_full(snap, now));
                }
                match to {
                    ThrottleState::Slowdown if from == ThrottleState::Open => {
                        out.push(LogLine::On {
                            headroom: t.headroom,
                            soft: t.soft,
                            limit: snap.limit,
                            rate: t.rate.unwrap_or(0),
                            ingest_rate: t.ingest_rate,
                            reclaim_rate: t.reclaim_rate,
                        });
                    }
                    ThrottleState::Stop => out.push(LogLine::Stop {
                        waiters: t.space_waiters,
                        limit: snap.limit,
                        used: snap.used(),
                    }),
                    ThrottleState::Full => out.push(LogLine::Full {
                        used: snap.used(),
                        limit: snap.limit,
                    }),
                    _ => {}
                }
            }
            self.logged = to;
            self.last_line = Some(now);
            self.last_periodic = Some(now);
        } else if to != ThrottleState::Open
            && !(to == ThrottleState::Slowdown && !active(snap))
            && to == self.logged
            && self
                .last_periodic
                .is_none_or(|l| now.saturating_sub(l) >= LOG_PERIOD)
        {
            self.last_periodic = Some(now);
            out.push(LogLine::Periodic {
                state: to,
                rate: t.rate.unwrap_or(0),
                headroom: t.headroom,
                delayed_writes: t.delayed_writes,
                delay_max_ms: t.delay_max_ms,
                gc_pressure: pressure,
            });
        }
        if to != ThrottleState::Full && self.logged != ThrottleState::Full {
            self.full_since = None;
        }
        out
    }

    fn not_full(&mut self, snap: &SpaceSnapshot, now: Duration) -> LogLine {
        let since = self.full_since.take().unwrap_or(now);
        LogLine::NotFull {
            headroom: snap.throttle.headroom,
            full_ms: u64::try_from(now.saturating_sub(since).as_millis()).unwrap_or(u64::MAX),
            enospc: snap.throttle.enospc,
        }
    }
}

impl Pressure {
    fn code(self) -> u8 {
        match self {
            Self::Normal => 0,
            Self::Raised => 1,
            Self::Urgent => 2,
        }
    }

    fn from_code(c: u8) -> Self {
        match c {
            0 => Self::Normal,
            1 => Self::Raised,
            _ => Self::Urgent,
        }
    }
}

impl Inner {
    /// The pressure as of the last tick.
    pub(crate) fn pressure_now(&self) -> Pressure {
        Pressure::from_code(self.pressure_level.load(Ordering::Acquire))
    }

    /// The collector's step budget now.
    pub(crate) fn gc_budget_now(&self) -> Option<GcBudget> {
        let base_rate = self.cfg.gc_params.map_or(0, |p| p.rate_bytes_per_s);
        let demand = self.gc_demand.load(Ordering::Acquire);
        self.cfg
            .gc_budget
            .map(|b| self.pressure_now().budget(b, base_rate, demand))
    }

    /// How much of a cut's time the collector gets after it: a third, or all
    /// of it (and a longer cap) while a gate pressure holds.
    pub(crate) fn gc_share_divisor(&self) -> u32 {
        if self.pressure_now() == Pressure::Normal {
            crate::coord::GC_SHARE_DIVISOR
        } else {
            SHARE_UNDER_PRESSURE
        }
    }

    /// The collector's settings now.
    pub(crate) fn gc_params_now(&self) -> Option<GcParams> {
        self.cfg.gc_params.map(|p| {
            let p = self.pressure_now().params(
                p,
                self.cfg.gc_pressure_rate_factor,
                self.gc_demand.load(Ordering::Acquire),
            );
            if self.gc_yields_to_ring() {
                yielding(p)
            } else {
                p
            }
        })
    }

    /// True while the collector yields the device to the cuts: a write waits
    /// for the journal ring and the space gate applies no pressure (milestone
    /// jstall; under pressure the volume needs the collector more).
    pub(crate) fn gc_yields_to_ring(&self) -> bool {
        // Also not within twice the soft mark of the band: the collector
        // must not be slowed to an eighth just before the volume needs it at
        // the ingest rate, or a ring-bound writer carries the volume into the
        // band and the `stop` with a collector that yielded its way there.
        if self.pressure_now() != Pressure::Normal || self.near_band.load(Ordering::Acquire) {
            return false;
        }
        let now = u64::try_from(self.clock.monotonic().as_millis()).unwrap_or(u64::MAX);
        if self.cuts.ring_waiters.load(Ordering::Relaxed) > 0 {
            // The switch holds for `YIELD_HOLD` after the last waiter, so the
            // compaction rate does not flip between two steps.
            self.cuts
                .yield_until_ms
                .store(now.saturating_add(YIELD_HOLD_MS), Ordering::Relaxed);
            return true;
        }
        now < self.cuts.yield_until_ms.load(Ordering::Relaxed)
    }

    /// Reads the gate at a tick: sets the collector's pressure and writes
    /// the throttle's log lines.
    pub(crate) fn update_pressure(&self) {
        let snap = self.space.snapshot();
        let now = self.clock.monotonic();
        let level = lock(&self.pressure).observe(snap.throttle.state, active(&snap), now);
        self.pressure_level.store(level.code(), Ordering::Release);
        let demand = if level == Pressure::Normal {
            0
        } else {
            gc_demand(&snap, self.cfg.gc_write_amp_percent)
        };
        self.gc_demand.store(demand, Ordering::Release);
        let t = &snap.throttle;
        self.near_band.store(
            t.soft > 0 && t.headroom < t.soft.saturating_mul(NEAR_BAND_MULTIPLE),
            Ordering::Release,
        );
        let lines = lock(&self.throttle_watch).observe(&snap, level, now);
        for l in lines {
            self.log_throttle(&l);
        }
    }

    fn log_throttle(&self, l: &LogLine) {
        match *l {
            LogLine::On {
                headroom,
                soft,
                limit,
                rate,
                ingest_rate,
                reclaim_rate,
            } => tracing::info!(
                headroom,
                soft,
                limit,
                rate,
                ingest_rate,
                reclaim_rate,
                "write throttle on"
            ),
            LogLine::Periodic {
                state,
                rate,
                headroom,
                delayed_writes,
                delay_max_ms,
                gc_pressure,
            } => tracing::info!(
                state = state.name(),
                rate,
                headroom,
                delayed_writes,
                delay_max_ms,
                gc_pressure = gc_pressure.name(),
                "write throttle"
            ),
            LogLine::Off {
                throttled_ms,
                delayed_writes,
                delayed_ms,
                delay_max_ms,
            } => tracing::info!(
                throttled_ms,
                delayed_writes,
                delayed_ms,
                delay_max_ms,
                "write throttle off"
            ),
            LogLine::Stop {
                waiters,
                limit,
                used,
            } => tracing::warn!(
                waiters,
                limit,
                used,
                full_timeout_ms =
                    u64::try_from(self.full.full_timeout().as_millis()).unwrap_or(u64::MAX),
                "writes wait for space"
            ),
            LogLine::Full { used, limit } => {
                let st = lock(&self.gc_status);
                tracing::error!(
                    used,
                    limit,
                    live_bytes = st.stats.live_bytes,
                    condemned_pending = st.condemned_pending,
                    failed = self.is_failed(),
                    "volume full: writes fail with ENOSPC until space is freed"
                );
            }
            LogLine::NotFull {
                headroom,
                full_ms,
                enospc,
            } => tracing::info!(headroom, full_ms, enospc, "volume no longer full"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lunix_format::IdLen;
    use lunix_iface::space::ThrottleFigures;

    fn secs(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    fn snap(state: ThrottleState) -> SpaceSnapshot {
        SpaceSnapshot {
            limit: 1000,
            throttle: ThrottleFigures {
                state,
                rate: Some(5),
                ingest_rate: 1,
                ..ThrottleFigures::default()
            },
            ..SpaceSnapshot::default()
        }
    }

    fn params() -> GcParams {
        GcParams {
            retain_last: 2,
            grace: secs(60),
            read_timeout: secs(10),
            rate_bytes_per_s: 64 << 20,
            live_threshold: 0.5,
            chunk_size: 65536,
            id_len: IdLen::Sixteen,
        }
    }

    #[test]
    fn a_yielding_collector_compacts_at_an_eighth_of_its_rate_never_below_the_floor() {
        let y = yielding(params());
        assert_eq!(y.rate_bytes_per_s, 8 << 20, "an eighth of 64 MiB/s");
        // The floor: an eighth of 16 MiB/s would be 2 MiB/s.
        let low = GcParams {
            rate_bytes_per_s: 16 << 20,
            ..params()
        };
        assert_eq!(yielding(low).rate_bytes_per_s, YIELD_RATE_FLOOR);
        // Never above what the volume configured.
        let slow = GcParams {
            rate_bytes_per_s: 1 << 20,
            ..params()
        };
        assert_eq!(yielding(slow).rate_bytes_per_s, 1 << 20);
        // No compaction stays none; nothing else changes.
        let none = GcParams {
            rate_bytes_per_s: 0,
            ..params()
        };
        assert_eq!(yielding(none).rate_bytes_per_s, 0);
        assert_eq!(
            GcParams {
                rate_bytes_per_s: 64 << 20,
                ..y
            },
            params()
        );
    }

    #[test]
    fn pressure_rises_at_once_and_falls_after_the_hold() {
        let mut t = PressureTracker::default();
        assert_eq!(
            t.observe(ThrottleState::Open, true, secs(0)),
            Pressure::Normal
        );
        assert_eq!(
            t.observe(ThrottleState::Slowdown, true, secs(1)),
            Pressure::Raised
        );
        assert_eq!(
            t.observe(ThrottleState::Stop, true, secs(2)),
            Pressure::Urgent
        );
        assert_eq!(
            t.observe(ThrottleState::Full, true, secs(3)),
            Pressure::Urgent
        );
        assert_eq!(
            t.observe(ThrottleState::Slowdown, true, secs(4)),
            Pressure::Raised
        );
        // Open, but the hold has not run.
        assert_eq!(
            t.observe(ThrottleState::Open, true, secs(5)),
            Pressure::Raised
        );
        assert_eq!(
            t.observe(ThrottleState::Open, true, secs(9)),
            Pressure::Raised
        );
        assert_eq!(
            t.observe(ThrottleState::Open, true, secs(10)),
            Pressure::Normal
        );
        // A slowdown inside the hold restarts it.
        assert_eq!(
            t.observe(ThrottleState::Slowdown, true, secs(11)),
            Pressure::Raised
        );
        assert_eq!(
            t.observe(ThrottleState::Open, true, secs(12)),
            Pressure::Raised
        );
        assert_eq!(
            t.observe(ThrottleState::Slowdown, true, secs(15)),
            Pressure::Raised
        );
        assert_eq!(
            t.observe(ThrottleState::Open, true, secs(16)),
            Pressure::Raised
        );
        assert_eq!(
            t.observe(ThrottleState::Open, true, secs(20)),
            Pressure::Raised
        );
        assert_eq!(
            t.observe(ThrottleState::Open, true, secs(21)),
            Pressure::Normal
        );
    }

    #[test]
    fn the_budget_and_the_params_follow_the_pressure() {
        let base = GcBudget {
            max_duration: Duration::from_millis(50),
            max_bytes: 64 << 20,
        };
        assert_eq!(Pressure::Normal.budget(base, 64 << 20, 0), base);
        for p in [Pressure::Raised, Pressure::Urgent] {
            let b = p.budget(base, 64 << 20, 0);
            assert_eq!(b.max_duration, Duration::from_millis(200));
            assert_eq!(b.max_bytes, 256 << 20);
        }
        let n = params();
        assert_eq!(Pressure::Normal.params(n, 4, 0), n);
        let r = Pressure::Raised.params(n, 4, 0);
        assert_eq!(r.rate_bytes_per_s, 256 << 20);
        assert_eq!(r.grace, secs(30), "10 s + 2 x read_timeout");
        assert_eq!(r.retain_last, 2);
        let u = Pressure::Urgent.params(n, 4, 0);
        assert_eq!(u.rate_bytes_per_s, u64::MAX, "uncapped at the limit");
        assert_eq!(u.grace, secs(30));
        // The floor grace is never longer than the configured one.
        let short = GcParams {
            grace: secs(20),
            ..n
        };
        assert_eq!(Pressure::Raised.params(short, 4, 0).grace, secs(20));
        // A configured read_timeout moves the floor.
        let slow = GcParams {
            read_timeout: secs(30),
            ..n
        };
        assert_eq!(Pressure::Raised.params(slow, 4, 0).grace, secs(60));
    }

    fn band(ingest: u64, headroom: u64, soft: u64) -> SpaceSnapshot {
        SpaceSnapshot {
            throttle: ThrottleFigures {
                state: ThrottleState::Slowdown,
                ingest_rate: ingest,
                headroom,
                soft,
                ..ThrottleFigures::default()
            },
            ..SpaceSnapshot::default()
        }
    }

    #[test]
    fn the_demand_is_the_ingest_times_the_amplification_rising_to_twice_at_the_limit() {
        let mib = 1u64 << 20;
        // At the soft mark's edge: 100 MiB/s x 2.0 = 200 MiB/s.
        let d = gc_demand(&band(100 * mib, 999 * mib, 1000 * mib), 200);
        assert!(d.abs_diff(200 * mib) <= mib, "{d}");
        // In the middle: 1.5 times that.
        let d = gc_demand(&band(100 * mib, 500 * mib, 1000 * mib), 200);
        assert_eq!(d, 300 * mib);
        // At the limit: twice.
        assert_eq!(gc_demand(&band(100 * mib, 0, 1000 * mib), 200), 400 * mib);
        // The amplification is the tunable: 1.5 measured-style.
        assert_eq!(gc_demand(&band(100 * mib, 0, 1000 * mib), 150), 300 * mib);
        // No ingest, no band, or the gate open: no demand.
        assert_eq!(gc_demand(&band(0, 0, 1000 * mib), 200), 0);
        assert_eq!(gc_demand(&band(100 * mib, 0, 0), 200), 0);
        assert_eq!(gc_demand(&band(100 * mib, 1000 * mib, 1000 * mib), 200), 0);
        let mut open = band(100 * mib, 10, 1000 * mib);
        open.throttle.state = ThrottleState::Open;
        assert_eq!(gc_demand(&open, 200), 0);
    }

    #[test]
    fn the_budget_and_the_rate_are_sized_to_the_demand_above_the_floors() {
        let mib = 1u64 << 20;
        let base = GcBudget {
            max_duration: Duration::from_millis(50),
            max_bytes: 64 * mib,
        };
        let n = params();
        // 1 GiB/s of demand against 64 MiB/s: 16 x the bytes, the time capped.
        let b = Pressure::Raised.budget(base, 64 * mib, 1024 * mib);
        assert_eq!(b.max_bytes, 16 * 64 * mib);
        assert_eq!(b.max_duration, Duration::from_millis(50 * MAX_TIME_FACTOR));
        assert_eq!(
            Pressure::Raised.params(n, 4, 1024 * mib).rate_bytes_per_s,
            1024 * mib
        );
        // Below the floors the floors hold.
        let b = Pressure::Raised.budget(base, 64 * mib, 10 * mib);
        assert_eq!(b.max_bytes, 4 * 64 * mib);
        assert_eq!(b.max_duration, Duration::from_millis(200));
        assert_eq!(
            Pressure::Raised.params(n, 4, 10 * mib).rate_bytes_per_s,
            256 * mib
        );
        // A hopeless demand is bounded.
        let b = Pressure::Raised.budget(base, 64 * mib, u64::MAX);
        assert_eq!(b.max_bytes, MAX_BUDGET_FACTOR * 64 * mib);
        // Normal ignores the demand; `no compaction` stays none.
        assert_eq!(Pressure::Normal.budget(base, 64 * mib, 1 << 40), base);
        let off = GcParams {
            rate_bytes_per_s: 0,
            ..n
        };
        assert_eq!(Pressure::Raised.params(off, 4, 1 << 40).rate_bytes_per_s, 0);
        assert_eq!(
            Pressure::Raised.budget(base, 0, 1 << 40).max_bytes,
            4 * 64 * mib
        );
    }

    #[test]
    fn one_line_per_transition_and_a_status_line_every_30_s() {
        let mut w = ThrottleWatch::default();
        let mut lines = |s: ThrottleState, t: u64| w.observe(&snap(s), Pressure::Raised, secs(t));
        assert!(lines(ThrottleState::Open, 0).is_empty());
        let on = lines(ThrottleState::Slowdown, 1);
        assert!(matches!(on.as_slice(), [LogLine::On { .. }]), "{on:?}");
        assert!(lines(ThrottleState::Slowdown, 10).is_empty());
        let per = lines(ThrottleState::Slowdown, 31);
        assert!(
            matches!(per.as_slice(), [LogLine::Periodic { .. }]),
            "{per:?}"
        );
        assert!(lines(ThrottleState::Slowdown, 40).is_empty());
        assert!(matches!(
            lines(ThrottleState::Stop, 45).as_slice(),
            [LogLine::Stop { .. }]
        ));
        assert!(matches!(
            lines(ThrottleState::Full, 60).as_slice(),
            [LogLine::Full { .. }]
        ));
        let off = lines(ThrottleState::Open, 100);
        assert!(
            matches!(
                off.as_slice(),
                [
                    LogLine::NotFull {
                        full_ms: 40_000,
                        ..
                    },
                    LogLine::Off { .. }
                ]
            ),
            "{off:?}"
        );
        assert!(lines(ThrottleState::Open, 200).is_empty());
    }

    #[test]
    fn transitions_closer_than_5_s_are_coalesced() {
        let mut w = ThrottleWatch::default();
        let mut lines = |s: ThrottleState, t: u64| w.observe(&snap(s), Pressure::Raised, secs(t));
        assert_eq!(lines(ThrottleState::Slowdown, 10).len(), 1);
        // Flaps inside 5 s of the last line write nothing ...
        assert!(lines(ThrottleState::Open, 11).is_empty());
        assert!(lines(ThrottleState::Slowdown, 12).is_empty());
        assert!(lines(ThrottleState::Open, 13).is_empty());
        // ... and the first observation that is due writes the state it sees.
        let off = lines(ThrottleState::Open, 16);
        assert!(matches!(off.as_slice(), [LogLine::Off { .. }]), "{off:?}");
        // A flap that is back where it was logged writes nothing.
        let mut w = ThrottleWatch::default();
        let mut lines = |s: ThrottleState, t: u64| w.observe(&snap(s), Pressure::Raised, secs(t));
        assert_eq!(lines(ThrottleState::Slowdown, 10).len(), 1);
        assert!(lines(ThrottleState::Open, 11).is_empty());
        assert!(lines(ThrottleState::Slowdown, 17).is_empty());
    }

    #[test]
    fn a_rate_of_zero_stays_zero_under_every_pressure() {
        let off = GcParams {
            rate_bytes_per_s: 0,
            ..params()
        };
        for p in [Pressure::Raised, Pressure::Urgent] {
            assert_eq!(p.params(off, 4, 0).rate_bytes_per_s, 0, "{p:?}");
            assert_eq!(p.params(off, 4, 0).grace, secs(30));
        }
    }

    #[test]
    fn a_volume_resting_inside_the_band_has_no_pressure_after_the_hold() {
        let mut t = PressureTracker::default();
        assert_eq!(
            t.observe(ThrottleState::Slowdown, true, secs(0)),
            Pressure::Raised
        );
        // The writer stops; the headroom stays under the soft mark.
        assert_eq!(
            t.observe(ThrottleState::Slowdown, false, secs(10)),
            Pressure::Raised
        );
        assert_eq!(
            t.observe(ThrottleState::Slowdown, false, secs(14)),
            Pressure::Raised
        );
        assert_eq!(
            t.observe(ThrottleState::Slowdown, false, secs(15)),
            Pressure::Normal
        );
        // A writer again: raised at once. Stop and full never decay.
        assert_eq!(
            t.observe(ThrottleState::Slowdown, true, secs(16)),
            Pressure::Raised
        );
        assert_eq!(
            t.observe(ThrottleState::Stop, false, secs(100)),
            Pressure::Urgent
        );
        assert_eq!(
            t.observe(ThrottleState::Full, false, secs(200)),
            Pressure::Urgent
        );
    }

    #[test]
    fn a_resting_band_volume_logs_no_status_line() {
        let mut w = ThrottleWatch::default();
        let mut idle = snap(ThrottleState::Slowdown);
        idle.throttle.ingest_rate = 0;
        assert_eq!(w.observe(&idle, Pressure::Normal, secs(1)).len(), 1);
        assert!(w.observe(&idle, Pressure::Normal, secs(100)).is_empty());
        assert!(w.observe(&idle, Pressure::Normal, secs(1000)).is_empty());
    }

    #[test]
    fn the_error_line_of_a_latch_is_never_merged_away() {
        let mut w = ThrottleWatch::default();
        let mut s = snap(ThrottleState::Slowdown);
        assert_eq!(w.observe(&s, Pressure::Raised, secs(10)).len(), 1);
        // A latch inside the 5 s window of that line: its line is written.
        s.throttle.state = ThrottleState::Full;
        s.throttle.fulls = 1;
        let l = w.observe(&s, Pressure::Urgent, secs(11));
        assert!(matches!(l.as_slice(), [LogLine::Full { .. }]), "{l:?}");
        // The latch closes at once, in the same window: the lines that
        // would follow wait, the latch's own line is out.
        s.throttle.state = ThrottleState::Slowdown;
        assert!(w.observe(&s, Pressure::Urgent, secs(12)).is_empty());
        // A second latch before they were due is written too.
        s.throttle.state = ThrottleState::Full;
        s.throttle.fulls = 2;
        let l = w.observe(&s, Pressure::Urgent, secs(13));
        assert!(matches!(l.as_slice(), [LogLine::Full { .. }]), "{l:?}");
        // A latch that opened and closed between two observations is seen
        // through the counter.
        s.throttle.state = ThrottleState::Slowdown;
        s.throttle.fulls = 3;
        let l = w.observe(&s, Pressure::Urgent, secs(14));
        assert!(matches!(l.as_slice(), [LogLine::Full { .. }]), "{l:?}");
        // The way out is written once it is due.
        let l = w.observe(&s, Pressure::Urgent, secs(20));
        assert!(matches!(l.as_slice(), [LogLine::NotFull { .. }]), "{l:?}");
    }
}
