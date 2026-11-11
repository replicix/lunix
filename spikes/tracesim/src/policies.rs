//! Cache policies at chunk granularity. `access` returns true on hit; on a
//! miss the key is inserted (evicting as needed). Capacity is in chunks.
use crate::ds::*;
use std::collections::VecDeque;

pub trait Cache {
    fn access(&mut self, k: u64) -> bool;
}

pub fn make(name: &str, cap: usize) -> Box<dyn Cache> {
    match name {
        "lru" => Box::new(Lru::new(cap)),
        "s3fifo" => Box::new(S3Fifo::new(cap)),
        "sieve" => Box::new(Sieve::new(cap)),
        "wtinylfu" => Box::new(WTinyLfu::new(cap)),
        _ => panic!("unknown policy {name}"),
    }
}
pub const POLICIES: [&str; 4] = ["lru", "s3fifo", "sieve", "wtinylfu"];

pub struct Lru {
    l: DList,
    cap: usize,
}
impl Lru {
    pub fn new(cap: usize) -> Self {
        Lru { l: DList::new(), cap: cap.max(1) }
    }
}
impl Cache for Lru {
    fn access(&mut self, k: u64) -> bool {
        if let Some(i) = self.l.idx(k) {
            self.l.move_front(i);
            return true;
        }
        if self.l.len() >= self.cap {
            self.l.pop_back();
        }
        self.l.push_front(k);
        false
    }
}

/// SIEVE (Zhang et al., NSDI 2024): FIFO order, a visited bit, and a hand
/// that sweeps from the oldest toward the newest, evicting the first
/// unvisited object; objects stay in place.
pub struct Sieve {
    l: DList,
    cap: usize,
    hand: usize,
}
impl Sieve {
    pub fn new(cap: usize) -> Self {
        Sieve { l: DList::new(), cap: cap.max(1), hand: NIL }
    }
    fn evict(&mut self) {
        let mut o = if self.hand != NIL { self.hand } else { self.l.tail };
        while self.l.nodes[o].flag {
            self.l.nodes[o].flag = false;
            o = self.l.nodes[o].prev;
            if o == NIL {
                o = self.l.tail;
            }
        }
        self.hand = self.l.nodes[o].prev;
        self.l.remove_idx(o);
    }
}
impl Cache for Sieve {
    fn access(&mut self, k: u64) -> bool {
        if let Some(i) = self.l.idx(k) {
            self.l.nodes[i].flag = true;
            return true;
        }
        if self.l.len() >= self.cap {
            self.evict();
        }
        self.l.push_front(k);
        false
    }
}

/// S3-FIFO (Yang et al., SOSP 2023): small FIFO 10 %, main FIFO 90 %, ghost
/// FIFO of main's size holding keys only; frequency capped at 3; promotion
/// from small to main when freq > 1; main reinserts objects with freq > 0.
pub struct S3Fifo {
    cap: usize,
    small_cap: usize,
    main_cap: usize,
    ent: FxMap<u64, (u8, bool)>, // freq, in_main
    small: VecDeque<u64>,
    main: VecDeque<u64>,
    ghost: VecDeque<(u64, u64)>,
    ghost_map: FxMap<u64, u64>,
    seq: u64,
}
impl S3Fifo {
    pub fn new(cap: usize) -> Self {
        let cap = cap.max(2);
        let small_cap = (cap / 10).max(1);
        S3Fifo {
            cap,
            small_cap,
            main_cap: cap - small_cap,
            ent: FxMap::default(),
            small: VecDeque::new(),
            main: VecDeque::new(),
            ghost: VecDeque::new(),
            ghost_map: FxMap::default(),
            seq: 0,
        }
    }
    fn ghost_add(&mut self, k: u64) {
        self.seq += 1;
        self.ghost.push_back((k, self.seq));
        self.ghost_map.insert(k, self.seq);
        while self.ghost_map.len() > self.main_cap {
            let (g, s) = self.ghost.pop_front().unwrap();
            if self.ghost_map.get(&g) == Some(&s) {
                self.ghost_map.remove(&g);
            }
        }
    }
    fn evict_main(&mut self) {
        while let Some(t) = self.main.pop_front() {
            let e = self.ent.get_mut(&t).unwrap();
            if e.0 > 0 {
                e.0 -= 1;
                self.main.push_back(t);
            } else {
                self.ent.remove(&t);
                return;
            }
        }
    }
    fn evict_small(&mut self) {
        while let Some(t) = self.small.pop_front() {
            let f = self.ent[&t].0;
            if f > 1 {
                if self.main.len() >= self.main_cap {
                    self.evict_main();
                }
                self.ent.insert(t, (0, true));
                self.main.push_back(t);
            } else {
                self.ent.remove(&t);
                self.ghost_add(t);
                return;
            }
        }
    }
    fn evict(&mut self) {
        if (self.small.len() >= self.small_cap || self.main.is_empty()) && !self.small.is_empty() {
            self.evict_small();
        } else {
            self.evict_main();
        }
    }
}
impl Cache for S3Fifo {
    fn access(&mut self, k: u64) -> bool {
        if let Some(e) = self.ent.get_mut(&k) {
            e.0 = (e.0 + 1).min(3);
            return true;
        }
        let in_ghost = self.ghost_map.remove(&k).is_some();
        while self.small.len() + self.main.len() >= self.cap {
            self.evict();
        }
        if in_ghost {
            self.main.push_back(k);
            self.ent.insert(k, (0, true));
        } else {
            self.small.push_back(k);
            self.ent.insert(k, (0, false));
        }
        false
    }
}

