//! [`LunixSpace`]: the real [`SpaceGate`].

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::Duration;

use lunix_format::local::SpaceSettings;
use lunix_iface::clock::Clock;
use lunix_iface::space::{
    Ladder, LadderStep, LadderStepKind, ReserveClass, ReserveOutcome, SpaceEvents, SpaceGate,
    SpaceKind, SpaceSnapshot, Urgency,
};
use lunix_iface::wake::WakeFd;
use lunix_iface::{Error, Result};

use crate::ledger::{FsView, Ledger};
use crate::policy::Policy;
use crate::probe::FsProbe;
use crate::throttle::ThrottleConfig;

/// How often the timer thread re-reads the file system.
pub const REFRESH_INTERVAL: Duration = Duration::from_secs(1);

/// The ladder thread's pause after a run that freed nothing.
const LADDER_IDLE_PAUSE: Duration = Duration::from_millis(100);

/// The daemon's default wait at the limit when `space.full_timeout_ms` is
/// null (ADR-32, FREEZE rule change F1): 60 s, the dm-thin default, under the
/// kernel's 120 s hung-task threshold.
pub const DEFAULT_FULL_TIMEOUT: Duration = Duration::from_secs(60);

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// What a [`LunixSpace`] is built from.
#[derive(Clone, Debug)]
pub struct SpaceConfig {
    /// The `space` object of `volume.json`.
    pub settings: SpaceSettings,
    /// The segment size; twice this is added to `settings.headroom` as the
    /// region writes can never take (DESIGN 4.3). 0 leaves the headroom alone.
    pub segment_size: u64,
    /// Whether the timer thread runs: it grants, expires waiters and re-reads
    /// the file system by itself (as the real daemon wants). Tests that drive
    /// a manual clock turn this off and call `tick` and `refresh` themselves.
    /// The ladder always runs on its own thread.
    pub background_timers: bool,
    /// The wait at the limit when `settings.full_timeout_ms` is null (the
    /// daemon's `full_timeout_ms`; [`DEFAULT_FULL_TIMEOUT`] by default).
    pub full_timeout: Duration,
    /// The paced band below the limit (ADR-32); the daemon's `throttle*`
    /// keys and the volume's `gc_rate`.
    pub throttle: ThrottleConfig,
}

impl SpaceConfig {
    /// The given settings, 16 MiB segments, background timers on, the default
    /// full timeout.
    pub fn new(settings: SpaceSettings) -> Self {
        Self {
            settings,
            segment_size: 16 << 20,
            background_timers: true,
            full_timeout: DEFAULT_FULL_TIMEOUT,
            throttle: ThrottleConfig::default(),
        }
    }
}

type SharedStep = Arc<Mutex<Box<dyn LadderStep>>>;

struct State {
    ledger: Ledger,
    /// Bytes the next ladder run should try to free; 0 means no run wanted.
    ladder_want: u64,
    /// `clock.monotonic()` of the last file-system reading.
    last_refresh: Duration,
    shutdown: bool,
}

struct Shared {
    clock: Arc<dyn Clock>,
    probe: Arc<dyn FsProbe>,
    timers: bool,
    /// The daemon's default `full_timeout` (for a null `full_timeout_ms`).
    default_timeout: Duration,
    st: Mutex<State>,
    /// Wakes the timer thread (a new deadline, shutdown).
    cv: Condvar,
    /// Wakes the ladder thread (a run is wanted, shutdown).
    ladder_cv: Condvar,
    steps: Mutex<BTreeMap<LadderStepKind, SharedStep>>,
    step_errors: AtomicU64,
    ladder_runs: AtomicU64,
}

