//! A multi-version oracle: every block keeps all its versions.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::sync::Mutex;

use lunix_format::WriteSeq;
use lunix_iface::journal::JournalLoc;
use lunix_iface::overlay::{BlockRange, CutChunk, Overlay, OverlayCut, Piece, PieceSource};

const BLOCK: u64 = 4096;

/// `lba -> (write_seq -> source)`. Cut: newest version `<= c` per block.
/// Release: drop every version `<= cut_seq`. Dirty bytes: all versions.
#[derive(Debug)]
pub struct Versioned {
    chunk_blocks: u64,
    blocks: Mutex<BTreeMap<u64, BTreeMap<u64, PieceSource>>>,
}

impl Versioned {
    pub fn new(chunk_blocks: u64) -> Self {
        Self {
            chunk_blocks,
            blocks: Mutex::new(BTreeMap::new()),
        }
    }

    fn put(&self, range: BlockRange, source: impl Fn(u64) -> PieceSource, seq: WriteSeq) {
        let mut m = self.blocks.lock().unwrap();
        for i in 0..range.blocks {
            m.entry(range.lba + i).or_default().insert(seq.0, source(i));
        }
    }
}

impl Overlay for Versioned {
    fn insert_write(&self, range: BlockRange, loc: JournalLoc, write_seq: WriteSeq) {
        self.put(
            range,
            |i| PieceSource::Journal(loc.advance(i * BLOCK).unwrap()),
            write_seq,
        );
    }

    fn insert_hole(&self, range: BlockRange, write_seq: WriteSeq) {
        self.put(range, |_| PieceSource::Hole, write_seq);
    }

    fn lookup(&self, range: BlockRange, out: &mut Vec<Piece>) {
        out.clear();
        let m = self.blocks.lock().unwrap();
        for lba in range.lba..range.lba + range.blocks {
            let (source, seq) = m
                .get(&lba)
                .and_then(|v| v.iter().next_back())
                .map_or((PieceSource::None, 0), |(&s, &src)| (src, s));
            out.push(Piece {
                range: BlockRange { lba, blocks: 1 },
                source,
                write_seq: WriteSeq(seq),
            });
        }
    }

    fn cut(&self, c: WriteSeq) -> OverlayCut {
        let m = self.blocks.lock().unwrap();
        let mut chunks: BTreeMap<u64, Vec<Piece>> = BTreeMap::new();
        for (&lba, versions) in m.iter() {
            if let Some((&seq, &source)) = versions.range(..=c.0).next_back() {
                chunks
                    .entry(lba / self.chunk_blocks)
                    .or_default()
                    .push(Piece {
                        range: BlockRange { lba, blocks: 1 },
                        source,
                        write_seq: WriteSeq(seq),
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
        let mut m = self.blocks.lock().unwrap();
        for v in m.values_mut() {
            v.retain(|&s, _| s > cut.cut_seq.0);
        }
        m.retain(|_, v| !v.is_empty());
    }

    fn dirty_bytes(&self) -> u64 {
        self.blocks
            .lock()
            .unwrap()
            .values()
            .map(|v| v.len() as u64)
            .sum::<u64>()
            * BLOCK
    }
}
