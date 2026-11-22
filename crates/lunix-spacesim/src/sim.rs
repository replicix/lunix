//! The simulation: a closed-loop writer against the real gate, the journal
//! ring, the cuts, the store and the collector, in steps of a few
//! milliseconds on a manual clock.
//!
//! Real code: `lunix_space::LunixSpace` (ledger, throttle, pace, full latch),
//! `lunix_engine::{Pressure, PressureTracker, RingPacer, DrainHistory,
//! yielding}`. Modelled: everything else (see the report).

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};
use lunix_engine::{yielding, DrainHistory, RingPacer};
use lunix_format::local::{FreeFloor, OnFull, SpaceSettings};
use lunix_format::{IdLen, TimeMs};
use lunix_iface::clock::{Clock, ManualClock};
use lunix_iface::gc::{GcBudget, GcParams};
use lunix_iface::space::{
    ReserveClass, ReserveOutcome, SpaceGate, SpaceKind, ThrottleState, Waiter, WaiterState,
};
use lunix_space::{LunixSpace, ManualFs, SpaceConfig, ThrottleConfig};

use crate::gc::{Books, Gc, GcCfg, GcEnv, Phase};
use crate::params::{BoundMode, Pattern, Scenario};
use crate::policy::{AdmissionView, GcSettings, Level, Policy, PolicyInput};
use crate::report::{window, CellReport, DeadSummary, Hist, RunReport, SecRow, StopEpisode};
use crate::rng::Rng;
use crate::store::{distinct_random, Store};

/// Seconds of simulated time before the overwrite starts (the volume was
/// attached and filled; the collector's last pass ended `gc_phase_s` before
/// the overwrite).
const BASE_SECS: u64 = 100;

#[derive(Debug)]
enum SlotSt {
    Idle { until: Duration },
    Held,
    Waiting(Waiter),
    RingParked { since: Duration },
    Appending,
}

#[derive(Debug)]
struct Slot {
    st: SlotSt,
    arrived: Duration,
}

#[derive(Debug)]
struct CutJob {
    start: Duration,
    end: Duration,
    bytes: u64,
    distinct: f64,
    store_bytes: f64,
    bound: u64,
    trued: bool,
}

#[derive(Debug)]
enum Coord {
    Idle,
    Cutting(CutJob),
    Gc { until: Duration },
    Blocked { until: Duration },
}

impl Coord {
    fn name(&self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Cutting(_) => "cut",
            Self::Gc { .. } => "gc",
            Self::Blocked { .. } => "urgent",
        }
    }
}

#[derive(Default)]
struct CellAcc {
    index: u32,
    start: f64,
    hist: Hist,
    writes: u64,
    per_sec: Vec<f64>,
    cur_sec: f64,
    stops0: u64,
    delayed0: u64,
    capped0: u64,
    enospc0: u64,
    throttled0: u64,
    episodes: Vec<StopEpisode>,
    peak_used: u64,
    ring_wait_max: f64,
    first_stop: Option<f64>,
}

struct Sim<'p> {
    sc: Scenario,
    policy: &'p mut dyn Policy,
    clock: Arc<ManualClock>,
    fs: Arc<ManualFs>,
    gate: Arc<LunixSpace>,
    rng: Rng,
    /// The stream of the timing jitter, apart from the deaths' stream, so
    /// that candidates of one seed see the same draws where they can.
    jit: Rng,
    now: Duration,
    base: Duration,
    dt: Duration,
    // workload
    slots: Vec<Slot>,
    waiting: Vec<usize>,
    held: VecDeque<usize>,
    append_q: VecDeque<usize>,
    ring_parked: VecDeque<usize>,
    app_budget: f64,
    req_bytes: u64,
    adm_tokens: f64,
    adm_last: Duration,
    adm_rate: Option<f64>,
    // journal and cuts
    ring_reserved: u64,
    ring_appended: u64,
    drain: DrainHistory,
    pacer: RingPacer,
    ring_pace_on: bool,
    coord: Coord,
    last_commit: Duration,
    prev_cut_end: Option<Duration>,
    cut_retry: Duration,
    // store and collector
    store: Store,
    books: Books,
    gc: Gc,
    gcfg: GcCfg,
    base_params: GcParams,
    base_budget: GcBudget,
    eff: GcSettings,
    yield_until: Duration,
    written: f64,
    cut_written: f64,
    cut_journal: f64,
    admitted_bytes: f64,
    // metrics
    cell: Option<CellAcc>,
    cells: Vec<CellReport>,
    trace: Vec<SecRow>,
    next_sample: Duration,
    sec_completed: f64,
    sec_lat_max: f64,
    stop_start: Option<f64>,
    prev_state: ThrottleState,
    band_entry: Option<(f64, u64)>,
    amp_pre_band: Option<f64>,
    a_hat: f64,
    a_prev: (f64, f64),
    a_hist: VecDeque<(f64, f64)>,
    first_stop_abs: Option<f64>,
    c5_skipped: u64,
    store_rate: f64,
    cw_prev: f64,
    last_refresh: Duration,
    peak_ratio: f64,
}

