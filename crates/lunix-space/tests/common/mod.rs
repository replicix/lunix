//! Shared builders for the `lunix-space` tests.
#![allow(dead_code)]

use std::sync::Arc;
use std::time::Duration;

use lunix_format::local::{FreeFloor, OnFull, SpaceSettings};
use lunix_format::TimeMs;
use lunix_iface::clock::{Clock, ManualClock};
use lunix_space::{FsProbe, LunixSpace, ManualFs, SimFs, SpaceConfig};

pub fn clock() -> Arc<ManualClock> {
    Arc::new(ManualClock::new(TimeMs(0)))
}

/// Settings with a zero floor, so that `capacity` bytes of file system give a
/// limit of `capacity - headroom`.
pub fn settings(headroom: u64, on_full: OnFull, timeout: Option<Duration>) -> SpaceSettings {
    SpaceSettings {
        free_floor: FreeFloor::Bytes(0),
        on_full,
        full_timeout_ms: timeout.map(|t| u64::try_from(t.as_millis()).expect("ms")),
        cache_max: None,
        headroom,
    }
}

/// No segment reserve, no background timers.
pub fn config(settings: SpaceSettings) -> SpaceConfig {
    let mut cfg = SpaceConfig::new(settings);
    cfg.segment_size = 0;
    cfg.background_timers = false;
    cfg
}

/// A gate over a simulated file system of `capacity` bytes that mirrors the
/// ledger (see `SimFs`): the limit is `capacity - headroom` whatever refreshes
/// happen.
pub fn gate(
    capacity: u64,
    settings: SpaceSettings,
    clock: Arc<dyn Clock>,
) -> (Arc<LunixSpace>, Arc<SimFs>) {
    gate_cfg(capacity, 0, config(settings), clock)
}

pub fn gate_cfg(
    capacity: u64,
    external: u64,
    cfg: SpaceConfig,
    clock: Arc<dyn Clock>,
) -> (Arc<LunixSpace>, Arc<SimFs>) {
    let fs = Arc::new(SimFs::new(capacity, external));
    let g = LunixSpace::new(cfg, fs.clone(), clock).expect("gate");
    fs.attach(&g);
    (g, fs)
}

/// A gate over a hand-set file system.
pub fn gate_manual(
    total: u64,
    free: u64,
    settings: SpaceSettings,
    clock: Arc<dyn Clock>,
) -> (Arc<LunixSpace>, Arc<ManualFs>) {
    let fs = Arc::new(ManualFs::new(total, free));
    let probe: Arc<dyn FsProbe> = fs.clone();
    let g = LunixSpace::new(config(settings), probe, clock).expect("gate");
    (g, fs)
}
