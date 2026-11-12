//! [`RadixMap`]: the tree, its caches, sealing and the walks.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard};

use lunix_format::commit::MapRoot;
use lunix_format::extent::MAX_NODE_ULEN;
use lunix_format::node::{
    child_slot, empty_hashes, fanout_bits, levels_for, Child, DataEntry, Entry, Interior, Leaf,
    LeafParams, LeafView, NodeLoc, MAX_DATA_CLEN, MAX_LEVEL,
};
use lunix_format::{ChunkId, Codec, ExtentKind, Level};
use lunix_iface::crypto::{EncodingDesc, ExtentCodec, OpenExpect, OpenOutput};
use lunix_iface::map::{
    BlockMap, Cached, ChunkNo, MapTxn, NodeHash, NodeRef, NodeSource, SealResult,
};
use lunix_iface::store::PackWrite;
use lunix_iface::{Error, Result};

use crate::config::MapConfig;
use crate::fastleaf::FastLeaf;
use crate::lru::{Hit, KeyBuild, Lru};
use crate::txn::Txn;

pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

pub(crate) fn corrupt(detail: &'static str) -> Error {
    Error::Corrupt {
        what: "map node",
        detail,
    }
}

/// A dirty leaf, shared with a running seal.
pub(crate) type Dirty = Arc<Leaf>;

/// Everything behind the state mutex. No lock is held across I/O.
pub(crate) struct Inner {
    pub(crate) chunk_count: u64,
    /// Levels of the current shape.
    pub(crate) levels: u8,
    /// Levels of the tree `root` names (smaller than `levels` after a `grow`
    /// that deepened the tree and before the next seal wraps the root).
    pub(crate) sealed_levels: u8,
    pub(crate) root: MapRoot,
    pub(crate) overlay: HashMap<u64, Dirty>,
    pub(crate) txn_open: bool,
    interior: HashMap<NodeHash, Arc<Interior>, KeyBuild>,
    leaves: Lru,
}

impl Inner {
    fn root_child(&self) -> Child {
        Child {
            hash: self.root.hash,
            loc: root_loc(&self.root),
        }
    }
}

fn root_loc(root: &MapRoot) -> Option<NodeLoc> {
    root.pack.map(|pack| NodeLoc {
        pack,
        off: root.off,
        len: root.len,
    })
}

/// Sizes of the caches, for tests and the stats the engine may export.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CacheStats {
    /// Encoded leaves cached.
    pub cached_leaves: usize,
    /// Bytes charged to `map_cache_max` (encodings, decoded tier, overhead).
    pub cached_bytes: usize,
    /// Leaves that also have a decoded form.
    pub decoded_leaves: usize,
    /// Interior nodes resident.
    pub resident_interior: usize,
}

/// Where a lookup ended.
pub(crate) enum Located {
    Hole,
    Decoded(Arc<Leaf>),
    Encoded {
        bytes: Arc<[u8]>,
        hash: NodeHash,
        promote: bool,
    },
}

enum Step {
    Done(Located),
    Need(Child, u8),
}

/// `fanout^level` leaves, saturating.
fn leaves_under(fanout: u16, level: u8) -> u64 {
    u64::from(fanout)
        .checked_pow(u32::from(level))
        .unwrap_or(u64::MAX)
}

/// Leaves under one child of a node at `level` (>= 1).
fn span(fanout: u16, level: u8) -> u64 {
    leaves_under(fanout, level - 1)
}

/// Scratch and results of one seal.
struct SealCtx<'a> {
    packs: &'a mut dyn PackWrite,
    buf: Vec<u8>,
    ext: Vec<u8>,
    leaves_written: u64,
    bytes: u64,
    retired: Vec<NodeHash>,
    fresh: Vec<(NodeHash, Arc<Interior>)>,
}

/// The shape of the tree being rebuilt.
struct Shape {
    /// Level of the sealed root.
    sl: u8,
    root_child: Child,
}

/// The radix Merkle map. See the [crate docs](crate).
pub struct RadixMap {
    codec: Arc<dyn ExtentCodec>,
    source: Arc<dyn NodeSource>,
    cfg: MapConfig,
    params: LeafParams,
    empty: Vec<NodeHash>,
    inner: Mutex<Inner>,
    /// Serialises the mutators (transaction commit, seal, grow); readers never
    /// take it.
    writer: Mutex<()>,
}