/// Runs the scenario under the policy.
///
/// # Errors
/// An invalid scenario or a gate that refuses its configuration.
pub fn run(sc: &Scenario, policy: &mut dyn Policy) -> Result<RunReport> {
    sc.validate()?;
    let mut sim = Sim::new(sc, policy)?;
    sim.run_all();
    Ok(sim.finish())
}

fn gate_settings(sc: &Scenario) -> SpaceSettings {
    SpaceSettings {
        free_floor: FreeFloor::Percent("10%".into()),
        on_full: OnFull::Wait,
        full_timeout_ms: None,
        cache_max: sc.cache_max,
        cache_max_percent: None,
        headroom: sc.headroom,
    }
}

impl<'p> Sim<'p> {
    fn new(sc: &Scenario, policy: &'p mut dyn Policy) -> Result<Self> {
        let clock = Arc::new(ManualClock::new(TimeMs(0)));
        let base = Duration::from_secs(BASE_SECS);
        clock.advance(base);
        let fs = Arc::new(ManualFs::new(sc.fs_total, sc.fs_total));
        let throttle = policy.gate(
            ThrottleConfig {
                enabled: sc.throttle,
                soft_percent: sc.soft_percent,
                soft_min: sc.soft_min,
                soft_max: sc.soft_max,
                min_rate: sc.min_rate,
                max_delay: Duration::from_secs_f64(sc.max_delay_s),
                mid_rate: 0,
            }
            .with_gc_rate(sc.gc_rate),
        );
        let cfg = SpaceConfig {
            settings: gate_settings(sc),
            segment_size: sc.segment,
            background_timers: false,
            full_timeout: Duration::from_secs_f64(sc.full_timeout_s),
            throttle,
        };
        let dyn_clock: Arc<dyn Clock> = clock.clone();
        let gate = LunixSpace::new(cfg, fs.clone(), dyn_clock).map_err(|e| anyhow!("gate: {e}"))?;
        let mut rng = Rng::new(sc.seed);
        let wc = sc.ws_chunks();
        let mut store = Store::new(wc, sc.chunk_stored(), sc.segment as f64, sc.pattern);
        store.retain_cuts = sc.retain_cuts;
        store.grace_from_death = sc.grace_from_death;
        // The volume is filled: the working set is on disk, one cut made it.
        let fill = store.write_segments(wc, wc, false);
        store.cut_done(0.0, base, &mut rng);
        let mut books = Books::default();
        let fill_b = fill.round() as u64;
        gate.charge(SpaceKind::Segments, fill_b)
            .map_err(|e| anyhow!("fill: {e}"))?;
        books.segments = fill_b;
        gate.refresh();
        let phase = sc
            .gc_phase_s
            .unwrap_or_else(|| rng.unit() * sc.pass_interval_s);
        let last_end = base.saturating_sub(Duration::from_secs_f64(phase));
        let gcfg = GcCfg {
            busy_interval: Duration::from_secs_f64(sc.busy_interval_s),
            pass_interval: Duration::from_secs_f64(sc.pass_interval_s),
            mark: Duration::from_secs_f64(
                (sc.mark_s + sc.mark_per_chunk_us * 1e-6 * wc)
                    * if sc.incremental_mark { 0.1 } else { 1.0 },
            ),
            gc_bw: sc.device.gc_bw,
            threshold: sc.live_threshold,
            condemn: Duration::from_secs_f64(sc.condemn_ms / 1000.0),
            delete: Duration::from_secs_f64(sc.delete_ms / 1000.0),
            batch: sc.batch_index,
            compaction: sc.compaction,
            punch_call: Duration::from_secs_f64(sc.punch_call_us * 1e-6),
            punch_sync: Duration::from_secs_f64(sc.punch_sync_ms * 1e-3),
            punch: sc.punch,
        };
        let base_params = GcParams {
            retain_last: 2,
            grace: Duration::from_secs_f64(sc.gc_grace_s),
            read_timeout: Duration::from_secs_f64(sc.read_timeout_s),
            rate_bytes_per_s: sc.gc_rate,
            live_threshold: sc.live_threshold,
            chunk_size: u32::try_from(sc.chunk).unwrap_or(u32::MAX),
            id_len: IdLen::Sixteen,
        };
        let base_budget = GcBudget {
            max_duration: Duration::from_millis(sc.step_budget_ms),
            max_bytes: sc.gc_rate,
        };
        let req_bytes = u64::from(sc.agg) * sc.record();
        let n_slots = (sc.in_flight / sc.agg).max(1) as usize;
        let slots = (0..n_slots)
            .map(|_| Slot {
                st: SlotSt::Idle { until: base },
                arrived: base,
            })
            .collect();
        let eff = GcSettings::for_tests();
        let now = base;
        let total = Duration::from_secs_f64(sc.total_secs());
        let _ = total;
        Ok(Self {
            sc: sc.clone(),
            policy,
            clock,
            fs,
            gate,
            rng,
            jit: Rng::new(sc.seed ^ 0x5DEE_CE66),
            now,
            base,
            dt: sc.step(),
            slots,
            waiting: Vec::new(),
            held: VecDeque::new(),
            append_q: VecDeque::new(),
            ring_parked: VecDeque::new(),
            app_budget: 0.0,
            req_bytes,
            adm_tokens: 0.0,
            adm_last: base,
            adm_rate: None,
            ring_reserved: 0,
            ring_appended: 0,
            drain: DrainHistory::default(),
            pacer: RingPacer::default(),
            ring_pace_on: false,
            coord: Coord::Idle,
            last_commit: base,
            prev_cut_end: None,
            cut_retry: Duration::ZERO,
            store,
            books,
            gc: Gc::new(Some(last_end)),
            gcfg,
            base_params,
            base_budget,
            eff,
            yield_until: Duration::ZERO,
            written: 0.0,
            cut_written: 0.0,
            cut_journal: 0.0,
            admitted_bytes: 0.0,
            cell: None,
            cells: Vec::new(),
            trace: Vec::new(),
            next_sample: base + Duration::from_secs(1),
            sec_completed: 0.0,
            sec_lat_max: 0.0,
            stop_start: None,
            prev_state: ThrottleState::Open,
            band_entry: None,
            amp_pre_band: None,
            a_hat: 1.0,
            a_prev: (0.0, 0.0),
            a_hist: VecDeque::new(),
            first_stop_abs: None,
            c5_skipped: 0,
            store_rate: 0.0,
            cw_prev: 0.0,
            last_refresh: base,
            peak_ratio: 0.0,
        })
    }

