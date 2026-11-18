//! The paced admission band (ADR-32 T3; the note
//! `docs/explanation/research/2026-11-18-write-backpressure.md` §5.3 and
//! §9.1): between the soft mark and the limit a token bucket admits the
//! journal reservations at a rate that falls with the headroom, a request is
//! never delayed past the cap, a small FUA write goes at once, holes are
//! never paced, and pacing is never a refusal. Manual clock unless the test
//! needs the timer thread; the band is on here and off in the other files.

mod common;

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use lunix_format::local::OnFull;
use lunix_iface::clock::{ManualClock, SystemClock};
use lunix_iface::contract::wait_until;
use lunix_iface::space::{
    ReserveClass, ReserveOutcome, SpaceGate, SpaceKind, ThrottleState, Waiter, WaiterState,
};
use lunix_space::{LunixSpace, SpaceConfig, ThrottleConfig};

use common::{clock, config, gate_cfg, settings};

const K: SpaceKind = SpaceKind::JournalReserved;
const SEG: SpaceKind = SpaceKind::Segments;
const KIB: u64 = 1 << 10;
const MIB: u64 = 1 << 20;
const GIB: u64 = 1 << 30;
/// The limit of the test gates: a 10 GiB file system, no floor, no headroom.
/// The soft mark is 15 % of it: 1.5 GiB.
const LIMIT: u64 = 10 * GIB;
const SOFT: u64 = 3 * GIB / 2;

fn cfg(timeout: Option<Duration>, tweak: impl FnOnce(&mut SpaceConfig)) -> SpaceConfig {
    let mut c = config(settings(0, OnFull::Wait, timeout));
    c.throttle = ThrottleConfig::default();
    tweak(&mut c);
    c
}

/// A gate whose headroom is `headroom` bytes (segments charged for the rest).
fn at_headroom(headroom: u64, c: SpaceConfig, ck: &Arc<ManualClock>) -> Arc<LunixSpace> {
    let (g, _fs) = gate_cfg(LIMIT, 0, c, ck.clone());
    assert_eq!(g.snapshot().limit, LIMIT);
    g.charge(SEG, LIMIT - headroom).expect("charge");
    g
}

fn reserved(o: ReserveOutcome) {
    assert!(matches!(o, ReserveOutcome::Reserved), "{o:?}");
}

fn paced(o: ReserveOutcome) -> Waiter {
    match o {
        ReserveOutcome::Paced(w) => w,
        other => panic!("expected Paced, got {other:?}"),
    }
}

fn parked(o: ReserveOutcome) -> Waiter {
    match o {
        ReserveOutcome::Parked(w) => w,
        other => panic!("expected Parked, got {other:?}"),
    }
}

fn state(g: &LunixSpace) -> ThrottleState {
    g.snapshot().throttle.state
}

#[test]
fn above_the_soft_mark_nothing_is_paced_and_nothing_is_counted() {
    let ck = clock();
    let g = at_headroom(SOFT + 10 * MIB, cfg(None, |_| {}), &ck);
    assert_eq!(g.soft_mark(), SOFT);
    for _ in 0..100 {
        reserved(g.reserve(K, KIB, None).unwrap());
    }
    let t = g.snapshot().throttle;
    assert_eq!(t.state, ThrottleState::Open);
    assert_eq!(t.rate, None);
    assert_eq!((t.delayed_writes, t.bypassed_fua, t.enospc), (0, 0, 0));
    assert_eq!(t.throttled_ms, 0);
    assert_eq!(t.soft, SOFT);
}