impl RadixMap {
    /// An empty map of `chunk_count` chunks.
    ///
    /// # Errors
    /// `Invalid` for a fanout other than 256 or 512, or a size beyond 15
    /// levels.
    pub fn new(
        codec: Arc<dyn ExtentCodec>,
        source: Arc<dyn NodeSource>,
        chunk_count: u64,
        cfg: MapConfig,
    ) -> Result<Self> {
        fanout_bits(cfg.fanout).map_err(|_| Error::invalid("map_fanout must be 256 or 512"))?;
        let levels = levels_of(chunk_count, cfg.fanout)?;
        let empty = empty_hashes(cfg.fanout, MAX_LEVEL, |b| {
            codec.chunk_id(ExtentKind::Node, b).0
        })?;
        let params = LeafParams {
            fanout: cfg.fanout,
            id_len: cfg.id_len,
            ovh: codec.mode().extent_ovh(),
        };
        let decoded_cost = usize::from(cfg.fanout) * crate::lru::entry_size() + 64;
        let root = MapRoot::empty(empty[usize::from(levels) - 1]);
        let inner = Inner {
            chunk_count,
            levels,
            sealed_levels: levels,
            root,
            overlay: HashMap::new(),
            txn_open: false,
            interior: HashMap::default(),
            leaves: Lru::new(cfg.map_cache_max, cfg.decoded_tier_leaves, decoded_cost),
        };
        Ok(Self {
            codec,
            source,
            cfg,
            params,
            empty,
            inner: Mutex::new(inner),
            writer: Mutex::new(()),
        })
    }

    /// Opens the map whose last sealed root is `root`, for a volume of
    /// `chunk_count` chunks (the commit's `size`). No node is read until it is
    /// needed.
    ///
    /// # Errors
    /// `Invalid` as for [`RadixMap::new`]; `Corrupt` when an empty root's hash
    /// is not the empty hash of any level this size allows.
    pub fn open(
        codec: Arc<dyn ExtentCodec>,
        source: Arc<dyn NodeSource>,
        root: &MapRoot,
        chunk_count: u64,
        cfg: MapConfig,
    ) -> Result<Self> {
        let map = Self::new(codec, source, chunk_count, cfg)?;
        {
            let mut g = lock(&map.inner);
            let levels = g.levels;
            g.sealed_levels = match root.pack {
                // A stored root is checked when it is first read.
                Some(_) => levels,
                None => {
                    let k = (1..levels)
                        .find(|&k| map.empty[usize::from(k)] == root.hash)
                        .ok_or(corrupt("empty root hash does not match the volume size"))?;
                    k + 1
                }
            };
            g.root = *root;
        }
        Ok(map)
    }

    /// The configuration.
    pub fn config(&self) -> &MapConfig {
        &self.cfg
    }

    /// The hash of the empty node of `level` (`E_level`).
    pub fn empty_hash(&self, level: u8) -> Option<NodeHash> {
        self.empty.get(usize::from(level)).copied()
    }

    /// The number of levels (leaves included) of the current shape.
    pub fn levels(&self) -> u8 {
        lock(&self.inner).levels
    }

    /// Loads every interior node of the committed tree and keeps it resident,
    /// as a mounted volume does ("interior nodes are always resident");
    /// returns how many were loaded. Leaves are not touched.
    ///
    /// # Errors
    /// Whatever reading and authenticating a node returns.
    pub fn warm_interior(&self) -> Result<u64> {
        let (root, sl) = {
            let g = lock(&self.inner);
            (g.root_child(), g.sealed_levels - 1)
        };
        let mut n = 0;
        self.warm_rec(&root, sl, &mut n)?;
        Ok(n)
    }

    fn warm_rec(&self, child: &Child, level: u8, n: &mut u64) -> Result<()> {
        if level == 0 || child.hash == self.empty[usize::from(level)] {
            return Ok(());
        }
        let node = self.interior(child, level)?;
        *n += 1;
        for c in &node.children {
            self.warm_rec(c, level - 1, n)?;
        }
        Ok(())
    }

    /// Cache sizes.
    pub fn cache_stats(&self) -> CacheStats {
        let g = lock(&self.inner);
        CacheStats {
            cached_leaves: g.leaves.len(),
            cached_bytes: g.leaves.charged(),
            decoded_leaves: g.leaves.decoded_len(),
            resident_interior: g.interior.len(),
        }
    }

    pub(crate) fn fanout(&self) -> usize {
        usize::from(self.cfg.fanout)
    }

