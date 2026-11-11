//! The radix Merkle map: mutable overlay, seal, lazy open, diff.
//!
//! Node hash: leaf = BLAKE3("LNXL" || canonical leaf bytes); interior at level
//! L = BLAKE3("LNXI" || L || child hashes). The child *locations* stored in an
//! interior node are hints and are deliberately NOT hashed, otherwise the root
//! would depend on pack layout and "identical content gives identical roots on
//! any backend" (ADR-8) would not hold. Leaves are level 0, the root is level
//! `depth-1`.
use crate::leaf::{wrap_dec, wrap_enc, Ent, LeafFmt, Lk, Wrap};
use crate::store::*;
use std::collections::HashMap;
use std::io;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

pub type Hash = [u8; 32];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Child {
    pub hash: Hash,
    pub loc: NodeLoc,
}

#[derive(Clone, Copy, Debug)]
pub struct Cfg {
    pub fan: usize,
    pub leaf: LeafFmt,
    pub interior_wrap: Wrap,
    pub nchunks: u64,
}
impl Cfg {
    pub fn nleaves(&self) -> u64 {
        self.nchunks.div_ceil(self.fan as u64)
    }
    /// Levels including the leaf level: ceil(log_fan(nchunks)), at least 2.
    pub fn depth(&self) -> usize {
        let mut d = 1;
        let mut cap = self.fan as u128;
        while cap < self.nchunks as u128 {
            cap *= self.fan as u128;
            d += 1;
        }
        d.max(2)
    }
    pub fn pow(&self, e: usize) -> u64 {
        (self.fan as u64).pow(e as u32)
    }
}

fn hash_leaf(raw: &[u8]) -> Hash {
    let mut h = blake3::Hasher::new();
    h.update(b"LNXL");
    h.update(raw);
    *h.finalize().as_bytes()
}
fn hash_interior(level: usize, kids: &[Child]) -> Hash {
    let mut h = blake3::Hasher::new();
    h.update(b"LNXI");
    h.update(&[level as u8]);
    for k in kids {
        h.update(&k.hash);
    }
    *h.finalize().as_bytes()
}
pub fn enc_interior(kids: &[Child]) -> Vec<u8> {
    let mut o = Vec::with_capacity(kids.len() * 48);
    for k in kids {
        o.extend_from_slice(&k.hash);
        o.extend_from_slice(&k.loc.pack.to_le_bytes());
        o.extend_from_slice(&k.loc.len.to_le_bytes());
        o.extend_from_slice(&k.loc.off.to_le_bytes());
    }
    o
}
fn dec_interior(b: &[u8], fan: usize) -> io::Result<Vec<Child>> {
    if b.len() != fan * 48 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "interior length",
        ));
    }
    Ok(b.chunks_exact(48)
        .map(|c| Child {
            hash: c[..32].try_into().unwrap(),
            loc: NodeLoc {
                pack: u32::from_le_bytes(c[32..36].try_into().unwrap()),
                len: u32::from_le_bytes(c[36..40].try_into().unwrap()),
                off: u64::from_le_bytes(c[40..48].try_into().unwrap()),
            },
        })
        .collect())
}
fn bad(s: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, s.to_string())
}

#[derive(Default, Clone, Debug)]
pub struct SealStats {
    pub leaves: u64,
    pub interior: u64,
    pub leaf_raw: u64,
    pub leaf_stored: u64,
    pub int_raw: u64,
    pub int_stored: u64,
    pub t_total: Duration,
    pub t_enc: Duration,
    pub t_hash: Duration,
    pub t_comp: Duration,
    pub t_write: Duration,
}
impl SealStats {
    pub fn raw(&self) -> u64 {
        self.leaf_raw + self.int_raw
    }
    pub fn stored(&self) -> u64 {
        self.leaf_stored + self.int_stored
    }
}

#[derive(Default, Clone, Debug)]
pub struct Counters {
    pub leaf_hits: u64,
    pub leaf_miss: u64,
    pub int_reads: u64,
}

