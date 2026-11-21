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
//! Before the band there is a lead zone: while the gate is open but the
//! headroom is below `soft + lead` (`lead` = the recent ingest times the
//! dead time of one reclaim, bounded), the collector gets the floor grace
//! only ([`Pressure::Lead`]), so garbage condemned earlier is deletable by
//! the time the headroom reaches the soft mark. The gate does not pace
//! there.
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

/// One collector pass period as the lead zone counts it: the daemon's
/// `busy_interval` (5 s, `GcConfig::daemon`), the time between passes while
/// garbage keeps coming.
pub const LEAD_PASS_PERIOD: Duration = Duration::from_secs(5);

/// Pass periods in one reclaim's dead time: a pass finds the garbage up to
/// one period after it died, the next one after its grace deletes it.
pub const LEAD_PASS_PERIODS: u32 = 2;

/// The lead zone is left only this far above its entry mark, as a divisor
/// of the lead (a margin of a quarter of the lead).
pub const LEAD_EXIT_MARGIN_DIVISOR: u64 = 4;

/// The step budget's multiple under pressure.
pub const BUDGET_FACTOR: u32 = 4;

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
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub enum Pressure {
    /// The configured budget, rate and grace.
    #[default]
    Normal,
    /// The lead zone, headroom below `soft + lead` with the gate still
    /// open: the floor grace, the budget and the rate unchanged.
    Lead,
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
            Self::Lead => "lead",
            Self::Raised => "raised",
            Self::Urgent => "urgent",
        }
    }

    /// The step budget at this pressure.
    #[must_use]
    pub fn budget(self, base: GcBudget) -> GcBudget {
        if matches!(self, Self::Normal | Self::Lead) {
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
        let grace = base.grace.min(grace_floor(&base));
        if self == Self::Lead {
            return GcParams { grace, ..base };
        }
        GcParams {
            grace,
            // A rate of 0 means "no compaction" and stays 0 (the volume's
            // choice outranks the pressure).
            rate_bytes_per_s: if base.rate_bytes_per_s == 0 {
                0
            } else if self == Self::Urgent {
                u64::MAX
            } else {
                base.rate_bytes_per_s.saturating_mul(rate_factor.max(1))
            },
            ..base
        }
    }
}

/// The grace floor: 10 s plus twice `read_timeout` (the urgent pass's rule).
#[must_use]
pub fn grace_floor(base: &GcParams) -> Duration {
    Duration::from_secs(10).saturating_add(base.read_timeout.saturating_mul(2))
}

/// The dead time of one reclaim at the grace the lead zone applies: the
/// floor grace (never longer than the configured one) plus
/// [`LEAD_PASS_PERIODS`] passes.
#[must_use]
pub fn lead_window(base: &GcParams) -> Duration {
    base.grace
        .min(grace_floor(base))
        .saturating_add(LEAD_PASS_PERIOD.saturating_mul(LEAD_PASS_PERIODS))
}

/// The lead in bytes of headroom: `ingest x window`, at most the soft mark
/// and a quarter of the limit; 0 for an idle writer (`ingest` 0) and with
/// the band off (`soft` 0), by the product and the bound.
#[must_use]
pub fn lead_bytes(ingest_rate: u64, window: Duration, soft: u64, limit: u64) -> u64 {
    let raw = u128::from(ingest_rate).saturating_mul(window.as_millis()) / 1000;
    u64::try_from(raw)
        .unwrap_or(u64::MAX)
        .min(soft)
        .min(limit / 4)
}

/// What the lead zone needs of the gate's snapshot.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Lead {
    /// `limit - used`.
    pub headroom: u64,
    /// The soft mark (0 with the band off).
    pub soft: u64,
    /// The lead ([`lead_bytes`]); 0 turns the zone off.
    pub bytes: u64,
}

impl Lead {
    /// No lead zone.
    pub const NONE: Self = Self {
        headroom: 0,
        soft: 0,
        bytes: 0,
    };

