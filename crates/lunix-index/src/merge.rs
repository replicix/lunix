//! Newest-wins merge of sorted entry streams (memtable and runs).

use std::cmp::Reverse;
use std::collections::BinaryHeap;

use lunix_format::index_run::IndexEntry;
use lunix_format::ChunkId;
use lunix_iface::Result;

pub(crate) type Source = Box<dyn Iterator<Item = Result<IndexEntry>>>;

/// Merges sources given oldest first; for equal ids the newest source wins and
/// the others are skipped. After an error the iterator ends.
pub(crate) struct Merge {
    sources: Vec<Source>,
    heads: Vec<Option<IndexEntry>>,
    heap: BinaryHeap<(Reverse<ChunkId>, usize)>,
    failed: bool,
}

impl Merge {
    pub(crate) fn new(sources: Vec<Source>) -> Result<Self> {
        let mut m = Self {
            heads: vec![None; sources.len()],
            sources,
            heap: BinaryHeap::new(),
            failed: false,
        };
        for i in 0..m.sources.len() {
            m.advance(i)?;
        }
        Ok(m)
    }

    fn advance(&mut self, i: usize) -> Result<()> {
        match self.sources[i].next() {
            Some(Ok(e)) => {
                self.heap.push((Reverse(e.chunk_id), i));
                self.heads[i] = Some(e);
            }
            Some(Err(e)) => return Err(e),
            None => self.heads[i] = None,
        }
        Ok(())
    }

    fn step(&mut self) -> Result<Option<IndexEntry>> {
        let Some((Reverse(id), winner)) = self.heap.pop() else {
            return Ok(None);
        };
        let entry = self.heads[winner].take();
        self.advance(winner)?;
        while self.heap.peek().is_some_and(|(Reverse(n), _)| *n == id) {
            let Some((_, i)) = self.heap.pop() else {
                break;
            };
            self.heads[i] = None;
            self.advance(i)?;
        }
        Ok(entry)
    }
}

impl Iterator for Merge {
    type Item = Result<IndexEntry>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed {
            return None;
        }
        match self.step() {
            Ok(Some(e)) => Some(Ok(e)),
            Ok(None) => None,
            Err(e) => {
                self.failed = true;
                Some(Err(e))
            }
        }
    }
}
