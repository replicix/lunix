//! The decoded-chunk read cache: bounded by bytes, CLOCK replacement, keyed
//! by the extent's location and the map entry's id prefix (engine-internal,
//! not a format). An extent location is never reused (object ids are never
//! reissued and containers are immutable), so a cached chunk can never be
//! stale; the prefix in the key keeps a hit tied to the R1 expectation it
//! was verified against.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use lunix_format::{Hint, IdPrefix};

/// A cache key.
pub(crate) type Key = (Hint, IdPrefix);

struct Slot {
    key: Key,
    data: Arc<[u8]>,
    referenced: bool,
}

struct State {
    index: HashMap<Key, usize>,
    slots: Vec<Option<Slot>>,
    hand: usize,
    used: u64,
}

/// The cache. Methods take a short lock and never block on I/O, so queue
/// threads may call them.
pub(crate) struct ChunkCache {
    cap: u64,
    st: Mutex<State>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl ChunkCache {
    pub(crate) fn new(cap_bytes: u64) -> Self {
        Self {
            cap: cap_bytes,
            st: Mutex::new(State {
                index: HashMap::new(),
                slots: Vec::new(),
                hand: 0,
                used: 0,
            }),
        }
    }

    pub(crate) fn get(&self, key: &Key) -> Option<Arc<[u8]>> {
        let mut st = lock(&self.st);
        let i = *st.index.get(key)?;
        let slot = st.slots.get_mut(i)?.as_mut()?;
        slot.referenced = true;
        Some(Arc::clone(&slot.data))
    }

    pub(crate) fn insert(&self, key: Key, data: Arc<[u8]>) {
        let len = data.len() as u64;
        if len > self.cap {
            return;
        }
        let mut st = lock(&self.st);
        if st.index.contains_key(&key) {
            return;
        }
        // CLOCK: sweep, clearing reference bits, until enough is free.
        let mut sweeps = 0usize;
        while st.used + len > self.cap && !st.slots.is_empty() {
            let n = st.slots.len();
            let h = st.hand % n;
            st.hand = (h + 1) % n;
            let evict = match st.slots[h].as_mut() {
                Some(s) if s.referenced && sweeps < 2 * n => {
                    s.referenced = false;
                    false
                }
                Some(_) => true,
                None => false,
            };
            sweeps += 1;
            if evict {
                if let Some(s) = st.slots[h].take() {
                    st.used -= s.data.len() as u64;
                    st.index.remove(&s.key);
                }
            }
            if sweeps > 4 * n {
                break;
            }
        }
        if st.used + len > self.cap {
            return;
        }
        let i = match st.slots.iter().position(Option::is_none) {
            Some(i) => i,
            None => {
                st.slots.push(None);
                st.slots.len() - 1
            }
        };
        st.slots[i] = Some(Slot {
            key,
            data,
            referenced: false,
        });
        st.index.insert(key, i);
        st.used += len;
    }

    /// Drops every cached chunk (an idle volume returns the memory; the cache
    /// refills from the store on the next reads). Returns the bytes released.
    pub(crate) fn clear(&self) -> u64 {
        let mut st = lock(&self.st);
        let used = st.used;
        st.index = HashMap::new();
        st.slots = Vec::new();
        st.hand = 0;
        st.used = 0;
        used
    }

    #[cfg(test)]
    pub(crate) fn used(&self) -> u64 {
        lock(&self.st).used
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lunix_format::{IdLen, ObjId};

    fn key(n: u32) -> Key {
        (
            Hint {
                seg: ObjId::new(0, 1).expect("id"),
                off: n,
                clen: 1,
            },
            IdPrefix::from_slice(IdLen::Zero, &[]).expect("prefix"),
        )
    }

    #[test]
    fn clear_releases_everything_and_the_cache_refills() {
        let c = ChunkCache::new(4 * 4096);
        for n in 0..3 {
            c.insert(key(n), Arc::from(vec![n as u8; 4096]));
        }
        assert_eq!(c.clear(), 3 * 4096);
        assert_eq!(c.used(), 0);
        assert!(c.get(&key(1)).is_none());
        c.insert(key(1), Arc::from(vec![1u8; 4096]));
        assert!(c.get(&key(1)).is_some());
    }

    #[test]
    fn the_cache_stays_within_its_byte_bound_and_keeps_referenced_entries() {
        let c = ChunkCache::new(4 * 4096);
        for n in 0..4 {
            c.insert(key(n), Arc::from(vec![n as u8; 4096]));
        }
        assert!(c.get(&key(0)).is_some());
        c.insert(key(9), Arc::from(vec![9u8; 4096]));
        assert!(c.used() <= 4 * 4096);
        assert!(
            c.get(&key(0)).is_some(),
            "a referenced entry survives a sweep"
        );
        assert!(c.get(&key(9)).is_some());
        c.insert(key(10), Arc::from(vec![1u8; 8 * 4096]));
        assert!(
            c.get(&key(10)).is_none(),
            "an entry above the cap is not cached"
        );
    }
}