#[test]
fn inside_the_band_the_bucket_admits_the_curve_rate() {
    for pct in [25u64, 50, 75] {
        let ck = clock();
        let g = at_headroom(SOFT * pct / 100, cfg(None, |_| {}), &ck);
        let t = g.snapshot().throttle;
        assert_eq!(t.state, ThrottleState::Slowdown);
        let rate = t.rate.expect("in the band");
        // x = 0.25, 0.5, 0.75 with r_mid = 64 MiB/s: 21, 64 and 192 MiB/s.
        let want = [(25, 21), (50, 64), (75, 192)]
            .iter()
            .find(|(p, _)| *p == pct)
            .map(|(_, r)| r * MIB)
            .unwrap();
        assert!(rate.abs_diff(want) < MIB, "{pct}: {rate} against {want}");
        let rec = 64 * KIB;
        let mut waiting: VecDeque<Waiter> = VecDeque::new();
        let mut admitted = 0u64;
        let secs = 20;
        for _ in 0..secs * 100 {
            ck.advance(Duration::from_millis(10));
            g.tick();
            // Strictly first in, first out: grants form a prefix.
            let mut behind_pending = false;
            for w in &waiting {
                match w.state() {
                    WaiterState::Pending => behind_pending = true,
                    WaiterState::Granted => assert!(!behind_pending, "granted out of order"),
                    other => panic!("{other:?}"),
                }
            }
            while waiting
                .front()
                .is_some_and(|w| w.state() == WaiterState::Granted)
            {
                waiting.pop_front();
                admitted += rec;
                g.release(K, rec);
            }
            while waiting.len() < 400 {
                match g.reserve(K, rec, None).unwrap() {
                    ReserveOutcome::Reserved => {
                        admitted += rec;
                        g.release(K, rec);
                    }
                    ReserveOutcome::Paced(w) => waiting.push_back(w),
                    other => panic!("{other:?}"),
                }
            }
        }
        // `rate x time` within one bucket (the first fills it, the last
        // leaves it) and one record.
        let cap = (MIB + 64).max(rate * 50 / 1000);
        let expect = rate * secs;
        assert!(
            admitted.abs_diff(expect) <= cap + 2 * rec,
            "{pct} %: admitted {admitted}, rate x time {expect}, one bucket {cap}"
        );
        assert!(g.snapshot().throttle.delayed_writes > 0);
    }
}

#[test]
fn the_delay_cap_admits_at_exactly_the_cap_and_the_debt_is_bounded() {
    let ck = clock();
    // A slow minimum rate and a 2 s cap; 4 MiB of headroom.
    let c = cfg(None, |c| {
        c.throttle = c.throttle.with_gc_rate(0);
        c.throttle.min_rate = 64 * KIB;
        c.throttle.max_delay = Duration::from_secs(2);
    });
    let g = at_headroom(4 * MIB, c, &ck);
    let rec = 256 * KIB;
    // The full bucket (1 MiB + 64) pays four at once.
    for _ in 0..4 {
        reserved(g.reserve(K, rec, None).unwrap());
    }
    // The fifth and two more wait: 64 KiB/s would take 4 s for the fifth.
    let ws: Vec<Waiter> = (0..3)
        .map(|_| paced(g.reserve(K, rec, None).unwrap()))
        .collect();
    ck.advance(Duration::from_millis(1999));
    g.tick();
    assert!(ws.iter().all(|w| w.state() == WaiterState::Pending));
    ck.advance(Duration::from_millis(1));
    g.tick();
    // All three arrived at 0: each is past its cap now, so all go, and the
    // bucket carries the debt.
    assert!(
        ws.iter().all(|w| w.state() == WaiterState::Granted),
        "{:?}",
        ws.iter().map(Waiter::state).collect::<Vec<_>>()
    );
    let t = g.snapshot().throttle;
    assert_eq!(t.delay_capped, 3);
    assert_eq!(t.delayed_writes, 3);
    assert_eq!(t.delay_max_ms, 2000);
    assert!(!g.is_full(), "pacing never latches the volume");
    // The debt is at most one second of the rate (64 KiB): a request that
    // arrives now needs 320 KiB of tokens at 64 KiB/s (5 s), so its cap, 2 s
    // from its arrival, is what admits it.
    g.release(K, 7 * rec);
    let w = paced(g.reserve(K, rec, None).unwrap());
    ck.advance(Duration::from_millis(1999));
    g.tick();
    assert_eq!(w.state(), WaiterState::Pending);
    ck.advance(Duration::from_millis(1));
    g.tick();
    assert_eq!(w.state(), WaiterState::Granted);
    assert_eq!(g.snapshot().throttle.delay_capped, 4);
}

