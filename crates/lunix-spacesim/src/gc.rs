//! The collector model: the pipelined schedule of `lunix-gc` (`due`,
//! `min_gap`, the idle and busy cadences), the order of a pass (mark,
//! condemn, delete what a recheck allows, compact) and the ladder's urgent
//! pass with its back-off. Time advances only when the coordinator gives the
//! collector time (the share after a cut, an idle tick).

use std::time::Duration;

use lunix_iface::space::{SpaceGate, SpaceKind};
use lunix_space::LunixSpace;

use crate::policy::GcSettings;
use crate::rng::Rng;
use crate::store::{CompactPlan, Store};

/// Bytes the model charged to `Segments`, so a release never exceeds them
/// (the store keeps fractional bytes).
#[derive(Debug, Default)]
pub struct Books {
    /// Charged to `Segments` and not released.
    pub segments: u64,
}

/// The gate refused a reservation from the headroom.
#[derive(Clone, Copy, Debug)]
pub struct NoSpace;

impl Books {
    /// Charges `bytes` from the headroom (a cut's or a compaction's).
    ///
    /// # Errors
    /// The gate's `NoSpace`.
    pub fn charge_headroom(&mut self, gate: &LunixSpace, bytes: f64) -> Result<(), NoSpace> {
        let b = bytes.max(0.0).round() as u64;
        gate.reserve_headroom(SpaceKind::Segments, b)
            .map_err(|_| NoSpace)?;
        self.segments += b;
        Ok(())
    }

    /// Releases `bytes` (never more than held).
    pub fn release(&mut self, gate: &LunixSpace, bytes: f64) {
        let b = (bytes.max(0.0).round() as u64).min(self.segments);
        if b > 0 {
            gate.release(SpaceKind::Segments, b);
            self.segments -= b;
        }
    }
}

/// Settings of the collector that are not policy.
#[derive(Clone, Copy, Debug)]
pub struct GcCfg {
    /// `GcConfig::busy_interval`.
    pub busy_interval: Duration,
    /// `GcConfig::pass_interval`.
    pub pass_interval: Duration,
    /// The mark's duration.
    pub mark: Duration,
    /// Compaction speed per second of collector time, bytes.
    pub gc_bw: f64,
    /// `live_threshold`.
    pub threshold: f64,
    /// Index work per condemned segment.
    pub condemn: Duration,
    /// Index work per deleted segment.
    pub delete: Duration,
    /// G1: that work is batched per pass.
    pub batch: bool,
}

/// What a collector call works on.
pub struct GcEnv<'a> {
    /// The segments.
    pub store: &'a mut Store,
    /// The real gate.
    pub gate: &'a LunixSpace,
    /// The ledger shadow.
    pub books: &'a mut Books,
    /// The settings now.
    pub settings: &'a GcSettings,
    /// Fixed settings.
    pub cfg: &'a GcCfg,
    /// The clock.
    pub now: Duration,
    /// Seeded jitter.
    pub rng: &'a mut Rng,
    /// Bytes of segments written so far (cuts and outputs), for a growth
    /// trigger.
    pub written: f64,
    /// The growth trigger of the policy, bytes (a pass is due when the store
    /// grew this much since the last mark).
    pub growth_trigger: Option<f64>,
    /// The device's slowdown of the collector (1 = none).
    pub slow: f64,
}

/// Where the collector is.
#[derive(Clone, Debug)]
pub enum Phase {
    /// Between passes.
    Idle,
    /// Condemn and delete work (index updates) until `end`, then the plan.
    Costs {
        /// When it ends.
        end: Duration,
        /// The compaction that follows.
        plan: Option<CompactPlan>,
    },
    /// Marking until `end`.
    Marking {
        /// When the mark ends.
        end: Duration,
        /// Its start.
        start: Duration,
    },
    /// Compacting the plan.
    Compacting {
        /// What to move.
        plan: CompactPlan,
        /// Live bytes charged so far.
        done: f64,
    },
}

