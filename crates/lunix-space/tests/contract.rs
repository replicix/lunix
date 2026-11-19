//! The `lunix_iface::contract::space` suite against the real gate.

mod common;

use std::sync::Arc;

use lunix_iface::clock::ManualClock;
use lunix_iface::contract::space::SpaceSetup;
use lunix_space::LunixSpace;

fn make(setup: SpaceSetup, clock: Arc<ManualClock>) -> Arc<LunixSpace> {
    let s = common::settings(setup.headroom, setup.on_full, setup.full_timeout);
    common::gate(setup.capacity, s, clock).0
}

lunix_iface::contract::space!(make);

/// The same suite against a gate with the paced band on: the class case's
/// bulk requests reach `Paced` there (the bucket holds its first megabyte
/// and the clock does not move).
fn make_band(setup: SpaceSetup, clock: Arc<ManualClock>) -> Arc<LunixSpace> {
    let s = common::settings(setup.headroom, setup.on_full, setup.full_timeout);
    let mut cfg = common::config(s);
    cfg.throttle = lunix_space::ThrottleConfig::default();
    common::gate_cfg(setup.capacity, 0, cfg, clock).0
}

lunix_iface::contract::space!(contract_band, make_band);

/// The `Paced` arm of the class case is taken against that gate.
#[test]
fn the_band_gate_paces_in_the_contract_case() {
    use lunix_iface::clock::Clock;
    use lunix_iface::space::{ReserveClass, ReserveOutcome, SpaceGate, SpaceKind};
    let clock = Arc::new(ManualClock::new(lunix_format::TimeMs(0)));
    let g = make_band(
        SpaceSetup {
            capacity: 64 << 20,
            headroom: 0,
            on_full: lunix_format::local::OnFull::Wait,
            full_timeout: None,
        },
        clock.clone(),
    );
    g.charge(SpaceKind::Segments, 56 << 20).unwrap();
    let now = clock.monotonic();
    let first = g
        .reserve_since(SpaceKind::Segments, 1 << 20, None, now, ReserveClass::Bulk)
        .unwrap();
    assert!(matches!(first, ReserveOutcome::Reserved), "{first:?}");
    let second = g
        .reserve_since(SpaceKind::Segments, 1 << 20, None, now, ReserveClass::Bulk)
        .unwrap();
    assert!(matches!(second, ReserveOutcome::Paced(_)), "{second:?}");
}
