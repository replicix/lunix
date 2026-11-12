//! Traits and shared types between Lunix crates, the control protocol and crashpoints.
//!
//! This crate is the seam of the workspace: every implementation crate depends
//! on it and on `lunix-format`, never on each other, and every fake in
//! `lunix-testkit` implements the same traits. It contains **no** implementation
//! of any trait and depends on no implementation crate.
//!
//! | module | what | implemented by |
//! |---|---|---|
//! | [`block`] | `BlockEngine`, `QueueHandler`, `Completer` | `lunix-engine`; consumed by `lunix-ublk` |
//! | [`journal`] | `Journal`, `JournalLane`, group flush, recovery | `lunix-journal` |
//! | [`overlay`] | `Overlay`, cuts | `lunix-overlay` |
//! | [`map`] | `BlockMap`, `MapTxn`, `NodeSource` | `lunix-map` |
//! | [`index`] | `ChunkIndex` | `lunix-index` |
//! | [`store`] | segments, packs, commits, checkpoint, pins, GC state, volume dir, ids | `lunix-store` |
//! | [`space`] | `SpaceGate`, `Ladder`, `Waiter` (with the accounting walk-through) | `lunix-space` |
//! | [`crypto`] | `ExtentCodec`, `MetaCodec`, `CommitCodec`, `Keys` | `lunix-crypto` |
//! | [`gc`] | `Collector`, `GcView`, `GcControl` | `lunix-gc` (and engine wiring) |
//! | [`ctl`] | control-protocol frames and messages | `lunixd`, `lunix-cli` |
//! | [`crashpoint`] | the `crashpoint!` macro and registry | every crate |
//! | [`contract`] | contract-test macros each implementation runs | tests |
//!
//! # Conventions
//!
//! * Every trait item says which thread may call it and whether it blocks.
//!   Three classes: **queue-thread** (non-blocking: no I/O, no waiting, no
//!   tokio; ADR-18), **worker** (may block on I/O; destage workers, read
//!   helpers, the coordinator) and **control** (startup, shutdown, control
//!   socket).
//! * Errors are the shared [`Error`]; implementations map their own failures
//!   onto it so callers decide by [`ErrorKind`].
//! * Sizes, offsets and sequence numbers use the `lunix-format` newtypes.
//! * `write_seq` and `commit_seq` are distinct types (D5).

#![forbid(unsafe_code)]

pub mod block;
pub mod clock;
pub mod contract;
pub mod crashpoint;
pub mod crypto;
pub mod ctl;
pub mod error;
pub mod gc;
pub mod index;
pub mod journal;
pub mod map;
pub mod overlay;
pub mod space;
pub mod stats;
pub mod store;
pub mod wake;

pub use error::{Error, ErrorKind, Result};
