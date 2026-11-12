//! Model tests: a straight-line reference of the ledger and the wait queue,
//! driven by proptest (5 000 sequences) against the real gate, plus the
//! million-pair exactness run.

mod common;

use std::sync::Arc;
use std::time::Duration;

use lunix_format::local::OnFull;
use lunix_iface::clock::ManualClock;
use lunix_iface::space::{ReserveOutcome, SpaceGate, SpaceKind, Waiter, WaiterState};
use lunix_space::{LunixSpace, SimFs};
use proptest::prelude::*;

const CAP: u64 = 1000;
const HEADROOM_IDX: usize = SpaceKind::Headroom as usize;

#[derive(Clone, Debug)]
enum Op {
    Reserve { kind: u8, bytes: u64 },
    Release { kind: u8, num: u8 },
    Head { kind: u8, bytes: u64 },
    Transfer { from: u8, to: u8, num: u8, num2: u8 },
    Advance(u64),
    Cancel(u8),
    SetExternal(u64),
    Refresh,
    Reconf { wait: bool, timeout_ms: Option<u64> },
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        6 => (0u8..6, 0u64..450).prop_map(|(kind, bytes)| Op::Reserve { kind, bytes }),
        4 => (0u8..7, any::<u8>()).prop_map(|(kind, num)| Op::Release { kind, num }),
        2 => (0u8..7, 0u64..250).prop_map(|(kind, bytes)| Op::Head { kind, bytes }),
        2 => (0u8..7, 0u8..7, any::<u8>(), any::<u8>())
            .prop_map(|(from, to, num, num2)| Op::Transfer { from, to, num, num2 }),
        3 => (0u64..700).prop_map(Op::Advance),
        1 => any::<u8>().prop_map(Op::Cancel),
        1 => (0u64..400).prop_map(Op::SetExternal),
        1 => Just(Op::Refresh),
        1 => (any::<bool>(), proptest::option::of(1u64..900))
            .prop_map(|(wait, timeout_ms)| Op::Reconf { wait, timeout_ms }),
    ]
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum St {
    Pending,
    Granted,
    TimedOut,
    Cancelled,
}

struct MW {
    id: usize,
    kind: usize,
    bytes: u64,
    deadline: Option<u64>,
    st: St,
}

struct Model {
    held: [u64; 7],
    q: Vec<MW>,
    all: Vec<St>,
    wait: bool,
    timeout: Option<u64>,
    now: u64,
    ext: u64,
    headroom: u64,
}

impl Model {
    fn used(&self) -> u64 {
        self.held.iter().sum::<u64>() - self.held[HEADROOM_IDX]
    }
    fn limit(&self) -> u64 {
        CAP.saturating_sub(self.ext).saturating_sub(self.headroom)
    }
    fn fits(&self, b: u64) -> bool {
        self.used() + b <= self.limit()
    }
    fn set(&mut self, id: usize, st: St) {
        self.all[id] = st;
    }
    fn prune_front(&mut self) {
        while self.q.first().is_some_and(|w| w.st != St::Pending) {
            self.q.remove(0);
        }
    }
    fn grant(&mut self) {
        loop {
            self.prune_front();
            let Some(f) = self.q.first() else { return };
            if !self.fits(f.bytes) {
                return;
            }
            let w = self.q.remove(0);
            self.held[w.kind] += w.bytes;
            self.set(w.id, St::Granted);
        }
    }
    fn tick(&mut self) -> usize {
        let now = self.now;
        let mut n = 0;
        let mut ids = vec![];
        for w in &mut self.q {
            if w.st == St::Pending && w.deadline.is_some_and(|d| d <= now) {
                w.st = St::TimedOut;
                ids.push(w.id);
                n += 1;
            }
        }
        for id in ids {
            self.set(id, St::TimedOut);
        }
        self.q.retain(|w| w.st == St::Pending);
        self.grant();
        n
    }
}

fn kind(i: u8) -> SpaceKind {
    SpaceKind::ALL[usize::from(i) % 7]
}