pub struct Map {
    pub cfg: Cfg,
    pub store: Arc<PackStore>,
    pub root: Child,
    pub empty: Vec<Hash>,
    empty_node: Vec<Arc<Vec<Child>>>,
    pub interior: HashMap<Hash, Arc<Vec<Child>>>,
    pub leaves: Lru,
    pub overlay: HashMap<u64, Box<[Ent]>>,
    pub ctr: Counters,
}

pub struct Builder<'a> {
    map: &'a mut Map,
    w: &'a mut PackWriter,
    partial: Vec<Vec<Child>>,
    pub st: SealStats,
}

impl Map {
    /// Empty map (every chunk a hole). Nothing is stored.
    pub fn new_empty(cfg: Cfg, store: Arc<PackStore>, cache_bytes: usize) -> Map {
        let depth = cfg.depth();
        let mut empty = Vec::with_capacity(depth);
        let mut raw = Vec::new();
        cfg.leaf.enc_raw(&vec![Ent::default(); cfg.fan], &mut raw);
        empty.push(hash_leaf(&raw));
        let mut empty_node: Vec<Arc<Vec<Child>>> = vec![Arc::new(Vec::new())];
        for l in 1..depth {
            let kids = vec![
                Child {
                    hash: empty[l - 1],
                    loc: LOC_NONE
                };
                cfg.fan
            ];
            empty.push(hash_interior(l, &kids));
            empty_node.push(Arc::new(kids));
        }
        let root = Child {
            hash: empty[depth - 1],
            loc: LOC_NONE,
        };
        Map {
            cfg,
            store,
            root,
            empty,
            empty_node,
            interior: HashMap::new(),
            leaves: Lru::new(cache_bytes),
            overlay: HashMap::new(),
            ctr: Counters::default(),
        }
    }
    /// Open an existing root lazily: no node is read until needed.
    pub fn open(cfg: Cfg, store: Arc<PackStore>, root: Child, cache_bytes: usize) -> Map {
        let mut m = Map::new_empty(cfg, store, cache_bytes);
        m.root = root;
        m
    }
    pub fn depth(&self) -> usize {
        self.cfg.depth()
    }

    fn interior_node(&mut self, level: usize, c: &Child) -> io::Result<Arc<Vec<Child>>> {
        if c.hash == self.empty[level] {
            return Ok(self.empty_node[level].clone());
        }
        if let Some(n) = self.interior.get(&c.hash) {
            return Ok(n.clone());
        }
        let stored = self.store.read(c.loc)?;
        let raw = wrap_dec(self.cfg.interior_wrap, &stored, self.cfg.fan * 48 + 64)?;
        let kids = dec_interior(&raw, self.cfg.fan)?;
        if hash_interior(level, &kids) != c.hash {
            return Err(bad("interior hash mismatch"));
        }
        self.ctr.int_reads += 1;
        let a = Arc::new(kids);
        self.interior.insert(c.hash, a.clone());
        Ok(a)
    }
    /// Canonical leaf bytes through the bounded LRU; `None` for the all-hole leaf.
    fn leaf_raw(&mut self, c: &Child) -> io::Result<Option<Arc<Vec<u8>>>> {
        if c.hash == self.empty[0] {
            return Ok(None);
        }
        if let Some(v) = self.leaves.get(&c.hash) {
            self.ctr.leaf_hits += 1;
            return Ok(Some(v));
        }
        self.ctr.leaf_miss += 1;
        let stored = self.store.read(c.loc)?;
        let raw = wrap_dec(self.cfg.leaf.wrap, &stored, self.cfg.fan * 64)?;
        if hash_leaf(&raw) != c.hash {
            return Err(bad("leaf hash mismatch"));
        }
        let a = Arc::new(raw);
        self.leaves.put(c.hash, a.clone());
        Ok(Some(a))
    }
    /// Leaf-level child record for leaf index `li`, walking from the root.
    pub fn leaf_child(&mut self, li: u64) -> io::Result<Child> {
        let depth = self.depth();
        let mut node = self.root;
        for level in (1..depth).rev() {
            if node.hash == self.empty[level] {
                return Ok(Child {
                    hash: self.empty[0],
                    loc: LOC_NONE,
                });
            }
            let kids = self.interior_node(level, &node)?;
            let slot = ((li / self.cfg.pow(level - 1)) % self.cfg.fan as u64) as usize;
            node = kids[slot];
        }
        Ok(node)
    }