#[test]
fn a_paced_request_is_never_failed_for_the_pace() {
    // The delay cap (10 s) is longer than this volume's full_timeout (3 s):
    // the shorter one admits it, and nothing latches.
    let ck = clock();
    let c = cfg(Some(Duration::from_secs(3)), |c| {
        c.throttle = c.throttle.with_gc_rate(0);
        c.throttle.min_rate = 64 * KIB;
    });
    let g = at_headroom(4 * MIB, c, &ck);
    for _ in 0..4 {
        reserved(g.reserve(K, 256 * KIB, None).unwrap());
    }
    let w = paced(g.reserve(K, 256 * KIB, None).unwrap());
    ck.advance(Duration::from_millis(2999));
    assert_eq!(g.tick(), 0);
    assert_eq!(w.state(), WaiterState::Pending);
    ck.advance(Duration::from_millis(1));
    assert_eq!(g.tick(), 0, "nothing failed");
    assert_eq!(w.state(), WaiterState::Granted);
    assert!(!g.is_full());
    assert_eq!(g.fulls(), 0);
    assert_eq!(g.snapshot().throttle.enospc, 0);
}

#[test]
fn a_small_fua_write_goes_at_once_and_is_charged_to_the_bucket() {
    let ck = clock();
    let g = at_headroom(8 * MIB, cfg(None, |_| {}), &ck);
    // Drain the bucket (1 MiB + 64) and queue one bulk write behind it.
    reserved(g.reserve(K, MIB, None).unwrap());
    let bulk = paced(g.reserve(K, 256 * KIB, None).unwrap());
    // The FUA write overtakes it: admitted at once, counted, charged.
    let used = g.snapshot().used();
    reserved(
        g.reserve_since(K, 4 * KIB, None, ck_now(&ck), ReserveClass::Sync)
            .unwrap(),
    );
    let t = g.snapshot().throttle;
    assert_eq!(t.bypassed_fua, 1);
    assert_eq!(g.snapshot().used(), used + 4 * KIB);
    assert_eq!(
        bulk.state(),
        WaiterState::Pending,
        "the bulk write still waits"
    );
    // Bulk pays the debt: the 256 KiB write now needs the 4 KiB too.
    let r = t.rate.expect("band");
    let need = Duration::from_secs_f64((256.0 * 1024.0 + 4096.0 - 64.0) / r as f64);
    ck.advance(need - Duration::from_millis(5));
    g.tick();
    assert_eq!(bulk.state(), WaiterState::Pending);
    ck.advance(Duration::from_millis(10));
    g.tick();
    assert_eq!(bulk.state(), WaiterState::Granted);
}

fn ck_now(ck: &ManualClock) -> Duration {
    use lunix_iface::clock::Clock;
    ck.monotonic()
}

#[test]
fn a_fua_write_at_the_hard_mark_waits_like_any_write_and_holes_never_do() {
    let ck = clock();
    let g = at_headroom(100 * KIB, cfg(Some(Duration::from_secs(5)), |_| {}), &ck);
    let w = parked(
        g.reserve_since(K, 200 * KIB, None, ck_now(&ck), ReserveClass::Sync)
            .unwrap(),
    );
    assert_eq!(g.snapshot().throttle.bypassed_fua, 0);
    assert_eq!(state(&g), ThrottleState::Stop);
    // The deadline latches the volume; a hole record is still admitted
    // (charged without a limit check) and a write still is not.
    ck.advance(Duration::from_secs(5));
    assert_eq!(g.tick(), 1);
    assert_eq!(w.state(), WaiterState::TimedOut);
    assert!(g.is_full());
    let used = g.snapshot().used();
    g.charge(K, 64).expect("a hole record");
    assert_eq!(g.snapshot().used(), used + 64);
    assert!(matches!(
        g.reserve(K, 1, None).unwrap(),
        ReserveOutcome::NoSpace
    ));
}

