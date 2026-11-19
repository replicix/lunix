//! Space gate contract tests. See the [`crate::space`] docs.

use std::sync::Arc;
use std::time::Duration;

use lunix_format::local::OnFull;

use super::wait_until;
use crate::clock::{Clock, ManualClock};
use crate::error::ErrorKind;
use crate::space::{
    Freed, LadderStep, LadderStepKind, ReserveOutcome, SpaceGate, SpaceKind, Urgency, WaiterState,
};

/// How to build the gate under test.
#[derive(Clone, Copy, Debug)]
pub struct SpaceSetup {
    /// Bytes the ledger may hold including the headroom region.
    pub capacity: u64,
    /// The headroom region: writes can never take it.
    pub headroom: u64,
    /// `error` or `wait`.
    pub on_full: OnFull,
    /// `full_timeout`; `None` waits forever.
    pub full_timeout: Option<Duration>,
}

impl SpaceSetup {
    fn new(capacity: u64, headroom: u64, on_full: OnFull) -> Self {
        Self {
            capacity,
            headroom,
            on_full,
            full_timeout: None,
        }
    }
}

const K: SpaceKind = SpaceKind::Segments;

fn used(g: &dyn SpaceGate) -> u64 {
    g.snapshot().used()
}

fn held(g: &dyn SpaceGate, kind: SpaceKind) -> u64 {
    g.snapshot().by_kind[kind.index()]
}

fn reserved(o: ReserveOutcome) {
    assert!(
        matches!(o, ReserveOutcome::Reserved),
        "expected Reserved, got {o:?}"
    );
}

/// Reserve and release are exact, per kind.
pub fn reserve_and_release_are_exact<
    G: SpaceGate,
    M: Fn(SpaceSetup, Arc<ManualClock>) -> Arc<G>,
>(
    make: &M,
) {
    let g = make(
        SpaceSetup::new(1000, 100, OnFull::Error),
        Arc::new(ManualClock::new(lunix_format::TimeMs(0))),
    );
    reserved(
        g.reserve(SpaceKind::JournalReserved, 10, None)
            .expect("reserve"),
    );
    reserved(g.reserve(K, 20, None).expect("reserve"));
    assert_eq!(used(&*g), 30);
    assert_eq!(held(&*g, K), 20);
    g.release(SpaceKind::JournalReserved, 10);
    g.release(K, 20);
    assert_eq!(used(&*g), 0);
}

/// A transfer moves bytes between kinds (or true-ups within one) exactly.
pub fn transfer_moves_between_kinds<G: SpaceGate, M: Fn(SpaceSetup, Arc<ManualClock>) -> Arc<G>>(
    make: &M,
) {
    let g = make(
        SpaceSetup::new(1000, 200, OnFull::Error),
        Arc::new(ManualClock::new(lunix_format::TimeMs(0))),
    );
    g.reserve_headroom(K, 100).expect("headroom");
    g.reserve_headroom(SpaceKind::Headroom, 50)
        .expect("headroom");
    g.transfer(K, K, 100, 60).expect("true-up");
    assert_eq!(held(&*g, K), 60);
    g.transfer(SpaceKind::Headroom, SpaceKind::Map, 50, 30)
        .expect("to map");
    assert_eq!(held(&*g, SpaceKind::Headroom), 0);
    assert_eq!(held(&*g, SpaceKind::Map), 30);
    assert_eq!(used(&*g), 90);
    let err = g
        .transfer(SpaceKind::Index, K, 5, 5)
        .expect_err("nothing held");
    assert_eq!(err.kind(), ErrorKind::Invalid);
    assert_eq!(used(&*g), 90, "a refused transfer changes nothing");
}

/// With `on_full = error` a reservation that does not fit is refused and the
/// ledger is untouched.
pub fn error_mode_refuses_without_state_change<
    G: SpaceGate,
    M: Fn(SpaceSetup, Arc<ManualClock>) -> Arc<G>,
