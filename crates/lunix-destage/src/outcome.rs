//! What a cut returns, and what a failed cut leaves behind.

use std::fmt;

use lunix_format::{ChunkId, Hint, SegId, WriteSeq};
use lunix_iface::index::ChunkIndex;
use lunix_iface::journal::WriteSeqRange;
use lunix_iface::map::TxnReport;
use lunix_iface::store::SealedSegment;
use lunix_iface::{Error, ErrorKind, Result};

use crate::stats::DestageStats;

/// A segment the cut appended to. Every one is sealed: durable, renamed into
/// `segments/`, readable.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct DestagedSegment {
    /// The segment id.
    pub seg: SegId,
    /// The `write_seq` range of the records that contributed data to it
    /// (on-disk-formats item 27).
    pub writes: WriteSeqRange,
    /// Extents in append order: the index inserts the coordinator makes once
    /// the commit that names the segment is durable.
    pub entries: Vec<(ChunkId, Hint)>,
    /// Extent bytes appended (before the footer and trailer).
    pub extent_bytes: u64,
    /// What sealing reported (file size for the ledger's true-up).
    pub sealed: SealedSegment,
}

/// The result of a successful cut.
///
/// By the time [`destage_cut`](crate::destage_cut) returns, every segment the
/// cut appended to is sealed and the map transaction is committed, so every
/// map entry names a readable file: a guest read of any block of the volume
/// through the map works at once, with no window in which an entry points at
/// an `.open` file. What remains for the coordinator (M11):
///
/// * C4 bookkeeping: name [`segments`](Self::segments) in the commit's
///   `new_segments` and true the ledger up with
///   [`sealed_bytes`](Self::sealed_bytes);
/// * [`insert_into_index`](Self::insert_into_index), **only after the commit
///   that names the segments is durable** (a cut that fails later leaves the
///   index untouched, and its segments are orphans for the open-time sweep or
///   GC; nothing durable references them).
pub struct DestageOutcome {
    /// The cut.
    pub cut_seq: WriteSeq,
    /// Every segment the cut appended to, in order, all sealed.
    pub segments: Vec<DestagedSegment>,
    /// What the map transaction did.
    pub map: TxnReport,
    /// The accounting.
    pub stats: DestageStats,
}

impl fmt::Debug for DestageOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DestageOutcome")
            .field("cut_seq", &self.cut_seq)
            .field("segments", &self.segments)
            .field("map", &self.map)
            .field("stats", &self.stats)
            .finish()
    }
}

impl DestageOutcome {
    /// Inserts every extent of the cut into the index. Call it once the commit
    /// that names the segments is durable (see the type's docs).
    ///
    /// # Errors
    /// The index's own errors.
    pub fn insert_into_index(&self, index: &dyn ChunkIndex) -> Result<()> {
        for seg in &self.segments {
            for (id, hint) in &seg.entries {
                index.insert(*id, *hint)?;
            }
        }
        Ok(())
    }

    /// The sealed segments' file bytes, for the ledger's true-up
    /// (`transfer(Segments, Segments, bound, actual)`).
    pub fn sealed_bytes(&self) -> u64 {
        self.segments.iter().map(|s| s.sealed.size_bytes).sum()
    }
}

/// What a failed cut left behind.
pub struct Partial {
    /// Segments this cut sealed before it failed. Nothing durable names them
    /// (no commit, no index entry), so the cut **deleted them** before
    /// returning; this is the list of what it deleted (or tried to). A crash
    /// during the cut leaves them instead, for the open-time sweep or GC.
    pub segments: Vec<DestagedSegment>,
    /// Bytes the deletions freed (give them back to the ledger).
    pub freed_bytes: u64,
    /// Counters up to the failure.
    pub stats: DestageStats,
}

impl fmt::Debug for Partial {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Partial")
            .field("segments", &self.segments)
            .field("freed_bytes", &self.freed_bytes)
            .field("stats", &self.stats)
            .finish()
    }
}