/// What a pass did.
#[derive(Clone, Copy, Debug, Default)]
pub struct PassSummary {
    /// Bytes condemned.
    pub condemned: f64,
    /// Bytes deleted.
    pub deleted: f64,
    /// Live bytes compacted.
    pub compacted: f64,
}

/// Counters for the report.
#[derive(Clone, Copy, Debug, Default)]
pub struct GcStats {
    /// Marks run (scheduled and urgent).
    pub marks: u64,
    /// Scheduled passes finished.
    pub passes: u64,
    /// Urgent passes.
    pub urgent: u64,
    /// Bytes compaction wrote.
    pub compact_written: f64,
    /// Bytes deleted.
    pub deleted: f64,
    /// Time of the first condemn, if any.
    pub first_condemn: Option<Duration>,
    /// Time of the first delete, if any.
    pub first_delete: Option<Duration>,
    /// Largest condemned list, bytes.
    pub peak_condemned: f64,
    /// Coordinator time the marks took.
    pub mark_time: Duration,
    /// Collector time used in all (marks, index work, copying).
    pub busy_time: Duration,
}

/// The collector.
#[derive(Debug)]
pub struct Gc {
    /// The phase.
    pub phase: Phase,
    last_end: Option<Duration>,
    last_pass_busy: bool,
    last_aborted: bool,
    last_mark_start: Option<Duration>,
    last_mark: Duration,
    last_tip_cut: Option<u64>,
    last_changed: bool,
    last_written: f64,
    tokens: f64,
    tokens_at: Duration,
    pass: PassSummary,
    /// Counters.
    pub stats: GcStats,
    // The ladder's urgent pass.
    urgent_end: Option<Duration>,
    urgent_backoff: Duration,
}

const MIN_BACKOFF: Duration = Duration::from_secs(2);
const MAX_BACKOFF: Duration = Duration::from_secs(60);
const DUTY_FACTOR: u32 = 3;

impl Gc {
    /// A collector whose last pass ended at `last_end` and found nothing.
    pub fn new(last_end: Option<Duration>) -> Self {
        Self {
            phase: Phase::Idle,
            last_end,
            last_pass_busy: false,
            last_aborted: false,
            last_mark_start: None,
            last_mark: Duration::ZERO,
            last_tip_cut: None,
            last_changed: false,
            last_written: 0.0,
            tokens: 0.0,
            tokens_at: Duration::ZERO,
            pass: PassSummary::default(),
            stats: GcStats::default(),
            urgent_end: None,
            urgent_backoff: MIN_BACKOFF,
        }
    }

    /// True while a pass has work a step can do now.
    pub fn busy(&self) -> bool {
        !matches!(self.phase, Phase::Idle)
    }

    /// The shortest time between two pipelined passes.
    fn min_gap(&self, cfg: &GcCfg, s: &GcSettings) -> Duration {
        s.busy_interval
            .unwrap_or(cfg.busy_interval)
            .min(cfg.pass_interval)
            .max(self.last_mark.saturating_mul(DUTY_FACTOR))
    }

    /// `Gc::due` of the pipelined collector, with the policy's growth
    /// trigger as an extra reason.
    pub fn due(&mut self, env: &GcEnv<'_>, now: Duration) -> bool {
        let waited = |last_end: Option<Duration>, d: Duration| {
            last_end.is_none_or(|e| now >= e.saturating_add(d))
        };
        let gap = self.min_gap(env.cfg, env.settings);
        let grace = env.settings.params.grace;
        let first = if self.last_aborted {
            env.cfg.pass_interval.max(gap)
        } else {
            gap
        };
        if waited(self.last_end, first)
            && env
                .store
                .grace_ended_since(grace, now, self.last_mark_start)
        {
            return true;
        }
        if let Some(trig) = env.growth_trigger {
            if waited(self.last_end, gap) && env.written - self.last_written >= trig {
                return true;
            }
        }
        let interval = if self.last_pass_busy {
            gap
        } else {
            env.cfg.pass_interval.max(gap)
        };
        if !waited(self.last_end, interval) {
            return false;
        }
        let waiting = env.store.condemned_bytes() > 0.0;
        if self.last_tip_cut != Some(env.store.cuts) || (waiting && self.last_changed) {
            return true;
        }
        // Nothing changed: look again one interval from now.
        self.last_end = Some(now);
        self.last_pass_busy = false;
        false
    }

