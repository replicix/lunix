//! Policies: what the engine does with the gate's state at each tick, as a
//! trait. A candidate is a small `impl Policy`, or the `tunable` policy with a
//! parameter file.
//!
//! * `main`: the code of `origin/main` 1738f2e. The pressure level is the
//!   real [`PressureTracker`]; the figures of a level are the real
//!   [`Pressure::budget`] and [`Pressure::params`].
//! * `gate-gc`: part 2 of `p1/gate-gc` (6bc041c), ported from its diff: the
//!   collector sized to the ingest, a share of all of a cut, a lifted
//!   per-pass cap, no yield to the ring near the band.
//! * `leadzone`: `p1/leadzone` (9ed3b6d), ported from its report: a `Lead`
//!   level before the band that moves only the grace to the floor.

use std::time::Duration;

use anyhow::{bail, Result};
use lunix_engine::{Pressure, PressureTracker, HOLD};
use lunix_iface::gc::{GcBudget, GcParams};
use lunix_iface::space::{SpaceSnapshot, ThrottleState};
use lunix_space::ThrottleConfig;

/// The names of the built-in policies.
pub const POLICY_NAMES: &[&str] = &["main", "gate-gc", "leadzone", "tunable"];

/// The collector's pressure level as the status prints it.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum Level {
    /// The configured figures.
    Normal,
    /// The leadzone's level: the floor grace only.
    Lead,
    /// Writes are paced.
    Raised,
    /// Writes wait or fail.
    Urgent,
}

impl Level {
    /// The status name.
    pub fn name(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Lead => "lead",
            Self::Raised => "raised",
            Self::Urgent => "urgent",
        }
    }

    fn from_pressure(p: Pressure) -> Self {
        match p {
            Pressure::Normal => Self::Normal,
            Pressure::Raised => Self::Raised,
            Pressure::Urgent => Self::Urgent,
        }
    }

    fn pressure(self) -> Pressure {
        match self {
            Self::Normal | Self::Lead => Pressure::Normal,
            Self::Raised => Pressure::Raised,
            Self::Urgent => Pressure::Urgent,
        }
    }
}

/// What a policy reads at a tick.
pub struct PolicyInput<'a> {
    /// The gate's snapshot.
    pub snap: &'a SpaceSnapshot,
    /// The simulated clock.
    pub now: Duration,
    /// The configured collector settings.
    pub base_params: GcParams,
    /// The configured step budget.
    pub base_budget: GcBudget,
    /// `gc_pressure_rate_factor`.
    pub rate_factor: u64,
    /// `compact_per_pass`.
    pub compact_per_pass: u64,
}

/// What an admission hook reads: a candidate that paces in store bytes
/// (P1, P5, P6 of the candidate note) sits in front of the real gate.
pub struct AdmissionView<'a> {
    /// The simulated clock.
    pub now: Duration,
    /// The gate's snapshot.
    pub snap: &'a SpaceSnapshot,
    /// Store bytes written per journal byte cut, smoothed.
    pub a_hat: f64,
    /// Journal bytes admitted and not yet cut.
    pub journal_debt: u64,
    /// Time until the next condemned object's grace ends and a mark may
    /// delete it, if any is listed (the forecast of P5 and P6).
    pub next_delete_in: Option<Duration>,
}

/// What the collector gets until the next tick.
#[derive(Clone, Copy, Debug)]
pub struct GcSettings {
    /// The level, for the trace.
    pub level: Level,
    /// Rate, grace and the rest of the per-step parameters.
    pub params: GcParams,
    /// The step budget.
    pub budget: GcBudget,
    /// The share after a cut is the cut's time over this.
    pub share_divisor: u32,
    /// The longest share.
    pub share_max: Duration,
    /// Live bytes one scheduled pass may compact.
    pub per_pass_bytes: u64,
    /// True where the collector must not yield to ring-parked writes.
    pub no_yield: bool,
    /// The busy cadence of the passes (none: the collector's own).
    pub busy_interval: Option<Duration>,
    /// A pass is due when the store grew by this much since the last mark.
    pub growth_trigger: Option<f64>,
    /// The compaction threshold (G5: from the headroom); none: the volume's.
    pub compact_threshold: Option<f64>,
}

