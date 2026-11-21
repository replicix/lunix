//! Milestone gate: the band reaches an equilibrium when the collector is sized
//! to the ingest (ADR-32 amendment), on a simulated store.
//!
//! The real gate (`LunixSpace`, the band's token bucket and delay cap) and the
//! engine's real pressure rules (`Pressure`, `PressureTracker`, `gc_demand`)
//! run on a manual clock against a model of the store and the collector:
//!
//! * a writer offers 100 MiB/s through one queue (strict FIFO, at most 256
//!   writes in flight, as the engine's queue does);
//! * every admitted byte is a byte of garbage over a live set of 1 GiB;
//!   garbage is reclaimable once it is older than the collector's grace;
//! * the collector's work in a second is bounded by the compaction rate it
//!   is handed, by its step budget (20 steps a second) and by a device
//!   ceiling of 120 MiB/s; freeing a byte takes 1.6 bytes of compaction
//!   work (the measured write amplification).
//!
//! The volume's `gc_rate` is 1 MiB/s (a volume that keeps compaction slow in
//! normal times): four times that, the old pressure's figure, cannot reclaim
//! even the band's minimum rate, so the old rules end in `stop`. Sized to the
//! ingest, the collector reclaims what the writer is admitted, the headroom
//! settles inside the band and the gate never stops.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use lunix_engine::{gc_demand, Pressure, PressureTracker, DEFAULT_WRITE_AMP_PERCENT};
use lunix_format::local::{FreeFloor, OnFull, SpaceSettings};
use lunix_format::{IdLen, TimeMs};
use lunix_iface::clock::{Clock, ManualClock};
use lunix_iface::gc::{GcBudget, GcParams};
use lunix_iface::space::{
    ReserveClass, ReserveOutcome, SpaceGate, SpaceKind, ThrottleState, Waiter, WaiterState,
};
use lunix_space::{LunixSpace, SimFs, SpaceConfig};

const MIB: u64 = 1 << 20;
const GIB: u64 = 1 << 30;
const LIMIT: u64 = 10 * GIB;
const REC: u64 = 64 << 10;
const GC_RATE: u64 = MIB;
const DEVICE: u64 = 120 * MIB;
const STEPS_PER_S: u64 = 20;
const OFFERED: u64 = 100 * MIB;
const DEPTH: usize = 256;
const DT: Duration = Duration::from_millis(10);
/// Steps in a second.
const PER_S: u64 = 100;

const K: SpaceKind = SpaceKind::JournalReserved;
const SEG: SpaceKind = SpaceKind::Segments;

struct Outcome {
    stops: u64,
    enospc: u64,
    /// The headroom, one sample a second.
    headroom: Vec<u64>,
    /// Bytes admitted in the last 200 s, per second.
    tail_rate: u64,
    /// The longest wait of a write from its arrival, ms (status figure).
    delay_max_ms: u64,
    soft: u64,
    state: ThrottleState,
}

