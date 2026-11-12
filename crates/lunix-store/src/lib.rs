//! Volume directory, segments, map packs, commit and checkpoint files (M7).
//!
//! [`LocalVolume`] is the one implementation of every store trait in
//! `lunix_iface::store`: it owns the volume directory and its process lock and
//! hands out segment and pack writers, the commit, checkpoint, pin and GC
//! state stores, and the object-id allocator. Nothing here is cryptographic:
//! extents are opaque, already-sealed byte strings (the caller verifies the tag
//! through its `ExtentCodec`), and footers, pins and GC evidence go through the
//! `MetaCodec` the volume was opened with.
//!
//! # Durable order
//!
//! Every object is made visible the same way: write a temp name (`*.tmp`, or
//! `<seg>.open` for a container being appended to), `fdatasync` it, rename it
//! into place (create-only objects with `RENAME_NOREPLACE`), then `fsync` the
//! directory. A delete is `unlink` then directory `fsync`. Each step has a
//! named crash point (`store.segment.*`, `store.pack.*`, `store.commit.*`,
//! `store.checkpoint.*`, `store.evidence.*`, `store.state.*`,
//! `store.delete.*`, `store.leftover.*`, `store.set_aside.*`); a crash leaves
//! the old state or the new one, never a mix.
//!
//! # Locking
//!
//! `LOCK` is a `flock(2)` lock. The kernel drops it when its holder dies, so a
//! lock left by a `kill -9` is free to take: that is the stale-lock takeover.
//! A live holder gives a second opener `Busy`.

mod checkpoint;
mod commits;
mod container;
mod fsops;
mod gcstate;
mod volume;

#[cfg(test)]
mod tests;

pub use checkpoint::{tails_usable, CheckpointHint};
pub use fsops::{FsOps, RealFs};
pub use volume::{read_volume_file, IdCounter, LocalVolume, StoreOptions, SEGMENT_TARGET};