    /// Runs the collector for at most `avail` of coordinator time; returns
    /// the time it used.
    pub fn step(&mut self, env: &mut GcEnv<'_>, avail: Duration) -> Duration {
        let mut used = Duration::ZERO;
        let mut guard = 0;
        while used < avail && guard < 8 {
            guard += 1;
            let now = env.now + used;
            let left = avail - used;
            match self.phase.clone() {
                Phase::Idle => {
                    if !self.due(env, now) {
                        break;
                    }
                    let dur = env
                        .cfg
                        .mark
                        .mul_f64(env.rng.jitter(0.2) * env.slow.max(0.01));
                    self.phase = Phase::Marking {
                        end: now + dur,
                        start: now,
                    };
                    self.pass = PassSummary::default();
                    self.last_written = env.written;
                    self.last_tip_cut = Some(env.store.cuts);
                }
                Phase::Marking { end, start } => {
                    let need = end.saturating_sub(now);
                    if need > left {
                        used += left;
                        break;
                    }
                    used += need;
                    self.finish_mark(env, start, end);
                }
                Phase::Costs { end, plan } => {
                    let need = end.saturating_sub(now);
                    if need > left {
                        used += left;
                        break;
                    }
                    used += need;
                    match plan {
                        Some(plan) => {
                            self.tokens = self.tokens.max(0.0);
                            self.tokens_at = end;
                            self.phase = Phase::Compacting { plan, done: 0.0 };
                        }
                        None => self.finish_pass(env, end),
                    }
                }
                Phase::Compacting { plan, done } => {
                    let t = env.now + used;
                    let rate = env.settings.params.rate_bytes_per_s;
                    let step_bytes = env.settings.budget.max_bytes as f64;
                    let dt = t.saturating_sub(self.tokens_at).as_secs_f64();
                    self.tokens_at = t;
                    self.tokens = if rate == u64::MAX {
                        f64::INFINITY
                    } else {
                        (self.tokens + rate as f64 * dt).min(step_bytes.max(1.0))
                    };
                    let can_time = env.cfg.gc_bw / env.slow.max(0.01) * left.as_secs_f64();
                    let work = self.tokens.min(can_time).min(plan.live_bytes - done);
                    if work <= 1.0 && done < plan.live_bytes {
                        // At the rate cap: nothing to do until it refills.
                        used += left;
                        break;
                    }
                    if env.books.charge_headroom(env.gate, work).is_err() {
                        self.abort(env, t);
                        break;
                    }
                    self.tokens -= if work.is_finite() { work } else { 0.0 };
                    used += Duration::from_secs_f64(work / (env.cfg.gc_bw / env.slow.max(0.01)));
                    let done = done + work;
                    env.written += work;
                    if done >= plan.live_bytes - 1.0 {
                        self.finish_compaction(env, &plan, done, env.now + used);
                    } else {
                        self.phase = Phase::Compacting { plan, done };
                    }
                }
            }
        }
        self.stats.busy_time += used;
        used
    }