    fn rel(&self) -> f64 {
        self.now.saturating_sub(self.base).as_secs_f64()
    }

    fn run_all(&mut self) {
        let total = self.sc.total_secs();
        while self.rel() < total {
            self.step();
        }
        self.end_cell();
    }

    // ----- the cells -----

    fn workload_on(&mut self) -> bool {
        let rel = self.rel();
        let period = self.sc.cell_secs + self.sc.gap_secs;
        let idx = (rel / period).floor();
        let within = rel - idx * period;
        let on = idx < f64::from(self.sc.cells) && within < self.sc.cell_secs;
        let want = if on { Some(idx as u32 + 1) } else { None };
        let have = self.cell.as_ref().map(|c| c.index);
        if have != want {
            self.end_cell();
            if let Some(i) = want {
                let snap = self.gate.snapshot();
                let t = snap.throttle;
                self.cell = Some(CellAcc {
                    index: i,
                    start: idx * period,
                    stops0: t.stops,
                    delayed0: t.delayed_writes,
                    capped0: t.delay_capped,
                    enospc0: t.enospc,
                    throttled0: t.throttled_ms,
                    ..CellAcc::default()
                });
            }
        }
        on
    }

    fn end_cell(&mut self) {
        let Some(c) = self.cell.take() else { return };
        let snap = self.gate.snapshot();
        let t = snap.throttle;
        let secs = self.sc.cell_secs;
        let mut per = c.per_sec.clone();
        if per.is_empty() {
            per.push(0.0);
        }
        let head: Vec<f64> = per.iter().take(10).copied().collect();
        let tail: Vec<f64> = per.iter().skip(10).copied().collect();
        let mean = |v: &[f64]| {
            if v.is_empty() {
                0.0
            } else {
                v.iter().sum::<f64>() / v.len() as f64
            }
        };
        self.cells.push(CellReport {
            index: c.index,
            secs,
            iops: c.writes as f64 / secs,
            burst_iops: mean(&head),
            sustained_iops: if tail.is_empty() {
                mean(&head)
            } else {
                mean(&tail)
            },
            min_1s_iops: per.iter().copied().fold(f64::MAX, f64::min),
            p50: c.hist.quantile(0.5),
            p99: c.hist.quantile(0.99),
            p999: c.hist.quantile(0.999),
            max: c.hist.max(),
            stops: t.stops.saturating_sub(c.stops0),
            episodes: c.episodes,
            throttled_s: t.throttled_ms.saturating_sub(c.throttled0) as f64 / 1000.0,
            delayed_writes: t.delayed_writes.saturating_sub(c.delayed0),
            delay_max_ms: t.delay_max_ms,
            delay_capped: t.delay_capped.saturating_sub(c.capped0),
            enospc: t.enospc.saturating_sub(c.enospc0),
            first_stop_s: c.first_stop,
            peak_used: c.peak_used,
            limit: snap.limit,
            ring_wait_max: c.ring_wait_max,
            writes: c.writes,
        });
    }

    // ----- one step -----

    fn step(&mut self) {
        self.clock.advance(self.dt);
        self.now = self.clock.monotonic();
        let on = self.workload_on();
        self.update_disk();
        if self.now.saturating_sub(self.last_refresh) >= Duration::from_secs(1) {
            self.gate.refresh();
            self.last_refresh = self.now;
        }
        self.gate.tick();
        let snap = self.gate.snapshot();
        self.track_state(&snap);
        self.engine_tick(&snap);
        self.release_held();
        self.poll_waiting();
        self.wake_ring();
        self.serve_appends();
        if on {
            self.issue_idle();
        }
        self.coordinator(&snap);
        if self.sc.gc_workers > 0 && !matches!(self.coord, Coord::Idle) {
            // G4: the collector works on its own threads.
            let dt = self.dt * self.sc.gc_workers;
            self.gc_step(dt);
        }
        self.sample(&snap);
    }

