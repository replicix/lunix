//! Small data structures: deterministic Fx-style hasher, arena doubly linked
//! list keyed by u64, percentile helper. Throwaway measurement code.
use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasherDefault, Hasher};

#[derive(Default, Clone, Copy)]
pub struct Fx(u64);
impl Hasher for Fx {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0.rotate_left(5) ^ b as u64).wrapping_mul(0x517c_c1b7_2722_0a95);
        }
    }
    fn write_u64(&mut self, i: u64) {
        self.0 = (self.0.rotate_left(5) ^ i).wrapping_mul(0x517c_c1b7_2722_0a95);
    }
}
pub type FxMap<K, V> = HashMap<K, V, BuildHasherDefault<Fx>>;
pub type FxSet<K> = HashSet<K, BuildHasherDefault<Fx>>;

pub fn mix(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}

pub const NIL: usize = usize::MAX;

#[derive(Clone)]
pub struct Node {
    pub key: u64,
    pub prev: usize, // toward head (newer)
    pub next: usize, // toward tail (older)
    pub flag: bool,
}

/// Doubly linked list with O(1) lookup by key. Head = newest, tail = oldest.
#[derive(Default)]
pub struct DList {
    pub nodes: Vec<Node>,
    free: Vec<usize>,
    map: FxMap<u64, usize>,
    pub head: usize,
    pub tail: usize,
}

impl DList {
    pub fn new() -> Self {
        DList { nodes: Vec::new(), free: Vec::new(), map: FxMap::default(), head: NIL, tail: NIL }
    }
    pub fn len(&self) -> usize {
        self.map.len()
    }
    pub fn idx(&self, k: u64) -> Option<usize> {
        self.map.get(&k).copied()
    }
    pub fn push_front(&mut self, key: u64) -> usize {
        let n = Node { key, prev: NIL, next: self.head, flag: false };
        let i = if let Some(i) = self.free.pop() {
            self.nodes[i] = n;
            i
        } else {
            self.nodes.push(n);
            self.nodes.len() - 1
        };
        if self.head != NIL {
            self.nodes[self.head].prev = i;
        }
        self.head = i;
        if self.tail == NIL {
            self.tail = i;
        }
        self.map.insert(key, i);
        i
    }
    fn unlink(&mut self, i: usize) {
        let (p, n) = (self.nodes[i].prev, self.nodes[i].next);
        if p != NIL {
            self.nodes[p].next = n;
        } else {
            self.head = n;
        }
        if n != NIL {
            self.nodes[n].prev = p;
        } else {
            self.tail = p;
        }
    }
    pub fn remove_idx(&mut self, i: usize) -> u64 {
        self.unlink(i);
        let k = self.nodes[i].key;
        self.map.remove(&k);
        self.free.push(i);
        k
    }
    pub fn remove(&mut self, k: u64) -> bool {
        match self.idx(k) {
            Some(i) => {
                self.remove_idx(i);
                true
            }
            None => false,
        }
    }
    pub fn move_front(&mut self, i: usize) {
        if self.head == i {
            return;
        }
        self.unlink(i);
        self.nodes[i].prev = NIL;
        self.nodes[i].next = self.head;
        if self.head != NIL {
            self.nodes[self.head].prev = i;
        }
        self.head = i;
        if self.tail == NIL {
            self.tail = i;
        }
    }
    pub fn pop_back(&mut self) -> Option<u64> {
        if self.tail == NIL {
            None
        } else {
            let t = self.tail;
            Some(self.remove_idx(t))
        }
    }
    pub fn tail_key(&self) -> Option<u64> {
        if self.tail == NIL {
            None
        } else {
            Some(self.nodes[self.tail].key)
        }
    }
}

/// Nearest-rank-style percentile on an unsorted slice (p in 0..=1).
pub fn pct(v: &[f64], p: f64) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let i = ((s.len() - 1) as f64 * p).round() as usize;
    s[i]
}
pub fn mean(v: &[f64]) -> f64 {
    if v.is_empty() {
        0.0
    } else {
        v.iter().sum::<f64>() / v.len() as f64
    }
}
