//! `MemSpace` passes the space contract suite.

use std::sync::Arc;

use lunix_format::local::OnFull;
use lunix_iface::clock::ManualClock;
use lunix_iface::contract::space::SpaceSetup;
use lunix_testkit::{MemSpace, MemSpaceConfig};

fn gate(setup: SpaceSetup, clock: Arc<ManualClock>) -> Arc<MemSpace> {
    let mut cfg = MemSpaceConfig::new(setup.capacity).headroom(setup.headroom);
    if setup.on_full == OnFull::Wait {
        cfg = cfg.wait(setup.full_timeout);
    }
    MemSpace::new(cfg, clock)
}

lunix_iface::contract::space!(gate);
