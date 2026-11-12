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