/// What the coordinator should do about a failed cut.
///
/// | class | errors | action |
/// |---|---|---|
/// | [`Transient`](Self::Transient) | `Io` (not `EIO`/`EROFS`), `Busy` | retry later |
/// | [`AfterSpace`](Self::AfterSpace) | `NoSpace` | run the space ladder, then retry |
/// | [`AfterMapSeal`](Self::AfterMapSeal) | `MapFull` with leaves already dirty | `BlockMap::seal`, then retry the same cut |
/// | [`CutTooLarge`](Self::CutTooLarge) | `MapFull` with no dirty leaf | **do not retry as is**: sealing the map cannot make room; the cut dirties more leaves than the cap, so the coordinator must bound the leaves per cut (`chunk / fanout` over the chunk list) or cut at a lower `write_seq` |
/// | [`Permanent`](Self::Permanent) | `Auth`, `Corrupt`, `Format`, `NotFound`, `Invalid`, `Unsupported`, `Refused`, `AlreadyExists` | **never retry**: a base extent is damaged or forged (`chunk` names it), or the overlay is buggy; the journal cannot be reclaimed and the volume needs an operator |
/// | [`Stop`](Self::Stop) | `ReadOnly`, `Closed`, host `EIO`/`EROFS` | stop cutting (shutdown, read-only) |
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ErrorClass {
    /// Retry later.
    Transient,
    /// Retry after the space ladder freed room.
    AfterSpace,
    /// Retry after sealing the map.
    AfterMapSeal,
    /// The cut cannot fit the map's dirty-leaf cap: never retry it as is.
    CutTooLarge,
    /// Retrying cannot help.
    Permanent,
    /// The volume is read-only or shutting down.
    Stop,
}

impl ErrorClass {
    /// True when the same cut may succeed after the action in the table.
    pub fn is_retryable(self) -> bool {
        matches!(
            self,
            Self::Transient | Self::AfterSpace | Self::AfterMapSeal
        )
    }
}

/// Linux `EIO` and `EROFS`: the host file system failed or went read-only,
/// which is fatal for the volume (DESIGN section 4.3).
pub(crate) fn is_fatal_host_error(e: &Error) -> bool {
    matches!(e, Error::Io(io) if matches!(io.raw_os_error(), Some(5 | 30)))
}

/// Classifies `e`; `map_has_dirty_leaves` tells the two kinds of `MapFull` apart.
pub(crate) fn classify(e: &Error, map_has_dirty_leaves: bool) -> ErrorClass {
    if is_fatal_host_error(e) {
        return ErrorClass::Stop;
    }
    match e.kind() {
        ErrorKind::NoSpace => ErrorClass::AfterSpace,
        ErrorKind::MapFull if map_has_dirty_leaves => ErrorClass::AfterMapSeal,
        ErrorKind::MapFull => ErrorClass::CutTooLarge,
        ErrorKind::Io | ErrorKind::Busy => ErrorClass::Transient,
        ErrorKind::ReadOnly | ErrorKind::Closed => ErrorClass::Stop,
        _ => ErrorClass::Permanent,
    }
}

/// A cut that failed. **Nothing was applied to the map**, and the segments the
/// cut sealed have been deleted (see [`Partial`]). The overlay still holds
/// the data.
#[derive(Debug, thiserror::Error)]
#[error("destage failed ({class:?}, chunk {chunk:?}): {error}")]
pub struct DestageError {
    /// Why.
    #[source]
    pub error: Error,
    /// What to do about it.
    pub class: ErrorClass,
    /// The chunk the failure happened at, where there is one (a base-chunk
    /// read, a seal); `None` for pool-wide failures.
    pub chunk: Option<u64>,
    /// What is left behind.
    pub partial: Box<Partial>,
}

impl DestageError {
    /// The class of the underlying error.
    pub fn kind(&self) -> ErrorKind {
        self.error.kind()
    }
}