    /// What the host file system holds, as `statvfs` would say: the segments
    /// on disk (condemned ones until they are deleted, the cut's output as it
    /// is written), the preallocated ring files (`journal_max`, of which the
    /// ledger counts only the records) and what others hold. The ledger's
    /// over-count (the C2 bound while a cut runs) is not on disk.
    fn update_disk(&mut self) {
        let mut disk =
            self.store.disk_bytes() + self.sc.journal_max as f64 + self.sc.fs_external as f64;
        if let Coord::Cutting(job) = &self.coord {
            let total = job.end.saturating_sub(job.start).as_secs_f64().max(1e-9);
            let done = (self.now.saturating_sub(job.start).as_secs_f64() / total).clamp(0.0, 1.0);
            disk += job.store_bytes * done;
        }
        let mut free = self.sc.fs_total as f64 - disk;
        if self.sc.anchor_disk {
            // P0 (c): the limit's anchor is the disk figure alone: what the
            // ledger holds above the disk (the bound in flight) is taken off.
            if let Coord::Cutting(job) = &self.coord {
                if !job.trued {
                    let total = job.end.saturating_sub(job.start).as_secs_f64().max(1e-9);
                    let done =
                        (self.now.saturating_sub(job.start).as_secs_f64() / total).clamp(0.0, 1.0);
                    free -= (job.bound as f64 - job.store_bytes * done).max(0.0);
                }
            }
        }
        let free = free.max(0.0) as u64;
        self.fs.set_free(free);
    }

    fn track_state(&mut self, snap: &lunix_iface::space::SpaceSnapshot) {
        let st = snap.throttle.state;
        let rel = self.rel();
        if self.prev_state == ThrottleState::Open
            && st != ThrottleState::Open
            && self.band_entry.is_none()
        {
            self.band_entry = Some((rel, snap.throttle.reclaim_rate));
            if self.cut_journal > 0.0 {
                self.amp_pre_band = Some(self.cut_written / self.cut_journal);
            }
        }
        let stopped = |s: ThrottleState| matches!(s, ThrottleState::Stop | ThrottleState::Full);
        if stopped(st) && self.stop_start.is_none() {
            self.stop_start = Some(rel);
            self.first_stop_abs.get_or_insert(rel);
            if let Some(c) = &mut self.cell {
                if c.first_stop.is_none() {
                    c.first_stop = Some(rel - c.start);
                }
            }
        }
        if !stopped(st) {
            if let Some(s) = self.stop_start.take() {
                let ep = StopEpisode {
                    start: s,
                    secs: rel - s,
                };
                if let Some(c) = &mut self.cell {
                    c.episodes.push(ep);
                } else if let Some(last) = self.cells.last_mut() {
                    last.episodes.push(ep);
                }
                self.gc.urgent_reset();
                if self.sc.p7 {
                    self.p7_reset();
                }
            }
        }
        self.prev_state = st;
        let used = snap.used();
        self.peak_ratio = self.peak_ratio.max(used as f64 / snap.limit.max(1) as f64);
        if let Some(c) = &mut self.cell {
            c.peak_used = c.peak_used.max(used);
        }
    }

    fn engine_tick(&mut self, snap: &lunix_iface::space::SpaceSnapshot) {
        let inp = PolicyInput {
            snap,
            now: self.now,
            base_params: self.base_params,
            base_budget: self.base_budget,
            rate_factor: self.sc.rate_factor,
            compact_per_pass: self.sc.compact_per_pass,
        };
        let mut s = self.policy.settings(&inp);
        if !self.ring_parked.is_empty() {
            self.yield_until = self.now + Duration::from_secs(2);
        }
        let yields = matches!(s.level, Level::Normal | Level::Lead)
            && !s.no_yield
            && self.now < self.yield_until;
        if yields {
            s.params = yielding(s.params);
        }
        self.eff = s;
        // The ring pace, as `update_ring_pace` feeds it.
        let ratio = self.ring_reserved as f64 / self.sc.journal_max as f64;
        let waiting = !self.ring_parked.is_empty();
        let rate = self.drain.sustained_rate().map(|r| r.max(1));
        let want = self.pacer.observe(ratio, waiting, rate, self.now);
        let on = want.is_some();
        if on != self.ring_pace_on || on {
            self.gate.set_ring_pace(want);
            self.ring_pace_on = on;
        }
        let forecast = if self.policy.wants_forecast() {
            self.store.forecast(self.eff.params.grace, self.now)
        } else {
            Vec::new()
        };
        let gap = self
            .gc
            .next_mark_at(&self.gcfg, &self.eff)
            .saturating_sub(self.now);
        let view = AdmissionView {
            now: self.now,
            snap,
            a_hat: self.a_hat,
            journal_debt: self.ring_reserved,
            next_delete_in: forecast.first().map(|f| f.0.max(gap)),
            forecast: &forecast,
            store_rate: self.store_rate,
            gap,
        };
        self.adm_rate = self.policy.admission_rate(&view);
    }

