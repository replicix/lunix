//! A FIFO of decoded segment footers, indexed by extent offset, bounded in
//! bytes of memory.
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

/// A byte count for the footer cache. A newtype so that a bare literal (a
/// segment count in the old API) cannot silently mean bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CacheBytes(usize);

impl CacheBytes {
    pub(crate) const fn new(bytes: usize) -> Self {
        Self(bytes)
    }
}

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

    /// The chunk ids of the footer.
    pub(crate) fn ids(&self) -> impl Iterator<Item = &ChunkId> {
        self.entries.iter().map(|e| &e.2)
    }

    fn heap_bytes(&self) -> usize {
        self.entries.capacity() * std::mem::size_of::<(u32, u32, ChunkId)>()
    }
}

/// What every cache slot is charged besides its footer's bytes (its map slot
/// and queue entry), and all a cached "no such segment" costs.
const SLOT_COST: usize = 64;

/// The charge of one cache slot: the footer's heap bytes plus the slot.
fn cost_of(f: Option<&Arc<FooterIndex>>) -> usize {
    f.map_or(SLOT_COST, |f| f.heap_bytes() + SLOT_COST)
}

/// The cache. `None` caches a segment that does not exist.
#[derive(Debug)]
pub(crate) struct FooterCache {
    /// Bound on the bytes the cached footers hold.
    cap_bytes: usize,
    /// Bytes held now (the sum of `cost_of` over `map`).
    bytes: usize,
    map: HashMap<SegId, Option<Arc<FooterIndex>>>,
    order: VecDeque<SegId>,
}

impl FooterCache {
    /// A cache that holds at most `cap` of footers (the newest footer is kept
    /// even when it alone is larger).
    pub(crate) fn new(cap: CacheBytes) -> Self {
        Self {
            cap_bytes: cap.0,
            bytes: 0,
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
        let cost = cost_of(loaded.as_ref());
        while self.bytes.saturating_add(cost) > self.cap_bytes {
            let Some(old) = self.order.pop_front() else {
                break;
            };
            if let Some(f) = self.map.remove(&old) {
                self.bytes = self.bytes.saturating_sub(cost_of(f.as_ref()));
            }
        }
        self.bytes = self.bytes.saturating_add(cost);
        self.order.push_back(seg);
        self.map.insert(seg, loaded.clone());
        Ok(loaded)
    }

    /// Forgets everything (a segment may have been deleted since).
    pub(crate) fn clear(&mut self) {
        self.map.clear();
        self.order.clear();
        self.bytes = 0;
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use lunix_format::IdLen;
    use lunix_iface::store::SegmentStore;

    use super::*;
    use crate::tests::sim::{Sim, SimCfg};

    fn volume_segments(n: u64) -> (Sim, Vec<SegId>) {
        let mut cfg = SimCfg::new(16 << 10, IdLen::Sixteen);
        cfg.chunk_count = 64;
        let mut s = Sim::new(cfg);
        for i in 0..n {
            s.write(i, 100 + i);
            s.commit().expect("commit");
        }
        let segs = SegmentStore::list(&s.store)
            .expect("list")
            .iter()
            .map(|x| x.seg)
            .collect();
        (s, segs)
    }

    /// The cache is bounded in bytes: it keeps the footers that fit, evicts
    /// the oldest first, and still returns a footer larger than the bound.
    #[test]
    fn the_cache_is_bounded_by_bytes_and_evicts_the_oldest() {
        let (s, segs) = volume_segments(6);
        assert!(segs.len() >= 4);
        let get = |c: &mut FooterCache, seg| c.get(&s.store, seg).expect("footer").expect("exists");

        // Room for all: every second get is a hit.
        let mut big = FooterCache::new(CacheBytes::new(1 << 20));
        let first: Vec<_> = segs.iter().map(|&g| get(&mut big, g)).collect();
        for (g, f) in segs.iter().zip(&first) {
            assert!(Arc::ptr_eq(f, &get(&mut big, *g)), "{g:?} stays cached");
        }

        // Room for exactly two footers of this size.
        let one = cost_of(Some(&first[0]));
        let mut small = FooterCache::new(CacheBytes::new(2 * one));
        let a = get(&mut small, segs[0]);
        let b = get(&mut small, segs[1]);
        let c = get(&mut small, segs[2]);
        assert!(
            !Arc::ptr_eq(&a, &get(&mut small, segs[0])),
            "oldest evicted"
        );
        // Loading segs[0] again evicted segs[1]; segs[2] is still there.
        assert!(!Arc::ptr_eq(&b, &get(&mut small, segs[1])));
        assert!(
            small.bytes <= 2 * one,
            "bytes {} over the bound",
            small.bytes
        );
        let _ = c;

        // A bound below one footer still serves it (the newest is kept).
        let mut tiny = FooterCache::new(CacheBytes::new(1));
        let t = get(&mut tiny, segs[0]);
        assert!(Arc::ptr_eq(&t, &get(&mut tiny, segs[0])));
        let _ = get(&mut tiny, segs[1]);
        assert!(tiny.map.len() == 1 && tiny.bytes <= one + SLOT_COST);
    }
}