impl GcSettings {
    /// Plain settings for unit tests.
    pub fn for_tests() -> Self {
        Self {
            level: Level::Normal,
            params: GcParams {
                retain_last: 2,
                grace: Duration::from_secs(60),
                read_timeout: Duration::from_secs(10),
                rate_bytes_per_s: 64 << 20,
                live_threshold: 0.5,
                chunk_size: 65_536,
                id_len: lunix_format::IdLen::Sixteen,
            },
            budget: GcBudget {
                max_duration: Duration::from_millis(50),
                max_bytes: 64 << 20,
            },
            share_divisor: 3,
            share_max: Duration::from_secs(1),
            per_pass_bytes: 256 << 20,
            no_yield: false,
            busy_interval: None,
            growth_trigger: None,
            compact_threshold: None,
        }
    }
}

/// A policy: the gate's configuration and the collector's settings per tick.
pub trait Policy {
    /// The name.
    fn name(&self) -> &str;
    /// Sets one parameter of the policy (`policy.<key>` in a parameter
    /// file); unknown keys are an error.
    ///
    /// # Errors
    /// Unknown key or bad value.
    fn set(&mut self, key: &str, _val: &str) -> Result<()> {
        bail!("policy {} has no parameter {key:?}", self.name())
    }
    /// The gate's throttle configuration (the default: the scenario's).
    fn gate(&self, base: ThrottleConfig) -> ThrottleConfig {
        base
    }
    /// The collector's settings at this tick.
    fn settings(&mut self, inp: &PolicyInput<'_>) -> GcSettings;
    /// A cap on the admitted journal bytes per second, applied by the
    /// simulator in front of the real gate (the hook for candidates that
    /// change what the gate charges); `None`: no extra pace.
    fn admission_rate(&mut self, _view: &AdmissionView<'_>) -> Option<f64> {
        None
    }
}

/// A built-in policy by name.
///
/// # Errors
/// Unknown name.
pub fn policy_by_name(name: &str) -> Result<Box<dyn Policy>> {
    Ok(match name {
        "main" => Box::new(MainPolicy::default()),
        "gate-gc" => Box::new(GateGcPolicy::default()),
        "leadzone" => Box::new(LeadzonePolicy::default()),
        "tunable" => Box::new(TunablePolicy::default()),
        _ => bail!(
            "unknown policy {name:?}; known: {}",
            POLICY_NAMES.join(", ")
        ),
    })
}

/// True while writers are active: bytes admitted in the last 10 s, or
/// requests waiting (`pressure::active` of the engine).
pub fn active(snap: &SpaceSnapshot) -> bool {
    snap.throttle.ingest_rate > 0 || snap.waiters > 0
}

fn base_settings(level: Level, inp: &PolicyInput<'_>) -> GcSettings {
    let p = level.pressure();
    GcSettings {
        level,
        params: p.params(inp.base_params, inp.rate_factor),
        budget: p.budget(inp.base_budget),
        share_divisor: 3,
        share_max: Duration::from_secs(1),
        per_pass_bytes: inp.compact_per_pass,
        no_yield: false,
        busy_interval: None,
        growth_trigger: None,
        compact_threshold: None,
    }
}

/// `main`.
#[derive(Default)]
pub struct MainPolicy {
    tracker: PressureTracker,
}

impl Policy for MainPolicy {
    fn name(&self) -> &str {
        "main"
    }

    fn settings(&mut self, inp: &PolicyInput<'_>) -> GcSettings {
        let level = self
            .tracker
            .observe(inp.snap.throttle.state, active(inp.snap), inp.now);
        base_settings(Level::from_pressure(level), inp)
    }
}

/// `p1/gate-gc` part 2.
pub struct GateGcPolicy {
    tracker: PressureTracker,
    amp_percent: u64,
}

impl Default for GateGcPolicy {
    fn default() -> Self {
        Self {
            tracker: PressureTracker::default(),
            amp_percent: 200,
        }
    }
}

/// `gc_demand` of the branch: the ingest times the amplification, from 1 at
/// the soft mark to 2 at the limit; 0 outside the band.
pub fn gc_demand(snap: &SpaceSnapshot, amp_percent: u64) -> u64 {
    let t = &snap.throttle;
    if t.soft == 0 || t.headroom >= t.soft || t.state == ThrottleState::Open {
        return 0;
    }
    let pos = u128::from(t.headroom.min(t.soft)) * 1000 / u128::from(t.soft);
    let milli = 2000 - pos;
    let d = u128::from(t.ingest_rate) * u128::from(amp_percent) * milli / 100_000;
    u64::try_from(d).unwrap_or(u64::MAX)
}

impl Policy for GateGcPolicy {
    fn name(&self) -> &str {
        "gate-gc"
    }