    /// P7, emulated: the gate left `stop`; the requests it holds are issued
    /// again with their arrival set to now (a cancel and a new reservation),
    /// so the delay cap counts from the exit. `full_timeout` restarts too,
    /// which the real P7 would not do.
    fn p7_reset(&mut self) {
        let list = std::mem::take(&mut self.waiting);
        for i in list {
            let w = match &self.slots[i].st {
                SlotSt::Waiting(w) => w.clone(),
                _ => continue,
            };
            if w.cancel() {
                self.issue(i, self.now, ReserveClass::Bulk);
            } else {
                self.waiting.push(i);
            }
        }
    }

    // ----- requests -----

    fn issue_idle(&mut self) {
        for i in 0..self.slots.len() {
            if let SlotSt::Idle { until } = self.slots[i].st {
                if self.now >= until {
                    self.issue(i, self.now, ReserveClass::Bulk);
                }
            }
        }
    }

    fn issue(&mut self, i: usize, arrived: Duration, class: ReserveClass) {
        self.slots[i].arrived = arrived;
        if class == ReserveClass::Bulk && self.adm_rate.is_some() && !self.admit_tokens() {
            self.slots[i].st = SlotSt::Held;
            self.held.push_back(i);
            return;
        }
        match self.gate.reserve_since(
            SpaceKind::JournalReserved,
            self.req_bytes,
            None,
            arrived,
            class,
        ) {
            Ok(ReserveOutcome::Reserved) => self.admitted(i),
            Ok(ReserveOutcome::Paced(w) | ReserveOutcome::Parked(w)) => {
                self.slots[i].st = SlotSt::Waiting(w);
                self.waiting.push(i);
            }
            Ok(ReserveOutcome::NoSpace) | Err(_) => self.fail(i),
        }
    }

    fn fail(&mut self, i: usize) {
        self.slots[i].st = SlotSt::Idle {
            until: self.now + Duration::from_millis(500),
        };
    }

    /// The admission hook's bucket (a candidate's pace in store bytes).
    fn admit_tokens(&mut self) -> bool {
        let Some(rate) = self.adm_rate else {
            return true;
        };
        let dt = self.now.saturating_sub(self.adm_last).as_secs_f64();
        self.adm_last = self.now;
        let cap = (rate * 0.05).max(self.req_bytes as f64 * 2.0);
        self.adm_tokens = (self.adm_tokens + rate * dt).min(cap);
        if self.adm_tokens >= self.req_bytes as f64 {
            self.adm_tokens -= self.req_bytes as f64;
            true
        } else {
            false
        }
    }

    fn release_held(&mut self) {
        while let Some(&i) = self.held.front() {
            if self.adm_rate.is_some() && !self.admit_tokens() {
                break;
            }
            self.held.pop_front();
            let arrived = self.slots[i].arrived;
            self.slots[i].st = SlotSt::Idle { until: self.now };
            // Already paid its tokens: straight to the gate.
            let was = self.adm_rate.take();
            self.issue(i, arrived, ReserveClass::Bulk);
            self.adm_rate = was;
        }
    }

    fn poll_waiting(&mut self) {
        let mut still = Vec::with_capacity(self.waiting.len());
        let list = std::mem::take(&mut self.waiting);
        for i in list {
            let state = match &self.slots[i].st {
                SlotSt::Waiting(w) => w.state(),
                _ => continue,
            };
            match state {
                WaiterState::Pending => still.push(i),
                WaiterState::Granted => self.admitted(i),
                WaiterState::TimedOut | WaiterState::Cancelled => self.fail(i),
            }
        }
        still.append(&mut self.waiting);
        self.waiting = still;
    }

    /// The gate admitted the request: it needs a place in the ring.
    fn admitted(&mut self, i: usize) {
        let b = self.req_bytes;
        if self.ring_reserved + b <= self.sc.ring_park {
            self.ring_reserved += b;
            self.admitted_bytes += b as f64;
            self.slots[i].st = SlotSt::Appending;
            self.append_q.push_back(i);
        } else {
            // The ring refused: the engine gives the charge back and retries.
            self.gate.release(SpaceKind::JournalReserved, b);
            self.slots[i].st = SlotSt::RingParked { since: self.now };
            self.ring_parked.push_back(i);
        }
    }

    fn wake_ring(&mut self) {
        while let Some(&i) = self.ring_parked.front() {
            if self.ring_reserved + self.req_bytes > self.sc.ring_park {
                break;
            }
            self.ring_parked.pop_front();
            if let SlotSt::RingParked { since } = self.slots[i].st {
                let w = self.now.saturating_sub(since).as_secs_f64();
                if let Some(c) = &mut self.cell {
                    c.ring_wait_max = c.ring_wait_max.max(w);
                }
            }
            let arrived = self.slots[i].arrived;
            self.slots[i].st = SlotSt::Idle { until: self.now };
            self.issue(i, arrived, ReserveClass::Retry);
        }
    }

    fn append_rate(&self) -> f64 {
        let d = &self.sc.device;
        let rec = self.sc.record() as f64;
        let mut r = if d.engine_iops > 0.0 {
            d.engine_iops
        } else {
            f64::INFINITY
        };
        let jbw = if d.shared_bw > 0.0 {
            if matches!(self.coord, Coord::Cutting(_)) {
                d.shared_bw * 0.4
            } else {
                d.shared_bw
            }
        } else {
            d.journal_bw
        };
        if jbw > 0.0 {
            r = r.min(jbw / rec);
        }
        r
    }