    pub(crate) fn dirty_cap(&self) -> u64 {
        self.cfg.dirty_leaf_cap
    }

    pub(crate) fn id_len(&self) -> lunix_format::IdLen {
        self.cfg.id_len
    }

    pub(crate) fn writer(&self) -> MutexGuard<'_, ()> {
        lock(&self.writer)
    }

    pub(crate) fn inner(&self) -> MutexGuard<'_, Inner> {
        lock(&self.inner)
    }

    // ---- node I/O --------------------------------------------------------

    /// Reads, authenticates and opens the node extent at `loc` whose id must
    /// be `hash`; returns the plaintext.
    fn read_plain(&self, hash: &NodeHash, loc: NodeLoc) -> Result<Vec<u8>> {
        let mut ext = Vec::with_capacity(loc.len as usize);
        self.source.load_node(loc, &mut ext)?;
        let id = ChunkId(*hash);
        let expect = OpenExpect {
            chunk_size: u32::try_from(MAX_NODE_ULEN).unwrap_or(u32::MAX),
            chunk_id: Some(&id),
            id_prefix: None,
            verify_id: false,
            output: OpenOutput::Plaintext,
        };
        let mut body = Vec::new();
        let meta = self
            .codec
            .open(ExtentKind::Node, &ext, &expect, &mut body)?;
        // R2: nodes are stored raw, and the header id is the node hash (the
        // caller then recomputes the hash from the decoded content).
        if meta.desc.codec != Codec::Raw {
            return Err(corrupt("node extent is not codec 0"));
        }
        if meta.chunk_id != id {
            return Err(Error::Auth {
                what: "map node id",
            });
        }
        Ok(body)
    }

    fn check_hash(&self, input: &[u8], want: &NodeHash) -> Result<()> {
        if self.codec.chunk_id(ExtentKind::Node, input).0 == *want {
            Ok(())
        } else {
            Err(Error::Auth {
                what: "map node hash",
            })
        }
    }

    /// Reads an interior node of `level` (any level when `None`) and checks it
    /// against `child.hash`.
    fn read_interior(&self, child: &Child, level: Option<u8>) -> Result<Interior> {
        let loc = child.loc.ok_or(corrupt("child without a location"))?;
        let body = self.read_plain(&child.hash, loc)?;
        let node = Interior::decode(&body, self.cfg.fanout)?;
        if level.is_some_and(|l| l != node.level) {
            return Err(corrupt("node level does not match its position"));
        }
        self.check_hash(&node.hash_input()?, &child.hash)?;
        let below = self.empty[usize::from(node.level) - 1];
        if node
            .children
            .iter()
            .any(|c| c.loc.is_none() && c.hash != below)
        {
            return Err(corrupt("non-empty child without a location"));
        }
        Ok(node)
    }

    /// An interior node for a walk: resident when it is, read (not cached)
    /// when it is not.
    fn interior_for_walk(&self, child: &Child, level: Option<u8>) -> Result<Arc<Interior>> {
        if let Some(n) = lock(&self.inner).interior.get(&child.hash) {
            return Ok(Arc::clone(n));
        }
        Ok(Arc::new(self.read_interior(child, level)?))
    }

    fn load_interior(&self, child: &Child, level: u8) -> Result<Arc<Interior>> {
        let node = Arc::new(self.read_interior(child, Some(level))?);
        lock(&self.inner)
            .interior
            .insert(child.hash, Arc::clone(&node));
        Ok(node)
    }

    /// Reads a leaf's plaintext, checks its hash and layout.
    fn read_leaf_bytes(&self, child: &Child) -> Result<Vec<u8>> {
        let loc = child.loc.ok_or(corrupt("child without a location"))?;
        let body = self.read_plain(&child.hash, loc)?;
        self.check_hash(&body, &child.hash)?;
        LeafView::parse(&body, &self.params)?;
        Ok(body)
    }

    fn load_leaf_bytes(&self, child: &Child) -> Result<Arc<[u8]>> {
        let bytes: Arc<[u8]> = Arc::from(self.read_leaf_bytes(child)?);
        lock(&self.inner).leaves.put(child.hash, Arc::clone(&bytes));
        Ok(bytes)
    }

    // ---- lookups ---------------------------------------------------------

    /// Walks the resident part of the tree to leaf `li`.
    fn resolve(&self, g: &mut Inner, li: u64) -> Result<Step> {
        let sl = g.sealed_levels - 1;
        if li >= leaves_under(self.cfg.fanout, sl) {
            return Ok(Step::Done(Located::Hole));
        }
        let mut node = g.root_child();
        let mut level = sl;
        while level >= 1 {
            if node.hash == self.empty[usize::from(level)] {
                return Ok(Step::Done(Located::Hole));
            }
            let Some(int) = g.interior.get(&node.hash) else {
                return Ok(Step::Need(node, level));
            };
            let slot = child_slot(li, level, self.cfg.fanout)?;
            node = *int
                .children
                .get(slot)
                .ok_or(corrupt("child slot past the node"))?;
            level -= 1;
        }
        if node.hash == self.empty[0] {
            return Ok(Step::Done(Located::Hole));
        }
        Ok(Step::Done(match g.leaves.get(&node.hash) {
            Some(Hit::Decoded(d)) => Located::Decoded(d),
            Some(Hit::Encoded { bytes, promote }) => Located::Encoded {
                bytes,
                hash: node.hash,
                promote,
            },
            None => return Ok(Step::Need(node, 0)),
        }))
    }

    /// Finds leaf `li` of the committed tree (not the overlay), loading what
    /// is missing.
    pub(crate) fn locate(&self, li: u64) -> Result<Located> {
        for _ in 0..(4 * usize::from(MAX_LEVEL) + 8) {
            let step = {
                let mut g = lock(&self.inner);
                self.resolve(&mut g, li)?
            };
            match step {
                Step::Done(l) => return Ok(l),
                Step::Need(child, 0) => {
                    let bytes = self.load_leaf_bytes(&child)?;
                    return Ok(Located::Encoded {
                        bytes,
                        hash: child.hash,
                        promote: false,
                    });
                }
                Step::Need(child, level) => {
                    self.load_interior(&child, level)?;
                }
            }
        }
        Err(Error::Busy {
            what: "map node cache",
        })
    }

    fn entry_of(&self, loc: Located, slot: usize) -> Result<Option<DataEntry>> {
        match loc {
            Located::Hole => Ok(None),
            Located::Decoded(d) => Ok(data(d.entries.get(slot).copied())),
            Located::Encoded {
                bytes,
                hash,
                promote,
            } => {
                if promote {
                    if let Ok(leaf) = Leaf::decode(&bytes, &self.params) {
                        let d = Arc::new(leaf);
                        lock(&self.inner)
                            .leaves
                            .attach_decoded(&hash, Arc::clone(&d));
                        return Ok(data(d.entries.get(slot).copied()));
                    }
                }
                let view = FastLeaf::parse(&bytes, &self.params)?;
                Ok(data(Some(view.get(slot)?)))
            }
        }
    }

    /// The decoded entries of leaf `li` of the committed tree.
    pub(crate) fn leaf_entries(&self, li: u64) -> Result<Vec<Entry>> {
        match self.locate(li)? {
            Located::Hole => Ok(vec![Entry::Hole; self.fanout()]),
            Located::Decoded(d) => Ok(d.entries.clone()),
            Located::Encoded { bytes, .. } => Ok(Leaf::decode(&bytes, &self.params)?.entries),
        }
    }

    fn split(&self, ci: u64) -> (u64, usize) {
        let f = u64::from(self.cfg.fanout);
        (ci / f, (ci % f) as usize)
    }

    // ---- sealing ---------------------------------------------------------

    /// Seals `ctx.buf` as the node extent of `hash` and appends it.
    fn seal_extent(&self, ctx: &mut SealCtx<'_>, hash: &NodeHash) -> Result<NodeLoc> {
        let ulen = u32::try_from(ctx.buf.len()).map_err(|_| Error::invalid("node too long"))?;
        let desc = EncodingDesc {
            codec: Codec::Raw,
            level: Level(0),
            ulen,
            dict_id: None,
        };
        ctx.ext.clear();
        self.codec.seal(
            ExtentKind::Node,
            &ChunkId(*hash),
            &desc,
            &ctx.buf,
            &mut ctx.ext,
        )?;
        let loc = ctx.packs.append_node(hash, &ctx.ext)?;
        ctx.bytes += u64::from(loc.len);
        Ok(loc)
    }

    fn seal_leaf(&self, ctx: &mut SealCtx<'_>, leaf: &Dirty, old: Child) -> Result<Child> {
        let empty = Child {
            hash: self.empty[0],
            loc: None,
        };
        if leaf.is_empty() {
            return Ok(empty);
        }
        ctx.buf.clear();
        leaf.encode(&self.params, &mut ctx.buf)?;
        let hash = self.codec.chunk_id(ExtentKind::Node, &ctx.buf).0;
        if hash == old.hash && old.loc.is_some() {
            return Ok(old);
        }
        let loc = self.seal_extent(ctx, &hash)?;
        ctx.leaves_written += 1;
        lock(&self.inner)
            .leaves
            .put(hash, Arc::from(ctx.buf.as_slice()));
        Ok(Child {
            hash,
            loc: Some(loc),
        })
    }

    fn empty_kids(&self, level: u8) -> Vec<Child> {
        vec![
            Child {
                hash: self.empty[usize::from(level) - 1],
                loc: None,
            };
            self.fanout()
        ]
    }

    /// Rebuilds the node at `level` covering leaves from `base`, applying
    /// `ups` (sorted, all inside the node's range). `old` is the node of the
    /// sealed tree at this position, `None` above the sealed root (levels a
    /// `grow` added, which exist only in memory until now).
    fn rebuild(
        &self,
        ctx: &mut SealCtx<'_>,
        shape: &Shape,
        level: u8,
        base: u64,
        old: Option<Child>,
        ups: &[(u64, &Dirty)],
    ) -> Result<Child> {
        let sp = span(self.cfg.fanout, level);
        let mut kids: Vec<Child> = match old {
            Some(c) if c.hash == self.empty[usize::from(level)] => self.empty_kids(level),
            Some(c) => self.interior(&c, level)?.children.clone(),
            None => {
                let mut k = self.empty_kids(level);
                if level == shape.sl + 1 && base == 0 {
                    k[0] = shape.root_child;
                }
                k
            }
        };
        let slot_of = |li: u64| ((li - base) / sp) as usize;
        let mut slot0_done = false;
        let mut i = 0;
        while i < ups.len() {
            let slot = slot_of(ups[i].0);
            let mut j = i + 1;
            while j < ups.len() && slot_of(ups[j].0) == slot {
                j += 1;
            }
            slot0_done |= slot == 0;
            kids[slot] = if level == 1 {
                self.seal_leaf(ctx, ups[i].1, kids[slot])?
            } else {
                let child_old = (level - 1 <= shape.sl).then_some(kids[slot]);
                self.rebuild(
                    ctx,
                    shape,
                    level - 1,
                    base + slot as u64 * sp,
                    child_old,
                    &ups[i..j],
                )?
            };
            i = j;
        }
        if !slot0_done && level > shape.sl + 1 && base == 0 {
            // The chain of wrapper nodes above the old root.
            kids[0] = self.rebuild(ctx, shape, level - 1, 0, None, &[])?;
        }
        let node = Interior {
            level,
            children: kids,
        };
        let hash = self.codec.chunk_id(ExtentKind::Node, &node.hash_input()?).0;
        if hash == self.empty[usize::from(level)] {
            return Ok(Child { hash, loc: None });
        }
        if let Some(o) = old {
            if o.hash == hash && o.loc.is_some() {
                return Ok(o);
            }
            ctx.retired.push(o.hash);
        }
        ctx.buf.clear();
        node.encode(&mut ctx.buf)?;
        let loc = self.seal_extent(ctx, &hash)?;
        ctx.fresh.push((hash, Arc::new(node)));
        Ok(Child {
            hash,
            loc: Some(loc),
        })
    }

    /// An interior node of the committed tree, resident or loaded (cached).
    fn interior(&self, child: &Child, level: u8) -> Result<Arc<Interior>> {
        if let Some(n) = lock(&self.inner).interior.get(&child.hash) {
            return Ok(Arc::clone(n));
        }
        self.load_interior(child, level)
    }

    // ---- walks -----------------------------------------------------------

    fn walk_nodes_rec(
        &self,
        node: &Interior,
        skip: &HashSet<NodeHash>,
        f: &mut dyn FnMut(&NodeRef),
    ) -> Result<()> {
        let below = node.level - 1;
        for c in &node.children {
            if c.hash == self.empty[usize::from(below)] || skip.contains(&c.hash) {
                continue;
            }
            let loc = c.loc.ok_or(corrupt("non-empty child without a location"))?;
            f(&NodeRef {
                hash: c.hash,
                level: below,
                loc,
            });
            if below >= 1 {
                let n = self.interior_for_walk(c, Some(below))?;
                self.walk_nodes_rec(&n, skip, f)?;
            }
        }
        Ok(())
    }

    fn walk_leaves_rec(
        &self,
        node: &Interior,
        base: u64,
        skip: &HashSet<NodeHash>,
        f: &mut dyn FnMut(ChunkNo, &DataEntry),
    ) -> Result<()> {
        let below = node.level - 1;
        let sp = span(self.cfg.fanout, node.level);
        let fan = u64::from(self.cfg.fanout);
        for (i, c) in node.children.iter().enumerate() {
            if c.hash == self.empty[usize::from(below)] || skip.contains(&c.hash) {
                continue;
            }
            let at = base + i as u64 * sp;
            if below >= 1 {
                let n = self.interior_for_walk(c, Some(below))?;
                self.walk_leaves_rec(&n, at, skip, f)?;
                continue;
            }
            let body = self.read_leaf_bytes(c)?;
            let leaf = Leaf::decode(&body, &self.params)?;
            for (slot, e) in leaf.entries.iter().enumerate() {
                if let Entry::Data(d) = e {
                    f(ChunkNo(at * fan + slot as u64), d);
                }
            }
        }
        Ok(())
    }

    /// The root node of `root` for a walk; `None` for an empty or skipped
    /// root.
    fn walk_root(
        &self,
        root: &MapRoot,
        skip: &HashSet<NodeHash>,
    ) -> Result<Option<(Arc<Interior>, NodeLoc)>> {
        let Some(loc) = root_loc(root) else {
            return Ok(None);
        };
        if skip.contains(&root.hash) {
            return Ok(None);
        }
        let child = Child {
            hash: root.hash,
            loc: Some(loc),
        };
        Ok(Some((self.interior_for_walk(&child, None)?, loc)))
    }
}

