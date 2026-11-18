//! The bounded stop (ADR-32 T1; the note
//! `docs/explanation/research/2026-11-18-write-backpressure.md` §2, §5.5 and
//! §9.1): every parked request has a deadline (P1), the volume latches full
//! on the first expiry and opens at the resume mark, `reconfigure` re-judges
//! the parked requests (P2), the quiesce rule fails them (P3), and a ladder
//! step that blocks delays no deadline (P5). Manual clock unless the test
//! needs the timer thread.

mod common;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use lunix_format::local::OnFull;
use lunix_iface::clock::SystemClock;
use lunix_iface::contract::wait_until;
use lunix_iface::space::{
    Freed, Ladder, LadderStep, LadderStepKind, ReserveOutcome, SpaceGate, SpaceKind, Urgency,
    Waiter, WaiterState,
};
use lunix_space::{LunixSpace, DEFAULT_FULL_TIMEOUT};

use common::{clock, gate, gate_cfg, settings};

const K: SpaceKind = SpaceKind::JournalReserved;

fn reserved(o: ReserveOutcome) {
    assert!(matches!(o, ReserveOutcome::Reserved), "{o:?}");
}

fn refused(o: ReserveOutcome) {
    assert!(matches!(o, ReserveOutcome::NoSpace), "{o:?}");
}

fn parked(o: ReserveOutcome) -> Waiter {
    match o {
        ReserveOutcome::Parked(w) => w,
        other => panic!("expected Parked, got {other:?}"),
    }
}

#[test]
fn a_null_full_timeout_is_the_daemon_default_of_60_s() {
    // P1: `full_timeout_ms = null` no longer waits for ever (FREEZE F1).
    assert_eq!(DEFAULT_FULL_TIMEOUT, Duration::from_secs(60));
    let ck = clock();
    let (g, _fs) = gate(100, settings(0, OnFull::Wait, None), ck.clone());
    assert_eq!(g.full_timeout(), DEFAULT_FULL_TIMEOUT);
    reserved(g.reserve(K, 100, None).unwrap());
    let w = parked(g.reserve(K, 1, None).unwrap());
    ck.advance(Duration::from_millis(59_999));
    assert_eq!(g.tick(), 0);
    assert_eq!(w.state(), WaiterState::Pending);
    ck.advance(Duration::from_millis(1));
    assert_eq!(g.tick(), 1);
    assert_eq!(w.state(), WaiterState::TimedOut);
}

#[test]
fn the_daemon_default_can_be_set_and_volume_json_wins_over_it() {
    let ck = clock();
    let mut cfg = common::config(settings(0, OnFull::Wait, None));
    cfg.full_timeout = Duration::from_secs(3);
    let (g, _fs) = gate_cfg(100, 0, cfg.clone(), ck.clone());
    assert_eq!(g.full_timeout(), Duration::from_secs(3));
    reserved(g.reserve(K, 100, None).unwrap());
    let w = parked(g.reserve(K, 1, None).unwrap());
    ck.advance(Duration::from_secs(3));
    assert_eq!(g.tick(), 1);
    assert_eq!(w.state(), WaiterState::TimedOut);
    // An explicit `full_timeout_ms` is the volume's own.
    g.reconfigure(&settings(0, OnFull::Wait, Some(Duration::from_secs(7))))
        .unwrap();
    assert_eq!(g.full_timeout(), Duration::from_secs(7));
    // A null one goes back to the daemon's.
    g.reconfigure(&settings(0, OnFull::Wait, None)).unwrap();
    assert_eq!(g.full_timeout(), Duration::from_secs(3));
    // A zero default is refused.
    cfg.full_timeout = Duration::ZERO;
    let fs = Arc::new(lunix_space::SimFs::new(100, 0));
    assert!(LunixSpace::new(cfg, fs, ck).is_err());
}

#[test]
fn the_latch_refuses_at_once_and_opens_at_the_resume_mark_not_below() {
    let ck = clock();
    let (g, _fs) = gate(
        1000,
        settings(0, OnFull::Wait, Some(Duration::from_secs(1))),
        ck.clone(),
    );
    // No segment reserve here: the mark is 1 % of the limit.
    assert_eq!(g.resume_mark(), 10);
    reserved(g.reserve(K, 1000, None).unwrap());
    let w = parked(g.reserve(K, 50, None).unwrap());
    assert!(!g.is_full(), "waiting is the stop state, not the latch");
    ck.advance(Duration::from_secs(1));
    assert_eq!(g.tick(), 1);
    assert_eq!(w.state(), WaiterState::TimedOut);
    assert!(g.is_full());
    assert_eq!(g.fulls(), 1);
    // Headroom 5, below the mark: a write that fits is refused at once and
    // nothing parks.
    g.release(K, 5);
    refused(g.reserve(K, 1, None).unwrap());
    assert_eq!(g.snapshot().waiters, 0);
    assert!(g.is_full());
    // At the mark the latch opens.
    g.release(K, 5);
    assert!(!g.is_full());
    reserved(g.reserve(K, 1, None).unwrap());
    // A write that does not fit parks again (a new stop), it does not latch.
    let w2 = parked(g.reserve(K, 50, None).unwrap());
    assert_eq!(w2.state(), WaiterState::Pending);
    assert_eq!(g.fulls(), 1);
}