    fn serve_appends(&mut self) {
        let rate = self.append_rate();
        let agg = f64::from(self.sc.agg);
        self.app_budget += rate * self.dt.as_secs_f64();
        let cap = rate * self.dt.as_secs_f64() * 2.0 + agg;
        if self.app_budget > cap {
            self.app_budget = cap;
        }
        while self.app_budget >= agg {
            let Some(i) = self.append_q.pop_front() else {
                break;
            };
            self.app_budget -= agg;
            let lat = self.now.saturating_sub(self.slots[i].arrived).as_secs_f64() * 1000.0;
            self.ring_appended += self.req_bytes;
            self.sec_completed += agg;
            self.sec_lat_max = self.sec_lat_max.max(lat);
            if let Some(c) = &mut self.cell {
                c.hist.record(lat, u64::from(self.sc.agg));
                c.writes += u64::from(self.sc.agg);
                c.cur_sec += agg;
            }
            self.slots[i].st = SlotSt::Idle { until: self.now };
            if self.cell.is_some() {
                self.issue(i, self.now, ReserveClass::Bulk);
            }
        }
    }

    // ----- the coordinator -----

    fn wanted(&self) -> bool {
        let cut_t = Duration::from_secs_f64(self.sc.commit_interval_s);
        let appended = self.ring_appended > 0;
        let full = self.ring_reserved as f64 >= self.sc.cut_trigger * self.sc.ring_park as f64;
        let cadence = self.sc.cut_cadence_s > 0.0
            && self.prev_state != ThrottleState::Open
            && self.now.saturating_sub(self.last_commit).as_secs_f64() >= self.sc.cut_cadence_s;
        (appended && cadence)
            || (appended
                && (full
                    || !self.ring_parked.is_empty()
                    || self.now.saturating_sub(self.last_commit) >= cut_t))
            || (self.store.has_unnamed()
                && self.now.saturating_sub(self.last_commit) >= Duration::from_secs(1))
    }

    fn coordinator(&mut self, snap: &lunix_iface::space::SpaceSnapshot) {
        match std::mem::replace(&mut self.coord, Coord::Idle) {
            Coord::Cutting(mut job) => {
                if self.now >= job.end {
                    self.finish_cut(job);
                } else {
                    if self.sc.bound_mode == BoundMode::Early && !job.trued {
                        let total = job.end.saturating_sub(job.start).as_secs_f64();
                        if self.now.saturating_sub(job.start).as_secs_f64() >= 0.7 * total {
                            self.true_up(&job);
                            job.trued = true;
                        }
                    }
                    self.coord = Coord::Cutting(job);
                }
            }
            Coord::Blocked { until } => {
                if self.now < until {
                    self.coord = Coord::Blocked { until };
                }
            }
            Coord::Gc { until } => {
                if self.now < until && self.gc.busy() {
                    self.coord = Coord::Gc { until };
                    let dt = self.dt;
                    self.gc_step(dt);
                }
            }
            Coord::Idle => self.coordinator_idle(snap),
        }
    }

    fn coordinator_idle(&mut self, snap: &lunix_iface::space::SpaceSnapshot) {
        let stopped = snap.throttle.space_waiters > 0 || snap.full;
        if stopped && self.gc.urgent_allowed(self.now) {
            let want = snap.throttle.resume as f64
                + snap.throttle.space_waiters as f64 * self.req_bytes as f64;
            self.run_urgent(want.max(1.0));
            return;
        }
        if self.now >= self.cut_retry && self.wanted() {
            self.start_cut();
            if matches!(self.coord, Coord::Cutting(_) | Coord::Blocked { .. }) {
                return;
            }
            // A commit that failed for space still lets the collector work.
        }
        let dt = self.dt;
        self.gc_step(dt);
    }

    /// The urgent pass (the ladder's compact step, or a commit that failed
    /// for space): holds the coordinator for its duration.
    fn run_urgent(&mut self, want: f64) {
        let floor =
            Duration::from_secs(10).saturating_add(self.base_params.read_timeout.saturating_mul(2));
        let mut env = GcEnv {
            store: &mut self.store,
            gate: &self.gate,
            books: &mut self.books,
            settings: &self.eff,
            cfg: &self.gcfg,
            now: self.now,
            rng: &mut self.jit,
            written: self.written,
            growth_trigger: None,
            slow: 1.0,
        };
        let spent = self.gc.urgent(&mut env, want, floor);
        self.written = env.written;
        self.coord = Coord::Blocked {
            until: self.now + spent,
        };
    }

    fn gc_step(&mut self, avail: Duration) -> Duration {
        let mut env = GcEnv {
            store: &mut self.store,
            gate: &self.gate,
            books: &mut self.books,
            settings: &self.eff,
            cfg: &self.gcfg,
            now: self.now,
            rng: &mut self.jit,
            written: self.written,
            growth_trigger: self.eff.growth_trigger,
            slow: 1.0,
        };
        let used = self.gc.step(&mut env, avail);
        self.written = env.written;
        used
    }

