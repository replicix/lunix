use std::collections::BTreeMap;
use std::sync::Mutex;

use lunix_format::WriteSeq;
use lunix_iface::journal::JournalLoc;
use lunix_iface::overlay::*;

use super::lock;

pub struct TOverlay {
    chunk_blocks: u64,
    blocks: Mutex<BTreeMap<u64, (PieceSource, WriteSeq)>>,
}

impl TOverlay {
    pub fn new(chunk_blocks: u64) -> Self {
        Self {
            chunk_blocks,
            blocks: Mutex::new(BTreeMap::new()),
        }
    }

    fn put(&self, range: BlockRange, f: impl Fn(u64) -> PieceSource, seq: WriteSeq) {
        let mut m = lock(&self.blocks);
        for i in 0..range.blocks {
            let e = m.entry(range.lba + i).or_insert((f(i), seq));
            if seq >= e.1 {
                *e = (f(i), seq);
            }
        }
    }
}

impl Overlay for TOverlay {
    fn insert_write(&self, range: BlockRange, loc: JournalLoc, seq: WriteSeq) {
        self.put(
            range,
            |i| PieceSource::Journal(loc.advance(i * 4096).expect("advance")),
            seq,
        );
    }

    fn insert_hole(&self, range: BlockRange, seq: WriteSeq) {
        self.put(range, |_| PieceSource::Hole, seq);
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
        let mut m = lock(&self.blocks);
        m.retain(|_, (_, seq)| *seq > cut.cut_seq);
    }

    fn dirty_bytes(&self) -> u64 {
        lock(&self.blocks).len() as u64 * 4096
    }
}
