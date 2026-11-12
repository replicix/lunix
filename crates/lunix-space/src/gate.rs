//! [`LunixSpace`]: the real [`SpaceGate`].

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::Duration;

use lunix_format::local::{OnFull, SpaceSettings};
use lunix_iface::clock::Clock;
use lunix_iface::space::{
    Ladder, LadderStep, LadderStepKind, ReserveOutcome, SpaceEvents, SpaceGate, SpaceKind,
    SpaceSnapshot, Urgency, Waiter,
};
use lunix_iface::wake::WakeFd;
use lunix_iface::{Error, Result};

use crate::ledger::{FsView, Ledger, Parked};
use crate::policy::Policy;
use crate::probe::FsProbe;

/// How often the background thread re-reads the file system.
pub const REFRESH_INTERVAL: Duration = Duration::from_secs(1);

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
    /// Whether the background thread also expires waiters and re-reads the
    /// file system by itself (as the real daemon wants). Tests that drive a
    /// manual clock turn this off and call `tick` and `refresh` themselves.
    /// The ladder always runs on the thread.
    pub background_timers: bool,
}

impl SpaceConfig {
    /// The given settings, 16 MiB segments, background timers on.
    pub fn new(settings: SpaceSettings) -> Self {
        Self {
            settings,
            segment_size: 16 << 20,
            background_timers: true,
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
    st: Mutex<State>,
    cv: Condvar,
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
            st.ledger.grant_waiters();
        }
        if err.is_some_and(|e| e.raw_os_error() == Some(5)) {
            self.fatal(&mut st);
        }
    }

    fn fatal(&self, st: &mut State) {
        st.ledger.read_only = true;
        st.ledger.expire_all();
    }

    fn kick_ladder(&self, st: &mut State, want: u64) {
        st.ladder_want = st.ladder_want.max(want.max(1));
        self.cv.notify_all();
    }