    fn start_cut(&mut self) {
        let rec = self.sc.record() as f64;
        let one_file = self.sc.ring_file;
        let half = self.sc.journal_max / 2;
        let budget = self
            .drain
            .budget(Duration::from_secs_f64(self.sc.cut_target_s))
            .map_or(one_file, |b| b.min(half))
            .max(one_file);
        let bytes = self.ring_appended.min(budget);
        let n = bytes as f64 / rec;
        let wc = self.sc.ws_chunks();
        let distinct = match self.sc.pattern {
            Pattern::Random => distinct_random(n, wc),
            Pattern::Sequential => (n * self.sc.block as f64 / self.sc.chunk as f64)
                .ceil()
                .min(wc),
        };
        let distinct = distinct * self.sc.merge;
        let store_bytes = distinct * self.sc.chunk_stored();
        // The bound the cut reserves: the uncompressed size of its chunks
        // (the code), or the compressed estimate with a margin (P0 a).
        let bound = match self.sc.bound_mode {
            BoundMode::Compressed => (store_bytes * 1.15) as u64,
            _ => (distinct * self.sc.chunk as f64) as u64,
        };
        if self
            .books
            .charge_headroom(&self.gate, bound as f64)
            .is_err()
        {
            // A commit that fails for space runs GC's urgent pass
            // (`space_short`, at least the back-off after the last one) and
            // tries again at the next tick.
            self.cut_retry = self.now + Duration::from_millis(100);
            if self.gc.urgent_allowed(self.now) {
                self.run_urgent(1.0);
            }
            return;
        }
        let d = &self.sc.device;
        let bw = if d.shared_bw > 0.0 {
            d.shared_bw * 0.6
        } else {
            d.store_bw
        };
        let secs = (d.cut_fixed_ms / 1000.0 + store_bytes / bw) * self.jit.jitter(d.jitter);
        let secs = secs.max(0.001);
        self.coord = Coord::Cutting(CutJob {
            start: self.now,
            end: self.now + Duration::from_secs_f64(secs),
            bytes,
            distinct,
            store_bytes,
            bound,
            trued: false,
        });
    }

    /// C4: the bound is trued up to what was written.
    fn true_up(&mut self, job: &CutJob) {
        let actual = job.store_bytes.round() as u64;
        let _ = self
            .gate
            .transfer(SpaceKind::Segments, SpaceKind::Segments, job.bound, actual);
        self.books.segments = self.books.segments.saturating_sub(job.bound) + actual;
    }

    fn finish_cut(&mut self, job: CutJob) {
        if !job.trued {
            self.true_up(&job);
        }
        // C9: the journal bytes go.
        if job.bytes > 0 {
            self.gate.release(SpaceKind::JournalReserved, job.bytes);
        }
        self.ring_reserved = self.ring_reserved.saturating_sub(job.bytes);
        self.ring_appended = self.ring_appended.saturating_sub(job.bytes);
        self.store.write_segments(job.distinct, job.distinct, false);
        self.store.cut_done(job.distinct, self.now, &mut self.rng);
        let mut c5_block = Duration::ZERO;
        if self.sc.condemn_at_cut {
            if self.sc.c5_cost {
                // A proposal: the collector flags what is dead (variant N+2);
                // it costs a list sync and a flag per segment, runs only
                // while no pass is in progress, and can fail.
                let n = self.store.condemnable_count();
                if n > 0 {
                    if self.gc.busy() || self.jit.unit() < self.sc.c5_error {
                        self.c5_skipped += 1;
                    } else {
                        let work = Duration::from_secs_f64(
                            (self.sc.list_sync_ms + n as f64 * self.sc.condemn_ms) / 1000.0,
                        );
                        let c = self.store.mark(self.now);
                        self.store.shift_condemned(self.now, work);
                        let listed = self.store.condemned_bytes();
                        self.gc.external_condemn(c, self.now, listed);
                        c5_block = work;
                    }
                }
            } else {
                let c = self.store.mark(self.now);
                let listed = self.store.condemned_bytes();
                self.gc.external_condemn(c, self.now, listed);
            }
        }
        self.cut_written += job.store_bytes;
        self.cut_journal += job.bytes as f64;
        if self.sc.a_lag && job.bytes > 0 {
            self.a_hist.push_back((job.store_bytes, job.bytes as f64));
            if self.a_hist.len() > 8 {
                self.a_hist.pop_front();
            }
            let (s8, j8) = self
                .a_hist
                .iter()
                .fold((0.0, 0.0), |a, x| (a.0 + x.0, a.1 + x.1));
            let last = job.store_bytes / job.bytes as f64;
            self.a_hat = (s8 / j8).max(last) * (1.0 + self.sc.a_bias);
        }
        self.written += job.store_bytes;
        let took = self.now.saturating_sub(job.start);
        let since = self.prev_cut_end.map(|e| self.now.saturating_sub(e));
        if job.bytes > 0 {
            self.drain.push(job.bytes, took, since);
        }
        self.prev_cut_end = Some(self.now);
        self.last_commit = self.now;
        // The collector's step at C10 and its share.
        let div = self.eff.share_divisor.max(1);
        let mut share = (took / div).min(self.eff.share_max);
        if self.now < self.yield_until && !self.eff.no_yield && self.eff.level < Level::Raised {
            share = self.eff.budget.max_duration;
        }
        let first = self
            .eff
            .budget
            .max_duration
            .min(self.dt.max(Duration::from_millis(1)));
        self.gc_step(first);
        if self.gc.busy() && !share.is_zero() {
            self.coord = Coord::Gc {
                until: self.now + share,
            };
        } else {
            self.coord = Coord::Idle;
        }
        if !c5_block.is_zero() {
            self.coord = Coord::Blocked {
                until: self.now + c5_block,
            };
        }
    }

