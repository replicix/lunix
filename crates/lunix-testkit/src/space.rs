//! [`MemSpace`]: the space ledger and ladder in memory, on an injected clock.
//!
//! It follows the [`SpaceGate`] contract and the accounting walk-through in
//! `lunix_iface::space`: FIFO waiters charged at grant time, a headroom region
//! writes cannot take, `transfer` atomic, timeouts on the gate's clock driven
//! by `tick`. The ladder runs on a short-lived helper thread per parked
//! request (the real gate has one background thread), so `reserve` never runs
//! a step on the caller's thread.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::Duration;

use lunix_format::local::{OnFull, SpaceSettings};
use lunix_iface::clock::Clock;
use lunix_iface::space::{
    Ladder, LadderStep, LadderStepKind, ReserveOutcome, SpaceEvents, SpaceGate, SpaceKind,
    SpaceSnapshot, Urgency, Waiter, WaiterState,
};
use lunix_iface::wake::WakeFd;
use lunix_iface::{Error, Result};

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// What a [`MemSpace`] is built from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct MemSpaceConfig {
    /// Bytes the ledger may hold including the headroom region.
    pub capacity: u64,
    /// The headroom region: writes can never take it.
    pub headroom: u64,
    /// What a request that does not fit does.
    pub on_full: OnFull,
    /// How long a parked request waits; `None` waits forever.
    pub full_timeout: Option<Duration>,
    /// The full latch (ADR-32), off when `None`: the first expiry fails
    /// every waiter and refuses writes at once until the headroom reaches
    /// this many bytes (at least 1).
    pub latch: Option<u64>,
}

impl MemSpaceConfig {
    /// `capacity` bytes, no headroom, `on_full = error`, no timeout.
    pub fn new(capacity: u64) -> Self {
        Self {
            capacity,
            headroom: 0,
            on_full: OnFull::Error,
            full_timeout: None,
            latch: None,
        }
    }

    /// With the full latch and its resume mark.
    #[must_use]
    pub fn latch(mut self, resume: u64) -> Self {
        self.latch = Some(resume);
        self
    }

    /// With a headroom region.
    #[must_use]
    pub fn headroom(mut self, headroom: u64) -> Self {
        self.headroom = headroom;
        self
    }

    /// With `on_full = wait`.
    #[must_use]
    pub fn wait(mut self, timeout: Option<Duration>) -> Self {
        self.on_full = OnFull::Wait;
        self.full_timeout = timeout;
        self
    }
}

struct Parked {
    waiter: Waiter,
    kind: SpaceKind,
    bytes: u64,
    deadline: Option<Duration>,
}

struct State {
    cfg: MemSpaceConfig,
    held: [u64; 7],
    waiters: VecDeque<Parked>,
    read_only: bool,
    full: bool,
}

impl State {
    fn used(&self) -> u64 {
        SpaceKind::ALL
            .iter()
            .filter(|k| **k != SpaceKind::Headroom)
            .map(|k| self.held[k.index()])
            .sum()
    }

    fn limit(&self) -> u64 {
        self.cfg.capacity.saturating_sub(self.cfg.headroom)
    }

    fn fits(&self, bytes: u64) -> bool {
        self.used().saturating_add(bytes) <= self.limit()
    }

    /// Opens the latch once the headroom reached the resume mark.
    fn update_latch(&mut self) {
        if let Some(resume) = self.cfg.latch {
            if self.full && self.limit().saturating_sub(self.used()) >= resume.max(1) {
                self.full = false;
            }
        }
    }

    /// Grants parked requests that now fit, strictly first in first out.
    fn grant_waiters(&mut self) {
        self.update_latch();
        while let Some(front) = self.waiters.front() {
            if front.waiter.state() != WaiterState::Pending {
                self.waiters.pop_front();
                continue;
            }
            if !self.fits(front.bytes) {
                break;
            }
            let Some(p) = self.waiters.pop_front() else {
                break;
            };
            self.held[p.kind.index()] += p.bytes;
            if !p.waiter.grant() {
                // Cancelled or expired between the check and the grant.
                self.held[p.kind.index()] -= p.bytes;
            }
        }
    }
}

/// The in-memory space gate.
pub struct MemSpace {
    clock: Arc<dyn Clock>,
    me: Weak<MemSpace>,
    st: Mutex<State>,
    steps: Mutex<BTreeMap<LadderStepKind, Box<dyn LadderStep>>>,
}

impl MemSpace {
    /// A gate with the given configuration, timing out on `clock`.
    pub fn new(cfg: MemSpaceConfig, clock: Arc<dyn Clock>) -> Arc<Self> {
        Arc::new_cyclic(|me| Self {
            clock,
            me: me.clone(),
            st: Mutex::new(State {
                cfg,
                held: [0; 7],
                waiters: VecDeque::new(),
                read_only: false,
                full: false,
            }),
            steps: Mutex::new(BTreeMap::new()),
        })
    }

    /// Runs the registered steps, in ladder order, until one frees something.
    fn run_ladder(&self, want: u64) {
        let mut steps = lock(&self.steps);
        for step in steps.values_mut() {
            // A step that fails is skipped, as a real gate logs and moves on.
            if step
                .run(want, Urgency::Urgent)
                .is_ok_and(|freed| freed.0 > 0)
            {
                break;
            }
        }
    }
}

impl Ladder for MemSpace {
    fn register(&self, kind: LadderStepKind, step: Box<dyn LadderStep>) {
        lock(&self.steps).insert(kind, step);
    }
}