    fn set(&mut self, key: &str, val: &str) -> Result<()> {
        match key {
            "amp_percent" => self.amp_percent = val.parse()?,
            _ => bail!("policy gate-gc has no parameter {key:?}"),
        }
        Ok(())
    }

    fn settings(&mut self, inp: &PolicyInput<'_>) -> GcSettings {
        let pressure = self
            .tracker
            .observe(inp.snap.throttle.state, active(inp.snap), inp.now);
        let mut s = base_settings(Level::from_pressure(pressure), inp);
        let t = &inp.snap.throttle;
        s.no_yield = t.soft > 0 && t.headroom < t.soft.saturating_mul(2);
        if pressure == Pressure::Normal {
            return s;
        }
        let demand = gc_demand(inp.snap, self.amp_percent);
        let base_rate = inp.base_params.rate_bytes_per_s;
        // The budget: 4x the base, or the demand over the base rate, at most
        // 64x the bytes and 8x the time.
        let want = if base_rate == 0 {
            0
        } else {
            demand.div_ceil(base_rate)
        };
        let f = want.clamp(4, 64);
        s.budget = GcBudget {
            max_duration: inp
                .base_budget
                .max_duration
                .saturating_mul(u32::try_from(f.min(8)).unwrap_or(8)),
            max_bytes: inp.base_budget.max_bytes.saturating_mul(f),
        };
        if s.params.rate_bytes_per_s != u64::MAX && s.params.rate_bytes_per_s != 0 {
            s.params.rate_bytes_per_s = s.params.rate_bytes_per_s.max(demand);
        }
        s.share_divisor = 1;
        s.share_max = Duration::from_secs(2);
        s.per_pass_bytes = inp
            .compact_per_pass
            .max(s.params.rate_bytes_per_s.saturating_mul(4));
        s
    }
}

/// `p1/leadzone`.
pub struct LeadzonePolicy {
    level: Level,
    low_since: Option<Duration>,
    /// Pass periods added to the grace in the lead window.
    pass_periods: f64,
    /// The pass period, seconds (the daemon's `busy_interval`).
    pass_period_s: f64,
}

impl Default for LeadzonePolicy {
    fn default() -> Self {
        Self {
            level: Level::Normal,
            low_since: None,
            pass_periods: 2.0,
            pass_period_s: 5.0,
        }
    }
}

impl LeadzonePolicy {
    /// `lead = min(ingest x (grace + n x period), soft, limit / 4)`.
    fn lead(&self, inp: &PolicyInput<'_>) -> u64 {
        let t = &inp.snap.throttle;
        if t.ingest_rate == 0 || t.soft == 0 {
            return 0;
        }
        let floor =
            Duration::from_secs(10).saturating_add(inp.base_params.read_timeout.saturating_mul(2));
        let window =
            inp.base_params.grace.min(floor).as_secs_f64() + self.pass_periods * self.pass_period_s;
        let want = (t.ingest_rate as f64 * window) as u64;
        want.min(t.soft).min(inp.snap.limit / 4)
    }
}

impl Policy for LeadzonePolicy {
    fn name(&self) -> &str {
        "leadzone"
    }

    fn set(&mut self, key: &str, val: &str) -> Result<()> {
        match key {
            "pass_periods" => self.pass_periods = val.parse()?,
            "pass_period_s" => self.pass_period_s = val.parse()?,
            _ => bail!("policy leadzone has no parameter {key:?}"),
        }
        Ok(())
    }