    /// Point lookup. `None` = hole.
    pub fn get(&mut self, ci: u64) -> io::Result<Option<Lk>> {
        let fan = self.cfg.fan as u64;
        let (li, j) = (ci / fan, (ci % fan) as usize);
        if let Some(l) = self.overlay.get(&li) {
            let e = &l[j];
            return Ok(if e.hole() {
                None
            } else {
                Some(Lk {
                    seg: e.seg,
                    off: e.off,
                    clen: e.clen,
                    idp: u64::from_le_bytes(e.id[..8].try_into().unwrap()),
                })
            });
        }
        let c = self.leaf_child(li)?;
        match self.leaf_raw(&c)? {
            None => Ok(None),
            Some(raw) => {
                let l = self.cfg.leaf.lk(&raw, j);
                Ok(if l.seg == 0 { None } else { Some(l) })
            }
        }
    }

    fn load_leaf(&mut self, li: u64) -> io::Result<Box<[Ent]>> {
        let c = self.leaf_child(li)?;
        let mut v = vec![Ent::default(); self.cfg.fan].into_boxed_slice();
        if let Some(raw) = self.leaf_raw(&c)? {
            self.cfg.leaf.dec_raw(&raw, &mut v);
        }
        Ok(v)
    }
    /// Write into the mutable overlay (copy-on-write of the leaf on first touch).
    pub fn set(&mut self, ci: u64, e: Ent) -> io::Result<()> {
        let fan = self.cfg.fan as u64;
        let (li, j) = (ci / fan, (ci % fan) as usize);
        if let Some(l) = self.overlay.get_mut(&li) {
            l[j] = e;
            return Ok(());
        }
        let mut l = self.load_leaf(li)?;
        l[j] = e;
        self.overlay.insert(li, l);
        Ok(())
    }

    /// Load every interior node (what a mounted volume keeps resident).
    pub fn warm_interior(&mut self) -> io::Result<u64> {
        fn rec(m: &mut Map, level: usize, c: Child) -> io::Result<u64> {
            if c.hash == m.empty[level] {
                return Ok(0);
            }
            let kids = m.interior_node(level, &c)?;
            let mut n = 1;
            if level > 1 {
                for k in kids.iter() {
                    n += rec(m, level - 1, *k)?;
                }
            }
            Ok(n)
        }
        let (d, r) = (self.depth(), self.root);
        rec(self, d - 1, r)
    }
    pub fn interior_bytes(&self) -> usize {
        self.interior.len() * self.cfg.fan * 48
    }

    /// Serialise dirty leaves, rehash the paths, append new nodes to `w`.
    pub fn seal(&mut self, w: &mut PackWriter) -> io::Result<SealStats> {
        let t0 = Instant::now();
        let mut st = SealStats::default();
        let mut dirty: Vec<(u64, Box<[Ent]>)> = self.overlay.drain().collect();
        dirty.sort_unstable_by_key(|d| d.0);
        let mut ups: Vec<(u64, Child)> = Vec::with_capacity(dirty.len());
        let mut raw = Vec::new();
        for (li, ents) in dirty {
            let old = self.leaf_child(li)?;
            if ents.iter().all(|e| e.hole()) {
                ups.push((
                    li,
                    Child {
                        hash: self.empty[0],
                        loc: LOC_NONE,
                    },
                ));
                continue;
            }
            let t = Instant::now();
            raw.clear();
            self.cfg.leaf.enc_raw(&ents, &mut raw);
            st.t_enc += t.elapsed();
            let t = Instant::now();
            let h = hash_leaf(&raw);
            st.t_hash += t.elapsed();
            if h == old.hash {
                ups.push((li, old));
                continue;
            }
            let t = Instant::now();
            let stored = wrap_enc(self.cfg.leaf.wrap, &raw);
            st.t_comp += t.elapsed();
            let loc = w.append(&stored);
            st.leaves += 1;
            st.leaf_raw += raw.len() as u64;
            st.leaf_stored += stored.len() as u64;
            ups.push((li, Child { hash: h, loc }));
        }
        let (d, r) = (self.depth(), self.root);
        if !ups.is_empty() {
            self.root = self.seal_node(d - 1, 0, r, &ups, w, &mut st)?;
        }
        w.finish();
        st.t_write = w.t_write;
        st.t_total = t0.elapsed();
        Ok(st)
    }

