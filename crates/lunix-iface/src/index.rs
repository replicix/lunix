//! The chunk index: chunk id to the extent that currently holds it (DESIGN
//! section 7 "Chunk index", ADR-14), an LSM of checksummed runs plus a memtable.
//!
//! # Threads and blocking
//!
//! Every method is **blocking-capable** (it may read a run) and thread-safe;
//! none is called from a queue thread. Lookups come from destage workers, the
//! read helper pool and GC; updates from the destage caller and the commit
//! coordinator.
//!
//! # Trust
//!
//! Index runs are checksummed, not authenticated (D17). A hit from a persisted
//! run is [`Confirm::Hinted`]: the caller must read the extent at the hint,
//! compare all 32 bytes of the chunk id and verify the tag or AEAD before using
//! it; an unconfirmed hit is a miss. A hit from the in-process table built from
//! verified footers is [`Confirm::Verified`].

use lunix_format::{ChunkId, Hint, IdPrefix, RunId, SegId};

use crate::error::Result;
use crate::store::SegmentFooters;

/// How far an index hit can be trusted.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Confirm {
    /// Built from a verified footer in this process.
    Verified,
    /// Read from a persisted run: confirm before use (D17).
    Hinted,
}

/// A dedup candidate.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DedupHit {
    /// Where the extent is.
    pub hint: Hint,
    /// Whether the caller must confirm it.
    pub confirm: Confirm,
}

/// An index entry with its full id (prefix lookups return candidates; the
/// caller confirms against the extent header).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct IndexedChunk {
    /// The full chunk id.
    pub id: ChunkId,
    /// The current copy.
    pub hint: Hint,
    /// True when the copy's segment is condemned.
    pub condemned: bool,
}

/// A persisted run, as a FULL commit's `index_packs[]` names it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct RunRef {
    /// The run id (`index/<run:016x>`).
    pub id: RunId,
    /// File size (what the ledger charges to `Index`).
    pub size_bytes: u64,
    /// Entries in it.
    pub entries: u64,
}

/// What `catch_up` did.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct CatchUpReport {
    /// Entries inserted from footers the manifest did not cover.
    pub inserted: u64,
    /// Entries dropped because their segment is gone.
    pub dropped: u64,
}

/// The chunk index. `Send + Sync`.
pub trait ChunkIndex: Send + Sync {
    /// The dedup lookup. **Never** returns an entry of a condemned segment, from
    /// the moment of `mark_condemned` (also after a reopen).
    fn get_for_dedup(&self, id: &ChunkId) -> Result<Option<DedupHit>>;

    /// The current copy of `id`, condemned or not (read fallback, GC liveness).
    fn get(&self, id: &ChunkId) -> Result<Option<IndexedChunk>>;

    /// Candidates whose id starts with `prefix`, condemned or not (stale-hint
    /// fallback, DESIGN section 7). The caller confirms each against the full
    /// id. Volumes with `id_len = 0` have no prefix: `Unsupported`.
    fn get_by_prefix(&self, prefix: &IdPrefix) -> Result<Vec<IndexedChunk>>;

    /// Records `id` at `hint`. The caller guarantees the segment is **sealed**;
    /// the entry stays in the memtable until `persist` covers its segment.
    fn insert(&self, id: ChunkId, hint: Hint) -> Result<()>;

    /// Compaction moved `id` from `old` to `new` (newest wins). Returns whether
    /// `old` was the current copy. The entry follows the persist rule: it stays
    /// in the memtable and is not written to a run until `persist` is told the
    /// segment of `new` is named by a durable commit.
    fn relocate(&self, id: &ChunkId, old: &Hint, new: &Hint) -> Result<bool>;

    /// Flags a segment as condemned (persisted in the manifest).
    fn mark_condemned(&self, seg: SegId) -> Result<()>;

    /// Clears the flag (a recheck found a new reference).
    fn unmark_condemned(&self, seg: SegId) -> Result<()>;

    /// The condemned segments.
    fn condemned(&self) -> Result<Vec<SegId>>;

    /// Removes the segment's entries from lookups at once and from runs at the
    /// next run compaction.
    fn drop_segment(&self, seg: SegId) -> Result<()>;

    /// Writes a run holding the memtable entries whose segment id is
    /// `<= upto_seg`, the highest segment named by a durable commit. Entries of
    /// higher segments, relocated ones included, stay in memory. Returns the
    /// runs created (possibly none). The run is named by the **next** FULL
    /// (D3).
    fn persist(&self, upto_seg: SegId) -> Result<Vec<RunRef>>;

    /// The runs the current manifest names.
    fn runs(&self) -> Result<Vec<RunRef>>;

    /// After an unclean stop: inserts the entries of the footers of `segments`
    /// (the ids named by the retained commit chain, never "newer than the
    /// manifest") that the manifest does not cover, and drops manifest entries
    /// whose segment `footers` reports `NotFound`.
    fn catch_up(&self, segments: &[SegId], footers: &dyn SegmentFooters) -> Result<CatchUpReport>;

    /// Rebuilds the whole index from the footers of `segments` (the
    /// `fsck --rebuild-index` engine).
    fn rebuild(&self, segments: &[SegId], footers: &dyn SegmentFooters) -> Result<()>;

    /// Deletes a run file; returns the bytes freed (missing frees 0). The run
    /// must no longer be named by the manifest or a retained FULL.
    fn delete_run(&self, run: RunRef) -> Result<u64>;

    /// Calls `f` for every entry (fsck equality checks, tests). Blocking.
    fn iter_entries(&self, f: &mut dyn FnMut(&IndexedChunk)) -> Result<()>;
}
