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
//!
//! Limitation: a killed holder whose last thread is stuck in the kernel (an
//! unkillable zombie) keeps its file table, so the lock stays held and every
//! open gets `Busy`. That is the safe direction (no takeover on a timeout);
//! telling such a holder from a live one needs the daemon's control socket, so
//! it is left to M12. `Error::Busy` cannot carry the holder's pid, so
//! [`lock_holder_pid`] reads it from `LOCK` for the caller's message.
//!
//! # Object ids
//!
//! The allocator starts above three inputs (on-disk-formats §3, D17): the
//! highest id of any segment, pack or index-run file present (`open` scans
//! `segments/`, `map/`, `index/`), the checkpoint's `id_high` (`open` reads
//! it), and the highest id the commit chain names (the caller feeds that
//! through `ids().observe()`). The disk scan alone is not enough: an `.open`
//! file's directory entry is never synced, so after a power cut it can vanish
//! and its id be reissued. That is safe because nothing durable names an
//! unsealed file (index inserts and commits come after the seal).
//!
//! # Temporary files and the sweep
//!
//! Every temporary name ends in `.tmp` and every unsealed container in
//! `.open`. `sweep_orphans` and `remove_leftovers` delete both anywhere below
//! the volume directory except `cache/`, including `journal/` and `index/`, so
//! other crates must use that suffix for scratch files and the engine must run
//! the sweep before they start writing. Files of a writer open in this process
//! are skipped. Run the sweep at open, before `catch_up`.

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
pub use volume::{
    lock_holder_pid, read_volume_file, IdCounter, LocalVolume, StoreOptions, SEGMENT_TARGET,
};
