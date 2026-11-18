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

/// The step budget's multiple under pressure.
pub const BUDGET_FACTOR: u32 = 4;

/// Transitions closer than this are coalesced into one log line.
pub const LOG_COALESCE: Duration = Duration::from_secs(5);

/// How often a throttled volume logs a status line.
pub const LOG_PERIOD: Duration = Duration::from_secs(30);

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

    /// The step budget at this pressure.
    #[must_use]
    pub fn budget(self, base: GcBudget) -> GcBudget {
        if self == Self::Normal {
            return base;
        }
        GcBudget {
            max_duration: base.max_duration.saturating_mul(BUDGET_FACTOR),
            max_bytes: base.max_bytes.saturating_mul(u64::from(BUDGET_FACTOR)),
        }
    }

    /// The collector's settings at this pressure: the compaction rate times
    /// `rate_factor` (uncapped when urgent) and the grace at the floor (10 s
    /// plus twice `read_timeout`, the urgent pass's rule), never longer than
    /// configured.
    #[must_use]
    pub fn params(self, base: GcParams, rate_factor: u64) -> GcParams {
        if self == Self::Normal {
            return base;
        }
        let floor = Duration::from_secs(10).saturating_add(base.read_timeout.saturating_mul(2));
        GcParams {
            grace: base.grace.min(floor),
            rate_bytes_per_s: if self == Self::Urgent {
                u64::MAX
            } else {
                base.rate_bytes_per_s.saturating_mul(rate_factor.max(1))
            },
            ..base
        }
    }
}

/// The pressure from the gate's state, with the hold.
#[derive(Debug, Default)]
pub struct PressureTracker {
    level: Pressure,
    open_since: Option<Duration>,
}

impl PressureTracker {
    /// The pressure at `now` given the gate's `state`: it rises at once and
    /// falls to `Normal` once the gate has been open for [`HOLD`].
    pub fn observe(&mut self, state: ThrottleState, now: Duration) -> Pressure {
        let want = match state {
            ThrottleState::Open => Pressure::Normal,
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
        self.cfg.gc_budget.map(|b| self.pressure_now().budget(b))
    }

    /// The collector's settings now.
    pub(crate) fn gc_params_now(&self) -> Option<GcParams> {
        self.cfg.gc_params.map(|p| {
            self.pressure_now()
                .params(p, self.cfg.gc_pressure_rate_factor)
        })
    }

    /// Reads the gate at a tick: sets the collector's pressure and writes
    /// the throttle's log lines.
    pub(crate) fn update_pressure(&self) {
        let snap = self.space.snapshot();
        let now = self.clock.monotonic();
        let level = lock(&self.pressure).observe(snap.throttle.state, now);
        self.pressure_level.store(level.code(), Ordering::Release);
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
    fn pressure_rises_at_once_and_falls_after_the_hold() {
        let mut t = PressureTracker::default();
        assert_eq!(t.observe(ThrottleState::Open, secs(0)), Pressure::Normal);
        assert_eq!(
            t.observe(ThrottleState::Slowdown, secs(1)),
            Pressure::Raised
        );
        assert_eq!(t.observe(ThrottleState::Stop, secs(2)), Pressure::Urgent);
        assert_eq!(t.observe(ThrottleState::Full, secs(3)), Pressure::Urgent);
        assert_eq!(
            t.observe(ThrottleState::Slowdown, secs(4)),
            Pressure::Raised
        );
        // Open, but the hold has not run.
        assert_eq!(t.observe(ThrottleState::Open, secs(5)), Pressure::Raised);
        assert_eq!(t.observe(ThrottleState::Open, secs(9)), Pressure::Raised);
        assert_eq!(t.observe(ThrottleState::Open, secs(10)), Pressure::Normal);
        // A slowdown inside the hold restarts it.
        assert_eq!(
            t.observe(ThrottleState::Slowdown, secs(11)),
            Pressure::Raised
        );
        assert_eq!(t.observe(ThrottleState::Open, secs(12)), Pressure::Raised);
        assert_eq!(
            t.observe(ThrottleState::Slowdown, secs(15)),
            Pressure::Raised
        );
        assert_eq!(t.observe(ThrottleState::Open, secs(16)), Pressure::Raised);
        assert_eq!(t.observe(ThrottleState::Open, secs(20)), Pressure::Raised);
        assert_eq!(t.observe(ThrottleState::Open, secs(21)), Pressure::Normal);
    }

    #[test]
    fn the_budget_and_the_params_follow_the_pressure() {
        let base = GcBudget {
            max_duration: Duration::from_millis(50),
            max_bytes: 64 << 20,
        };
        assert_eq!(Pressure::Normal.budget(base), base);
        for p in [Pressure::Raised, Pressure::Urgent] {
            let b = p.budget(base);
            assert_eq!(b.max_duration, Duration::from_millis(200));
            assert_eq!(b.max_bytes, 256 << 20);
        }
        let n = params();
        assert_eq!(Pressure::Normal.params(n, 4), n);
        let r = Pressure::Raised.params(n, 4);
        assert_eq!(r.rate_bytes_per_s, 256 << 20);
        assert_eq!(r.grace, secs(30), "10 s + 2 x read_timeout");
        assert_eq!(r.retain_last, 2);
        let u = Pressure::Urgent.params(n, 4);
        assert_eq!(u.rate_bytes_per_s, u64::MAX, "uncapped at the limit");
        assert_eq!(u.grace, secs(30));
        // The floor grace is never longer than the configured one.
        let short = GcParams {
            grace: secs(20),
            ..n
        };
        assert_eq!(Pressure::Raised.params(short, 4).grace, secs(20));
        // A configured read_timeout moves the floor.
        let slow = GcParams {
            read_timeout: secs(30),
            ..n
        };
        assert_eq!(Pressure::Raised.params(slow, 4).grace, secs(60));
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
}
