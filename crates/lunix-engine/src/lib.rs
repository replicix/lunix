//! The Lunix block engine: I/O handlers, flush groups, cut and commit, startup
//! recovery (plan M11; DESIGN §3.1, §4, §8).
//!
//! The engine implements [`lunix_iface::block::BlockEngine`] over the traits of
//! `lunix-iface` only: it never names an implementation crate. `lunixd` (M12)
//! wires the real journal, overlay, map, index, store, space and crypto crates
//! in through [`EngineDeps`]; the tests here wire the `lunix-testkit` fakes.
//!
//! # Threads
//!
//! * **Queue threads** run [`QueueHandler`](lunix_iface::block::QueueHandler)s
//!   built by [`queue_handler`](lunix_iface::block::BlockEngine::queue_handler).
//!   They never block: every wait parks the tag and resumes from `poll`.
//! * The **coordinator** cuts and commits (C1-C10). It runs on the engine's
//!   background thread when [`EngineConfig::background`] is set, and on the
//!   caller's thread for [`Engine::commit_now`], [`Engine::tick`],
//!   [`grow`](lunix_iface::block::BlockEngine::grow) and
//!   [`quiesce`](lunix_iface::block::BlockEngine::quiesce). One mutex
//!   serialises it, so at most one cut is in progress.
//! * The **read helpers** (a bounded pool) do every map miss and every store
//!   read, then wake the queue.
//!
//! # Invariants
//!
//! * A write completes only after its journal record's `Append` completion
//!   and its overlay insert; a FLUSH or FUA completes only after a group flush
//!   whose `upto` covers every write acknowledged before it (ADR-3, ADR-28).
//! * Every commit is a cut: the coordinator waits (C1) until every write with
//!   `write_seq <= c` is in the overlay, so the overlay cut holds all of them.
//! * Durable order C3 (segments sealed inside the destage) < C5 (map pack) <
//!   C6 (commit) < C7 (floor) < C8 < C9: nothing is named before it is
//!   durable, the rollback floor is never above the durable tip, and the
//!   journal is reclaimed only after the commit covering it is durable.

#![forbid(unsafe_code)]

#[macro_use]
mod points;

mod cache;
mod config;
mod coord;
mod cut;
mod deps;
mod destage;
mod engine;
mod error;
mod flush;
mod full;
mod gc;
mod pressure;
mod queue;
mod read;
mod recover;
mod refs;
mod stats;

pub use config::{CondemnOnCut, EngineConfig, OnJournalDamage};
pub use cut::{CutStats, WriteStats, LATENCY_BUCKETS};
pub use deps::{EngineDeps, EngineHooks, MapOpener, NoHooks, RollbackFloorStore};
pub use destage::{DestageCtx, DestageFailure, DestageOutcome, Destager, Retry, SimpleDestager};
#[cfg(feature = "testsupport")]
pub use engine::CoordHold;
pub use engine::{CommitOutcome, Engine, GcStatusHandle, TipView};
pub use error::OpenError;
pub use pressure::{Pressure, PressureTracker};
#[doc(hidden)]
pub use read::REVALIDATE_MAX;
pub use read::{ChunkReader, Expectation};
pub use recover::RecoveryReport;

/// Names of every crash point of the engine, in protocol order.
pub const CRASH_POINTS: &[&str] = &[
    "engine.c1.waited",
    "engine.c2.cut",
    "engine.c3.destaged",
    "engine.c4.named",
    "engine.c5.map_sealed",
    "engine.c6.committed",
    "engine.c7.checkpoint",
    "engine.c7.floor",
    "engine.c8.index",
    "engine.c9.reclaimed",
    "engine.recover.verified",
    "engine.recover.swept",
    "engine.recover.caught_up",
    "engine.recover.journal",
    "engine.recover.floor",
];