>(
    make: &M,
) {
    let g = make(
        SpaceSetup::new(100, 0, OnFull::Error),
        Arc::new(ManualClock::new(lunix_format::TimeMs(0))),
    );
    reserved(g.reserve(K, 100, None).expect("reserve"));
    assert!(matches!(
        g.reserve(K, 1, None).expect("reserve"),
        ReserveOutcome::NoSpace
    ));
    assert_eq!(used(&*g), 100);
}

/// With `on_full = wait` requests park behind a handle, are granted strictly
/// first in, first out as space returns, and are charged when granted.
pub fn wait_mode_parks_and_grants_fifo<
    G: SpaceGate,
    M: Fn(SpaceSetup, Arc<ManualClock>) -> Arc<G>,
>(
    make: &M,
) {
    let g = make(
        SpaceSetup::new(100, 0, OnFull::Wait),
        Arc::new(ManualClock::new(lunix_format::TimeMs(0))),
    );
    reserved(g.reserve(K, 100, None).expect("reserve"));
    let ReserveOutcome::Parked(a) = g.reserve(K, 60, None).expect("reserve") else {
        panic!("expected a parked request");
    };
    let ReserveOutcome::Parked(b) = g.reserve(K, 10, None).expect("reserve") else {
        panic!("expected a parked request");
    };
    assert_eq!(g.snapshot().waiters, 2);
    g.release(K, 10);
    assert_eq!(a.state(), WaiterState::Pending);
    assert_eq!(
        b.state(),
        WaiterState::Pending,
        "the later request does not overtake"
    );
    g.release(K, 60);
    assert_eq!(a.state(), WaiterState::Granted);
    assert_eq!(b.state(), WaiterState::Granted);
    assert_eq!(used(&*g), 100 - 70 + 60 + 10);
    assert_eq!(g.snapshot().waiters, 0);
}

/// A parked request expires on the gate's clock and charges nothing.
pub fn timeout_expires_waiters_on_the_clock<
    G: SpaceGate,
    M: Fn(SpaceSetup, Arc<ManualClock>) -> Arc<G>,
>(
    make: &M,
) {
    let clock = Arc::new(ManualClock::new(lunix_format::TimeMs(0)));
    let mut setup = SpaceSetup::new(100, 0, OnFull::Wait);
    setup.full_timeout = Some(Duration::from_secs(1));
    let g = make(setup, clock.clone());
    reserved(g.reserve(K, 100, None).expect("reserve"));
    let ReserveOutcome::Parked(w) = g.reserve(K, 10, None).expect("reserve") else {
        panic!("expected a parked request");
    };
    clock.advance(Duration::from_millis(900));
    assert_eq!(g.tick(), 0);
    assert_eq!(w.state(), WaiterState::Pending);
    clock.advance(Duration::from_millis(150));
    assert_eq!(g.tick(), 1);
    assert_eq!(w.state(), WaiterState::TimedOut);
    assert_eq!(used(&*g), 100);
}

/// `reserve_since`: a parked request's `full_timeout` counts from its
/// arrival, not from the call (ADR-32).
pub fn a_wait_counts_from_the_arrival<
    G: SpaceGate,
    M: Fn(SpaceSetup, Arc<ManualClock>) -> Arc<G>,