    fn settings(&mut self, inp: &PolicyInput<'_>) -> GcSettings {
        let t = &inp.snap.throttle;
        let lead = self.lead(inp);
        let lead_wanted = lead > 0 && {
            let edge = if self.level >= Level::Lead {
                t.soft.saturating_add(lead).saturating_add(lead / 4)
            } else {
                t.soft.saturating_add(lead)
            };
            t.headroom < edge
        };
        let want = match t.state {
            ThrottleState::Open => {
                if lead_wanted {
                    Level::Lead
                } else {
                    Level::Normal
                }
            }
            ThrottleState::Slowdown if !active(inp.snap) => {
                if lead_wanted {
                    Level::Lead
                } else {
                    Level::Normal
                }
            }
            ThrottleState::Slowdown => Level::Raised,
            ThrottleState::Stop | ThrottleState::Full => Level::Urgent,
        };
        if want >= self.level {
            self.level = want;
            self.low_since = None;
        } else if self.level == Level::Urgent && want == Level::Raised {
            self.level = Level::Raised;
            self.low_since = None;
        } else {
            let since = *self.low_since.get_or_insert(inp.now);
            if inp.now.saturating_sub(since) >= HOLD {
                self.level = want;
                self.low_since = None;
            }
        }
        let mut s = base_settings(self.level, inp);
        if self.level == Level::Lead {
            let floor = Duration::from_secs(10)
                .saturating_add(inp.base_params.read_timeout.saturating_mul(2));
            s.params.grace = inp.base_params.grace.min(floor);
        }
        s
    }
}

/// `main` with knobs, for sweeps and parameter files.
///
/// * `pressure_from`: Raised pressure starts below `soft x pressure_from`.
/// * `grace_s`: the grace under pressure, seconds, below the floor too
///   (P3/P4 of the candidate note; 0 = the floor, as `main`; a negative
///   value sets it to zero).
/// * `busy_interval_s`: the pass cadence under pressure (P4).
/// * `growth_gib`: a pass is due when the store grew this much (P2).
/// * `p1`: pace in store bytes against the projected headroom (P1).
pub struct TunablePolicy {
    tracker: PressureTracker,
    pressure_from: f64,
    grace_s: f64,
    rate_factor: u64,
    busy_interval_s: f64,
    growth_gib: f64,
    p1: bool,
}

impl Default for TunablePolicy {
    fn default() -> Self {
        Self {
            tracker: PressureTracker::default(),
            pressure_from: 1.0,
            grace_s: 0.0,
            rate_factor: 0,
            busy_interval_s: 0.0,
            growth_gib: 0.0,
            p1: false,
        }
    }
}

impl Policy for TunablePolicy {
    fn name(&self) -> &str {
        "tunable"
    }

    fn set(&mut self, key: &str, val: &str) -> Result<()> {
        match key {
            "pressure_from" => self.pressure_from = val.parse()?,
            "grace_s" => self.grace_s = val.parse()?,
            "rate_factor" => self.rate_factor = val.parse()?,
            "busy_interval_s" => self.busy_interval_s = val.parse()?,
            "growth_gib" => self.growth_gib = val.parse()?,
            "p1" => self.p1 = matches!(val, "true" | "on" | "1"),
            _ => bail!("policy tunable has no parameter {key:?}"),
        }
        Ok(())
    }

    fn settings(&mut self, inp: &PolicyInput<'_>) -> GcSettings {
        let t = &inp.snap.throttle;
        let early = self.pressure_from > 1.0
            && t.state == ThrottleState::Open
            && active(inp.snap)
            && t.soft > 0
            && (t.headroom as f64) < t.soft as f64 * self.pressure_from;
        // An early pressure is the band's, a little sooner: the tracker is
        // told the gate is slowing down.
        let state = if early {
            ThrottleState::Slowdown
        } else {
            t.state
        };
        let p = self.tracker.observe(state, active(inp.snap), inp.now);
        let rf = if self.rate_factor == 0 {
            inp.rate_factor
        } else {
            self.rate_factor
        };
        let with_rf = PolicyInput {
            snap: inp.snap,
            now: inp.now,
            base_params: inp.base_params,
            base_budget: inp.base_budget,
            rate_factor: rf,
            compact_per_pass: inp.compact_per_pass,
        };
        let mut s = base_settings(Level::from_pressure(p), &with_rf);
        if p != Pressure::Normal {
            if self.grace_s != 0.0 {
                s.params.grace =
                    Duration::from_secs_f64(self.grace_s.max(0.0)).min(inp.base_params.grace);
            }
            if self.busy_interval_s > 0.0 {
                s.busy_interval = Some(Duration::from_secs_f64(self.busy_interval_s));
            }
        }
        if self.growth_gib > 0.0 {
            s.growth_trigger = Some(self.growth_gib * (1u64 << 30) as f64);
        }
        s
    }