impl Shared {
    /// Reads the file system (no lock held across the call) and applies it.
    ///
    /// The reading is anchored to the ledger level it belongs to. The ledger
    /// is sampled before and after the call; if it moved the call is repeated
    /// (three tries), then the lower sample is used: bytes released during the
    /// call may already be off the disk, bytes reserved during it may not be
    /// on it yet, and the lower anchor is the safe one in both cases.
    fn refresh(&self) {
        let mut mark = 0;
        let mut stat = None;
        let mut err = None;
        let now = self.clock.monotonic();
        for _ in 0..3 {
            let before = lock(&self.st).ledger.used();
            match self.probe.stat() {
                Ok(s) => {
                    let after = lock(&self.st).ledger.used();
                    mark = before.min(after);
                    stat = Some(s);
                    err = None;
                    if before == after {
                        break;
                    }
                }
                Err(e) => {
                    err = Some(e);
                    break;
                }
            }
        }
        let mut st = lock(&self.st);
        st.last_refresh = now;
        if let Some(stat) = stat {
            st.ledger.fs = FsView {
                stat,
                used_mark: mark,
            };
            let t = self.clock.monotonic();
            st.ledger.grant_waiters(t);
        }
        if err.is_some_and(|e| e.raw_os_error() == Some(5)) {
            self.fatal(&mut st);
        }
    }

    fn fatal(&self, st: &mut State) {
        st.ledger.read_only = true;
        let now = self.clock.monotonic();
        st.ledger.expire_all(now);
    }

    fn kick_ladder(&self, st: &mut State, want: u64) {
        st.ladder_want = st.ladder_want.max(want.max(1));
        self.ladder_cv.notify_all();
    }