>(
    make: &M,
) {
    let clock = Arc::new(ManualClock::new(lunix_format::TimeMs(0)));
    let mut setup = SpaceSetup::new(100, 0, OnFull::Wait);
    setup.full_timeout = Some(Duration::from_secs(1));
    let g = make(setup, clock.clone());
    reserved(g.reserve(K, 100, None).expect("reserve"));
    let arrived = clock.monotonic();
    clock.advance(Duration::from_millis(600));
    let ReserveOutcome::Parked(w) = g.reserve_since(K, 10, None, arrived).expect("reserve") else {
        panic!("expected a parked request");
    };
    clock.advance(Duration::from_millis(450));
    assert_eq!(g.tick(), 1, "due at its arrival plus full_timeout");
    assert_eq!(w.state(), WaiterState::TimedOut);
    // Already past it, on a fresh (unlatched) gate: refused without
    // parking, and without latching the volume (it never waited in the gate).
    let clock = Arc::new(ManualClock::new(lunix_format::TimeMs(0)));
    let mut setup = SpaceSetup::new(100, 0, OnFull::Wait);
    setup.full_timeout = Some(Duration::from_secs(1));
    let g = make(setup, clock.clone());
    reserved(g.reserve(K, 100, None).expect("reserve"));
    let arrived = clock.monotonic();
    clock.advance(Duration::from_secs(2));
    assert!(!g.snapshot().full);
    assert!(matches!(
        g.reserve_since(K, 10, None, arrived).expect("reserve"),
        ReserveOutcome::NoSpace
    ));
    let snap = g.snapshot();
    assert_eq!(snap.waiters, 0);
    assert!(!snap.full, "a late refusal does not latch");
}

/// Writes cannot take the headroom; commit-time reservations can.
pub fn headroom_is_not_for_writes<G: SpaceGate, M: Fn(SpaceSetup, Arc<ManualClock>) -> Arc<G>>(
    make: &M,
) {
    let g = make(
        SpaceSetup::new(100, 40, OnFull::Error),
        Arc::new(ManualClock::new(lunix_format::TimeMs(0))),
    );
    reserved(
        g.reserve(SpaceKind::JournalReserved, 60, None)
            .expect("reserve"),
    );
    assert!(matches!(
        g.reserve(SpaceKind::JournalReserved, 1, None)
            .expect("reserve"),
        ReserveOutcome::NoSpace
    ));
    g.reserve_headroom(SpaceKind::Commits, 30)
        .expect("headroom is for commits");
    assert_eq!(held(&*g, SpaceKind::Commits), 30);
}

/// After `fatal_io` the volume is read-only and reservations fail.
pub fn fatal_io_makes_the_volume_read_only<
    G: SpaceGate,
    M: Fn(SpaceSetup, Arc<ManualClock>) -> Arc<G>,
>(
    make: &M,
) {
    let g = make(
        SpaceSetup::new(100, 0, OnFull::Error),
        Arc::new(ManualClock::new(lunix_format::TimeMs(0))),
    );
    assert!(!g.is_read_only());
    g.fatal_io("test");
    assert!(g.is_read_only());
    assert!(g.snapshot().read_only);
    assert_eq!(
        g.reserve(K, 1, None).expect_err("read-only").kind(),
        ErrorKind::ReadOnly
    );
}

struct Freeing<G: SpaceGate>(Arc<G>);

impl<G: SpaceGate> LadderStep for Freeing<G> {
    fn run(&mut self, want_bytes: u64, _urgency: Urgency) -> crate::Result<Freed> {
        self.0.release(K, want_bytes);
        Ok(Freed(want_bytes))
    }
}

/// A registered compact step frees space and a parked write is granted.
pub fn ladder_frees_space_for_a_parked_write<
    G: SpaceGate + 'static,
    M: Fn(SpaceSetup, Arc<ManualClock>) -> Arc<G>,
>(
    make: &M,
) {
    let g = make(
        SpaceSetup::new(100, 0, OnFull::Wait),
        Arc::new(ManualClock::new(lunix_format::TimeMs(0))),
    );
    reserved(g.reserve(K, 100, None).expect("reserve"));
    g.register(LadderStepKind::Compact, Box::new(Freeing(g.clone())));
    let ReserveOutcome::Parked(w) = g.reserve(SpaceKind::Map, 30, None).expect("reserve") else {
        panic!("expected a parked request");
    };
    wait_until("the ladder to free space", Duration::from_secs(5), || {
        w.state() == WaiterState::Granted
    });
    assert_eq!(held(&*g, SpaceKind::Map), 30);
}