/// Count-min sketch of 4-bit counters (stored in u8, cap 15) behind a
/// doorkeeper bloom filter; halved and the doorkeeper cleared every
/// 10 x capacity recorded accesses.
struct Sketch {
    rows: [Vec<u8>; 4],
    mask: u64,
    door: Vec<u64>,
    dmask: u64,
    adds: u64,
    period: u64,
}
impl Sketch {
    fn new(cap: usize) -> Self {
        let n = cap.next_power_of_two().max(64);
        Sketch {
            rows: [vec![0; n], vec![0; n], vec![0; n], vec![0; n]],
            mask: n as u64 - 1,
            door: vec![0; (n * 8).div_ceil(64)],
            dmask: (n as u64 * 8) - 1,
            adds: 0,
            period: 10 * cap as u64,
        }
    }
    fn door_idx(&self, k: u64) -> [u64; 2] {
        [mix(k ^ 0xa5a5) & self.dmask, mix(k ^ 0x5a5a) & self.dmask]
    }
    fn door_has(&self, k: u64) -> bool {
        self.door_idx(k).iter().all(|&b| self.door[(b / 64) as usize] >> (b % 64) & 1 == 1)
    }
    fn door_add(&mut self, k: u64) {
        for b in self.door_idx(k) {
            self.door[(b / 64) as usize] |= 1 << (b % 64);
        }
    }
    fn est(&self, k: u64) -> u32 {
        let m = (0..4).map(|i| self.rows[i][(mix(k.wrapping_add(i as u64 * 0x1234_5678)) & self.mask) as usize]).min().unwrap();
        m as u32 + self.door_has(k) as u32
    }
    fn record(&mut self, k: u64) {
        if !self.door_has(k) {
            self.door_add(k);
        } else {
            for i in 0..4 {
                let j = (mix(k.wrapping_add(i as u64 * 0x1234_5678)) & self.mask) as usize;
                if self.rows[i][j] < 15 {
                    self.rows[i][j] += 1;
                }
            }
        }
        self.adds += 1;
        if self.adds >= self.period {
            self.adds = 0;
            for r in self.rows.iter_mut() {
                for c in r.iter_mut() {
                    *c >>= 1;
                }
            }
            self.door.iter_mut().for_each(|w| *w = 0);
        }
    }
}