    /// From a snapshot and the collector's settings (no settings, no lead).
    #[must_use]
    pub fn of(snap: &SpaceSnapshot, params: Option<&GcParams>) -> Self {
        let t = &snap.throttle;
        let bytes = params.map_or(0, |p| {
            lead_bytes(t.ingest_rate, lead_window(p), t.soft, snap.limit)
        });
        Self {
            headroom: t.headroom,
            soft: t.soft,
            bytes,
        }
    }

    /// The headroom below which the zone is entered.
    fn entry(self) -> u64 {
        self.soft.saturating_add(self.bytes)
    }

    /// The headroom below which a zone already entered is kept.
    fn exit(self) -> u64 {
        self.entry()
            .saturating_add(self.bytes / LEAD_EXIT_MARGIN_DIVISOR)
    }
}

/// The pressure from the gate's state, with the hold.
#[derive(Debug, Default)]
pub struct PressureTracker {
    level: Pressure,
    /// Since when a lower level than the current one has been wanted.
    low_since: Option<Duration>,
}

impl PressureTracker {
    /// The pressure at `now` given the gate's `state` and the lead zone
    /// (`lead`): it rises at once; it falls to a lower level once that
    /// level has been wanted for [`HOLD`] (the exception, `Urgent` to
    /// `Raised`, is at once). A volume that rests inside the band with no
    /// writer (`active` false: nothing admitted in the last 10 s, nothing
    /// waiting) counts as open, so the pressure ends there too (review m4);
    /// `stop` and `full` stay urgent, since writers are waiting or failing.
    /// While the gate is open the lead zone is entered below `soft + lead`
    /// and left above that mark plus a quarter of the lead.
    pub fn observe(
        &mut self,
        state: ThrottleState,
        active: bool,
        lead: Lead,
        now: Duration,
    ) -> Pressure {
        let in_zone = |was: bool| {
            lead.bytes > 0 && lead.headroom < if was { lead.exit() } else { lead.entry() }
        };
        let want = match state {
            ThrottleState::Open | ThrottleState::Slowdown if !active => Pressure::Normal,
            ThrottleState::Open if in_zone(self.level != Pressure::Normal) => Pressure::Lead,
            ThrottleState::Open => Pressure::Normal,
            ThrottleState::Slowdown => Pressure::Raised,
            ThrottleState::Stop | ThrottleState::Full => Pressure::Urgent,
        };
        let at_once =
            want >= self.level || (want == Pressure::Raised && self.level == Pressure::Urgent);
        if at_once {
            self.level = want;
            self.low_since = None;
        } else {
            let since = *self.low_since.get_or_insert(now);
            if now.saturating_sub(since) >= HOLD {
                self.level = want;
                self.low_since = None;
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
            Self::Lead => 1,
            Self::Raised => 2,
            Self::Urgent => 3,
        }
    }

    fn from_code(c: u8) -> Self {
        match c {
            0 => Self::Normal,
            1 => Self::Lead,
            2 => Self::Raised,
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
            let p = self
                .pressure_now()
                .params(p, self.cfg.gc_pressure_rate_factor);
            if self.gc_yields_to_ring() {
                yielding(p)
            } else {
                p
            }
        })
    }