fn run(sized: bool, seconds: u64) -> Outcome {
    let ck = Arc::new(ManualClock::new(TimeMs(0)));
    let settings = SpaceSettings {
        free_floor: FreeFloor::Bytes(0),
        on_full: OnFull::Wait,
        full_timeout_ms: Some(60_000),
        cache_max: None,
        cache_max_percent: None,
        headroom: 0,
    };
    let mut cfg = SpaceConfig::new(settings);
    cfg.segment_size = 0;
    cfg.background_timers = false;
    cfg.throttle = cfg.throttle.with_gc_rate(GC_RATE);
    let fs = Arc::new(SimFs::new(LIMIT, 0));
    let g = LunixSpace::new(cfg, fs.clone(), ck.clone()).expect("gate");
    fs.attach(&g);

    // 1 GiB live and 1 GiB of garbage old enough to reclaim.
    ck.advance(Duration::from_secs(1000));
    g.charge(SEG, 2 * GIB).expect("initial store");
    let mut garbage: VecDeque<(Duration, u64)> = VecDeque::new();
    garbage.push_back((Duration::ZERO, GIB));

    let base_budget = GcBudget {
        max_duration: Duration::from_millis(50),
        max_bytes: GC_RATE.max(MIB),
    };
    let base_params = GcParams {
        retain_last: 2,
        grace: Duration::from_secs(60),
        read_timeout: Duration::from_secs(10),
        rate_bytes_per_s: GC_RATE,
        live_threshold: 0.5,
        chunk_size: 16 << 10,
        id_len: IdLen::Sixteen,
    };
    let mut tracker = PressureTracker::default();

    let mut pending: VecDeque<Duration> = VecDeque::new();
    let mut front: Option<(Waiter, Duration)> = None;
    let mut credit = 0u64;
    let mut work_credit = 0f64;
    let mut admitted_total = 0u64;
    let mut admitted_at_tail = 0u64;
    let mut headroom = Vec::new();
    let tail_from = seconds.saturating_sub(200);
    let start = ck.monotonic();

    let admit = |garbage: &mut VecDeque<(Duration, u64)>, now: Duration, total: &mut u64| {
        // The record leaves the journal for the store: the bytes are garbage
        // (an overwrite) from now on.
        g.transfer(K, SEG, REC, REC).expect("destage");
        garbage.push_back((now, REC));
        *total += REC;
    };

    let steps = seconds * 1000 / DT.as_millis() as u64;
    for step in 0..steps {
        let now = ck.monotonic();
        // The writer.
        credit = (credit + OFFERED * DT.as_millis() as u64 / 1000).min(OFFERED);
        while credit >= REC && pending.len() < DEPTH {
            credit -= REC;
            pending.push_back(now);
        }
        // The queue: only the front is in the gate.
        loop {
            if let Some((w, _)) = &front {
                if w.state() != WaiterState::Granted {
                    break;
                }
                front = None;
                admit(&mut garbage, now, &mut admitted_total);
            }
            let Some(arrived) = pending.pop_front() else {
                break;
            };
            match g
                .reserve_since(K, REC, None, arrived, ReserveClass::Bulk)
                .expect("reserve")
            {
                ReserveOutcome::Reserved => admit(&mut garbage, now, &mut admitted_total),
                ReserveOutcome::Paced(w) | ReserveOutcome::Parked(w) => {
                    front = Some((w, arrived));
                }
                ReserveOutcome::NoSpace => {}
            }
        }
        // The collector, as the engine hands it its figures at each tick.
        let snap = g.snapshot();
        let active = snap.throttle.ingest_rate > 0 || snap.waiters > 0;
        let level = tracker.observe(snap.throttle.state, active, now);
        let demand = if sized && level != Pressure::Normal {
            gc_demand(&snap, DEFAULT_WRITE_AMP_PERCENT)
        } else {
            0
        };
        let budget = level.budget(base_budget, base_params.rate_bytes_per_s, demand);
        let params = level.params(base_params, 4, demand);
        let by_steps = budget.max_bytes.saturating_mul(STEPS_PER_S);
        let capacity = params.rate_bytes_per_s.min(DEVICE).min(by_steps);
        work_credit += capacity as f64 * DT.as_secs_f64();
        let mut freed_now = 0u64;
        while let Some(&(t, bytes)) = garbage.front() {
            if now.saturating_sub(t) < params.grace {
                break;
            }
            let need = bytes as f64 * 1.6;
            let take = if work_credit >= need {
                bytes
            } else {
                (work_credit / 1.6) as u64
            };
            if take == 0 {
                break;
            }
            work_credit -= take as f64 * 1.6;
            freed_now += take;
            if take == bytes {
                garbage.pop_front();
            } else if let Some(f) = garbage.front_mut() {
                f.1 -= take;
                break;
            }
        }
        // No work is banked beyond a second.
        work_credit = work_credit.min(capacity as f64);
        if freed_now > 0 {
            g.release(SEG, freed_now);
        }
        ck.advance(DT);
        g.tick();
        if step % PER_S == PER_S - 1 {
            headroom.push(g.snapshot().throttle.headroom);
        }
        if (ck.monotonic() - start).as_secs() == tail_from && step % PER_S == PER_S - 1 {
            admitted_at_tail = admitted_total;
        }
    }
    let t = g.snapshot().throttle;
    let o = Outcome {
        stops: t.stops,
        enospc: t.enospc,
        headroom,
        tail_rate: (admitted_total - admitted_at_tail) / 200,
        delay_max_ms: t.delay_max_ms,
        soft: t.soft,
        state: t.state,
    };
    g.shutdown();
    o
}

#[test]
fn sized_to_the_ingest_the_band_reaches_a_steady_state_and_never_stops() {
    let o = run(true, 900);
    eprintln!(
        "sized: stops {} enospc {} state {:?} tail {} MiB/s delay_max {} ms headroom tail {:?}",
        o.stops,
        o.enospc,
        o.state,
        o.tail_rate / MIB,
        o.delay_max_ms,
        &o.headroom[o.headroom.len() - 5..]
    );
    assert_eq!(o.stops, 0, "the band ended in stop");
    assert_eq!(o.enospc, 0);
    assert_eq!(o.state, ThrottleState::Slowdown, "it rests inside the band");
    // Steady: over the last 200 s the headroom stays inside the band, away
    // from the hard mark, and moves by a small part of it.
    let tail = &o.headroom[o.headroom.len() - 200..];
    let (lo, hi) = (
        *tail.iter().min().expect("tail"),
        *tail.iter().max().expect("tail"),
    );
    assert!(
        lo > o.soft / 20,
        "too near the hard mark: {lo} of {}",
        o.soft
    );
    assert!(hi < o.soft, "left the band: {hi}");
    assert!(hi - lo < o.soft / 4, "not steady: {lo}..{hi} of {}", o.soft);
    // The admitted rate is what the collector reclaims (device ceiling over
    // the amplification: 75 MiB/s), neither a trickle nor more.
    assert!(
        (60 * MIB..=90 * MIB).contains(&o.tail_rate),
        "{} MiB/s",
        o.tail_rate / MIB
    );
}

#[test]
fn with_the_old_fixed_figures_the_same_load_ends_in_stop() {
    // The rule removed: the floors only (four times a 1 MiB/s base).
    let o = run(false, 900);
    eprintln!(
        "fixed: stops {} enospc {} state {:?} tail {} MiB/s delay_max {} ms",
        o.stops,
        o.enospc,
        o.state,
        o.tail_rate / MIB,
        o.delay_max_ms
    );
    assert!(
        o.stops >= 1,
        "expected the old figures to stop: {}",
        o.stops
    );
}
