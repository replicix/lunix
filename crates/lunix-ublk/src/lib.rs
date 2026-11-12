//! Raw ublk control ring, queues, recovery, QUIESCE, TRY_STOP and resize
//! (milestone M10; DESIGN §3, ADR-16, ADR-17, ADR-18, ADR-20).
//!
//! The layer sits between the kernel's ublk driver and a
//! [`lunix_iface::block::BlockEngine`]. It owns tags, buffers and the
//! `io_uring`s; the engine owns I/O semantics.
//!
//! * [`ctl`]: the raw control ring. libublk 0.4.8 cannot set four control
//!   flags and has no `TRY_STOP_DEV`, so every control command goes through
//!   here, each under a watchdog ([`watchdog`]).
//! * [`features`]: the `GET_FEATURES` mask, the mandatory flags (kernel 6.16
//!   or newer) and the flag-set rules found in phase 0.
//! * [`config`]: [`UblkConfig`] and the `SET_PARAMS` payload of DESIGN §3.
//! * [`session`]: the command sequences (add, recover, resize, stop,
//!   quiesce) over the [`ctl::ControlPlane`] trait, unit-tested with a fake.
//! * [`identity`]: the checks before a device is recovered.
//! * [`device`]: [`UblkDevice`], the attached device and its queue threads.
//!
//! # Safety rules (phase 0, W1 to W7)
//!
//! * **There is no stop command.** [`ctl::CtlOp`] has no `STOP_DEV`, and
//!   libublk's `run_target()` (which sends one when its queue threads end,
//!   and deadlocked the kernel after a quiesce) is not used. A unit test
//!   scans the sources for either.
//! * After [`UblkDevice::quiesce`] the daemon exits ([`Quiesced::exit`]).
//! * Detach is `TRY_STOP_DEV` ([`UblkDevice::detach`]); it is `Busy` while
//!   the device is mounted.
//! * A fatal error in a queue thread, a panic, or a request completed twice
//!   calls `std::process::abort()` (like `kill -9`, recoverable with
//!   `REISSUE`). Nothing exits with started requests in flight.
//! * Every control command has a watchdog (30 s, `DEL_DEV` 60 s); a timeout
//!   is reported and nothing is killed or retried.
//! * `BATCH_IO` is never used; the copy path is the default and `USER_COPY`
//!   is opt-in.

pub mod config;
pub mod ctl;
pub mod device;
pub mod error;
pub mod features;
pub mod harden;
pub mod identity;
pub mod queue;
pub mod session;
pub mod watchdog;

pub use config::{
    build_params, DataPath, RecoveryPolicy, Timeouts, UblkConfig, BLOCK_BYTES, DEFAULT_DEPTH,
    DEFAULT_IO_BUF_BYTES, MIN_IO_BUF_BYTES,
};
pub use ctl::{ControlPlane, CtlOp, DevInfo, Params, RingControl};
pub use device::{Quiesced, UblkDevice};
pub use error::{Errno, Result, SizeError, UblkError};
pub use features::{check_flag_rules, flag, Features, KERNEL_FLOOR};
pub use harden::{harden_process, Harden};
pub use identity::{check_identity, current_boot_id, IdentityMismatch, KernelView};
pub use queue::{fatal, QueueCounters, QueueSnapshot};
pub use session::{SizeChange, SizeGuard};