#[test]
fn the_resume_mark_is_four_segments_or_one_percent_and_at_most_half_the_limit() {
    let ck = clock();
    let mut cfg = common::config(settings(0, OnFull::Wait, None));
    cfg.segment_size = 16 << 20;
    // Limit 4 GiB minus the 32 MiB segment reserve: 4 segments (64 MiB) win.
    let (g, _fs) = gate_cfg(4 << 30, 0, cfg.clone(), ck.clone());
    assert_eq!(g.resume_mark(), 64 << 20);
    // 100 GiB: 1 % of the limit wins.
    let (g, _fs) = gate_cfg(100 << 30, 0, cfg.clone(), ck.clone());
    let limit = g.snapshot().limit;
    assert_eq!(g.resume_mark(), limit / 100);
    // A 96 MiB limit: half of it.
    let (g, _fs) = gate_cfg(128 << 20, 0, cfg, ck);
    assert_eq!(g.resume_mark(), (96 << 20) / 2);
}

#[test]
fn on_full_error_refuses_at_once_and_does_not_latch() {
    let (g, _fs) = gate(100, settings(0, OnFull::Error, None), clock());
    reserved(g.reserve(K, 100, None).unwrap());
    refused(g.reserve(K, 1, None).unwrap());
    assert!(!g.is_full());
    g.release(K, 1);
    reserved(g.reserve(K, 1, None).unwrap());
}

#[test]
fn reconfigure_to_error_fails_every_parked_request_now() {
    // P2: a policy change re-judges the parked requests.
    let (g, _fs) = gate(100, settings(0, OnFull::Wait, None), clock());
    reserved(g.reserve(K, 100, None).unwrap());
    let a = parked(g.reserve(K, 10, None).unwrap());
    let b = parked(g.reserve(K, 20, None).unwrap());
    g.reconfigure(&settings(0, OnFull::Error, None)).unwrap();
    assert_eq!(a.state(), WaiterState::TimedOut);
    assert_eq!(b.state(), WaiterState::TimedOut);
    assert_eq!(g.snapshot().waiters, 0);
    assert_eq!(g.snapshot().used(), 100, "nothing charged for them");
    assert!(!g.is_full(), "error mode does not latch");
}

#[test]
fn a_new_full_timeout_counts_from_each_arrival() {
    let ck = clock();
    let (g, _fs) = gate(100, settings(0, OnFull::Wait, None), ck.clone());
    reserved(g.reserve(K, 100, None).unwrap());
    let a = parked(g.reserve(K, 10, None).unwrap());
    ck.advance(Duration::from_secs(5));
    let b = parked(g.reserve(K, 10, None).unwrap());
    ck.advance(Duration::from_secs(5));
    // 20 s: `a` waited 10 s, `b` 5 s; neither is overdue.
    g.reconfigure(&settings(0, OnFull::Wait, Some(Duration::from_secs(20))))
        .unwrap();
    assert_eq!(a.state(), WaiterState::Pending);
    assert_eq!(b.state(), WaiterState::Pending);
    // `a` is due at its arrival plus 20 s, not 20 s after the change.
    ck.advance(Duration::from_secs(10));
    assert_eq!(g.tick(), 2, "a expires and fails b with it");
    assert_eq!(a.state(), WaiterState::TimedOut);
    assert_eq!(b.state(), WaiterState::TimedOut);
}

#[test]
fn a_shorter_full_timeout_fails_the_overdue_requests_at_once() {
    let ck = clock();
    let (g, _fs) = gate(1000, settings(0, OnFull::Wait, None), ck.clone());
    reserved(g.reserve(K, 1000, None).unwrap());
    let a = parked(g.reserve(K, 10, None).unwrap());
    ck.advance(Duration::from_secs(10));
    g.reconfigure(&settings(0, OnFull::Wait, Some(Duration::from_secs(8))))
        .unwrap();
    assert_eq!(a.state(), WaiterState::TimedOut);
    assert!(g.is_full(), "an expiry is an expiry: the volume latches");
}

#[test]
fn a_larger_cache_max_grants_the_parked_and_opens_the_latch() {
    let ck = clock();
    let mut s = settings(0, OnFull::Wait, Some(Duration::from_secs(1)));
    s.cache_max = Some(1000);
    let (g, _fs) = gate(1 << 30, s.clone(), ck.clone());
    reserved(g.reserve(K, 1000, None).unwrap());
    let a = parked(g.reserve(K, 100, None).unwrap());
    s.cache_max = Some(1100);
    g.reconfigure(&s).unwrap();
    assert_eq!(a.state(), WaiterState::Granted);
    // Full again, latched by an expiry, then a larger cap opens it.
    let b = parked(g.reserve(K, 100, None).unwrap());
    ck.advance(Duration::from_secs(1));
    assert_eq!(g.tick(), 1);
    assert_eq!(b.state(), WaiterState::TimedOut);
    assert!(g.is_full());
    s.cache_max = Some(2000);
    g.reconfigure(&s).unwrap();
    assert!(!g.is_full());
    reserved(g.reserve(K, 100, None).unwrap());
}

