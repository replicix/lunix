//! The encoded-leaf cache: a byte-bounded LRU keyed by node hash, with an
//! optional decoded tier for leaves that keep being hit.
//!
//! Keys are node hashes, which are uniformly distributed already, so the maps
//! use the first eight bytes as the hash instead of paying for SipHash on the
//! lookup path.

use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};
use std::sync::Arc;

use lunix_format::node::{Entry, Leaf};

use crate::NodeHash;

/// Passes the first eight key bytes through.
#[derive(Default, Clone, Copy)]
pub(crate) struct KeyHasher(u64);

impl Hasher for KeyHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        if let Some(head) = bytes.first_chunk::<8>() {
            self.0 = u64::from_le_bytes(*head);
        } else {
            for &b in bytes {
                self.0 = self.0.rotate_left(8) ^ u64::from(b);
            }
        }
    }
}

pub(crate) type KeyBuild = BuildHasherDefault<KeyHasher>;

/// Bytes charged per cached leaf on top of its encoding (slot, map entry).
const SLOT_OVERHEAD: usize = 96;
/// Hits on the encoded form before a leaf is offered for promotion.
const PROMOTE_HITS: u8 = 4;
const NIL: usize = usize::MAX;

struct Slot {
    key: NodeHash,
    bytes: Arc<[u8]>,
    decoded: Option<Arc<Leaf>>,
    hits: u8,
    prev: usize,
    next: usize,
}

/// What a cache hit hands back.
pub(crate) enum Hit {
    /// Decoded entries.
    Decoded(Arc<Leaf>),
    /// Canonical leaf bytes; `promote` says the caller should decode the leaf
    /// and hand it to [`Lru::attach_decoded`].
    Encoded { bytes: Arc<[u8]>, promote: bool },
}

pub(crate) struct Lru {
    cap: usize,
    bytes: usize,
    decoded_cost: usize,
    decoded_max: usize,
    decoded_count: usize,
    map: HashMap<NodeHash, usize, KeyBuild>,
    slab: Vec<Slot>,
    free: Vec<usize>,
    head: usize,
    tail: usize,
}

impl Lru {
    pub(crate) fn new(cap: usize, decoded_max: usize, decoded_cost: usize) -> Self {
        Self {
            cap,
            bytes: 0,
            decoded_cost,
            decoded_max,
            decoded_count: 0,
            map: HashMap::default(),
            slab: Vec::new(),
            free: Vec::new(),
            head: NIL,
            tail: NIL,
        }
    }

    /// Bytes charged now.
    pub(crate) fn charged(&self) -> usize {
        self.bytes
    }

    /// Cached leaves.
    pub(crate) fn len(&self) -> usize {
        self.map.len()
    }

    /// Leaves with a decoded form.
    pub(crate) fn decoded_len(&self) -> usize {
        self.decoded_count
    }

    fn unlink(&mut self, i: usize) {
        let (p, n) = (self.slab[i].prev, self.slab[i].next);
        if p == NIL {
            self.head = n;
        } else {
            self.slab[p].next = n;
        }
        if n == NIL {
            self.tail = p;
        } else {
            self.slab[n].prev = p;
        }
    }

    fn push_front(&mut self, i: usize) {
        self.slab[i].prev = NIL;
        self.slab[i].next = self.head;
        if self.head != NIL {
            let h = self.head;
            self.slab[h].prev = i;
        }
        self.head = i;
        if self.tail == NIL {
            self.tail = i;
        }
    }

    /// Looks `key` up and makes it the most recent.
    pub(crate) fn get(&mut self, key: &NodeHash) -> Option<Hit> {
        let i = *self.map.get(key)?;
        if self.head != i {
            self.unlink(i);
            self.push_front(i);
        }
        let s = &mut self.slab[i];
        if let Some(d) = &s.decoded {
            return Some(Hit::Decoded(Arc::clone(d)));
        }
        s.hits = s.hits.saturating_add(1);
        let promote =
            self.decoded_max > 0 && s.hits >= PROMOTE_HITS && self.decoded_count < self.decoded_max;
        Some(Hit::Encoded {
            bytes: Arc::clone(&s.bytes),
            promote,
        })
    }

    /// Adds a leaf; one larger than the whole budget is not kept.
    pub(crate) fn put(&mut self, key: NodeHash, bytes: Arc<[u8]>) {
        let cost = bytes.len() + SLOT_OVERHEAD;
        if self.map.contains_key(&key) || cost > self.cap {
            return;
        }
        self.bytes += cost;
        let slot = Slot {
            key,
            bytes,
            decoded: None,
            hits: 0,
            prev: NIL,
            next: NIL,
        };
        let i = if let Some(i) = self.free.pop() {
            self.slab[i] = slot;
            i
        } else {
            self.slab.push(slot);
            self.slab.len() - 1
        };
        self.map.insert(key, i);
        self.push_front(i);
        while self.bytes > self.cap && self.tail != NIL {
            self.evict_tail();
        }
    }

    fn evict_tail(&mut self) {
        let i = self.tail;
        self.unlink(i);
        let s = &mut self.slab[i];
        self.bytes -= s.bytes.len() + SLOT_OVERHEAD;
        if s.decoded.take().is_some() {
            self.bytes -= self.decoded_cost;
            self.decoded_count -= 1;
        }
        let key = s.key;
        s.bytes = Arc::from(&[][..]);
        self.map.remove(&key);
        self.free.push(i);
    }

    /// Keeps the decoded form of a cached leaf.
    pub(crate) fn attach_decoded(&mut self, key: &NodeHash, d: Arc<Leaf>) {
        if self.decoded_count >= self.decoded_max {
            return;
        }
        let Some(&i) = self.map.get(key) else { return };
        if self.slab[i].decoded.is_some() {
            return;
        }
        self.slab[i].decoded = Some(d);
        self.decoded_count += 1;
        self.bytes += self.decoded_cost;
        while self.bytes > self.cap && self.tail != NIL && self.tail != i {
            self.evict_tail();
        }
    }
}

/// Used by [`Entry`] size reporting in tests.
pub(crate) fn entry_size() -> usize {
    std::mem::size_of::<Entry>()
}
