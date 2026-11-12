//! In-memory fakes and oracles for the Lunix traits (dev-dependency only).
//!
//! Part 1 (M1.B2a):
//!
//! | item | stands in for | notes |
//! |---|---|---|
//! | [`PlainCodec`] | `lunix-crypto` | all three crypto traits, no encryption, raw codec only |
//! | [`FlatOverlay`] | `lunix-overlay` | per-block, also the oracle for M4 |
//! | [`FlatMap`] (+ [`MemPacks`]) | `lunix-map` | flat leaves, the oracle for M5 |
//! | [`MemSpace`] | `lunix-space` | ledger, FIFO waiters, ladder, manual-clock timeouts |
//! | [`strategies`] | | proptest op sequences for I/O, overlay and map |
//!
//! Part 2 (M1.B2b) adds the journal, index, store, block engine and the device
//! oracle. Every fake here passes the matching `lunix_iface::contract` suite;
//! a fake that differs from its documented contract is a bug.

#![forbid(unsafe_code)]

mod digest;
pub mod map;
pub mod overlay;
pub mod packs;
pub mod plain;
pub mod space;
pub mod strategies;

pub use map::FlatMap;
pub use overlay::{coalesce, FlatOverlay};
pub use packs::MemPacks;
pub use plain::PlainCodec;
pub use space::{MemSpace, MemSpaceConfig};
