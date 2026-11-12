//! What a cut returns, and what a failed cut leaves behind.

use std::fmt;

use lunix_format::{ChunkId, Hint, SegId, WriteSeq};
use lunix_iface::index::ChunkIndex;
use lunix_iface::journal::WriteSeqRange;
use lunix_iface::map::TxnReport;
use lunix_iface::store::{SealedSegment, SegmentWriter};
use lunix_iface::{Error, ErrorKind, Result};

use crate::stats::DestageStats;

/// A segment the cut appended to.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct DestagedSegment {
    /// The segment id.
    pub seg: SegId,
    /// The `write_seq` range of the chunks stored in it.
    pub writes: WriteSeqRange,
    /// Extents in append order: the index inserts the caller makes once the
    /// segment is sealed.
    pub entries: Vec<(ChunkId, Hint)>,
    /// Extent bytes appended (before the footer and trailer).
    pub extent_bytes: u64,
    /// Set once the segment is sealed (full segments are sealed by the cut,
    /// the last one by [`DestageOutcome::seal_open`]).
    pub sealed: Option<SealedSegment>,
}

/// The result of a successful cut.
///
/// The map already holds the new entries (one `MapTxn` was committed). Entries
/// that point into the last segment point at a file that is still open until
/// the caller seals it (C4); a read through the map during that window must
/// not happen, which holds because the overlay still serves those blocks (C9
/// releases it after the commit).
pub struct DestageOutcome {
    /// The cut.
    pub cut_seq: WriteSeq,
    /// Every segment the cut appended to, in order, with the extents it holds
    /// and the segment ids the caller reserves space for and names in the
    /// commit's `new_segments`.
    pub segments: Vec<DestagedSegment>,
    /// The writer of the last segment, still open, if any extent went into it.
    pub open: Option<Box<dyn SegmentWriter>>,
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
            .field("open", &self.open.as_ref().map(|w| w.id()))
            .field("map", &self.map)
            .field("stats", &self.stats)
            .finish()
    }
}

impl DestageOutcome {
    /// Seals the segment still open (C4) with the `write_seq` range of its
    /// chunks. `None` when nothing is open. Idempotent.
    ///
    /// # Errors
    /// Whatever `SegmentWriter::seal` reports (`NoSpace`, `Io`). The writer is
    /// consumed either way.
    pub fn seal_open(&mut self) -> Result<Option<SealedSegment>> {
        let Some(writer) = self.open.take() else {
            return Ok(None);
        };
        let Some(seg) = self
            .segments
            .iter_mut()
            .rev()
            .find(|s| s.seg == writer.id() && s.sealed.is_none())
        else {
            return Err(Error::invalid("open writer without a segment record"));
        };
        let sealed = writer.seal(Some(seg.writes))?;
        seg.sealed = Some(sealed);
        Ok(Some(sealed))
    }

    /// Inserts every extent of the cut into the index. The caller does this
    /// once the cut's segments are sealed and not before: an entry must name a
    /// sealed segment, and a cut that failed after C4 must leave nothing in the
    /// index (its segments are orphans for the open-time sweep).
    ///
    /// # Errors
    /// `Invalid` while a segment is unsealed; the index's own errors.
    pub fn insert_into_index(&self, index: &dyn ChunkIndex) -> Result<()> {
        if self.open.is_some() || self.segments.iter().any(|s| s.sealed.is_none()) {
            return Err(Error::invalid(
                "index insert before the segments are sealed",
            ));
        }
        for seg in &self.segments {
            for (id, hint) in &seg.entries {
                index.insert(*id, *hint)?;
            }
        }
        Ok(())
    }

    /// The sealed segments' file bytes, for the ledger's true-up
    /// (`transfer(Segments, Segments, bound, actual)`); unsealed ones count 0.
    pub fn sealed_bytes(&self) -> u64 {
        self.segments
            .iter()
            .filter_map(|s| s.sealed.map(|x| x.size_bytes))
            .sum()
    }
}

/// What a failed cut leaves for the caller.
pub struct Partial {
    /// Segments appended to before the failure. The sealed ones stay on disk
    /// unreferenced (the map was not touched, the index gets nothing); the one
    /// that was open has been abandoned.
    pub segments: Vec<DestagedSegment>,
    /// Counters up to the failure.
    pub stats: DestageStats,
}

/// A cut that failed. **Nothing was applied to the map.**
#[derive(Debug, thiserror::Error)]
#[error("destage failed: {error}")]
pub struct DestageError {
    /// Why.
    #[source]
    pub error: Error,
    /// What is left behind.
    pub partial: Box<Partial>,
}

impl DestageError {
    /// The class of the underlying error (`MapFull` asks the caller to seal the
    /// map and cut again).
    pub fn kind(&self) -> ErrorKind {
        self.error.kind()
    }
}

impl fmt::Debug for Partial {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Partial")
            .field("segments", &self.segments)
            .field("stats", &self.stats)
            .finish()
    }
}