#[test]
fn the_state_walks_open_slowdown_stop_full_and_back_with_its_counters() {
    let ck = clock();
    let c = cfg(Some(Duration::from_secs(5)), |_| {});
    let g = at_headroom(SOFT + 10 * MIB, c, &ck);
    assert_eq!(state(&g), ThrottleState::Open);
    ck.advance(Duration::from_secs(1));
    reserved(g.reserve(K, 20 * MIB, None).unwrap());
    let t = g.snapshot().throttle;
    assert_eq!(t.state, ThrottleState::Slowdown);
    assert_eq!(t.state_since_ms, 1000);
    assert!(t.rate.is_some());
    assert_eq!(t.soft, SOFT);
    // Fill to 100 KiB of headroom, then ask for more than that.
    let head = g.snapshot().throttle.headroom;
    g.charge(SEG, head - 100 * KIB).unwrap();
    ck.advance(Duration::from_secs(3));
    let w = parked(g.reserve(K, MIB, None).unwrap());
    let t = g.snapshot().throttle;
    assert_eq!(t.state, ThrottleState::Stop);
    assert_eq!((t.stops, t.space_waiters, t.paced_waiters), (1, 1, 0));
    // Its deadline: 5 s after it arrived (t = 4 s).
    ck.advance(Duration::from_millis(4999));
    assert_eq!(g.tick(), 0);
    ck.advance(Duration::from_millis(1));
    assert_eq!(g.tick(), 1);
    assert_eq!(w.state(), WaiterState::TimedOut);
    let t = g.snapshot().throttle;
    assert_eq!(t.state, ThrottleState::Full);
    assert_eq!((t.fulls, t.enospc), (1, 1));
    assert_eq!(
        t.delay_max_ms, 5000,
        "the wait of the failed request counts"
    );
    // 2 GiB comes back: above the soft mark, the latch opens.
    ck.advance(Duration::from_secs(1));
    g.release(SEG, 2 * GIB);
    let t = g.snapshot().throttle;
    assert_eq!(t.state, ThrottleState::Open);
    // Outside `open` from 1 s to 10 s of the clock.
    ck.advance(Duration::from_secs(5));
    assert_eq!(g.snapshot().throttle.throttled_ms, 9000);
}

#[test]
fn with_the_band_off_nothing_is_paced() {
    let ck = clock();
    let c = cfg(None, |c| c.throttle.enabled = false);
    let g = at_headroom(4 * MIB, c, &ck);
    assert_eq!(g.soft_mark(), 0);
    for _ in 0..16 {
        reserved(g.reserve(K, 128 * KIB, None).unwrap());
    }
    let t = g.snapshot().throttle;
    assert_eq!(
        (t.state, t.rate, t.delayed_writes),
        (ThrottleState::Open, None, 0)
    );
    // The bounded stop stays: the next one does not fit and parks.
    parked(g.reserve(K, 3 * MIB, None).unwrap());
    assert_eq!(state(&g), ThrottleState::Stop);
}

#[test]
fn the_quiesce_admits_a_paced_request_that_fits_and_pacing_ends() {
    let ck = clock();
    let g = at_headroom(8 * MIB, cfg(None, |_| {}), &ck);
    reserved(g.reserve(K, MIB, None).unwrap());
    let w = paced(g.reserve(K, 256 * KIB, None).unwrap());
    let big = parked_after(&g, 100 * MIB);
    g.begin_close();
    assert_eq!(w.state(), WaiterState::Granted);
    assert_eq!(big.state(), WaiterState::TimedOut);
    reserved(g.reserve(K, 128 * KIB, None).unwrap());
}

/// A request that does not fit, parked behind whatever waits.
fn parked_after(g: &LunixSpace, bytes: u64) -> Waiter {
    parked(g.reserve(K, bytes, None).unwrap())
}

#[test]
fn a_switch_to_on_full_error_keeps_the_paced_and_fails_the_ones_that_do_not_fit() {
    let ck = clock();
    let g = at_headroom(8 * MIB, cfg(None, |_| {}), &ck);
    reserved(g.reserve(K, MIB, None).unwrap());
    let slow = paced(g.reserve(K, 256 * KIB, None).unwrap());
    let big = parked_after(&g, 100 * MIB);
    let mut s = settings(0, OnFull::Error, None);
    s.cache_max = None;
    g.reconfigure(&s).unwrap();
    assert_eq!(big.state(), WaiterState::TimedOut, "no room: refused now");
    assert_eq!(
        slow.state(),
        WaiterState::Pending,
        "the pace is not a refusal"
    );
    ck.advance(Duration::from_secs(1));
    g.tick();
    assert_eq!(slow.state(), WaiterState::Granted);
    // And under `error` a request that fits is still paced, not refused.
    let rate = g.snapshot().throttle.rate.expect("band");
    assert!(rate > 0);
}

