//! The radix Merkle block map (DESIGN section 7, ADR-8).
//!
//! # Threads and blocking
//!
//! * [`BlockMap::try_get_cached`] is **non-blocking** (queue threads).
//! * [`BlockMap::get`], `begin_update`, `seal`, `grow` and the walks
//!   **block** (read helpers, destage, the coordinator, GC, fsck).
//!
//! The node cache and the dirty-leaf overlay are the implementation's; the
//! contract is the content: `get(ci)` returns the newest committed entry, and a
//! root equals the root of any other map holding the same content, whatever the
//! pack locations (the node hash covers child hashes and level, never
//! locations).
//!
//! Nodes are read through [`NodeSource`] and sealed or opened through
//! [`ExtentCodec`](crate::crypto::ExtentCodec); both are constructor
//! arguments of the implementation, so `open(root)` is its constructor and not
//! a trait method.

use std::collections::HashSet;

use lunix_format::commit::MapRoot;
use lunix_format::node::{DataEntry, NodeLoc};
use lunix_format::{Hint, PackId};

use crate::error::Result;
use crate::store::PackRead;
use crate::store::PackWrite;

/// A chunk number: `byte_offset / chunk_size`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct ChunkNo(pub u64);

/// The hash of a map node.
pub type NodeHash = [u8; 32];

/// The hint a map entry carries.
pub fn entry_hint(e: &DataEntry) -> Hint {
    Hint {
        seg: e.seg,
        off: e.off,
        clen: e.clen,
    }
}

/// Where map nodes are read from. Implemented for every [`PackRead`].
pub trait NodeSource: Send + Sync {
    /// Appends the stored bytes of the node extent at `loc` to `out`.
    fn load_node(&self, loc: NodeLoc, out: &mut Vec<u8>) -> Result<()>;
}

impl<T: PackRead + ?Sized> NodeSource for T {
    fn load_node(&self, loc: NodeLoc, out: &mut Vec<u8>) -> Result<()> {
        self.read_node(loc, out)
    }
}

/// The answer of a non-blocking lookup.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cached {
    /// The leaf is resident and the chunk is stored here.
    Hit(DataEntry),
    /// The leaf is resident and the chunk is a hole (reads as zeros).
    Hole,
    /// The leaf is not resident: go to a read helper and call `get`.
    Miss,
}

/// One node as the walks report it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct NodeRef {
    /// The node hash.
    pub hash: NodeHash,
    /// 0 for a leaf.
    pub level: u8,
    /// Where it is stored.
    pub loc: NodeLoc,
}

/// What [`BlockMap::seal`] produced.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SealResult {
    /// The new root, to be named by the FULL commit's `map_root`.
    pub root: MapRoot,
    /// Packs the nodes were written to (at most one in phase 1; empty when
    /// nothing was dirty). The caller seals the pack and names it in
    /// `map_packs[]`.
    pub new_packs: Vec<PackId>,
    /// Dirty leaves that were written.
    pub dirty_leaves: u64,
    /// Extent bytes appended to the pack (the amount to charge to `Map`).
    pub bytes: u64,
}

/// What committing a [`MapTxn`] did.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct TxnReport {
    /// `set` and `set_hole` calls applied.
    pub sets: u64,
    /// `rehint` calls that found the old hint and applied.
    pub rehints_applied: u64,
    /// `rehint` calls skipped because a newer write changed the entry.
    pub rehints_skipped: u64,
}

/// A batch of updates, applied atomically on `commit` and discarded on drop.
/// Only one is open at a time.
pub trait MapTxn: Send {
    /// Sets chunk `ci`.
    ///
    /// # Errors
    /// `MapFull` when the dirty-leaf cap would be exceeded (drop the
    /// transaction, `seal`, retry); `Invalid` past the map's size.
    fn set(&mut self, ci: ChunkNo, entry: DataEntry) -> Result<()>;

    /// Makes chunk `ci` a hole.
    ///
    /// # Errors
    /// As for `set`.
    fn set_hole(&mut self, ci: ChunkNo) -> Result<()>;

    /// Compaction's `moved` record: replaces the entry's hint with `new` only
    /// if the entry **at commit time** still has hint `old` (a newer write
    /// wins). Never `MapFull` for a leaf the transaction already dirtied; may
    /// be for a new one.
    fn rehint(&mut self, ci: ChunkNo, old: Hint, new: Hint) -> Result<()>;

    /// Applies everything at once.
    fn commit(self: Box<Self>) -> Result<TxnReport>;
}

/// The map. `Send + Sync`.
pub trait BlockMap: Send + Sync {
    /// The map's size in chunks.
    fn chunk_count(&self) -> u64;

    /// The newest entry of `ci` (dirty overlay first), `None` for a hole.
    /// Blocking (may load nodes).
    fn get(&self, ci: ChunkNo) -> Result<Option<DataEntry>>;

    /// Like `get` but answers only from resident leaves. Non-blocking.
    fn try_get_cached(&self, ci: ChunkNo) -> Cached;

    /// Starts a transaction.
    ///
    /// # Errors
    /// `Busy` while another is open; `MapFull` when the cap is already hit.
    fn begin_update(&self) -> Result<Box<dyn MapTxn + '_>>;

    /// Writes the dirty leaves and the interior nodes above them into `packs`,
    /// recomputes hashes up the paths, and returns the new root. The map then
    /// reads from the new root; the dirty set is empty. The caller seals the
    /// pack (C5) before naming the root in a commit.
    fn seal(&self, packs: &mut dyn PackWrite) -> Result<SealResult>;

    /// Grows the map to `new_chunk_count` chunks (the new total), wrapping the
    /// old root as child 0 of new roots when the depth grows. Unchanged
    /// subtrees keep their hashes.
    ///
    /// # Errors
    /// `Invalid` when shrinking.
    fn grow(&self, new_chunk_count: u64) -> Result<()>;

    /// The root as of the last `seal` (or `open`).
    fn root(&self) -> MapRoot;

    /// Dirty leaves not yet sealed.
    fn dirty_leaf_count(&self) -> u64;

    /// Read-only traversal of **any** root (GC, fsck): calls `f` for every
    /// non-hole entry, holes omitted, in ascending chunk order.
    fn walk_leaves(&self, root: &MapRoot, f: &mut dyn FnMut(ChunkNo, &DataEntry)) -> Result<()> {
        self.walk_leaves_skipping(root, &HashSet::new(), f)
    }

    /// Like `walk_leaves` but does not descend into subtrees whose node hash is
    /// in `skip` (GC's mark skips what an already-walked root shares; the set
    /// comes from `walk_nodes` of that root).
    fn walk_leaves_skipping(
        &self,
        root: &MapRoot,
        skip: &HashSet<NodeHash>,
        f: &mut dyn FnMut(ChunkNo, &DataEntry),
    ) -> Result<()>;

    /// Read-only traversal of **any** root: calls `f` once per stored node,
    /// **including the root node itself** (its pack is a live pack).
    fn walk_nodes(&self, root: &MapRoot, f: &mut dyn FnMut(&NodeRef)) -> Result<()>;

    /// Like `walk_nodes` but does not descend into, or report, subtrees whose
    /// node hash is in `skip`, so two roots can be walked for their symmetric
    /// difference.
    fn walk_nodes_skipping(
        &self,
        root: &MapRoot,
        skip: &HashSet<NodeHash>,
        f: &mut dyn FnMut(&NodeRef),
    ) -> Result<()>;
}