/// W-TinyLFU (Einziger et al.; Caffeine layout): 1 % LRU window, SLRU main
/// (80 % protected), candidate from the window competes with the probation
/// victim on sketch frequency (strictly greater wins; no random jitter, so
/// runs are deterministic).
pub struct WTinyLfu {
    sk: Sketch,
    win: DList,
    prob: DList,
    prot: DList,
    win_cap: usize,
    main_cap: usize,
    prot_cap: usize,
}
impl WTinyLfu {
    pub fn new(cap: usize) -> Self {
        let cap = cap.max(4);
        let win_cap = (cap / 100).max(1);
        let main_cap = cap - win_cap;
        WTinyLfu {
            sk: Sketch::new(cap),
            win: DList::new(),
            prob: DList::new(),
            prot: DList::new(),
            win_cap,
            main_cap,
            prot_cap: main_cap * 8 / 10,
        }
    }
}
impl Cache for WTinyLfu {
    fn access(&mut self, k: u64) -> bool {
        self.sk.record(k);
        if let Some(i) = self.win.idx(k) {
            self.win.move_front(i);
            return true;
        }
        if let Some(i) = self.prot.idx(k) {
            self.prot.move_front(i);
            return true;
        }
        if self.prob.remove(k) {
            self.prot.push_front(k);
            if self.prot.len() > self.prot_cap {
                let d = self.prot.pop_back().unwrap();
                self.prob.push_front(d);
            }
            return true;
        }
        self.win.push_front(k);
        if self.win.len() > self.win_cap {
            let cand = self.win.pop_back().unwrap();
            if self.prob.len() + self.prot.len() < self.main_cap {
                self.prob.push_front(cand);
            } else {
                let (victim, in_prob) = match self.prob.tail_key() {
                    Some(v) => (v, true),
                    None => (self.prot.tail_key().unwrap(), false),
                };
                if self.sk.est(cand) > self.sk.est(victim) {
                    if in_prob {
                        self.prob.remove(victim);
                    } else {
                        self.prot.remove(victim);
                    }
                    self.prob.push_front(cand);
                }
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(c: &mut dyn Cache, seq: &[u64]) -> Vec<bool> {
        seq.iter().map(|&k| c.access(k)).collect()
    }

    #[test]
    fn lru_basic() {
        let mut c = Lru::new(2);
        assert_eq!(run(&mut c, &[1, 2, 1, 3, 2, 1]), [false, false, true, false, false, false]);
    }

    #[test]
    fn sieve_paper_example() {
        // cap 3: 1 2 3 hit-2 insert-4: hand starts at tail (1, unvisited) -> evict 1.
        let mut c = Sieve::new(3);
        assert_eq!(run(&mut c, &[1, 2, 3, 2, 4, 1, 2]), [false, false, false, true, false, false, true]);
        // 2 was visited, so inserting 1 clears its bit and evicts 3 instead; 2 stays resident.
        assert!(c.l.idx(2).is_some() && c.l.idx(3).is_none());
    }

    #[test]
    fn s3fifo_scan_resistant() {
        let mut c = S3Fifo::new(20);
        let hot: Vec<u64> = (0..10).collect();
        for _ in 0..4 {
            run(&mut c, &hot);
        }
        let scan: Vec<u64> = (1000..1200).collect();
        run(&mut c, &scan);
        let r = run(&mut c, &hot);
        assert!(r.iter().filter(|&&h| h).count() >= 8, "hot set survived the scan: {r:?}");
        // LRU does not survive the same pattern.
        let mut l = Lru::new(20);
        for _ in 0..4 {
            run(&mut l, &hot);
        }
        run(&mut l, &scan);
        assert_eq!(run(&mut l, &hot).iter().filter(|&&h| h).count(), 0);
    }

    #[test]
    fn capacity_respected() {
        for name in POLICIES {
            let mut c = make(name, 50);
            let mut x = 12345u64;
            let mut hits = 0;
            for _ in 0..20000 {
                x = mix(x);
                hits += c.access(x % 200) as u32;
            }
            // uniform over 200 keys with cap 50 -> hit ratio around 25 %
            assert!((2500..7500).contains(&hits), "{name}: {hits}");
        }
    }

    #[test]
    fn wtinylfu_keeps_frequent_over_one_hit() {
        let mut c = WTinyLfu::new(100);
        let hot: Vec<u64> = (0..50).collect();
        for _ in 0..10 {
            run(&mut c, &hot);
        }
        let scan: Vec<u64> = (5000..5500).collect();
        run(&mut c, &scan);
        let r = run(&mut c, &hot);
        assert!(r.iter().filter(|&&h| h).count() >= 40, "{r:?}");
    }
}