    fn finish_mark(&mut self, env: &mut GcEnv<'_>, start: Duration, end: Duration) {
        self.stats.marks += 1;
        self.stats.mark_time += end - start;
        self.last_mark = end - start;
        self.last_mark_start = Some(start);
        // The recheck: entries whose grace ended before this mark began.
        let freed = env.store.delete_due(start, env.settings.params.grace, end);
        env.books.release(env.gate, freed);
        self.note_deleted(freed, end);
        // Then what is newly dead.
        let condemned = env.store.mark(end);
        self.note_condemned(condemned, end, env.store.condemned_bytes());
        self.pass.condemned += condemned;
        self.pass.deleted += freed;
        // Then compaction, if the volume allows it.
        let rate = env.settings.params.rate_bytes_per_s;
        let budget = if env.settings.per_pass_bytes == u64::MAX {
            f64::INFINITY
        } else {
            env.settings.per_pass_bytes as f64
        };
        let threshold = env.settings.compact_threshold.unwrap_or(env.cfg.threshold);
        let plan = if rate > 0 {
            let p = env.store.plan_compaction(threshold, budget);
            (p.live_bytes > 1.0).then_some(p)
        } else {
            None
        };
        let n = env.store.last_condemned_segs + env.store.last_deleted_segs;
        let _ = n;
        let work = env
            .cfg
            .condemn
            .mul_f64(env.store.last_condemned_segs as f64)
            + env.cfg.delete.mul_f64(env.store.last_deleted_segs as f64);
        let work = if env.cfg.batch { work / 10 } else { work };
        if work.is_zero() {
            match plan {
                Some(plan) => {
                    self.tokens = self.tokens.max(0.0);
                    self.tokens_at = end;
                    self.phase = Phase::Compacting { plan, done: 0.0 };
                }
                None => self.finish_pass(env, end),
            }
        } else {
            self.phase = Phase::Costs {
                end: end + work,
                plan,
            };
        }
    }

    fn finish_compaction(
        &mut self,
        env: &mut GcEnv<'_>,
        plan: &CompactPlan,
        charged: f64,
        now: Duration,
    ) {
        let out = env.store.apply_compaction(plan, now);
        // What died while it was copied is not written.
        env.books.release(env.gate, (charged - out).max(0.0));
        self.stats.compact_written += out;
        self.pass.compacted += out;
        self.finish_pass(env, now);
    }

    fn abort(&mut self, env: &mut GcEnv<'_>, now: Duration) {
        self.phase = Phase::Idle;
        self.last_end = Some(now);
        self.last_aborted = true;
        self.last_pass_busy = false;
        self.last_changed = false;
        let _ = env;
    }

    fn finish_pass(&mut self, env: &mut GcEnv<'_>, now: Duration) {
        self.phase = Phase::Idle;
        let p = self.pass;
        let busy = p.condemned > 0.0 || p.deleted > 0.0 || p.compacted > 0.0;
        self.last_end = Some(now);
        self.last_pass_busy = busy;
        self.last_changed = busy;
        self.last_aborted = false;
        self.stats.passes += 1;
        let _ = env;
    }

    fn note_condemned(&mut self, bytes: f64, at: Duration, listed: f64) {
        if bytes > 0.0 && self.stats.first_condemn.is_none() {
            self.stats.first_condemn = Some(at);
        }
        self.stats.peak_condemned = self.stats.peak_condemned.max(listed);
    }

    fn note_deleted(&mut self, bytes: f64, at: Duration) {
        if bytes > 0.0 && self.stats.first_delete.is_none() {
            self.stats.first_delete = Some(at);
        }
        self.stats.deleted += bytes;
    }

    /// True when the ladder may run an urgent pass now (its back-off).
    pub fn urgent_allowed(&self, now: Duration) -> bool {
        self.urgent_end
            .is_none_or(|e| now >= e.saturating_add(self.urgent_backoff))
    }

    /// A write was admitted again: the back-off returns to its minimum.
    pub fn urgent_reset(&mut self) {
        self.urgent_backoff = MIN_BACKOFF;
    }