    fn seal_node(
        &mut self,
        level: usize,
        base: u64,
        old: Child,
        ups: &[(u64, Child)],
        w: &mut PackWriter,
        st: &mut SealStats,
    ) -> io::Result<Child> {
        let span = self.cfg.pow(level - 1);
        let mut kids: Vec<Child> = if old.hash == self.empty[level] {
            (*self.empty_node[level]).clone()
        } else {
            (*self.interior_node(level, &old)?).clone()
        };
        let mut i = 0;
        while i < ups.len() {
            let slot = ((ups[i].0 - base) / span) as usize;
            let mut j = i;
            while j < ups.len() && ((ups[j].0 - base) / span) as usize == slot {
                j += 1;
            }
            kids[slot] = if level == 1 {
                ups[i].1
            } else {
                self.seal_node(
                    level - 1,
                    base + slot as u64 * span,
                    kids[slot],
                    &ups[i..j],
                    w,
                    st,
                )?
            };
            i = j;
        }
        self.finish_node(level, kids, Some(old), w, st)
    }

    /// Hash, (maybe) write and register one interior node.
    fn finish_node(
        &mut self,
        level: usize,
        kids: Vec<Child>,
        old: Option<Child>,
        w: &mut PackWriter,
        st: &mut SealStats,
    ) -> io::Result<Child> {
        let t = Instant::now();
        let h = hash_interior(level, &kids);
        st.t_hash += t.elapsed();
        if h == self.empty[level] {
            return Ok(Child {
                hash: h,
                loc: LOC_NONE,
            });
        }
        if let Some(o) = old {
            if o.hash == h {
                return Ok(o);
            }
        }
        let t = Instant::now();
        let raw = enc_interior(&kids);
        let stored = wrap_enc(self.cfg.interior_wrap, &raw);
        st.t_comp += t.elapsed();
        let loc = w.append(&stored);
        st.interior += 1;
        st.int_raw += raw.len() as u64;
        st.int_stored += stored.len() as u64;
        self.interior.insert(h, Arc::new(kids));
        Ok(Child { hash: h, loc })
    }

    /// Changed leaf-index ranges between two roots of this tree shape, pruning
    /// equal subtrees. Returns (ranges, interior node reads).
    pub fn diff(&mut self, a: Child, b: Child) -> io::Result<(Vec<(u64, u64)>, u64)> {
        let mut leaves = Vec::new();
        let mut reads = 0u64;
        let d = self.depth();
        self.diff_rec(d - 1, 0, a, b, &mut leaves, &mut reads)?;
        let mut out: Vec<(u64, u64)> = Vec::new();
        for l in leaves {
            match out.last_mut() {
                Some(r) if r.1 == l => r.1 = l + 1,
                _ => out.push((l, l + 1)),
            }
        }
        Ok((out, reads))
    }
    fn diff_rec(
        &mut self,
        level: usize,
        base: u64,
        a: Child,
        b: Child,
        out: &mut Vec<u64>,
        reads: &mut u64,
    ) -> io::Result<()> {
        if a.hash == b.hash {
            return Ok(());
        }
        if level == 0 {
            out.push(base);
            return Ok(());
        }
        let e = self.empty[level];
        let na = self.interior_node(level, &a)?;
        let nb = self.interior_node(level, &b)?;
        *reads += (a.hash != e) as u64 + (b.hash != e) as u64;
        let span = self.cfg.pow(level - 1);
        for i in 0..self.cfg.fan {
            if na[i].hash != nb[i].hash {
                self.diff_rec(level - 1, base + i as u64 * span, na[i], nb[i], out, reads)?;
            }
        }
        Ok(())
    }
    /// Number of differing entries inside the given leaf ranges (reads leaves).
    pub fn diff_entries(&mut self, a: Child, b: Child, ranges: &[(u64, u64)]) -> io::Result<u64> {
        let saved = self.root;
        let mut n = 0;
        let fan = self.cfg.fan;
        let mut ea = vec![Ent::default(); fan];
        let mut eb = vec![Ent::default(); fan];
        for &(lo, hi) in ranges {
            for li in lo..hi {
                for (root, buf) in [(a, &mut ea), (b, &mut eb)] {
                    self.root = root;
                    let c = self.leaf_child(li)?;
                    buf.iter_mut().for_each(|e| *e = Ent::default());
                    if let Some(raw) = self.leaf_raw(&c)? {
                        self.cfg.leaf.dec_raw(&raw, buf);
                    }
                }
                n += ea.iter().zip(&eb).filter(|(x, y)| x != y).count() as u64;
            }
        }
        self.root = saved;
        Ok(n)
    }
}