fn data(e: Option<Entry>) -> Option<DataEntry> {
    match e {
        Some(Entry::Data(d)) => Some(d),
        _ => None,
    }
}

fn levels_of(chunk_count: u64, fanout: u16) -> Result<u8> {
    levels_for(chunk_count, 1, fanout).map_err(|_| Error::invalid("map too large"))
}

impl BlockMap for RadixMap {
    fn chunk_count(&self) -> u64 {
        lock(&self.inner).chunk_count
    }

    fn get(&self, ci: ChunkNo) -> Result<Option<DataEntry>> {
        let (li, slot) = self.split(ci.0);
        {
            let g = lock(&self.inner);
            if ci.0 >= g.chunk_count {
                return Err(Error::invalid("chunk past the end of the map"));
            }
            if let Some(d) = g.overlay.get(&li) {
                return Ok(data(d.entries.get(slot).copied()));
            }
        }
        let loc = self.locate(li)?;
        self.entry_of(loc, slot)
    }

    fn try_get_cached(&self, ci: ChunkNo) -> Cached {
        let (li, slot) = self.split(ci.0);
        let loc = {
            let mut g = lock(&self.inner);
            if ci.0 >= g.chunk_count {
                return Cached::Miss;
            }
            if let Some(d) = g.overlay.get(&li) {
                return match data(d.entries.get(slot).copied()) {
                    Some(e) => Cached::Hit(e),
                    None => Cached::Hole,
                };
            }
            match self.resolve(&mut g, li) {
                Ok(Step::Done(l)) => l,
                _ => return Cached::Miss,
            }
        };
        // Parsing happens outside the lock.
        let loc = match loc {
            Located::Encoded { bytes, hash, .. } => Located::Encoded {
                bytes,
                hash,
                promote: false,
            },
            other => other,
        };
        match self.entry_of(loc, slot) {
            Ok(Some(e)) => Cached::Hit(e),
            Ok(None) => Cached::Hole,
            Err(_) => Cached::Miss,
        }
    }

