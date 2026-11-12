//! The dirty overlay: what the journal holds that the map does not yet (DESIGN
//! sections 4.2, 8.1).
//!
//! All ranges are in 4 KiB blocks. The newest `write_seq` wins per block.
//!
//! # Threads and blocking
//!
//! `insert_write`, `insert_hole` and `lookup` are called from queue threads
//! and are **non-blocking** in the sense of ADR-18: a short in-memory shard
//! lock is allowed, I/O and waiting are not. `cut` and `release` are called by
//! the destage / commit coordinator thread and may take longer locks.

use lunix_format::WriteSeq;

use crate::journal::JournalLoc;

/// A range of 4 KiB blocks.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct BlockRange {
    /// First block.
    pub lba: u64,
    /// Number of blocks (at least 1).
    pub blocks: u64,
}

impl BlockRange {
    /// One past the last block, or `None` on overflow.
    pub fn end(&self) -> Option<u64> {
        self.lba.checked_add(self.blocks)
    }
}

/// Where the bytes of a range come from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PieceSource {
    /// Journal data starting at this location (block `range.lba` is at `loc`,
    /// following blocks follow contiguously).
    Journal(JournalLoc),
    /// Explicitly zero (DISCARD / WRITE_ZEROES).
    Hole,
    /// Not in the overlay: the map decides.
    None,
}

/// A run of blocks with one source and the `write_seq` that put it there.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Piece {
    /// The blocks.
    pub range: BlockRange,
    /// Their source.
    pub source: PieceSource,
    /// The record that wrote them (0 for `None`).
    pub write_seq: WriteSeq,
}

/// The entries of one chunk in a cut.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct CutChunk {
    /// The chunk number (`lba * 4096 / chunk_size`).
    pub chunk: u64,
    /// Pieces inside the chunk with `write_seq <= cut`, ascending by `lba`,
    /// non-overlapping, newest-wins already resolved; blocks the overlay does
    /// not cover are simply absent (never `PieceSource::None`).
    pub pieces: Vec<Piece>,
}

/// A frozen MVCC view: the entries with `write_seq <= cut_seq` as of
/// [`Overlay::cut`], later inserts invisible, grouped by chunk ascending.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct OverlayCut {
    /// The cut `c`.
    pub cut_seq: WriteSeq,
    /// Dirty chunks, ascending.
    pub chunks: Vec<CutChunk>,
}

/// The overlay. `Send + Sync`; methods take `&self`.
pub trait Overlay: Send + Sync {
    /// Records that `range` now reads from the journal at `loc`. Non-blocking.
    /// Called after `try_append` accepted the record (the data is readable at
    /// once; the overlay entry may be hidden from readers until the write
    /// completes, which is the engine's rule, not the overlay's).
    fn insert_write(&self, range: BlockRange, loc: JournalLoc, write_seq: WriteSeq);

    /// Records that `range` reads as zeros. Non-blocking.
    fn insert_hole(&self, range: BlockRange, write_seq: WriteSeq);

    /// Fills `out` (cleared first) with pieces that exactly tile `range` in
    /// ascending order, newest visible version per block, `None` pieces for
    /// blocks the overlay does not hold. Adjacent journal pieces with
    /// contiguous locations and equal `write_seq` may be merged. Non-blocking.
    fn lookup(&self, range: BlockRange, out: &mut Vec<Piece>);

    /// Freezes the entries with `write_seq <= c`. Later inserts, including
    /// overwrites of the same blocks, do not change the returned view.
    /// Destage thread.
    fn cut(&self, c: WriteSeq) -> OverlayCut;

    /// Removes the entries with `write_seq <= cut.cut_seq` that are still the
    /// visible version of their block; a later overwrite survives. Destage
    /// thread, after the commit covering the cut is durable (C9).
    fn release(&self, cut: &OverlayCut);

    /// Dirty bytes currently held (for the `B` commit trigger). Non-blocking.
    fn dirty_bytes(&self) -> u64;
}