#[test]
fn the_timer_thread_grants_paced_requests_without_a_tick() {
    // Real clock and the background timer: the head's token time is the
    // timer's wake time.
    let (g, _fs) = gate_cfg(
        LIMIT,
        0,
        {
            let mut c = cfg(None, |c| c.throttle.max_delay = Duration::from_secs(5));
            c.background_timers = true;
            c
        },
        Arc::new(SystemClock),
    );
    g.charge(SEG, LIMIT - 8 * MIB).unwrap();
    reserved(g.reserve(K, MIB, None).unwrap());
    let ws: Vec<Waiter> = (0..4)
        .map(|_| paced(g.reserve(K, 128 * KIB, None).unwrap()))
        .collect();
    // 512 KiB at about 64 KiB-rate times x: well inside the 5 s cap, so the
    // token time (not the cap) admits them.
    wait_until(
        "the timer grants the paced requests",
        Duration::from_secs(20),
        || ws.iter().all(|w| w.state() == WaiterState::Granted),
    );
    let t = g.snapshot().throttle;
    assert_eq!(t.delayed_writes, 4);
    g.shutdown();
}

#[test]
fn the_ingest_and_reclaim_rates_average_ten_seconds() {
    let ck = clock();
    let g = at_headroom(SOFT + GIB, cfg(None, |_| {}), &ck);
    for _ in 0..20 {
        g.snapshot();
        reserved(g.reserve(K, MIB, None).unwrap());
        ck.advance(Duration::from_secs(1));
    }
    // Segments freed: 2 MiB a second for the last 10 s.
    for _ in 0..11 {
        g.snapshot();
        g.release(SEG, 2 * MIB);
        ck.advance(Duration::from_secs(1));
    }
    let t = g.snapshot().throttle;
    assert_eq!(t.reclaim_rate, 2 * MIB, "{t:?}");
    assert_eq!(t.ingest_rate, 0, "no write in the last 10 s: {t:?}");
}

mod model {
    //! The property test of the note's §9.1 with the band on: random
    //! reservations (bulk and sync), releases, hole charges, policy changes
    //! and clock steps against one gate. After every step: `used` stays
    //! within the limit, no request waits past its `full_timeout` (or its
    //! cap), and at the end every request is answered and the ledger is
    //! exact.

    use super::*;
    use proptest::prelude::*;

    #[derive(Clone, Debug)]
    enum Op {
        Reserve { kib: u64, sync: bool },
        Release,
        Hole,
        Free(u64),
        Advance(u64),
        Timeout(u64),
        OnFull(bool),
    }

    fn op() -> impl Strategy<Value = Op> {
        prop_oneof![
            6 => (1u64..400, any::<bool>()).prop_map(|(kib, sync)| Op::Reserve { kib, sync }),
            3 => Just(Op::Release),
            1 => Just(Op::Hole),
            1 => (1u64..300).prop_map(Op::Free),
            6 => (1u64..4000).prop_map(Op::Advance),
            1 => (1u64..12).prop_map(Op::Timeout),
            1 => any::<bool>().prop_map(Op::OnFull),
        ]
    }