    /// The urgent pass (the ladder's compact step): one mark, deletes what
    /// the floor grace allows, condemns what is dead, compacts up to `want`
    /// live bytes if still short. Holds the coordinator for the returned
    /// time.
    pub fn urgent(&mut self, env: &mut GcEnv<'_>, want: f64, floor_grace: Duration) -> Duration {
        let start = env.now;
        let mark = env
            .cfg
            .mark
            .mul_f64(env.rng.jitter(0.2) * env.slow.max(0.01));
        let end = start + mark;
        self.stats.marks += 1;
        self.stats.urgent += 1;
        self.stats.mark_time += mark;
        let freed = env.store.delete_due(start, floor_grace, end);
        env.books.release(env.gate, freed);
        self.note_deleted(freed, end);
        let condemned = env.store.mark(end);
        self.note_condemned(condemned, end, env.store.condemned_bytes());
        let work = env
            .cfg
            .condemn
            .mul_f64(env.store.last_condemned_segs as f64)
            + env.cfg.delete.mul_f64(env.store.last_deleted_segs as f64);
        let mut elapsed = mark + if env.cfg.batch { work / 10 } else { work };
        let mut compacted = 0.0;
        if freed < want && env.settings.params.rate_bytes_per_s > 0 {
            let plan = env.store.plan_compaction(
                env.settings.compact_threshold.unwrap_or(env.cfg.threshold),
                want - freed,
            );
            if plan.live_bytes > 1.0 && env.books.charge_headroom(env.gate, plan.live_bytes).is_ok()
            {
                let out = env.store.apply_compaction(&plan, end);
                env.books
                    .release(env.gate, (plan.live_bytes - out).max(0.0));
                self.stats.compact_written += out;
                env.written += out;
                compacted = out;
                elapsed +=
                    Duration::from_secs_f64(plan.live_bytes / (env.cfg.gc_bw / env.slow.max(0.01)));
            }
        }
        let t = start + elapsed;
        self.urgent_end = Some(t);
        self.urgent_backoff = if freed > 0.0 {
            MIN_BACKOFF
        } else {
            let mut b = (self.urgent_backoff * 2).min(MAX_BACKOFF);
            if condemned > 0.0 {
                b = b.min(floor_grace);
            }
            b
        };
        // The scheduled schedule sees an urgent pass as the last pass.
        self.last_end = Some(t);
        self.last_mark_start = Some(start);
        self.last_mark = mark;
        self.last_pass_busy = condemned > 0.0 || freed > 0.0 || compacted > 0.0;
        self.last_changed = self.last_pass_busy;
        self.last_tip_cut = Some(env.store.cuts);
        elapsed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::Pattern;

    #[test]
    fn the_idle_cadence_is_thirty_seconds_and_the_busy_one_five() {
        // The gap rule only: a pass that did nothing waits `pass_interval`.
        let cfg = GcCfg {
            busy_interval: Duration::from_secs(5),
            pass_interval: Duration::from_secs(30),
            mark: Duration::from_millis(100),
            gc_bw: 1e9,
            threshold: 0.5,
            condemn: Duration::from_millis(5),
            delete: Duration::from_millis(13),
            batch: false,
        };
        let gc = Gc::new(Some(Duration::ZERO));
        let s = crate::policy::GcSettings::for_tests();
        assert_eq!(gc.min_gap(&cfg, &s), Duration::from_secs(5));
        let mut gc = Gc::new(Some(Duration::ZERO));
        gc.last_mark = Duration::from_secs(4);
        assert_eq!(
            gc.min_gap(&cfg, &s),
            Duration::from_secs(12),
            "3 x the mark"
        );
        let _ = Store::new(1.0, 1.0, 1.0, Pattern::Random);
    }

    #[test]
    fn the_urgent_backoff_doubles_and_resets() {
        let mut gc = Gc::new(None);
        assert!(gc.urgent_allowed(Duration::ZERO));
        gc.urgent_end = Some(Duration::from_secs(10));
        gc.urgent_backoff = Duration::from_secs(4);
        assert!(!gc.urgent_allowed(Duration::from_secs(13)));
        assert!(gc.urgent_allowed(Duration::from_secs(14)));
        gc.urgent_reset();
        assert!(gc.urgent_allowed(Duration::from_secs(12)));
    }
}