fn run_case(headroom: u64, wait: bool, timeout_ms: Option<u64>, ops: &[Op]) {
    let ck = Arc::new(ManualClock::new(lunix_format::TimeMs(0)));
    let on_full = if wait { OnFull::Wait } else { OnFull::Error };
    let s = common::settings(headroom, on_full, timeout_ms.map(Duration::from_millis));
    let (g, fs): (Arc<LunixSpace>, Arc<SimFs>) = common::gate(CAP, s, ck.clone());
    let mut m = Model {
        held: [0; 7],
        q: vec![],
        all: vec![],
        wait,
        timeout: timeout_ms,
        now: 0,
        ext: 0,
        headroom,
    };
    let mut waiters: Vec<Waiter> = vec![];

    for o in ops {
        match o {
            Op::Reserve { kind: k, bytes } => {
                let k = kind(*k);
                if k == SpaceKind::Headroom {
                    continue;
                }
                let got = g.reserve(k, *bytes, None).expect("reserve");
                // The model.
                let want: u8 = if *bytes == 0 {
                    0
                } else {
                    let limit = m.limit();
                    if m.used() + bytes >= limit - limit / 20 {
                        m.grant();
                    }
                    m.prune_front();
                    if (m.q.is_empty() || !m.wait) && m.fits(*bytes) {
                        m.held[k.index()] += bytes;
                        0
                    } else if !m.wait {
                        1
                    } else {
                        let id = m.all.len();
                        m.all.push(St::Pending);
                        m.q.push(MW {
                            id,
                            kind: k.index(),
                            bytes: *bytes,
                            deadline: m.timeout.map(|t| m.now + t),
                            st: St::Pending,
                        });
                        2
                    }
                };
                match (want, got) {
                    (0, ReserveOutcome::Reserved) | (1, ReserveOutcome::NoSpace) => {}
                    (2, ReserveOutcome::Parked(w)) => waiters.push(w),
                    (w, g) => panic!("model says {w}, gate says {g:?} for {o:?}"),
                }
            }
            Op::Release { kind: k, num } => {
                let k = kind(*k);
                let b = m.held[k.index()] * u64::from(*num) / 255;
                g.release(k, b);
                m.held[k.index()] -= b;
                m.grant();
            }
            Op::Head { kind: k, bytes } => {
                let k = kind(*k);
                let got = g.reserve_headroom(k, *bytes);
                let ceiling = m.limit() + m.headroom;
                let ok = (k != SpaceKind::Headroom || m.held[HEADROOM_IDX] + bytes <= m.headroom)
                    && m.used() + m.held[HEADROOM_IDX] + bytes <= ceiling;
                assert_eq!(got.is_ok(), ok, "{o:?}");
                if ok {
                    m.held[k.index()] += bytes;
                }
            }
            Op::Transfer {
                from,
                to,
                num,
                num2,
            } => {
                let (f, t) = (kind(*from), kind(*to));
                if t == SpaceKind::Headroom && f != t {
                    // Not part of the protocol: bounds enter Headroom only
                    // through `reserve_headroom`, which caps them.
                    continue;
                }
                let fb = m.held[f.index()] * u64::from(*num) / 255;
                let tb = fb * u64::from(*num2) / 255;
                g.transfer(f, t, fb, tb).expect("transfer");
                m.held[f.index()] -= fb;
                m.held[t.index()] += tb;
                m.grant();
            }
            Op::Advance(ms) => {
                ck.advance(Duration::from_millis(*ms));
                m.now += ms;
                let got = g.tick();
                assert_eq!(got, m.tick(), "tick count");
            }
            Op::Cancel(i) => {
                if waiters.is_empty() {
                    continue;
                }
                let i = usize::from(*i) % waiters.len();
                let won = waiters[i].cancel();
                assert_eq!(won, m.all[i] == St::Pending);
                if won {
                    m.set(i, St::Cancelled);
                    for w in &mut m.q {
                        if w.id == i {
                            w.st = St::Cancelled;
                        }
                    }
                }
            }
            Op::SetExternal(e) => {
                // Only shrink the limit to where the invariant still holds.
                let limit2 = CAP.saturating_sub(*e).saturating_sub(headroom);
                if limit2 < m.used() || limit2 + headroom < m.used() + m.held[HEADROOM_IDX] {
                    continue;
                }
                fs.set_external(*e);
                m.ext = *e;
                g.refresh();
                m.grant();
            }
            Op::Refresh => {
                g.refresh();
                m.grant();
            }
            Op::Reconf {
                wait: w,
                timeout_ms: t,
            } => {
                let on_full = if *w { OnFull::Wait } else { OnFull::Error };
                g.reconfigure(&common::settings(
                    headroom,
                    on_full,
                    t.map(Duration::from_millis),
                ))
                .expect("reconfigure");
                m.wait = *w;
                m.timeout = *t;
                m.grant();
            }
        }
        check(&g, &m, &waiters);
    }

    // Drain: everything returns and the ledger is exactly zero.
    for w in &waiters {
        w.cancel();
    }
    for _ in 0..3 {
        for k in SpaceKind::ALL {
            let h = g.snapshot().by_kind[k.index()];
            g.release(k, h);
        }
    }
    let snap = g.snapshot();
    assert_eq!(snap.by_kind, [0; 7], "ledger is exactly zero");
    assert_eq!(snap.waiters, 0);
}

