//! `MemSpace` beyond the contract suite.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use lunix_format::local::{OnFull, SpaceSettings};
use lunix_format::TimeMs;
use lunix_iface::clock::ManualClock;
use lunix_iface::contract::wait_until;
use lunix_iface::space::{
    Freed, Ladder, LadderStep, LadderStepKind, ReserveOutcome, SpaceEvents, SpaceGate, SpaceKind,
    Urgency, WaiterState,
};
use lunix_iface::wake::WakeFd;
use lunix_iface::ErrorKind;
use lunix_testkit::{MemSpace, MemSpaceConfig};

fn clock() -> Arc<ManualClock> {
    Arc::new(ManualClock::new(TimeMs(0)))
}

const K: SpaceKind = SpaceKind::Segments;

#[test]
fn a_parked_request_signals_its_wake_fd_when_granted() {
    let g = MemSpace::new(MemSpaceConfig::new(100).wait(None), clock());
    let wake = WakeFd::new().expect("eventfd");
    assert!(matches!(
        g.reserve(K, 100, None).expect("reserve"),
        ReserveOutcome::Reserved
    ));
    let ReserveOutcome::Parked(w) = g.reserve(K, 10, Some(&wake)).expect("reserve") else {
        panic!("expected parked");
    };
    assert_eq!(wake.drain(), 0);
    g.release(K, 20);
    assert_eq!(w.state(), WaiterState::Granted);
    assert_eq!(wake.drain(), 1);
    assert_eq!(g.snapshot().by_kind[K.index()], 90);
}

#[test]
fn a_cancelled_waiter_is_skipped_and_charged_nothing() {
    let g = MemSpace::new(MemSpaceConfig::new(100).wait(None), clock());
    g.reserve(K, 100, None).expect("reserve");
    let ReserveOutcome::Parked(a) = g.reserve(K, 50, None).expect("reserve") else {
        panic!("parked");
    };
    let ReserveOutcome::Parked(b) = g.reserve(K, 10, None).expect("reserve") else {
        panic!("parked");
    };
    assert!(a.cancel());
    g.release(K, 10);
    assert_eq!(b.state(), WaiterState::Granted);
    assert_eq!(g.snapshot().by_kind[K.index()], 100);
    assert_eq!(g.snapshot().waiters, 0);
}

#[test]
fn an_expired_head_unblocks_the_requests_behind_it() {
    let clk = clock();
    let g = MemSpace::new(
        MemSpaceConfig::new(100).wait(Some(Duration::from_secs(1))),
        clk.clone(),
    );
    g.reserve(K, 90, None).expect("reserve");
    let ReserveOutcome::Parked(big) = g.reserve(K, 50, None).expect("reserve") else {
        panic!("parked");
    };
    clk.advance(Duration::from_millis(500));
    let ReserveOutcome::Parked(small) = g.reserve(K, 5, None).expect("reserve") else {
        panic!("parked behind the head");
    };
    clk.advance(Duration::from_millis(600));
    assert_eq!(g.tick(), 1, "only the head is past its deadline");
    assert_eq!(big.state(), WaiterState::TimedOut);
    assert_eq!(small.state(), WaiterState::Granted);
}

#[test]
fn reconfigure_applies_and_validates() {
    let g = MemSpace::new(MemSpaceConfig::new(100), clock());
    g.reserve(K, 100, None).expect("reserve");
    assert!(matches!(
        g.reserve(K, 1, None).expect("reserve"),
        ReserveOutcome::NoSpace
    ));
    let settings = SpaceSettings {
        on_full: OnFull::Wait,
        full_timeout_ms: Some(0),
        headroom: 0,
        ..SpaceSettings::default()
    };
    assert_eq!(
        g.reconfigure(&settings).expect_err("zero timeout").kind(),
        ErrorKind::Invalid
    );
    let settings = SpaceSettings {
        full_timeout_ms: None,
        ..settings
    };
    g.reconfigure(&settings).expect("reconfigure");
    assert!(matches!(
        g.reserve(K, 1, None).expect("reserve"),
        ReserveOutcome::Parked(_)
    ));
    let too_big = SpaceSettings {
        headroom: 101,
        ..settings
    };
    assert!(g.reconfigure(&too_big).is_err());
}

#[test]
fn ladder_steps_run_in_order_and_off_the_callers_thread() {
    struct Step(
        &'static str,
        u64,
        Arc<Mutex<Vec<(&'static str, std::thread::ThreadId)>>>,
    );
    impl LadderStep for Step {
        fn run(&mut self, _want: u64, _u: Urgency) -> lunix_iface::Result<Freed> {
            self.2
                .lock()
                .expect("log")
                .push((self.0, std::thread::current().id()));
            Ok(Freed(self.1))
        }
    }
    let g = MemSpace::new(MemSpaceConfig::new(10).wait(None), clock());
    let log = Arc::new(Mutex::new(Vec::new()));
    // Registered out of order on purpose.
    g.register(
        LadderStepKind::Compact,
        Box::new(Step("compact", 1, log.clone())),
    );
    g.register(
        LadderStepKind::EvictCache,
        Box::new(Step("evict", 0, log.clone())),
    );
    g.reserve(K, 10, None).expect("reserve");
    let _parked = g.reserve(K, 1, None).expect("reserve");
    wait_until("both steps", Duration::from_secs(5), || {
        log.lock().expect("log").len() == 2
    });
    let log = log.lock().expect("log");
    assert_eq!(
        log.iter().map(|(n, _)| *n).collect::<Vec<_>>(),
        ["evict", "compact"],
        "the first step freed nothing, so the next one ran"
    );
    assert!(log.iter().all(|(_, t)| *t != std::thread::current().id()));
}

#[test]
fn headroom_is_bounded_by_the_capacity() {
    let g = MemSpace::new(MemSpaceConfig::new(100).headroom(40), clock());
    g.reserve_headroom(SpaceKind::Headroom, 40).expect("fits");
    let e = g
        .reserve_headroom(SpaceKind::Headroom, 61)
        .expect_err("exhausted");
    assert_eq!(e.kind(), ErrorKind::NoSpace);
    assert_eq!(g.snapshot().used(), 0, "headroom is not counted as used");
}

#[test]
fn a_read_only_gate_refuses_reservations_but_still_releases() {
    let g = MemSpace::new(MemSpaceConfig::new(100), clock());
    g.fatal_io("write");
    assert!(g.is_read_only());
    assert_eq!(
        g.reserve(K, 1, None).expect_err("ro").kind(),
        ErrorKind::ReadOnly
    );
    g.release(K, 0);
}
