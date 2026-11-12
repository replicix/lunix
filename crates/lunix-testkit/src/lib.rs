//! In-memory fakes and oracles for the Lunix traits (dev-dependency only).
//!
//! Part 1 (M1.B2a):
//!
//! | item | stands in for | notes |
//! |---|---|---|
//! | [`PlainCodec`] | `lunix-crypto` | all three crypto traits, no encryption; BLAKE3 ids and tags, raw, LZ4 and zstd |
//! | [`FlatOverlay`] | `lunix-overlay` | per-block, also the oracle for M4 |
//! | [`FlatMap`] (+ [`MemPacks`]) | `lunix-map` | flat leaves, the oracle for M5 |
//! | [`MemSpace`] | `lunix-space` | ledger, FIFO waiters, ladder, manual-clock timeouts |
//! | [`strategies`] | | proptest op sequences for I/O, overlay and map |
//!
//! Part 2 (M1.B2b):
//!
//! | item | stands in for | notes |
//! |---|---|---|
//! | [`MemJournal`] | `lunix-journal` | floor-based recovery, lanes, group flush keyed on `upto`, poisoning, crash model |
//! | [`MemIndex`] | `lunix-index` | memtable, runs, condemn, relocate, catch-up, rebuild |
//! | [`MemStore`] | `lunix-store` | segments, packs, commits, checkpoint, pins, GC state, volume dir, fault points |
//! | [`MemBlockEngine`] | `lunix-engine` | I/O semantics over a sparse device; immediate, deferred or mixed completions |
//! | [`DeviceOracle`], [`OracleLog`] | | flat reference device; prefix-consistency check after a crash |
//! | [`FakeCtlServer`] | `lunixd` | control-protocol server on a Unix socket for M13 |
//!
//! Every fake here passes the matching `lunix_iface::contract` suite; a fake
//! that differs from its documented contract is a bug.

#![forbid(unsafe_code)]

pub mod block;
pub mod ctl;
pub mod faults;
pub mod index;
pub mod journal;
pub mod map;
pub mod oracle;
pub mod overlay;
pub mod packs;
pub mod plain;
pub mod space;
pub mod store;
pub mod strategies;

pub use block::{
    run_request, CompletionMode, MemBlockEngine, MemEngineConfig, MemEngineCounters, TestCompleter,
};
pub use ctl::FakeCtlServer;
pub use faults::{Fault, Faults};
pub use index::{IndexImage, MemIndex};
pub use journal::{CrashModel, JournalImage, MemJournal, MemJournalConfig};
pub use map::FlatMap;
pub use oracle::{Applied, DeviceOracle, LogOp, OracleLog};
pub use overlay::{coalesce, FlatOverlay};
pub use packs::MemPacks;
pub use plain::PlainCodec;
pub use space::{MemSpace, MemSpaceConfig};
pub use store::{tails_usable, CheckpointHint, MemIds, MemStore, MemStoreConfig, StoreImage};
