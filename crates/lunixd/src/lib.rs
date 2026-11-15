//! The Lunix daemon: config, registry, control server, wiring and signals
//! (plan M12a; DESIGN §2, §3, §4.4, §16).
//!
//! | module | what |
//! |---|---|
//! | [`config`] | `/etc/lunix/lunixd.toml` |
//! | [`registry`] | `<state_dir>/volumes.json` (D8) |
//! | [`stack`] | the real crates wired into one volume: create (descriptor, keyring, genesis) and open (attach check, store, journal, overlay, map, index, space, destage, engine) |
//! | [`device`] | the ublk device (or none) in front of an engine |
//! | [`daemon`] | the volumes and the control-call handlers |
//! | [`server`] | the control socket and the signals |
//! | [`audit`], [`log`] | the audit log and the stderr subscriber |
//! | [`trim`] | idle heap trimming (the memory gate after a workload) |
//! | [`check`] | `lunixd check`, the offline store self-check (stand-in for `lunix fsck` until M14) |
//!
//! # Threads
//!
//! The tokio runtime runs only the control server and the signal handlers;
//! every handler is blocking work on its own thread. Queue threads are
//! `lunix-ublk`'s, the coordinator and the read helpers the engine's, the
//! destage pool M9's.

#![deny(unsafe_code)]

pub mod audit;
pub mod check;
pub mod config;
pub mod daemon;
pub mod device;
pub mod fsio;
pub mod log;
pub mod registry;
pub mod server;
pub mod stack;
pub mod trim;

#[cfg(feature = "testsupport")]
pub mod testsupport;

use std::sync::{Mutex, MutexGuard};

/// Locks `m`, taking the value of a poisoned mutex (a panicking holder does
/// not make the volume unusable for the rest of the daemon).
pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The crash points of the build: the registry when built with the
/// `crashpoints` feature, otherwise empty.
pub fn crash_points() -> Vec<&'static str> {
    #[cfg(feature = "crashpoints")]
    {
        lunix_iface::crashpoint::registered()
    }
    #[cfg(not(feature = "crashpoints"))]
    {
        Vec::new()
    }
}