    // ----- the trace -----

    fn sample(&mut self, snap: &lunix_iface::space::SpaceSnapshot) {
        if self.now < self.next_sample {
            return;
        }
        self.next_sample += Duration::from_secs(1);
        let t = snap.throttle;
        // A over the last interval: store bytes per journal byte cut.
        let (cw, cj) = (self.cut_written, self.cut_journal);
        self.store_rate = 0.7 * self.store_rate + 0.3 * (cw - self.cw_prev);
        self.cw_prev = cw;
        if !self.sc.a_lag && cj - self.a_prev.1 > 1e6 {
            let a = (cw - self.a_prev.0) / (cj - self.a_prev.1);
            self.a_hat = 0.7 * self.a_hat + 0.3 * a;
            self.a_prev = (cw, cj);
        }
        let row = SecRow {
            t: self.rel(),
            cell: self.cell.as_ref().map_or(0, |c| c.index),
            iops: self.sec_completed,
            lat_max_ms: self.sec_lat_max,
            state: t.state.name(),
            level: self.eff.level.name(),
            used: snap.used(),
            limit: snap.limit,
            headroom: t.headroom,
            journal: snap.by_kind[SpaceKind::JournalReserved.index()],
            disk: self.store.disk_bytes() as u64,
            live: self.store.live_bytes() as u64,
            condemned: self.store.condemned_bytes() as u64,
            dead_uncondemned: self.store.dead_uncondemned_bytes() as u64,
            ingest_rate: t.ingest_rate,
            reclaim_rate: t.reclaim_rate,
            stops: t.stops,
            waiters: snap.waiters,
            delayed: t.delayed_writes,
            a_hat: self.a_hat,
            coord: self.coord.name(),
            gc: match self.gc.phase {
                Phase::Idle => "idle",
                Phase::Marking { .. } => "mark",
                Phase::Costs { .. } => "index",
                Phase::Compacting { .. } => "compact",
            },
        };
        if let Some(c) = &mut self.cell {
            c.per_sec.push(c.cur_sec);
            c.cur_sec = 0.0;
        }
        self.sec_completed = 0.0;
        self.sec_lat_max = 0.0;
        self.trace.push(row);
    }

    // ----- the end -----

    fn finish(self) -> RunReport {
        let st = self.gc.stats;
        let rel = |d: Option<Duration>| d.map(|d| d.saturating_sub(self.base).as_secs_f64());
        let first_condemn_s = rel(st.first_condemn);
        let first_delete_s = rel(st.first_delete);
        let mut dead = DeadSummary::default();
        let mut w = 0.0;
        for d in &self.store.dead_times {
            dead.segments += 1;
            w += d.bytes;
            dead.retention += d.retention * d.bytes;
            dead.detection += d.detection * d.bytes;
            dead.grace += d.grace * d.bytes;
            dead.recheck += d.recheck * d.bytes;
        }
        if w > 0.0 {
            dead.retention /= w;
            dead.detection /= w;
            dead.grace /= w;
            dead.recheck /= w;
        }
        if st.release_bytes > 0.0 {
            dead.delete = st.release_weighted / st.release_bytes;
        }
        let total = self.sc.total_secs();
        // The first-fill window is the first 90 s for every row, whatever the
        // candidate does to the first delete.
        let ff_end = 90.0_f64.min(total);
        let first_fill = window(&self.trace, 0.0, ff_end);
        let steady = window(&self.trace, ff_end, total + 1.0);
        RunReport {
            scenario: self.sc.name.clone(),
            policy: self.policy.name().to_string(),
            seed: self.sc.seed,
            cells: self.cells,
            band_entry: self.band_entry.map(|(when, rate)| (rate as f64, when)),
            trace: self.trace,
            first_condemn_s,
            first_delete_s,
            amp_pre_band: self.amp_pre_band,
            amp_total: if self.cut_journal > 0.0 {
                self.cut_written / self.cut_journal
            } else {
                0.0
            },
            gc_wa: if self.cut_written > 0.0 {
                st.compact_written / self.cut_written
            } else {
                0.0
            },
            marks: st.marks,
            urgent: st.urgent,
            first_stop_abs: self.first_stop_abs,
            peak_condemned: st.peak_condemned,
            reclaim_whole: self.store.condemned_whole,
            reclaim_moved: self.store.condemned_moved,
            victim_hist: self.store.victim_hist,
            gc_share: st.busy_time.as_secs_f64() / total.max(1.0),
            dead,
            peak_used_over_limit: self.peak_ratio,
            first_fill,
            steady,
        }
    }
}
