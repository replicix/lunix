//! [`FlatOverlay`]: one entry per 4 KiB block. The simplest thing that obeys
//! the [`Overlay`] contract, and the oracle for `lunix-overlay` (M4).
//!
//! Pieces come out one block wide so that nothing in this file can be wrong
//! about merging; compare against another implementation with [`coalesce`] on
//! both sides.

use std::collections::BTreeMap;
use std::sync::{Mutex, MutexGuard};

use lunix_format::WriteSeq;
use lunix_iface::journal::JournalLoc;
use lunix_iface::overlay::{BlockRange, CutChunk, Overlay, OverlayCut, Piece, PieceSource};

const BLOCK: u64 = 4096;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// Merges neighbouring pieces that a real overlay is allowed to merge: the
/// same `write_seq`, adjacent blocks, and either the same non-journal source or
/// journal locations that continue each other. The result is canonical for
/// the per-block content, so two overlays agree iff their `coalesce`d lookups
/// are equal.
pub fn coalesce(pieces: &[Piece]) -> Vec<Piece> {
    let mut out: Vec<Piece> = Vec::with_capacity(pieces.len());
    for p in pieces {
        if let Some(last) = out.last_mut() {
            let adjacent = last.range.end() == Some(p.range.lba);
            let same_seq = last.write_seq == p.write_seq;
            let continues = match (last.source, p.source) {
                (PieceSource::Journal(a), PieceSource::Journal(b)) => {
                    a.advance(last.range.blocks * BLOCK).is_ok_and(|n| n == b)
                }
                (PieceSource::Hole, PieceSource::Hole) | (PieceSource::None, PieceSource::None) => {
                    true
                }
                _ => false,
            };
            if adjacent && same_seq && continues {
                last.range.blocks += p.range.blocks;
                continue;
            }
        }
        out.push(*p);
    }
    out
}

/// The per-block overlay.
#[derive(Debug)]
pub struct FlatOverlay {
    chunk_blocks: u64,
    blocks: Mutex<BTreeMap<u64, (PieceSource, WriteSeq)>>,
}

impl FlatOverlay {
    /// An empty overlay whose cuts group blocks into chunks of `chunk_blocks`
    /// 4 KiB blocks.
    ///
    /// # Panics
    /// If `chunk_blocks` is zero.
    pub fn new(chunk_blocks: u64) -> Self {
        assert!(chunk_blocks > 0, "chunk_blocks must be at least 1");
        Self {
            chunk_blocks,
            blocks: Mutex::new(BTreeMap::new()),
        }
    }

    /// Blocks currently held.
    pub fn len_blocks(&self) -> u64 {
        lock(&self.blocks).len() as u64
    }

    fn put(&self, range: BlockRange, source: impl Fn(u64) -> PieceSource, seq: WriteSeq) {
        let mut m = lock(&self.blocks);
        for i in 0..range.blocks {
            let lba = range.lba + i;
            // Newest write_seq wins; an equal one is a later insert of the same
            // record's range and replaces.
            if m.get(&lba).is_none_or(|(_, old)| seq >= *old) {
                m.insert(lba, (source(i), seq));
            }
        }
    }
}

impl Overlay for FlatOverlay {
    fn insert_write(&self, range: BlockRange, loc: JournalLoc, write_seq: WriteSeq) {
        self.put(
            range,
            |i| {
                PieceSource::Journal(
                    loc.advance(i * BLOCK)
                        .expect("journal offset overflow in a test"),
                )
            },
            write_seq,
        );
    }

    fn insert_hole(&self, range: BlockRange, write_seq: WriteSeq) {
        self.put(range, |_| PieceSource::Hole, write_seq);
    }

    fn lookup(&self, range: BlockRange, out: &mut Vec<Piece>) {
        out.clear();
        let m = lock(&self.blocks);
        for lba in range.lba..range.lba + range.blocks {
            let (source, write_seq) = m
                .get(&lba)
                .copied()
                .unwrap_or((PieceSource::None, WriteSeq(0)));
            out.push(Piece {
                range: BlockRange { lba, blocks: 1 },
                source,
                write_seq,
            });
        }
    }

    fn cut(&self, c: WriteSeq) -> OverlayCut {
        let m = lock(&self.blocks);
        let mut chunks: BTreeMap<u64, Vec<Piece>> = BTreeMap::new();
        for (&lba, &(source, write_seq)) in m.iter() {
            if write_seq <= c {
                chunks
                    .entry(lba / self.chunk_blocks)
                    .or_default()
                    .push(Piece {
                        range: BlockRange { lba, blocks: 1 },
                        source,
                        write_seq,
                    });
            }
        }
        OverlayCut {
            cut_seq: c,
            chunks: chunks
                .into_iter()
                .map(|(chunk, pieces)| CutChunk { chunk, pieces })
                .collect(),
        }
    }

    fn release(&self, cut: &OverlayCut) {
        // A block overwritten after the cut holds a newer write_seq, so
        // "still the visible version" is exactly "write_seq <= cut_seq".
        lock(&self.blocks).retain(|_, (_, seq)| *seq > cut.cut_seq);
    }

    fn dirty_bytes(&self) -> u64 {
        lock(&self.blocks).len() as u64 * BLOCK
    }
}