impl<'a> Builder<'a> {
    pub fn new(map: &'a mut Map, w: &'a mut PackWriter) -> Self {
        let d = map.depth();
        Builder {
            map,
            w,
            partial: vec![Vec::new(); d],
            st: SealStats::default(),
        }
    }
    /// Push the next leaf (index order). `virt` = do not store the bytes, point
    /// at the model instead (10 TiB case); `ents = None` = all-hole leaf.
    pub fn push_leaf(
        &mut self,
        li: u64,
        ents: Option<&[Ent]>,
        virt: bool,
        raw: &mut Vec<u8>,
    ) -> io::Result<()> {
        let c = match ents {
            None => Child {
                hash: self.map.empty[0],
                loc: LOC_NONE,
            },
            Some(e) => {
                let t = Instant::now();
                raw.clear();
                self.map.cfg.leaf.enc_raw(e, raw);
                self.st.t_enc += t.elapsed();
                let t = Instant::now();
                let h = hash_leaf(raw);
                self.st.t_hash += t.elapsed();
                let t = Instant::now();
                let stored = wrap_enc(self.map.cfg.leaf.wrap, raw);
                self.st.t_comp += t.elapsed();
                self.st.leaves += 1;
                self.st.leaf_raw += raw.len() as u64;
                self.st.leaf_stored += stored.len() as u64;
                let loc = if virt {
                    NodeLoc {
                        pack: PACK_VIRT,
                        len: stored.len() as u32,
                        off: li,
                    }
                } else {
                    self.w.append(&stored)
                };
                Child { hash: h, loc }
            }
        };
        self.push(1, c)
    }
    /// Push a child destined for a level-`level` node.
    fn push(&mut self, level: usize, c: Child) -> io::Result<()> {
        self.partial[level].push(c);
        if self.partial[level].len() == self.map.cfg.fan {
            let kids = std::mem::take(&mut self.partial[level]);
            let node = self
                .map
                .finish_node(level, kids, None, self.w, &mut self.st)?;
            if level + 1 < self.partial.len() {
                self.push(level + 1, node)?;
            } else {
                self.map.root = node;
            }
        }
        Ok(())
    }
    /// Pad the partial nodes with empty children and return the root.
    pub fn finish(mut self) -> io::Result<(Child, SealStats)> {
        let depth = self.partial.len();
        for level in 1..depth {
            if self.partial[level].is_empty() {
                continue;
            }
            let fill = Child {
                hash: self.map.empty[level - 1],
                loc: LOC_NONE,
            };
            while self.partial[level].len() < self.map.cfg.fan {
                self.partial[level].push(fill);
            }
            let kids = std::mem::take(&mut self.partial[level]);
            let node = self
                .map
                .finish_node(level, kids, None, self.w, &mut self.st)?;
            if level + 1 < depth {
                self.partial[level + 1].push(node);
            } else {
                self.map.root = node;
            }
        }
        self.w.finish();
        self.st.t_write = self.w.t_write;
        Ok((self.map.root, self.st))
    }
}

pub fn gets(s: &PackStore) -> u64 {
    s.gets.load(Ordering::Relaxed)
}