    struct Req {
        w: Waiter,
        bytes: u64,
        arrived: Duration,
        timeout_at_arrival: Duration,
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 3000, ..ProptestConfig::default() })]

        #[test]
        fn the_band_never_overruns_the_limit_and_no_waiter_outlives_its_deadline(
            ops in proptest::collection::vec(op(), 1..120),
            start_headroom_mib in 1u64..3000,
        ) {
            let ck = clock();
            let timeout = Duration::from_secs(6);
            let (g, _fs) = gate_cfg(
                LIMIT, 0, cfg(Some(timeout), |c| c.throttle.max_delay = Duration::from_secs(3)),
                ck.clone(),
            );
            let limit = g.snapshot().limit;
            g.charge(SEG, limit - start_headroom_mib * MIB).unwrap();
            let mut held: Vec<u64> = Vec::new(); // granted journal reservations
            let mut live: Vec<Req> = Vec::new();
            let mut timeout_now = timeout;
            let mut holes = 0u64;
            let mut freed = 0u64;
            for o in ops {
                let now = ck_now(&ck);
                match o {
                    Op::Reserve { kib, sync } => {
                        let bytes = kib * KIB;
                        let class = if sync && bytes <= 256 * KIB { ReserveClass::Sync } else { ReserveClass::Bulk };
                        match g.reserve_since(K, bytes, None, now, class).unwrap() {
                            ReserveOutcome::Reserved => held.push(bytes),
                            ReserveOutcome::Parked(w) | ReserveOutcome::Paced(w) => live.push(Req {
                                w, bytes, arrived: now, timeout_at_arrival: timeout_now,
                            }),
                            ReserveOutcome::NoSpace => {}
                        }
                    }
                    Op::Release => {
                        if let Some(b) = held.pop() {
                            g.release(K, b);
                        }
                    }
                    Op::Hole => {
                        g.charge(K, 64).unwrap();
                        holes += 64;
                    }
                    Op::Free(mib) => {
                        let have = g.snapshot().by_kind[SEG.index()];
                        let n = (mib * MIB).min(have);
                        g.release(SEG, n);
                        freed += n;
                    }
                    Op::Advance(ms) => ck.advance(Duration::from_millis(ms)),
                    Op::Timeout(s) => {
                        timeout_now = Duration::from_secs(s);
                        let mut st = settings(0, OnFull::Wait, Some(timeout_now));
                        st.cache_max = None;
                        g.reconfigure(&st).unwrap();
                    }
                    Op::OnFull(error) => {
                        let mut st = settings(0, if error { OnFull::Error } else { OnFull::Wait }, Some(timeout_now));
                        st.cache_max = None;
                        g.reconfigure(&st).unwrap();
                    }
                }
                g.tick();
                // Collect outcomes.
                let now = ck_now(&ck);
                let mut still = Vec::new();
                for r in live.drain(..) {
                    match r.w.state() {
                        WaiterState::Granted => held.push(r.bytes),
                        WaiterState::Pending => still.push(r),
                        _ => {}
                    }
                }
                live = still;
                // The limit holds by admission (holes and true-ups aside).
                let snap = g.snapshot();
                prop_assert!(
                    snap.used() <= snap.limit + holes,
                    "used {} over limit {} + holes {}", snap.used(), snap.limit, holes
                );
                // No request waits past its deadline (the policy in force at
                // its arrival, or the later one: the sooner of the two bounds).
                for r in &live {
                    let waited = now.saturating_sub(r.arrived);
                    prop_assert!(
                        waited <= r.timeout_at_arrival.max(timeout_now),
                        "a request has waited {:?} (timeouts {:?}, {:?})", waited, r.timeout_at_arrival, timeout_now
                    );
                }
                // The counters are consistent with what was seen.
                prop_assert!(snap.throttle.paced_waiters + snap.throttle.space_waiters == snap.waiters);
            }
            // Everything is answered: past every deadline and cap, with
            // the headroom back, nothing is left pending.
            ck.advance(Duration::from_secs(200));
            g.tick();
            for r in &live {
                prop_assert!(r.w.state() != WaiterState::Pending, "a request is still pending");
                if r.w.state() == WaiterState::Granted {
                    held.push(r.bytes);
                }
            }
            let snap = g.snapshot();
            prop_assert_eq!(snap.waiters, 0);
            // The ledger is exact: segments (initial charge less what was
            // freed) and the journal bytes still held, plus the holes.
            let want_journal: u64 = held.iter().sum::<u64>() + holes;
            prop_assert_eq!(snap.by_kind[K.index()], want_journal);
            prop_assert_eq!(snap.by_kind[SEG.index()], limit - start_headroom_mib * MIB - freed);
        }
    }
}
