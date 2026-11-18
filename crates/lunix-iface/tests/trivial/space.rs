use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lunix_format::local::{OnFull, SpaceSettings};
use lunix_iface::clock::{Clock, ManualClock};
use lunix_iface::contract::space::SpaceSetup;
use lunix_iface::space::*;
use lunix_iface::wake::WakeFd;
use lunix_iface::{Error, Result};

use super::lock;

struct Parked {
    waiter: Waiter,
    kind: SpaceKind,
    bytes: u64,
    deadline: Option<Duration>,
}

struct State {
    held: [u64; 7],
    waiters: VecDeque<Parked>,
    read_only: bool,
}

pub struct TSpace {
    setup: SpaceSetup,
    clock: Arc<ManualClock>,
    st: Mutex<State>,
    steps: Mutex<HashMap<LadderStepKind, Box<dyn LadderStep>>>,
}

impl TSpace {
    pub fn new(setup: SpaceSetup, clock: Arc<ManualClock>) -> Arc<Self> {
        Arc::new(Self {
            setup,
            clock,
            st: Mutex::new(State {
                held: [0; 7],
                waiters: VecDeque::new(),
                read_only: false,
            }),
            steps: Mutex::new(HashMap::new()),
        })
    }

    fn used(st: &State) -> u64 {
        SpaceKind::ALL
            .iter()
            .filter(|k| **k != SpaceKind::Headroom)
            .map(|k| st.held[k.index()])
            .sum()
    }

    fn fits(&self, st: &State, bytes: u64) -> bool {
        Self::used(st) + bytes <= self.setup.capacity - self.setup.headroom
    }

    fn process(&self, st: &mut State) {
        while let Some(front) = st.waiters.front() {
            if front.waiter.state() != WaiterState::Pending {
                st.waiters.pop_front();
                continue;
            }
            if !self.fits(st, front.bytes) {
                break;
            }
            let p = st.waiters.pop_front().expect("front");
            st.held[p.kind.index()] += p.bytes;
            if !p.waiter.grant() {
                st.held[p.kind.index()] -= p.bytes;
            }
        }
    }
}

impl Ladder for TSpace {
    fn register(&self, kind: LadderStepKind, step: Box<dyn LadderStep>) {
        lock(&self.steps).insert(kind, step);
    }
}

impl SpaceEvents for TSpace {
    fn fatal_io(&self, _what: &str) {
        lock(&self.st).read_only = true;
    }
    fn is_read_only(&self) -> bool {
        lock(&self.st).read_only
    }
}

impl SpaceGate for TSpace {
    fn reserve(
        &self,
        kind: SpaceKind,
        bytes: u64,
        wake: Option<&WakeFd>,
    ) -> Result<ReserveOutcome> {
        self.reserve_since(kind, bytes, wake, self.clock.monotonic())
    }

    fn reserve_since(
        &self,
        kind: SpaceKind,
        bytes: u64,
        wake: Option<&WakeFd>,
        arrived: std::time::Duration,
    ) -> Result<ReserveOutcome> {
        let mut st = lock(&self.st);
        if st.read_only {
            return Err(Error::ReadOnly);
        }
        if st.waiters.is_empty() && self.fits(&st, bytes) {
            st.held[kind.index()] += bytes;
            return Ok(ReserveOutcome::Reserved);
        }
        if self.setup.on_full == OnFull::Error {
            return Ok(ReserveOutcome::NoSpace);
        }
        let want =
            (Self::used(&st) + bytes).saturating_sub(self.setup.capacity - self.setup.headroom);
        let waiter = Waiter::new(wake.cloned());
        let now = self.clock.monotonic();
        let deadline = self.setup.full_timeout.map(|t| arrived.min(now) + t);
        if deadline.is_some_and(|d| d <= now) {
            return Ok(ReserveOutcome::NoSpace);
        }
        st.waiters.push_back(Parked {
            waiter: waiter.clone(),
            kind,
            bytes,
            deadline,
        });
        drop(st);
        // Run the ladder with the state unlocked: steps call `release`.
        for step in lock(&self.steps).values_mut() {
            if step.run(want, Urgency::Urgent)?.0 > 0 {
                break;
            }
        }
        Ok(ReserveOutcome::Parked(waiter))
    }

    fn reserve_headroom(&self, kind: SpaceKind, bytes: u64) -> Result<()> {
        let mut st = lock(&self.st);
        if Self::used(&st) + st.held[SpaceKind::Headroom.index()] + bytes > self.setup.capacity {
            return Err(Error::NoSpace);
        }
        st.held[kind.index()] += bytes;
        Ok(())
    }

    fn release(&self, kind: SpaceKind, bytes: u64) {
        let mut st = lock(&self.st);
        debug_assert!(st.held[kind.index()] >= bytes);
        st.held[kind.index()] = st.held[kind.index()].saturating_sub(bytes);
        self.process(&mut st);
    }

    fn transfer(
        &self,
        from: SpaceKind,
        to: SpaceKind,
        from_bytes: u64,
        to_bytes: u64,
    ) -> Result<()> {
        let mut st = lock(&self.st);
        if st.held[from.index()] < from_bytes {
            return Err(Error::invalid("transfer exceeds what the kind holds"));
        }
        st.held[from.index()] -= from_bytes;
        st.held[to.index()] += to_bytes;
        self.process(&mut st);
        Ok(())
    }

    fn tick(&self) -> usize {
        let now = self.clock.monotonic();
        let mut st = lock(&self.st);
        let mut n = 0;
        for p in &st.waiters {
            if p.deadline.is_some_and(|d| d <= now) && p.waiter.expire() {
                n += 1;
            }
        }
        st.waiters
            .retain(|p| p.waiter.state() == WaiterState::Pending);
        n
    }

    fn snapshot(&self) -> SpaceSnapshot {
        let st = lock(&self.st);
        let used = Self::used(&st);
        SpaceSnapshot {
            by_kind: st.held,
            limit: self.setup.capacity - self.setup.headroom,
            fs_free: self.setup.capacity.saturating_sub(used),
            headroom_total: self.setup.headroom,
            waiters: st
                .waiters
                .iter()
                .filter(|p| p.waiter.state() == WaiterState::Pending)
                .count() as u64,
            read_only: st.read_only,
            full: false,
        }
    }

    fn reconfigure(&self, _settings: &SpaceSettings) -> Result<()> {
        Ok(())
    }
}
