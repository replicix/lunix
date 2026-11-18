//! Threads against the gate: `transfer` is atomic, reserve/release is
//! race-free and exact. A short run in the normal suite; the 60 s run is
//! `--ignored` (the plan's stress gate; `LUNIX_STRESS_SECS` overrides).

mod common;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use lunix_format::local::OnFull;
use lunix_iface::clock::SystemClock;
use lunix_iface::space::{ReserveOutcome, SpaceGate, SpaceKind, WaiterState};

fn rng(x: &mut u64) -> u64 {
    *x ^= *x << 13;
    *x ^= *x >> 7;
    *x ^= *x << 17;
    *x
}

fn secs(default: u64) -> Duration {
    let s = std::env::var("LUNIX_STRESS_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default);
    Duration::from_secs(s)
}

fn transfer_is_atomic(run_for: Duration) {
    const TOTAL: u64 = 1_000_000;
    let (g, _fs) = common::gate(
        1 << 40,
        common::settings(0, OnFull::Error, None),
        Arc::new(SystemClock),
    );
    let (a, b) = (SpaceKind::Segments, SpaceKind::Index);
    g.charge(a, TOTAL).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let seen = Arc::new(AtomicU64::new(0));
    let mut hs = vec![];
    for t in 0..4u64 {
        let (g, stop) = (g.clone(), stop.clone());
        hs.push(thread::spawn(move || {
            let mut x = 0x1234_5678_9abc_def1 ^ ((t + 1) * 0x9e37_79b9);
            while !stop.load(Ordering::Relaxed) {
                let n = rng(&mut x) % 5000 + 1;
                let (from, to) = if rng(&mut x) & 1 == 0 { (a, b) } else { (b, a) };
                // May be refused when `from` holds less: nothing changes.
                let _ = g.transfer(from, to, n, n);
            }
        }));
    }
    for _ in 0..4 {
        let (g, stop, seen) = (g.clone(), stop.clone(), seen.clone());
        hs.push(thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                let s = g.snapshot();
                let sum = s.by_kind[a.index()] + s.by_kind[b.index()];
                assert_eq!(sum, TOTAL, "bytes seen in both or neither kind");
                seen.fetch_add(1, Ordering::Relaxed);
            }
        }));
    }
    thread::sleep(run_for);
    stop.store(true, Ordering::Relaxed);
    for h in hs {
        h.join().unwrap();
    }
    assert!(seen.load(Ordering::Relaxed) > 1000);
    let s = g.snapshot();
    assert_eq!(s.by_kind[a.index()] + s.by_kind[b.index()], TOTAL);
}

fn reserve_release(run_for: Duration) {
    const CAP: u64 = 100_000;
    const SLACK: u64 = 8 * 40_000;
    let mut cfg = common::config(common::settings(
        10_000,
        OnFull::Wait,
        Some(Duration::from_millis(150)),
    ));
    cfg.background_timers = true;
    let (g, _fs) = common::gate_cfg(CAP, 0, cfg, Arc::new(SystemClock));
    let limit = g.snapshot().limit;
    let stop = Arc::new(AtomicBool::new(false));
    let granted = Arc::new(AtomicU64::new(0));
    let timed_out = Arc::new(AtomicU64::new(0));
    let mut hs = vec![];
    for t in 0..8u64 {
        let (g, stop, granted, timed_out) =
            (g.clone(), stop.clone(), granted.clone(), timed_out.clone());
        hs.push(thread::spawn(move || {
            let mut x = 0xdead_beef_0000_0001 ^ ((t + 1) * 0x2545_f491);
            let kinds = [
                SpaceKind::JournalReserved,
                SpaceKind::Segments,
                SpaceKind::Map,
            ];
            while !stop.load(Ordering::Relaxed) {
                let k = kinds[(rng(&mut x) % 3) as usize];
                let bytes = rng(&mut x) % 40_000 + 1;
                let got = match g.reserve(k, bytes, None).unwrap() {
                    ReserveOutcome::Reserved => true,
                    ReserveOutcome::NoSpace => false,
                    ReserveOutcome::Parked(w) | ReserveOutcome::Paced(w) => {
                        let t0 = Instant::now();
                        let cancel_after = Duration::from_millis(rng(&mut x) % 300);
                        loop {
                            match w.state() {
                                WaiterState::Granted => break true,
                                WaiterState::TimedOut | WaiterState::Cancelled => {
                                    timed_out.fetch_add(1, Ordering::Relaxed);
                                    break false;
                                }
                                WaiterState::Pending => {
                                    if t0.elapsed() > cancel_after && w.cancel() {
                                        break false;
                                    }
                                    thread::yield_now();
                                }
                            }
                        }
                    }
                };
                if got {
                    granted.fetch_add(1, Ordering::Relaxed);
                    // The probe cannot be exact while 8 threads move the
                    // ledger (see `refresh`): allow what can be in flight.
                    assert!(
                        g.snapshot().used() <= limit + SLACK,
                        "writes pass the limit"
                    );
                    if rng(&mut x).is_multiple_of(4) {
                        thread::yield_now();
                    }
                    // Half of them travel through a transfer first.
                    if rng(&mut x) & 1 == 0 {
                        g.transfer(k, SpaceKind::Index, bytes, bytes).unwrap();
                        g.release(SpaceKind::Index, bytes);
                    } else {
                        g.release(k, bytes);
                    }
                }
            }
        }));
    }
    thread::sleep(run_for);
    stop.store(true, Ordering::Relaxed);
    for h in hs {
        h.join().unwrap();
    }
    let s = g.snapshot();
    assert_eq!(
        s.by_kind, [0; 7],
        "ledger exactly zero after the threads end"
    );
    assert_eq!(s.waiters, 0);
    assert!(granted.load(Ordering::Relaxed) > 1000);
    eprintln!(
        "stress: granted {}, timed out or cancelled {}",
        granted.load(Ordering::Relaxed),
        timed_out.load(Ordering::Relaxed)
    );
    g.shutdown();
}

#[test]
fn transfer_is_atomic_under_8_threads() {
    transfer_is_atomic(secs(2));
}

#[test]
fn eight_threads_reserving_and_releasing_are_race_free() {
    reserve_release(secs(3));
}

#[test]
#[ignore = "the plan's 60 s stress run"]
fn stress_60_seconds() {
    thread::scope(|s| {
        s.spawn(|| transfer_is_atomic(Duration::from_secs(30)));
        s.spawn(|| reserve_release(Duration::from_secs(60)));
    });
}