#[test]
fn begin_close_grants_what_fits_fails_the_rest_and_parks_nothing_after() {
    // P3, decision M5: a quiesce must be able to drain.
    let (g, _fs) = gate(100, settings(0, OnFull::Wait, None), clock());
    reserved(g.reserve(K, 90, None).unwrap());
    let a = parked(g.reserve(K, 20, None).unwrap());
    let b = parked(g.reserve(K, 5, None).unwrap());
    g.release(K, 10);
    // `a` fits now; `b` behind it does not.
    assert_eq!(a.state(), WaiterState::Granted);
    assert_eq!(b.state(), WaiterState::Pending, "5 more do not fit");
    g.begin_close();
    assert_eq!(b.state(), WaiterState::TimedOut);
    assert_eq!(g.snapshot().waiters, 0);
    // After it: what fits is admitted, what does not fails at once.
    g.release(K, 10);
    reserved(g.reserve(K, 5, None).unwrap());
    refused(g.reserve(K, 50, None).unwrap());
    assert_eq!(g.snapshot().waiters, 0);
}

/// A step that blocks until told, counting its runs and recording `want`.
struct Blocking {
    go: Arc<Mutex<mpsc::Receiver<()>>>,
    runs: Arc<AtomicU64>,
    want: Arc<AtomicU64>,
}

impl LadderStep for Blocking {
    fn run(&mut self, want: u64, _u: Urgency) -> lunix_iface::Result<Freed> {
        self.want.store(want, Ordering::SeqCst);
        self.runs.fetch_add(1, Ordering::SeqCst);
        let rx = self.go.lock().unwrap_or_else(|p| p.into_inner());
        let _ = rx.recv_timeout(Duration::from_secs(20));
        Ok(Freed(0))
    }
}

#[test]
fn a_blocking_ladder_step_delays_no_deadline() {
    // P5: deadlines run on the timer thread, apart from the ladder.
    let mut cfg = common::config(settings(0, OnFull::Wait, Some(Duration::from_millis(300))));
    cfg.background_timers = true;
    let (g, _fs) = gate_cfg(100, 0, cfg, Arc::new(SystemClock));
    let (tx, rx) = mpsc::channel();
    let runs = Arc::new(AtomicU64::new(0));
    let want = Arc::new(AtomicU64::new(0));
    g.register(
        LadderStepKind::Compact,
        Box::new(Blocking {
            go: Arc::new(Mutex::new(rx)),
            runs: runs.clone(),
            want: want.clone(),
        }),
    );
    reserved(g.reserve(K, 100, None).unwrap());
    let t0 = Instant::now();
    let w = parked(g.reserve(K, 30, None).unwrap());
    wait_until("the step to start", Duration::from_secs(5), || {
        runs.load(Ordering::SeqCst) == 1
    });
    // The step blocks; the deadline still fires on time.
    wait_until("the expiry", Duration::from_secs(5), || {
        w.state() == WaiterState::TimedOut
    });
    let waited = t0.elapsed();
    assert!(
        waited >= Duration::from_millis(300),
        "not early: {waited:?}"
    );
    assert!(waited < Duration::from_secs(3), "prompt: {waited:?}");
    assert_eq!(runs.load(Ordering::SeqCst), 1, "the step is still running");
    // A grant does not wait for the step either.
    g.release(K, 100);
    reserved(g.reserve(K, 10, None).unwrap());
    drop(tx);
    g.shutdown();
}

#[test]
fn the_ladder_wants_every_waiting_reservation_plus_the_resume_mark() {
    let (g, _fs) = gate(1000, settings(0, OnFull::Wait, None), clock());
    let (tx, rx) = mpsc::channel();
    let runs = Arc::new(AtomicU64::new(0));
    let want = Arc::new(AtomicU64::new(0));
    g.register(
        LadderStepKind::Compact,
        Box::new(Blocking {
            go: Arc::new(Mutex::new(rx)),
            runs: runs.clone(),
            want: want.clone(),
        }),
    );
    reserved(g.reserve(K, 1000, None).unwrap());
    let _a = parked(g.reserve(K, 30, None).unwrap());
    // The first run (for `a`) blocks; the next one is asked for by `b` and
    // `c` and sees all three.
    wait_until("the first run", Duration::from_secs(5), || {
        runs.load(Ordering::SeqCst) == 1
    });
    let _b = parked(g.reserve(K, 20, None).unwrap());
    let _c = parked(g.reserve(K, 5, None).unwrap());
    tx.send(()).unwrap();
    wait_until("the second run", Duration::from_secs(5), || {
        runs.load(Ordering::SeqCst) == 2
    });
    // 30 + 20 + 5 waiting, a resume mark of 10, no headroom.
    assert_eq!(want.load(Ordering::SeqCst), 65);
    drop(tx);
    g.shutdown();
}