    /// True while the collector yields the device to the cuts: a write waits
    /// for the journal ring and the space gate applies no pressure beyond
    /// the lead zone (milestone jstall; under pressure the volume needs the
    /// collector more). The lead zone changes only the grace, so the yield
    /// holds there.
    pub(crate) fn gc_yields_to_ring(&self) -> bool {
        if self.pressure_now() > Pressure::Lead {
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

    /// Reads the gate at a tick: sets the collector's pressure (the lead
    /// zone included) and writes
    /// the throttle's log lines.
    pub(crate) fn update_pressure(&self) {
        let snap = self.space.snapshot();
        let now = self.clock.monotonic();
        let lead = Lead::of(&snap, self.cfg.gc_params.as_ref());
        let level = lock(&self.pressure).observe(snap.throttle.state, active(&snap), lead, now);
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
            t.observe(ThrottleState::Open, true, Lead::NONE, secs(0)),
            Pressure::Normal
        );
        assert_eq!(
            t.observe(ThrottleState::Slowdown, true, Lead::NONE, secs(1)),
            Pressure::Raised
        );
        assert_eq!(
            t.observe(ThrottleState::Stop, true, Lead::NONE, secs(2)),
            Pressure::Urgent
        );
        assert_eq!(
            t.observe(ThrottleState::Full, true, Lead::NONE, secs(3)),
            Pressure::Urgent
        );
        assert_eq!(
            t.observe(ThrottleState::Slowdown, true, Lead::NONE, secs(4)),
            Pressure::Raised
        );
        // Open, but the hold has not run.
        assert_eq!(
            t.observe(ThrottleState::Open, true, Lead::NONE, secs(5)),
            Pressure::Raised
        );
        assert_eq!(
            t.observe(ThrottleState::Open, true, Lead::NONE, secs(9)),
            Pressure::Raised
        );
        assert_eq!(
            t.observe(ThrottleState::Open, true, Lead::NONE, secs(10)),
            Pressure::Normal
        );
        // A slowdown inside the hold restarts it.
        assert_eq!(
            t.observe(ThrottleState::Slowdown, true, Lead::NONE, secs(11)),
            Pressure::Raised
        );
        assert_eq!(
            t.observe(ThrottleState::Open, true, Lead::NONE, secs(12)),
            Pressure::Raised
        );
        assert_eq!(
            t.observe(ThrottleState::Slowdown, true, Lead::NONE, secs(15)),
            Pressure::Raised
        );
        assert_eq!(
            t.observe(ThrottleState::Open, true, Lead::NONE, secs(16)),
            Pressure::Raised
        );
        assert_eq!(
            t.observe(ThrottleState::Open, true, Lead::NONE, secs(20)),
            Pressure::Raised
        );
        assert_eq!(
            t.observe(ThrottleState::Open, true, Lead::NONE, secs(21)),
            Pressure::Normal
        );
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

    #[test]
    fn a_rate_of_zero_stays_zero_under_every_pressure() {
        let off = GcParams {
            rate_bytes_per_s: 0,
            ..params()
        };
        for p in [Pressure::Raised, Pressure::Urgent] {
            assert_eq!(p.params(off, 4).rate_bytes_per_s, 0, "{p:?}");
            assert_eq!(p.params(off, 4).grace, secs(30));
        }
    }

    #[test]
    fn a_volume_resting_inside_the_band_has_no_pressure_after_the_hold() {
        let mut t = PressureTracker::default();
        assert_eq!(
            t.observe(ThrottleState::Slowdown, true, Lead::NONE, secs(0)),
            Pressure::Raised
        );
        // The writer stops; the headroom stays under the soft mark.
        assert_eq!(
            t.observe(ThrottleState::Slowdown, false, Lead::NONE, secs(10)),
            Pressure::Raised
        );
        assert_eq!(
            t.observe(ThrottleState::Slowdown, false, Lead::NONE, secs(14)),
            Pressure::Raised
        );
        assert_eq!(
            t.observe(ThrottleState::Slowdown, false, Lead::NONE, secs(15)),
            Pressure::Normal
        );
        // A writer again: raised at once. Stop and full never decay.
        assert_eq!(
            t.observe(ThrottleState::Slowdown, true, Lead::NONE, secs(16)),
            Pressure::Raised
        );
        assert_eq!(
            t.observe(ThrottleState::Stop, false, Lead::NONE, secs(100)),
            Pressure::Urgent
        );
        assert_eq!(
            t.observe(ThrottleState::Full, false, Lead::NONE, secs(200)),
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

    // ------------------------------------------------------------ lead zone

    const GIB: u64 = 1 << 30;
    const MIB: u64 = 1 << 20;

    #[test]
    fn the_lead_window_is_the_floor_grace_plus_two_pass_periods() {
        // 10 s + 2 x 10 s read_timeout = 30 s, plus 2 x 5 s.
        assert_eq!(lead_window(&params()), secs(40));
        // The configured grace is the floor or longer: the floor counts.
        let slow = GcParams {
            read_timeout: secs(30),
            grace: secs(120),
            ..params()
        };
        assert_eq!(lead_window(&slow), secs(80), "70 s floor + 10 s");
        // A grace shorter than the floor is kept (as `Pressure::params`).
        let short = GcParams {
            grace: secs(20),
            ..params()
        };
        assert_eq!(lead_window(&short), secs(30));
    }

    #[test]
    fn the_lead_is_the_ingest_times_the_window() {
        // 100 MiB/s for 40 s, well under the soft mark and a quarter of the limit.
        assert_eq!(
            lead_bytes(100 * MIB, secs(40), 10 * GIB, 100 * GIB),
            4000 * MIB
        );
        // Linear in the ingest and in the window.
        assert_eq!(
            lead_bytes(50 * MIB, secs(40), 10 * GIB, 100 * GIB),
            2000 * MIB
        );
        assert_eq!(
            lead_bytes(100 * MIB, secs(20), 10 * GIB, 100 * GIB),
            2000 * MIB
        );
    }

    #[test]
    fn the_lead_is_zero_for_an_idle_writer_and_with_the_band_off() {
        assert_eq!(lead_bytes(0, secs(40), 10 * GIB, 100 * GIB), 0);
        assert_eq!(lead_bytes(100 * MIB, secs(40), 0, 100 * GIB), 0);
    }

    #[test]
    fn the_lead_is_bounded_by_the_soft_mark_and_a_quarter_of_the_limit() {
        // 1 GiB/s for 40 s would be 40 GiB: the soft mark caps it.
        assert_eq!(lead_bytes(GIB, secs(40), 6 * GIB, 100 * GIB), 6 * GIB);
        // A small limit (soft = limit / 2): a quarter of the limit caps it.
        assert_eq!(lead_bytes(GIB, secs(40), 4 * GIB, 8 * GIB), 2 * GIB);
        // Nothing overflows.
        assert_eq!(
            lead_bytes(u64::MAX, Duration::MAX, u64::MAX, u64::MAX),
            u64::MAX / 4
        );
    }

    #[test]
    fn the_lead_of_a_snapshot_uses_the_ingest_the_soft_mark_and_the_settings() {
        let mut s = snap(ThrottleState::Open);
        s.limit = 100 * GIB;
        s.throttle.ingest_rate = 100 * MIB;
        s.throttle.soft = 10 * GIB;
        s.throttle.headroom = 12 * GIB;
        let l = Lead::of(&s, Some(&params()));
        assert_eq!(
            l,
            Lead {
                headroom: 12 * GIB,
                soft: 10 * GIB,
                bytes: 4000 * MIB
            }
        );
        // No collector settings: no collector, no lead.
        assert_eq!(Lead::of(&s, None).bytes, 0);
    }

    fn lead(headroom: u64) -> Lead {
        Lead {
            headroom,
            soft: 1000,
            bytes: 400,
        }
    }

    #[test]
    fn the_lead_zone_starts_below_soft_plus_lead_and_only_while_the_gate_is_open() {
        let mut t = PressureTracker::default();
        let mut at = |h: u64, state, now| t.observe(state, true, lead(h), secs(now));
        // soft + lead = 1400: at the mark is outside, one below is inside.
        assert_eq!(at(1400, ThrottleState::Open, 0), Pressure::Normal);
        assert_eq!(at(1399, ThrottleState::Open, 1), Pressure::Lead);
        // The band is raised as ever, whatever the lead says.
        assert_eq!(at(999, ThrottleState::Slowdown, 2), Pressure::Raised);
        assert_eq!(at(10, ThrottleState::Stop, 3), Pressure::Urgent);
    }

    #[test]
    fn the_lead_zone_is_kept_up_to_a_quarter_of_the_lead_above_its_entry() {
        let mut t = PressureTracker::default();
        let mut at = |h: u64, now| t.observe(ThrottleState::Open, true, lead(h), secs(now));
        assert_eq!(at(1399, 0), Pressure::Lead);
        // The exit mark is 1400 + 100 = 1500: inside it the level stays for
        // ever, however long.
        for now in 1..=30 {
            assert_eq!(at(1499, now), Pressure::Lead, "{now}");
        }
        // Past it the level falls only after the hold.
        assert_eq!(at(1500, 31), Pressure::Lead);
        assert_eq!(at(1500, 35), Pressure::Lead);
        assert_eq!(at(1500, 36), Pressure::Normal);
        // And a volume at 1450 that was never in the zone does not enter it.
        assert_eq!(at(1450, 37), Pressure::Normal);
    }

    #[test]
    fn a_headroom_that_wobbles_around_the_mark_does_not_flap_the_level() {
        let mut t = PressureTracker::default();
        let mut at = |h: u64, now| t.observe(ThrottleState::Open, true, lead(h), secs(now));
        assert_eq!(at(1399, 0), Pressure::Lead);
        for now in 1..=20 {
            let h = if now % 2 == 0 { 1390 } else { 1480 };
            assert_eq!(at(h, now), Pressure::Lead, "{now}");
        }
        // Out for 3 s, in again: the hold starts over.
        assert_eq!(at(1600, 21), Pressure::Lead);
        assert_eq!(at(1600, 24), Pressure::Lead);
        assert_eq!(at(1450, 25), Pressure::Lead);
        assert_eq!(at(1600, 26), Pressure::Lead);
        assert_eq!(at(1600, 30), Pressure::Lead, "4 s into the second hold");
        assert_eq!(at(1600, 31), Pressure::Normal);
    }

    #[test]
    fn the_level_falls_from_the_band_to_the_lead_zone_after_the_hold() {
        let mut t = PressureTracker::default();
        let mut at = |h: u64, state, now| t.observe(state, true, lead(h), secs(now));
        assert_eq!(at(900, ThrottleState::Slowdown, 0), Pressure::Raised);
        // The gate is open again with the headroom still in the lead zone:
        // raised for the hold, then the lead level, not normal.
        assert_eq!(at(1100, ThrottleState::Open, 1), Pressure::Raised);
        assert_eq!(at(1100, ThrottleState::Open, 5), Pressure::Raised);
        assert_eq!(at(1100, ThrottleState::Open, 6), Pressure::Lead);
        // Stop falls to raised at once, to the lead zone after the hold.
        assert_eq!(at(10, ThrottleState::Stop, 7), Pressure::Urgent);
        assert_eq!(at(900, ThrottleState::Slowdown, 8), Pressure::Raised);
        assert_eq!(at(1100, ThrottleState::Open, 9), Pressure::Raised);
        assert_eq!(at(1100, ThrottleState::Open, 14), Pressure::Lead);
    }

    #[test]
    fn a_writer_that_stops_ends_the_lead_zone_after_the_hold() {
        // The lead is 0 for an idle writer (see `Lead::of`): the zone ends.
        let mut t = PressureTracker::default();
        assert_eq!(
            t.observe(ThrottleState::Open, true, lead(1100), secs(0)),
            Pressure::Lead
        );
        let idle = Lead {
            bytes: 0,
            ..lead(1100)
        };
        assert_eq!(
            t.observe(ThrottleState::Open, false, idle, secs(1)),
            Pressure::Lead
        );
        assert_eq!(
            t.observe(ThrottleState::Open, false, idle, secs(6)),
            Pressure::Normal
        );
    }

    #[test]
    fn the_lead_level_changes_the_grace_only() {
        let n = params();
        let l = Pressure::Lead.params(n, 4);
        assert_eq!(l.grace, secs(30), "10 s + 2 x read_timeout");
        assert_eq!(
            GcParams {
                grace: n.grace,
                ..l
            },
            n,
            "nothing else moves"
        );
        let base = GcBudget {
            max_duration: Duration::from_millis(50),
            max_bytes: 64 << 20,
        };
        assert_eq!(Pressure::Lead.budget(base), base);
        // Never longer than the configured grace.
        let short = GcParams {
            grace: secs(20),
            ..n
        };
        assert_eq!(Pressure::Lead.params(short, 4).grace, secs(20));
    }

    #[test]
    fn the_levels_are_ordered_and_their_names_are_the_status_names() {
        assert!(Pressure::Normal < Pressure::Lead);
        assert!(Pressure::Lead < Pressure::Raised);
        assert!(Pressure::Raised < Pressure::Urgent);
        assert_eq!(Pressure::Lead.name(), "lead");
        for p in [
            Pressure::Normal,
            Pressure::Lead,
            Pressure::Raised,
            Pressure::Urgent,
        ] {
            assert_eq!(Pressure::from_code(p.code()), p);
        }
    }
}