    fn begin_update(&self) -> Result<Box<dyn MapTxn + '_>> {
        let mut g = lock(&self.inner);
        if g.txn_open {
            return Err(Error::Busy {
                what: "map transaction",
            });
        }
        if g.overlay.len() as u64 >= self.cfg.dirty_leaf_cap {
            return Err(Error::MapFull);
        }
        g.txn_open = true;
        Ok(Box::new(Txn::new(self)))
    }

    fn seal(&self, packs: &mut dyn PackWrite) -> Result<SealResult> {
        let _w = self.writer();
        let (dirty, levels, sealed_levels, root, root_child) = {
            let g = lock(&self.inner);
            let mut dirty: Vec<(u64, Dirty)> = g
                .overlay
                .iter()
                .map(|(li, d)| (*li, Arc::clone(d)))
                .collect();
            dirty.sort_unstable_by_key(|d| d.0);
            (dirty, g.levels, g.sealed_levels, g.root, g.root_child())
        };
        if dirty.is_empty() && levels == sealed_levels {
            return Ok(SealResult {
                root,
                new_packs: Vec::new(),
                dirty_leaves: 0,
                bytes: 0,
            });
        }
        let mut ctx = SealCtx {
            packs,
            buf: Vec::with_capacity(32 << 10),
            ext: Vec::with_capacity(32 << 10),
            leaves_written: 0,
            bytes: 0,
            retired: Vec::new(),
            fresh: Vec::new(),
        };
        let shape = Shape {
            sl: sealed_levels - 1,
            root_child,
        };
        let ups: Vec<(u64, &Dirty)> = dirty.iter().map(|(li, d)| (*li, d)).collect();
        let old = (levels == sealed_levels).then_some(root_child);
        let top = self.rebuild(&mut ctx, &shape, levels - 1, 0, old, &ups)?;
        let new_root = match top.loc {
            Some(l) => MapRoot {
                hash: top.hash,
                pack: Some(l.pack),
                off: l.off,
                len: l.len,
            },
            None => MapRoot::empty(top.hash),
        };
        {
            let mut g = lock(&self.inner);
            g.root = new_root;
            g.sealed_levels = levels;
            g.overlay.clear();
            for h in &ctx.retired {
                g.interior.remove(h);
            }
            for (h, n) in ctx.fresh.drain(..) {
                g.interior.insert(h, n);
            }
        }
        let wrote = ctx.bytes > 0;
        Ok(SealResult {
            root: new_root,
            new_packs: if wrote {
                vec![ctx.packs.id()]
            } else {
                Vec::new()
            },
            dirty_leaves: ctx.leaves_written,
            bytes: ctx.bytes,
        })
    }

    fn grow(&self, new_chunk_count: u64) -> Result<()> {
        let _w = self.writer();
        let mut g = lock(&self.inner);
        if new_chunk_count < g.chunk_count {
            return Err(Error::invalid("cannot shrink"));
        }
        let levels = levels_of(new_chunk_count, self.cfg.fanout)?;
        g.chunk_count = new_chunk_count;
        g.levels = levels.max(g.levels);
        Ok(())
    }

    fn root(&self) -> MapRoot {
        lock(&self.inner).root
    }

    fn dirty_leaf_count(&self) -> u64 {
        lock(&self.inner).overlay.len() as u64
    }

    fn walk_leaves_skipping(
        &self,
        root: &MapRoot,
        skip: &HashSet<NodeHash>,
        f: &mut dyn FnMut(ChunkNo, &DataEntry),
    ) -> Result<()> {
        match self.walk_root(root, skip)? {
            Some((node, _)) => self.walk_leaves_rec(&node, 0, skip, f),
            None => Ok(()),
        }
    }

    fn walk_nodes(&self, root: &MapRoot, f: &mut dyn FnMut(&NodeRef)) -> Result<()> {
        self.walk_nodes_skipping(root, &HashSet::new(), f)
    }

    fn walk_nodes_skipping(
        &self,
        root: &MapRoot,
        skip: &HashSet<NodeHash>,
        f: &mut dyn FnMut(&NodeRef),
    ) -> Result<()> {
        let Some((node, loc)) = self.walk_root(root, skip)? else {
            return Ok(());
        };
        f(&NodeRef {
            hash: root.hash,
            level: node.level,
            loc,
        });
        self.walk_nodes_rec(&node, skip, f)
    }
}

/// Entry validity a leaf can encode (checked when it is set, not at seal).
pub(crate) fn check_entry(e: &DataEntry, id_len: lunix_format::IdLen) -> Result<()> {
    if e.lineage.0 != 0 {
        return Err(Error::invalid("lineage_idx must be 0 in v1"));
    }
    if e.prefix.id_len() != id_len {
        return Err(Error::invalid("id prefix length differs from the map's"));
    }
    if e.clen == 0 || e.clen > MAX_DATA_CLEN {
        return Err(Error::invalid("clen out of range"));
    }
    Ok(())
}