    fn run_ladder(&self, mut want: u64) {
        let steps: Vec<SharedStep> = lock(&self.steps).values().cloned().collect();
        self.ladder_runs.fetch_add(1, Ordering::Relaxed);
        for step in steps {
            let outcome = lock(&step).run(want, Urgency::Urgent);
            match outcome {
                Ok(freed) if freed.0 > 0 => {
                    let st = lock(&self.st);
                    want = st.ledger.head_shortfall();
                    if want == 0 {
                        return;
                    }
                }
                Ok(_) => {}
                Err(_) => {
                    self.step_errors.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    }

    fn thread_main(&self) {
        let mut st = lock(&self.st);
        loop {
            if st.shutdown {
                return;
            }
            if st.ladder_want > 0 {
                let want = std::mem::take(&mut st.ladder_want);
                drop(st);
                self.run_ladder(want);
                st = lock(&self.st);
                continue;
            }
            let mut sleep = Duration::from_secs(3600);
            if self.timers {
                let now = self.clock.monotonic();
                st.ledger.expire(now);
                let due = st.last_refresh + REFRESH_INTERVAL;
                if now >= due {
                    drop(st);
                    self.refresh();
                    st = lock(&self.st);
                    // Parked and still short: try the ladder again.
                    let short = st.ledger.head_shortfall();
                    if short > 0 && !lock(&self.steps).is_empty() {
                        self.kick_ladder(&mut st, short);
                    }
                    continue;
                }
                sleep = sleep.min(due - now);
                if let Some(d) = st.ledger.waiters.iter().filter_map(|p| p.deadline).min() {
                    sleep = sleep.min(d.saturating_sub(now));
                }
                sleep = sleep.max(Duration::from_millis(1));
            }
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
/// observers; the file system is read outside it. A background thread runs
/// the ladder steps (and, with `background_timers`, expires waiters and
/// refreshes the file-system reading). A registered step that holds an
/// `Arc` of the gate forms a cycle: call [`shutdown`](Self::shutdown) to
/// break it and stop the thread.
pub struct LunixSpace {
    sh: Arc<Shared>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl LunixSpace {
    /// A gate over `probe`, expiring waiters on `clock`. Reads the file system
    /// once before returning.
    ///
    /// # Errors
    /// `Invalid` for settings that fail `VolumeJson` validation; the probe's
    /// I/O error if the first reading fails.
    pub fn new(
        config: SpaceConfig,
        probe: Arc<dyn FsProbe>,
        clock: Arc<dyn Clock>,
    ) -> Result<Arc<Self>> {
        let policy = Policy::from_settings(&config.settings)?;
        let stat = probe.stat()?;
        let mut ledger = Ledger::new(policy, config.segment_size);
        ledger.fs = FsView { stat, used_mark: 0 };
        let last_refresh = clock.monotonic();
        let sh = Arc::new(Shared {
            clock,
            probe,
            timers: config.background_timers,
            st: Mutex::new(State {
                ledger,
                ladder_want: 0,
                last_refresh,
                shutdown: false,
            }),
            cv: Condvar::new(),
            steps: Mutex::new(BTreeMap::new()),
            step_errors: AtomicU64::new(0),
            ladder_runs: AtomicU64::new(0),
        });
        let worker = Arc::clone(&sh);
        let handle = std::thread::Builder::new()
            .name("lunix-space".into())
            .spawn(move || worker.thread_main())
            .map_err(Error::Io)?;
        Ok(Arc::new(Self {
            sh,
            thread: Mutex::new(Some(handle)),
        }))
    }

    /// Reads the file system now and re-evaluates the parked requests.
    pub fn refresh(&self) {
        self.sh.refresh();
    }

    /// Stops the background thread and drops the registered steps (which may
    /// hold the gate). Later calls still work on the ledger; no ladder runs.
    pub fn shutdown(&self) {
        {
            let mut st = lock(&self.sh.st);
            st.shutdown = true;
            self.sh.cv.notify_all();
        }
        let handle = lock(&self.thread).take();
        if let Some(h) = handle {
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
        st.ledger.prune_front();
        let queue_free = st.ledger.waiters.is_empty() || st.ledger.policy.on_full == OnFull::Error;
        if queue_free && st.ledger.fits(bytes) {
            st.ledger.charge_raw(kind, bytes);
            return Ok(ReserveOutcome::Reserved);
        }
        let want = st
            .ledger
            .used()
            .saturating_add(bytes)
            .saturating_sub(st.ledger.limit());
        if st.ledger.policy.on_full == OnFull::Error {
            self.sh.kick_ladder(&mut st, want);
            return Ok(ReserveOutcome::NoSpace);
        }
        let waiter = Waiter::new(wake.cloned());
        let deadline = st
            .ledger
            .policy
            .full_timeout
            .map(|t| self.sh.clock.monotonic() + t);
        st.ledger.waiters.push_back(Parked {
            waiter: waiter.clone(),
            kind,
            bytes,
            deadline,
        });
        self.sh.kick_ladder(&mut st, want);
        Ok(ReserveOutcome::Parked(waiter))
    }

    fn reserve_headroom(&self, kind: SpaceKind, bytes: u64) -> Result<()> {
        lock(&self.sh.st).ledger.reserve_headroom(kind, bytes)
    }

    fn release(&self, kind: SpaceKind, bytes: u64) {
        lock(&self.sh.st).ledger.release(kind, bytes);
    }

    fn transfer(
        &self,
        from_kind: SpaceKind,
        to_kind: SpaceKind,
        from_bytes: u64,
        to_bytes: u64,
    ) -> Result<()> {
        lock(&self.sh.st)
            .ledger
            .transfer(from_kind, to_kind, from_bytes, to_bytes)
    }

    fn tick(&self) -> usize {
        let now = self.sh.clock.monotonic();
        lock(&self.sh.st).ledger.expire(now)
    }

    fn snapshot(&self) -> SpaceSnapshot {
        lock(&self.sh.st).ledger.snapshot()
    }

    fn reconfigure(&self, settings: &SpaceSettings) -> Result<()> {
        let policy = Policy::from_settings(settings)?;
        let mut st = lock(&self.sh.st);
        st.ledger.policy = policy;
        st.ledger.grant_waiters();
        Ok(())
    }
}