fn check(g: &LunixSpace, m: &Model, waiters: &[Waiter]) {
    let snap = g.snapshot();
    assert_eq!(snap.by_kind, m.held, "held per kind");
    assert_eq!(snap.limit, m.limit(), "limit");
    assert_eq!(
        snap.waiters,
        m.q.iter().filter(|w| w.st == St::Pending).count() as u64
    );
    // `used` never exceeds the limit by more than the reserved region, even
    // counting the bounds still to be transferred.
    assert!(
        snap.used() + snap.by_kind[HEADROOM_IDX] <= snap.limit + snap.headroom_total,
        "used {} + bounds {} > limit {} + region {}",
        snap.used(),
        snap.by_kind[HEADROOM_IDX],
        snap.limit,
        snap.headroom_total
    );
    for (i, w) in waiters.iter().enumerate() {
        let want = match m.all[i] {
            St::Pending => WaiterState::Pending,
            St::Granted => WaiterState::Granted,
            St::TimedOut => WaiterState::TimedOut,
            St::Cancelled => WaiterState::Cancelled,
        };
        assert_eq!(w.state(), want, "waiter {i}");
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 5000, ..ProptestConfig::default() })]

    #[test]
    fn the_gate_equals_the_reference_model(
        headroom in 0u64..250,
        wait in any::<bool>(),
        timeout_ms in proptest::option::of(1u64..900),
        ops in proptest::collection::vec(op(), 1..70),
    ) {
        run_case(headroom, wait, timeout_ms, &ops);
    }
}

#[test]
fn waiters_are_granted_in_arrival_order_whatever_their_size() {
    let ck = Arc::new(ManualClock::new(lunix_format::TimeMs(0)));
    let (g, _fs) = common::gate(CAP, common::settings(0, OnFull::Wait, None), ck);
    let k = SpaceKind::Segments;
    assert!(matches!(
        g.reserve(k, CAP, None).unwrap(),
        ReserveOutcome::Reserved
    ));
    let sizes = [300u64, 10, 500, 5, 200, 1];
    let ws: Vec<Waiter> = sizes
        .iter()
        .map(|s| match g.reserve(k, *s, None).unwrap() {
            ReserveOutcome::Parked(w) => w,
            o => panic!("{o:?}"),
        })
        .collect();
    // Release one byte at a time in big steps; after every step the granted
    // ones are a prefix of the arrival order.
    for _ in 0..40 {
        g.release(k, 50.min(g.snapshot().by_kind[k.index()]));
        let states: Vec<bool> = ws
            .iter()
            .map(|w| w.state() == WaiterState::Granted)
            .collect();
        let first_pending = states.iter().position(|g| !g).unwrap_or(states.len());
        assert!(states[first_pending..].iter().all(|g| !g), "{states:?}");
    }
}

#[test]
fn full_timeout_fires_within_50ms_of_its_deadline() {
    for late in [0u64, 1, 25, 49] {
        let ck = Arc::new(ManualClock::new(lunix_format::TimeMs(0)));
        let s = common::settings(0, OnFull::Wait, Some(Duration::from_millis(700)));
        let (g, _fs) = common::gate(100, s, ck.clone());
        let k = SpaceKind::Segments;
        g.reserve(k, 100, None).unwrap();
        let ReserveOutcome::Parked(w) = g.reserve(k, 1, None).unwrap() else {
            panic!("parked")
        };
        ck.advance(Duration::from_millis(699));
        assert_eq!(g.tick(), 0);
        ck.advance(Duration::from_millis(1 + late));
        assert_eq!(g.tick(), 1);
        assert_eq!(w.state(), WaiterState::TimedOut);
    }
}

#[test]
fn a_million_reserve_release_pairs_leave_the_ledger_at_zero() {
    let ck = Arc::new(ManualClock::new(lunix_format::TimeMs(0)));
    let (g, _fs) = common::gate(1 << 40, common::settings(1 << 20, OnFull::Wait, None), ck);
    let kinds = [
        SpaceKind::JournalReserved,
        SpaceKind::Segments,
        SpaceKind::Map,
        SpaceKind::Index,
        SpaceKind::Commits,
        SpaceKind::Cache,
    ];
    let mut x = 0x9e37_79b9_7f4a_7c15_u64;
    for i in 0..1_000_000u64 {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        let k = kinds[(x % 6) as usize];
        let bytes = 64 + (x >> 8) % (1 << 20);
        let got = g.reserve(k, bytes, None).unwrap();
        assert!(matches!(got, ReserveOutcome::Reserved), "pair {i}");
        if i % 5 == 0 {
            // Some pairs go through a transfer and a true-up.
            g.transfer(k, SpaceKind::Segments, bytes, bytes / 2 + 1)
                .unwrap();
            g.release(SpaceKind::Segments, bytes / 2 + 1);
        } else {
            g.release(k, bytes);
        }
    }
    let snap = g.snapshot();
    assert_eq!(snap.by_kind, [0; 7]);
    assert_eq!(snap.waiters, 0);
}
