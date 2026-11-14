//! A small LRU of decoded segment footers, indexed by extent offset.
//!
//! The mark resolves a map entry's hint against the **authenticated footer**
//! of its segment (R3: `SegmentStore::footer` returns it only after the
//! `MetaCodec` check), never against an unauthenticated extent header and
//! never against an index run (index runs are only checksummed, D17). The
//! footer gives the full chunk id stored at `(off, clen)`.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use lunix_format::{ChunkId, Hint, SegId};
use lunix_iface::store::SegmentStore;
use lunix_iface::{ErrorKind, Result};

/// One footer: `(off, clen, chunk_id)` sorted by `off`.
#[derive(Debug)]
pub(crate) struct FooterIndex {
    entries: Vec<(u32, u32, ChunkId)>,
}

impl FooterIndex {
    /// The chunk id stored at exactly `hint.off` with `hint.clen`.
    pub(crate) fn id_at(&self, hint: &Hint) -> Option<ChunkId> {
        let i = self.entries.binary_search_by_key(&hint.off, |e| e.0).ok()?;
        let (_, clen, id) = self.entries.get(i)?;
        (*clen == hint.clen).then_some(*id)
    }

    fn heap_bytes(&self) -> usize {
        self.entries.capacity() * std::mem::size_of::<(u32, u32, ChunkId)>()
    }
}

/// The cache. `None` caches a segment that does not exist.
#[derive(Debug)]
pub(crate) struct FooterCache {
    cap: usize,
    map: HashMap<SegId, Option<Arc<FooterIndex>>>,
    order: VecDeque<SegId>,
}

impl FooterCache {
    pub(crate) fn new(cap: usize) -> Self {
        Self {
            cap: cap.max(1),
            map: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    /// The footer of `seg`, `None` when the segment does not exist.
    ///
    /// # Errors
    /// `Io`, `Corrupt` and `Auth` from the store: a footer that does not
    /// authenticate stops the mark (a pass never guesses).
    pub(crate) fn get(
        &mut self,
        store: &dyn SegmentStore,
        seg: SegId,
    ) -> Result<Option<Arc<FooterIndex>>> {
        if let Some(f) = self.map.get(&seg) {
            return Ok(f.clone());
        }
        let loaded = match store.footer(seg) {
            Ok(body) => {
                let mut entries: Vec<(u32, u32, ChunkId)> = body
                    .entries
                    .iter()
                    .map(|e| (e.off, e.clen, e.chunk_id))
                    .collect();
                entries.sort_unstable_by_key(|e| e.0);
                Some(Arc::new(FooterIndex { entries }))
            }
            Err(e) if e.kind() == ErrorKind::NotFound => None,
            Err(e) => return Err(e),
        };
        if self.order.len() >= self.cap {
            if let Some(old) = self.order.pop_front() {
                self.map.remove(&old);
            }
        }
        self.order.push_back(seg);
        self.map.insert(seg, loaded.clone());
        Ok(loaded)
    }

    /// Forgets everything (a segment may have been deleted since).
    pub(crate) fn clear(&mut self) {
        self.map.clear();
        self.order.clear();
    }

    pub(crate) fn heap_bytes(&self) -> usize {
        self.map
            .values()
            .flatten()
            .map(|f| f.heap_bytes())
            .sum::<usize>()
            + self.map.capacity() * 48
    }
}
