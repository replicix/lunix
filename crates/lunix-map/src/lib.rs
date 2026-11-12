//! The radix Merkle block map (DESIGN section 7, ADR-8), implementing
//! [`lunix_iface::map::BlockMap`].
//!
//! # Shape
//!
//! A fixed-radix tree over chunk numbers: `fanout` (256 or 512, the
//! descriptor's `map_fanout`) chunks per leaf, `fanout` children per interior
//! node, `levels = max(2, ceil(log_fanout(chunk_count)))` levels counting the
//! leaves. The shape depends on the chunk number only, so two maps with the
//! same content and size have the same root hash however they were built.
//! Absent subtrees are the empty node of their level (`E_L`, a constant per
//! mode and fanout) with a zero location and are never stored.
//!
//! # Hashes and storage
//!
//! A leaf's hash covers its `rb16` encoding; an interior node's covers its
//! level and child hashes, never the child locations (ADR-8 amendment 1:
//! locations are hints). Hashing goes through
//! [`ExtentCodec::chunk_id`](lunix_iface::crypto::ExtentCodec::chunk_id) with
//! `ExtentKind::Node`, so encrypted volumes get their keyed hash. Nodes are
//! stored as extents of codec 0 (interior nodes are raw, ADR-8 amendment 2),
//! sealed and opened through the same codec, in map packs through
//! [`PackWrite`](lunix_iface::store::PackWrite).
//!
//! Every node read is authenticated twice: the extent tag, and a hash
//! recomputed from the decoded node and compared to the parent's `child_hash`
//! (the root's, to the commit's). A mismatch is `Auth`, a structural fault
//! `Corrupt`; no read returns an entry from an unverified node.
//!
//! # Memory
//!
//! * **Interior nodes** load lazily and stay resident, 24 KiB of children at
//!   fanout 512 on disk and about 28 KiB in memory; 643 of them at 10 TiB.
//! * **The dirty overlay** holds one *decoded* leaf per dirty leaf:
//!   `fanout * size_of::<Entry>()` bytes (32 KiB at fanout 512, measured by
//!   the `entry_is_small_enough` test) against about 11.4 KiB sealed. The cap
//!   ([`MapConfig::dirty_leaf_cap`], default 20 000, so 640 MiB at 512) makes
//!   [`MapTxn::set`](lunix_iface::map::MapTxn::set) return
//!   [`Error::MapFull`](lunix_iface::Error::MapFull) so the engine seals.
//! * **The leaf cache** keeps canonical leaf bytes under a byte budget
//!   ([`MapConfig::map_cache_max`], default 256 MiB), LRU, looked up in
//!   encoded form without decoding. A small decoded tier
//!   ([`MapConfig::decoded_tier_leaves`]) holds leaves that keep being hit and
//!   is charged against the same budget.
//!
//! # Threads
//!
//! Queue threads call only [`BlockMap::try_get_cached`]; it takes one short
//! mutex and never waits for I/O (no lock is held across a read, a seal or a
//! transaction's work). Everything else blocks and is called from the
//! coordinator, read helpers, GC and fsck.
//!
//! [`BlockMap`]: lunix_iface::map::BlockMap
//! [`BlockMap::try_get_cached`]: lunix_iface::map::BlockMap::try_get_cached

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod config;
mod fastleaf;
mod lru;
mod map;
mod txn;

pub use config::{
    MapConfig, DEFAULT_DECODED_TIER_LEAVES, DEFAULT_DIRTY_LEAF_CAP, DEFAULT_MAP_CACHE_MAX,
};
pub use map::{CacheStats, RadixMap};

pub(crate) use lunix_iface::map::NodeHash;