    /// Runs the steps in order until the waiting requests fit; returns the
    /// bytes they freed.
    fn run_ladder(&self, mut want: u64) -> u64 {
        let steps: Vec<SharedStep> = lock(&self.steps).values().cloned().collect();
        self.ladder_runs.fetch_add(1, Ordering::Relaxed);
        let mut total = 0u64;
        for step in steps {
            let outcome = lock(&step).run(want, Urgency::Urgent);
            match outcome {
                Ok(freed) if freed.0 > 0 => {
                    total = total.saturating_add(freed.0);
                    let st = lock(&self.st);
                    want = st.ledger.ladder_want();
                    if want == 0 || (!st.ledger.full && st.ledger.head_shortfall() == 0) {
                        return total;
                    }
                }
                Ok(_) => {}
                Err(_) => {
                    self.step_errors.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
        total
    }

    /// The ladder thread: runs the steps when a run is wanted. It may block
    /// (the compact step waits for the engine's coordinator); grants and
    /// deadlines never wait for it (the timer thread, `tick`, `release`).
    fn ladder_main(&self) {
        let mut st = lock(&self.st);
        loop {
            if st.shutdown {
                return;
            }
            if st.ladder_want > 0 {
                let want = std::mem::take(&mut st.ladder_want);
                drop(st);
                let freed = self.run_ladder(want);
                st = lock(&self.st);
                if freed == 0 {
                    // A run that freed nothing (the compact step's own
                    // back-off answers at once): pause before the next one,
                    // whatever asks meanwhile, so refused writes in a loop
                    // cannot spin this thread. Shutdown ends the pause.
                    let until = std::time::Instant::now() + LADDER_IDLE_PAUSE;
                    while !st.shutdown {
                        let left = until.saturating_duration_since(std::time::Instant::now());
                        if left.is_zero() {
                            break;
                        }
                        st = self
                            .ladder_cv
                            .wait_timeout(st, left)
                            .unwrap_or_else(|p| p.into_inner())
                            .0;
                    }
                }
                continue;
            }
            st = self.ladder_cv.wait(st).unwrap_or_else(|p| p.into_inner());
        }
    }

    /// The timer thread (with `background_timers`): expires waiters at their
    /// deadline, re-reads the file system every [`REFRESH_INTERVAL`] and asks
    /// the ladder again while requests stay parked or the volume is full. It
    /// takes only the gate's lock and the probe, never a ladder step.
    fn timer_main(&self) {
        let mut st = lock(&self.st);
        loop {
            if st.shutdown {
                return;
            }
            let now = self.clock.monotonic();
            st.ledger.expire(now);
            let due = st.last_refresh + REFRESH_INTERVAL;
            if now >= due {
                drop(st);
                self.refresh();
                st = lock(&self.st);
                // Parked and still short: try the ladder again (the compact
                // step itself backs off between urgent passes).
                if (st.ledger.head_shortfall() > 0 || st.ledger.full)
                    && !lock(&self.steps).is_empty()
                {
                    let want = st.ledger.ladder_want();
                    self.kick_ladder(&mut st, want);
                }
                continue;
            }
            let mut sleep = due - now;
            if let Some(d) = st.ledger.next_wake(now) {
                sleep = sleep.min(d.saturating_sub(now));
            }
            sleep = sleep.max(Duration::from_millis(1));
            st = self
                .cv
                .wait_timeout(st, sleep)
                .unwrap_or_else(|p| p.into_inner())
                .0;
        }
    }
}

/// The space ledger and ladder of one volume.
///
/// One short mutex guards the counters, so every method is atomic for
/// observers; the file system is read outside it. A ladder thread runs the
/// ladder steps; with `background_timers` a timer thread expires waiters and
/// refreshes the file-system reading. A registered step that holds an `Arc`
/// of the gate forms a cycle: call [`shutdown`](Self::shutdown) to break it
/// and stop the threads.
pub struct LunixSpace {
    sh: Arc<Shared>,
    threads: Mutex<Vec<JoinHandle<()>>>,
}

impl LunixSpace {
    /// A gate over `probe`, expiring waiters on `clock`. Reads the file system
    /// once before returning.
    ///
    /// # Errors
    /// `Invalid` for settings that fail `VolumeJson` validation or a zero
    /// default full timeout; the probe's I/O error if the first reading fails.
    pub fn new(
        config: SpaceConfig,
        probe: Arc<dyn FsProbe>,
        clock: Arc<dyn Clock>,
    ) -> Result<Arc<Self>> {
        if config.full_timeout.is_zero() {
            return Err(Error::invalid("the default full timeout must be positive"));
        }
        let policy = Policy::from_settings(&config.settings, config.full_timeout)?;
        let stat = probe.stat()?;
        let last_refresh = clock.monotonic();
        let mut ledger = Ledger::new(policy, config.segment_size, config.throttle, last_refresh);
        ledger.fs = FsView { stat, used_mark: 0 };
        let sh = Arc::new(Shared {
            clock,
            probe,
            timers: config.background_timers,
            default_timeout: config.full_timeout,
            st: Mutex::new(State {
                ledger,
                ladder_want: 0,
                last_refresh,
                shutdown: false,
            }),
            cv: Condvar::new(),
            ladder_cv: Condvar::new(),
            steps: Mutex::new(BTreeMap::new()),
            step_errors: AtomicU64::new(0),
            ladder_runs: AtomicU64::new(0),
        });
        let mut threads = Vec::with_capacity(2);
        let worker = Arc::clone(&sh);
        threads.push(
            std::thread::Builder::new()
                .name("lunix-space-ladder".into())
                .spawn(move || worker.ladder_main())
                .map_err(Error::Io)?,
        );
        if sh.timers {
            let worker = Arc::clone(&sh);
            let spawned = std::thread::Builder::new()
                .name("lunix-space-timer".into())
                .spawn(move || worker.timer_main());
            match spawned {
                Ok(h) => threads.push(h),
                Err(e) => {
                    let gate = Self {
                        sh,
                        threads: Mutex::new(threads),
                    };
                    gate.shutdown();
                    return Err(Error::Io(e));
                }
            }
        }
        Ok(Arc::new(Self {
            sh,
            threads: Mutex::new(threads),
        }))
    }

    /// Reads the file system now and re-evaluates the parked requests.
    pub fn refresh(&self) {
        self.sh.refresh();
    }

    /// The quiesce rule (ADR-32, decision M5): from now on nothing parks.
    /// What fits is granted, every request still waiting for space fails
    /// (`TimedOut`, answered `ENOSPC`), and later reservations that do not
    /// fit fail at once, so the queues can drain. One-way: the gate of a
    /// quiesced volume is not reopened.
    pub fn begin_close(&self) {
        let now = self.sh.clock.monotonic();
        let mut st = lock(&self.sh.st);
        st.ledger.closing = true;
        st.ledger.grant_waiters(now);
        st.ledger.fail_waiters(now, true);
    }

    /// The soft mark of the band now, bytes of headroom (0 with the band
    /// off).
    pub fn soft_mark(&self) -> u64 {
        lock(&self.sh.st).ledger.soft()
    }

    /// True while the volume is latched full: writes fail at once until the
    /// headroom reaches the resume mark.
    pub fn is_full(&self) -> bool {
        let mut st = lock(&self.sh.st);
        st.ledger.update_latch();
        st.ledger.full
    }

    /// How many times the volume latched full since the gate was built.
    pub fn fulls(&self) -> u64 {
        lock(&self.sh.st).ledger.fulls
    }

    /// The resume mark of the full latch now, in bytes of headroom.
    pub fn resume_mark(&self) -> u64 {
        lock(&self.sh.st).ledger.resume_mark()
    }

    /// The `full_timeout` in force (the policy's, or the daemon default for
    /// a null `full_timeout_ms`).
    pub fn full_timeout(&self) -> Duration {
        lock(&self.sh.st).ledger.policy.full_timeout
    }

    /// Stops the threads and drops the registered steps (which may hold the
    /// gate). Later calls still work on the ledger; no ladder runs.
    pub fn shutdown(&self) {
        {
            let mut st = lock(&self.sh.st);
            st.shutdown = true;
            self.sh.cv.notify_all();
            self.sh.ladder_cv.notify_all();
        }
        let handles = std::mem::take(&mut *lock(&self.threads));
        for h in handles {
            if h.thread().id() != std::thread::current().id() {
                let _ = h.join();
            }
        }
        lock(&self.sh.steps).clear();
    }

    /// How many ladder steps returned an error so far.
    pub fn step_errors(&self) -> u64 {
        self.sh.step_errors.load(Ordering::Relaxed)
    }

    /// How many times the ladder ran so far.
    pub fn ladder_runs(&self) -> u64 {
        self.sh.ladder_runs.load(Ordering::Relaxed)
    }
}

impl Drop for LunixSpace {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl Ladder for LunixSpace {
    fn register(&self, kind: LadderStepKind, step: Box<dyn LadderStep>) {
        lock(&self.sh.steps).insert(kind, Arc::new(Mutex::new(step)));
    }
}

impl SpaceEvents for LunixSpace {
    fn fatal_io(&self, _what: &str) {
        let mut st = lock(&self.sh.st);
        self.sh.fatal(&mut st);
    }

    fn is_read_only(&self) -> bool {
        lock(&self.sh.st).ledger.read_only
    }
}

impl SpaceGate for LunixSpace {
    fn reserve(
        &self,
        kind: SpaceKind,
        bytes: u64,
        wake: Option<&WakeFd>,
    ) -> Result<ReserveOutcome> {
        self.reserve_since(
            kind,
            bytes,
            wake,
            self.sh.clock.monotonic(),
            ReserveClass::Bulk,
        )
    }

    fn reserve_since(
        &self,
        kind: SpaceKind,
        bytes: u64,
        wake: Option<&WakeFd>,
        arrived: Duration,
        class: ReserveClass,
    ) -> Result<ReserveOutcome> {
        if kind == SpaceKind::Headroom {
            return Err(Error::invalid(
                "the Headroom kind is charged with reserve_headroom",
            ));
        }
        let mut st = lock(&self.sh.st);
        if st.ledger.read_only {
            return Err(Error::ReadOnly);
        }
        if bytes == 0 {
            return Ok(ReserveOutcome::Reserved);
        }
        if st.ledger.near_limit(bytes) {
            // Within 5 % of the limit: read the file system before deciding.
            drop(st);
            self.sh.refresh();
            st = lock(&self.sh.st);
            if st.ledger.read_only {
                return Err(Error::ReadOnly);
            }
        }
        let now = self.sh.clock.monotonic();
        let out = st.ledger.reserve(kind, bytes, wake, arrived, class, now);
        match &out {
            ReserveOutcome::Reserved => {}
            ReserveOutcome::Paced(_) => {
                // A new wake time: the timer thread recomputes its sleep.
                self.sh.cv.notify_all();
            }
            ReserveOutcome::Parked(_) => {
                let want = st.ledger.ladder_want();
                self.sh.kick_ladder(&mut st, want);
                self.sh.cv.notify_all();
            }
            ReserveOutcome::NoSpace => {
                // Latched: the ladder frees up to the resume mark. A plain
                // refusal (`on_full = error`): what this request is short.
                let want = if st.ledger.full {
                    st.ledger.ladder_want()
                } else {
                    st.ledger
                        .used()
                        .saturating_add(bytes)
                        .saturating_sub(st.ledger.limit())
                };
                self.sh.kick_ladder(&mut st, want);
                self.sh.cv.notify_all();
            }
        }
        Ok(out)
    }

    fn reserve_headroom(&self, kind: SpaceKind, bytes: u64) -> Result<()> {
        lock(&self.sh.st).ledger.reserve_headroom(kind, bytes)
    }

    fn release(&self, kind: SpaceKind, bytes: u64) {
        let now = self.sh.clock.monotonic();
        let mut st = lock(&self.sh.st);
        st.ledger.release(kind, bytes, now);
        if !st.ledger.waiters.is_empty() {
            self.sh.cv.notify_all();
        }
    }

    fn transfer(
        &self,
        from_kind: SpaceKind,
        to_kind: SpaceKind,
        from_bytes: u64,
        to_bytes: u64,
    ) -> Result<()> {
        let now = self.sh.clock.monotonic();
        let mut st = lock(&self.sh.st);
        let r = st
            .ledger
            .transfer(from_kind, to_kind, from_bytes, to_bytes, now);
        if !st.ledger.waiters.is_empty() {
            self.sh.cv.notify_all();
        }
        r
    }

    fn tick(&self) -> usize {
        let now = self.sh.clock.monotonic();
        lock(&self.sh.st).ledger.expire(now)
    }

    fn snapshot(&self) -> SpaceSnapshot {
        let now = self.sh.clock.monotonic();
        let mut st = lock(&self.sh.st);
        st.ledger.update_latch();
        st.ledger.snapshot(now)
    }

    /// The ring pace (milestone jstall): the engine's drain rate while the
    /// journal ring is nearly full. A new rate or its end re-judges the parked
    /// requests and wakes the timer thread for its new deadlines.
    fn set_ring_pace(&self, rate: Option<u64>) {
        let now = self.sh.clock.monotonic();
        let mut st = lock(&self.sh.st);
        if st.ledger.ring_pace == rate.map(|r| r.max(st.ledger.throttle.min_rate)) {
            return;
        }
        st.ledger.set_ring_pace(rate);
        st.ledger.grant_waiters(now);
        self.sh.cv.notify_all();
    }

    /// Re-judges every parked request under the new policy (ADR-32):
    /// `on_full = error` fails them now, a new `full_timeout` counts from
    /// each request's arrival (the overdue ones fail now and latch the
    /// volume), a larger limit grants what fits and opens the latch.
    fn reconfigure(&self, settings: &SpaceSettings) -> Result<()> {
        let policy = Policy::from_settings(settings, self.sh.default_timeout)?;
        let now = self.sh.clock.monotonic();
        let mut st = lock(&self.sh.st);
        st.ledger.policy = policy;
        st.ledger.rejudge(now);
        self.sh.cv.notify_all();
        Ok(())
    }
}