impl SpaceEvents for MemSpace {
    fn fatal_io(&self, _what: &str) {
        lock(&self.st).read_only = true;
    }

    fn is_read_only(&self) -> bool {
        lock(&self.st).read_only
    }
}

impl SpaceGate for MemSpace {
    fn reserve(
        &self,
        kind: SpaceKind,
        bytes: u64,
        wake: Option<&WakeFd>,
    ) -> Result<ReserveOutcome> {
        self.reserve_since(kind, bytes, wake, self.clock.monotonic())
    }

    /// The wait of a parked request counts from `arrived` (ADR-32).
    fn reserve_since(
        &self,
        kind: SpaceKind,
        bytes: u64,
        wake: Option<&WakeFd>,
        arrived: Duration,
    ) -> Result<ReserveOutcome> {
        let mut st = lock(&self.st);
        if st.read_only {
            return Err(Error::ReadOnly);
        }
        st.update_latch();
        if st.full {
            return Ok(ReserveOutcome::NoSpace);
        }
        if st.waiters.is_empty() && st.fits(bytes) {
            st.held[kind.index()] += bytes;
            return Ok(ReserveOutcome::Reserved);
        }
        if st.cfg.on_full == OnFull::Error {
            return Ok(ReserveOutcome::NoSpace);
        }
        let want = st.used().saturating_add(bytes).saturating_sub(st.limit());
        let waiter = Waiter::new(wake.cloned());
        let now = self.clock.monotonic();
        let deadline = st.cfg.full_timeout.map(|t| arrived.min(now) + t);
        if deadline.is_some_and(|d| d <= now) {
            // Past its full_timeout already: refused, not parked.
            return Ok(ReserveOutcome::NoSpace);
        }
        st.waiters.push_back(Parked {
            waiter: waiter.clone(),
            kind,
            bytes,
            deadline,
        });
        drop(st);
        if let Some(gate) = self.me.upgrade() {
            // Steps call `release`, so they run with no lock held, off this thread.
            std::thread::spawn(move || gate.run_ladder(want));
        }
        Ok(ReserveOutcome::Parked(waiter))
    }

    fn reserve_headroom(&self, kind: SpaceKind, bytes: u64) -> Result<()> {
        let mut st = lock(&self.st);
        // Headroom draws on the whole capacity: the ledger plus the bounds
        // already held must stay within it.
        let committed = st
            .used()
            .saturating_add(st.held[SpaceKind::Headroom.index()]);
        if committed.saturating_add(bytes) > st.cfg.capacity {
            return Err(Error::NoSpace);
        }
        st.held[kind.index()] += bytes;
        Ok(())
    }

    fn release(&self, kind: SpaceKind, bytes: u64) {
        let mut st = lock(&self.st);
        debug_assert!(
            st.held[kind.index()] >= bytes,
            "released more than {kind:?} holds"
        );
        st.held[kind.index()] = st.held[kind.index()].saturating_sub(bytes);
        st.grant_waiters();
    }

    fn transfer(
        &self,
        from_kind: SpaceKind,
        to_kind: SpaceKind,
        from_bytes: u64,
        to_bytes: u64,
    ) -> Result<()> {
        let mut st = lock(&self.st);
        if st.held[from_kind.index()] < from_bytes {
            return Err(Error::invalid("transfer exceeds what the kind holds"));
        }
        st.held[from_kind.index()] -= from_bytes;
        st.held[to_kind.index()] += to_bytes;
        st.grant_waiters();
        Ok(())
    }

    fn tick(&self) -> usize {
        let now = self.clock.monotonic();
        let mut st = lock(&self.st);
        let expired = st
            .waiters
            .iter()
            .filter(|p| p.deadline.is_some_and(|d| d <= now) && p.waiter.expire())
            .count();
        let mut failed = expired;
        if expired > 0 && st.cfg.latch.is_some() {
            failed += st.waiters.iter().filter(|p| p.waiter.expire()).count();
            st.full = true;
        }
        st.waiters
            .retain(|p| p.waiter.state() == WaiterState::Pending);
        // An expired head may have been blocking smaller requests behind it.
        st.grant_waiters();
        failed
    }

    fn snapshot(&self) -> SpaceSnapshot {
        let mut st = lock(&self.st);
        st.update_latch();
        SpaceSnapshot {
            by_kind: st.held,
            limit: st.limit(),
            fs_free: st.cfg.capacity.saturating_sub(st.used()),
            headroom_total: st.cfg.headroom,
            waiters: st
                .waiters
                .iter()
                .filter(|p| p.waiter.state() == WaiterState::Pending)
                .count() as u64,
            read_only: st.read_only,
            full: st.full,
        }
    }

    fn reconfigure(&self, settings: &SpaceSettings) -> Result<()> {
        if settings.full_timeout_ms == Some(0) {
            return Err(Error::invalid("full_timeout_ms must be at least 1"));
        }
        // As the real gate: a percentage cap reaches a gate resolved.
        if settings.cache_max_percent.is_some() {
            return Err(Error::invalid(
                "space.cache_max_percent must be resolved to cache_max against the volume size",
            ));
        }
        let mut st = lock(&self.st);
        if settings.headroom > st.cfg.capacity {
            return Err(Error::invalid("headroom exceeds the capacity"));
        }
        st.cfg.on_full = settings.on_full;
        st.cfg.full_timeout = settings.full_timeout_ms.map(Duration::from_millis);
        st.cfg.headroom = settings.headroom;
        st.grant_waiters();
        Ok(())
    }
}