    fn admission_rate(&mut self, v: &AdmissionView<'_>) -> Option<f64> {
        if !self.p1 {
            return None;
        }
        let t = &v.snap.throttle;
        if t.soft == 0 {
            return None;
        }
        let extra = (v.a_hat - 1.0).max(0.0) * v.journal_debt as f64;
        let h_proj = (t.headroom as f64 - extra).max(0.0);
        let x = h_proj / t.soft as f64;
        if x >= 1.0 {
            return None;
        }
        // The gate's curve read as a store rate, charged A_hat per byte.
        let r_mid = 64.0 * 1048576.0;
        let r_min = 4.0 * 1048576.0;
        let store = (r_mid * x / (1.0 - x).max(1e-6)).max(r_min);
        Some(store / v.a_hat.max(1.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lunix_format::IdLen;
    use lunix_iface::space::ThrottleFigures;

    fn params() -> GcParams {
        GcParams {
            retain_last: 2,
            grace: Duration::from_secs(60),
            read_timeout: Duration::from_secs(10),
            rate_bytes_per_s: 64 << 20,
            live_threshold: 0.5,
            chunk_size: 65_536,
            id_len: IdLen::Sixteen,
        }
    }

    fn snap(state: ThrottleState, headroom: u64, soft: u64, ingest: u64) -> SpaceSnapshot {
        SpaceSnapshot {
            limit: 18 << 30,
            throttle: ThrottleFigures {
                state,
                headroom,
                soft,
                ingest_rate: ingest,
                ..ThrottleFigures::default()
            },
            ..SpaceSnapshot::default()
        }
    }

    fn inp<'a>(s: &'a SpaceSnapshot, now: u64) -> PolicyInput<'a> {
        PolicyInput {
            snap: s,
            now: Duration::from_secs(now),
            base_params: params(),
            base_budget: GcBudget {
                max_duration: Duration::from_millis(50),
                max_bytes: 64 << 20,
            },
            rate_factor: 4,
            compact_per_pass: 256 << 20,
        }
    }

    #[test]
    fn main_raises_in_the_band_with_the_floor_grace() {
        let mut p = MainPolicy::default();
        let open = snap(ThrottleState::Open, 10 << 30, 2 << 30, 1 << 28);
        assert_eq!(p.settings(&inp(&open, 0)).level, Level::Normal);
        let band = snap(ThrottleState::Slowdown, 1 << 30, 2 << 30, 1 << 28);
        let s = p.settings(&inp(&band, 1));
        assert_eq!(s.level, Level::Raised);
        assert_eq!(s.params.grace, Duration::from_secs(30));
        assert_eq!(s.params.rate_bytes_per_s, 256 << 20);
    }

    #[test]
    fn leadzone_starts_the_floor_grace_before_the_band() {
        let mut p = LeadzonePolicy::default();
        // ingest 100 MiB/s x 40 s = 4000 MiB, capped by soft (2 GiB).
        let near = snap(ThrottleState::Open, 3 << 30, 2 << 30, 100 << 20);
        let s = p.settings(&inp(&near, 0));
        assert_eq!(s.level, Level::Lead);
        assert_eq!(s.params.grace, Duration::from_secs(30));
        assert_eq!(s.params.rate_bytes_per_s, 64 << 20, "only the grace moves");
        let far = snap(ThrottleState::Open, 10 << 30, 2 << 30, 100 << 20);
        // The fall waits for the hold.
        assert_eq!(p.settings(&inp(&far, 1)).level, Level::Lead);
        assert_eq!(p.settings(&inp(&far, 7)).level, Level::Normal);
    }

    #[test]
    fn gate_gc_sizes_the_collector_to_the_ingest() {
        let mut p = GateGcPolicy::default();
        let band = snap(ThrottleState::Slowdown, 0, 2 << 30, 400 << 20);
        let s = p.settings(&inp(&band, 0));
        assert_eq!(
            s.params.rate_bytes_per_s,
            1600 << 20,
            "400 MiB/s x 2 x 2 at the limit"
        );
        assert_eq!(s.share_divisor, 1);
        assert!(s.no_yield);
        assert!(s.per_pass_bytes >= 3 << 30);
    }

    #[test]
    fn every_policy_name_builds() {
        for n in POLICY_NAMES {
            assert_eq!(policy_by_name(n).unwrap().name(), *n);
        }
        assert!(policy_by_name("x").is_err());
    }
}
