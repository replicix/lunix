//! [`FlatMap`]: a block map with one flat level of leaves. The oracle for
//! `lunix-map` (M5).
//!
//! The content is a `BTreeMap<chunk, entry>`; sealing groups it into leaves of
//! [`LEAF_CHUNKS`] chunks and one root node listing the non-empty leaves. Nodes
//! are real extents (sealed by the [`ExtentCodec`] you pass) appended to the
//! [`PackWrite`] given to `seal`, so old roots stay readable through the
//! [`PackRead`] and a real `lunix-store` works behind it.
//!
//! What it deliberately is not: the radix Merkle tree. Its root hash is a
//! function of the size and the leaf hashes only (never of pack locations or
//! update order), which is all the [`BlockMap`] contract asks, but it will not
//! equal the root hash of `lunix-map` for the same content. Compare content
//! (`get`, `walk_leaves`), not root hashes, across implementations. The root
//! node has to fit a map node (64 KiB), which caps a FlatMap at about 1 100
//! non-empty leaves (about 560 000 populated chunks); `seal` fails `Invalid`
//! beyond that.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::{Arc, Mutex, MutexGuard};

use lunix_format::commit::MapRoot;
use lunix_format::node::{DataEntry, NodeLoc};
use lunix_format::{extent::MAX_NODE_ULEN, Codec};
use lunix_format::{ChunkId, ExtentKind, Hint, IdLen, IdPrefix, Level, LineageIdx, ObjId};
use lunix_iface::crypto::{EncodingDesc, ExtentCodec, OpenExpect, OpenOutput};
use lunix_iface::map::{
    entry_hint, BlockMap, Cached, ChunkNo, MapTxn, NodeHash, NodeRef, SealResult, TxnReport,
};
use lunix_iface::store::{PackRead, PackWrite};
use lunix_iface::{Error, Result};

/// Chunks per leaf.
pub const LEAF_CHUNKS: u64 = 512;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

fn corrupt(detail: &'static str) -> Error {
    Error::Corrupt {
        what: "flat map node",
        detail,
    }
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let (a, b) = self.0.split_at_checked(n).ok_or(corrupt("short"))?;
        self.0 = b;
        Ok(a)
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().expect("4")))
    }
    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().expect("8")))
    }
    fn finish(self) -> Result<()> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(corrupt("trailing bytes"))
        }
    }
}

fn encode_entry(ci: u64, e: &DataEntry, out: &mut Vec<u8>) {
    out.extend_from_slice(&ci.to_le_bytes());
    out.push(e.lineage.0);
    out.extend_from_slice(&e.seg.raw().to_le_bytes());
    out.extend_from_slice(&e.off.to_le_bytes());
    out.extend_from_slice(&e.clen.to_le_bytes());
    let p = e.prefix.as_bytes();
    out.push(u8::try_from(p.len()).expect("prefix is at most 32 bytes"));
    out.extend_from_slice(p);
}

fn decode_entry(r: &mut Reader<'_>, id_len: IdLen) -> Result<(u64, DataEntry)> {
    let ci = r.u64()?;
    let lineage = LineageIdx(r.u8()?);
    let seg = ObjId::from_raw(r.u64()?)?;
    let off = r.u32()?;
    let clen = r.u32()?;
    let n = usize::from(r.u8()?);
    let prefix = IdPrefix::from_slice(id_len, r.take(n)?)?;
    Ok((
        ci,
        DataEntry {
            lineage,
            seg,
            off,
            clen,
            prefix,
        },
    ))
}

fn leaf_body(entries: &[(u64, DataEntry)]) -> Vec<u8> {
    let n = u32::try_from(entries.len()).expect("a leaf has at most 512 entries");
    let mut out = n.to_le_bytes().to_vec();
    for (ci, e) in entries {
        encode_entry(*ci, e, &mut out);
    }
    out
}

type Leaves = BTreeMap<u64, (NodeHash, NodeLoc)>;

/// The location-free summary the root hash covers.
fn root_summary(chunk_count: u64, leaves: &Leaves) -> Vec<u8> {
    let mut b = chunk_count.to_le_bytes().to_vec();
    for (li, (h, _)) in leaves {
        b.extend_from_slice(&li.to_le_bytes());
        b.extend_from_slice(h);
    }
    b
}

fn root_body(chunk_count: u64, leaves: &Leaves) -> Vec<u8> {
    let n = u32::try_from(leaves.len()).expect("fewer than 2^32 leaves");
    let mut b = chunk_count.to_le_bytes().to_vec();
    b.extend_from_slice(&n.to_le_bytes());
    for (li, (h, loc)) in leaves {
        b.extend_from_slice(&li.to_le_bytes());
        b.extend_from_slice(h);
        b.extend_from_slice(&loc.pack.raw().to_le_bytes());
        b.extend_from_slice(&loc.off.to_le_bytes());
        b.extend_from_slice(&loc.len.to_le_bytes());
    }
    b
}

struct State {
    chunk_count: u64,
    content: BTreeMap<u64, DataEntry>,
    dirty: BTreeSet<u64>,
    size_changed: bool,
    leaves: Leaves,
    root: MapRoot,
    txn_open: bool,
}

/// The flat block map.
pub struct FlatMap {
    codec: Arc<dyn ExtentCodec>,
    packs: Arc<dyn PackRead>,
    id_len: IdLen,
    dirty_cap: Option<u64>,
    st: Mutex<State>,
}

impl FlatMap {
    /// An empty map of `chunk_count` chunks. Nodes are sealed with `codec`;
    /// old nodes are read back through `packs`.
    pub fn new(
        codec: Arc<dyn ExtentCodec>,
        packs: Arc<dyn PackRead>,
        chunk_count: u64,
        id_len: IdLen,
    ) -> Self {
        let leaves = Leaves::new();
        let hash = codec
            .chunk_id(ExtentKind::Node, &root_summary(chunk_count, &leaves))
            .0;
        Self {
            codec,
            packs,
            id_len,
            dirty_cap: None,
            st: Mutex::new(State {
                chunk_count,
                content: BTreeMap::new(),
                dirty: BTreeSet::new(),
                size_changed: false,
                leaves,
                root: MapRoot::empty(hash),
                txn_open: false,
            }),
        }
    }

    /// Opens the map whose last sealed root is `root`. `chunk_count` is the
    /// size the commit names (an empty root does not record it).
    ///
    /// # Errors
    /// `Corrupt`/`Auth`/`NotFound` from the nodes, `Corrupt` when the root
    /// does not describe `chunk_count` chunks.
    pub fn open(
        codec: Arc<dyn ExtentCodec>,
        packs: Arc<dyn PackRead>,
        root: &MapRoot,
        chunk_count: u64,
        id_len: IdLen,
    ) -> Result<Self> {
        let mut map = Self::new(codec, packs, chunk_count, id_len);
        let (count, leaves) = map.read_root(root)?.unwrap_or((chunk_count, Leaves::new()));
        if count != chunk_count {
            return Err(corrupt("root records another chunk count"));
        }
        if map.summary_hash(count, &leaves) != root.hash {
            return Err(corrupt("root hash does not match its children"));
        }
        let mut content = BTreeMap::new();
        for (hash, loc) in leaves.values() {
            for (ci, e) in map.read_leaf(*hash, *loc)? {
                content.insert(ci, e);
            }
        }
        let st = map.st.get_mut().unwrap_or_else(|p| p.into_inner());
        st.content = content;
        st.leaves = leaves;
        st.root = *root;
        Ok(map)
    }

    /// Fails `begin_update` and a `commit` that would dirty more than `cap`
    /// leaves with `MapFull` (the real map's dirty-leaf cap).
    #[must_use]
    pub fn with_dirty_leaf_cap(mut self, cap: u64) -> Self {
        self.dirty_cap = Some(cap);
        self
    }

    /// Every populated chunk, ascending: the content a model should equal.
    pub fn snapshot(&self) -> BTreeMap<u64, DataEntry> {
        lock(&self.st).content.clone()
    }

    fn summary_hash(&self, chunk_count: u64, leaves: &Leaves) -> NodeHash {
        self.codec
            .chunk_id(ExtentKind::Node, &root_summary(chunk_count, leaves))
            .0
    }

    fn open_node(&self, hash: &NodeHash, loc: NodeLoc, verify_id: bool) -> Result<Vec<u8>> {
        let mut bytes = Vec::new();
        self.packs.read_node(loc, &mut bytes)?;
        let id = ChunkId(*hash);
        let expect = OpenExpect {
            chunk_size: u32::try_from(MAX_NODE_ULEN).expect("64 KiB"),
            chunk_id: Some(&id),
            id_prefix: None,
            verify_id,
            output: OpenOutput::Plaintext,
        };
        let mut body = Vec::new();
        self.codec
            .open(ExtentKind::Node, &bytes, &expect, &mut body)?;
        Ok(body)
    }

    fn read_root(&self, root: &MapRoot) -> Result<Option<(u64, Leaves)>> {
        let Some(pack) = root.pack else {
            return Ok(None);
        };
        let loc = NodeLoc {
            pack,
            off: root.off,
            len: root.len,
        };
        // The root's id is the location-free summary hash, not the hash of the
        // stored body, so `open` cannot recompute it; callers compare it.
        let body = self.open_node(&root.hash, loc, false)?;
        let mut r = Reader(&body);
        let count = r.u64()?;
        let n = r.u32()?;
        let mut leaves = Leaves::new();
        let mut prev = None;
        for _ in 0..n {
            let li = r.u64()?;
            if prev.is_some_and(|p| p >= li) {
                return Err(corrupt("leaves out of order"));
            }
            prev = Some(li);
            let hash: NodeHash = r.take(32)?.try_into().expect("32");
            let pack = ObjId::from_raw(r.u64()?)?;
            let (off, len) = (r.u32()?, r.u32()?);
            leaves.insert(li, (hash, NodeLoc { pack, off, len }));
        }
        r.finish()?;
        Ok(Some((count, leaves)))
    }

    fn read_leaf(&self, hash: NodeHash, loc: NodeLoc) -> Result<Vec<(u64, DataEntry)>> {
        let body = self.open_node(&hash, loc, true)?;
        let mut r = Reader(&body);
        let n = r.u32()?;
        let mut out = Vec::new();
        for _ in 0..n {
            out.push(decode_entry(&mut r, self.id_len)?);
        }
        r.finish()?;
        Ok(out)
    }

    fn append(&self, packs: &mut dyn PackWrite, body: &[u8], hash: NodeHash) -> Result<NodeLoc> {
        if body.len() as u64 > MAX_NODE_ULEN {
            return Err(Error::invalid("FlatMap node exceeds the 64 KiB node limit"));
        }
        let desc = EncodingDesc {
            codec: Codec::Raw,
            level: Level(0),
            ulen: u32::try_from(body.len()).map_err(|_| Error::invalid("node too long"))?,
            dict_id: None,
        };
        let mut extent = Vec::new();
        self.codec
            .seal(ExtentKind::Node, &ChunkId(hash), &desc, body, &mut extent)?;
        packs.append_node(&hash, &extent)
    }
}

enum Op {
    Set(u64, DataEntry),
    Hole(u64),
    Rehint(u64, Hint, Hint),
}

struct Txn<'a> {
    map: &'a FlatMap,
    ops: Vec<Op>,
}

impl Txn<'_> {
    fn check(&self, ci: ChunkNo) -> Result<()> {
        if ci.0 >= lock(&self.map.st).chunk_count {
            return Err(Error::invalid("chunk past the end of the map"));
        }
        Ok(())
    }
}

impl MapTxn for Txn<'_> {
    fn set(&mut self, ci: ChunkNo, entry: DataEntry) -> Result<()> {
        self.check(ci)?;
        self.ops.push(Op::Set(ci.0, entry));
        Ok(())
    }

    fn set_hole(&mut self, ci: ChunkNo) -> Result<()> {
        self.check(ci)?;
        self.ops.push(Op::Hole(ci.0));
        Ok(())
    }

    fn rehint(&mut self, ci: ChunkNo, old: Hint, new: Hint) -> Result<()> {
        self.check(ci)?;
        self.ops.push(Op::Rehint(ci.0, old, new));
        Ok(())
    }

    fn commit(mut self: Box<Self>) -> Result<TxnReport> {
        let ops = std::mem::take(&mut self.ops);
        let mut st = lock(&self.map.st);
        // Evaluate against a scratch of the touched chunks first, so that a
        // MapFull leaves the map untouched.
        let mut scratch: BTreeMap<u64, Option<DataEntry>> = BTreeMap::new();
        let mut touched = BTreeSet::new();
        let mut report = TxnReport::default();
        for op in ops {
            match op {
                Op::Set(ci, e) => {
                    scratch.insert(ci, Some(e));
                    touched.insert(ci / LEAF_CHUNKS);
                    report.sets += 1;
                }
                Op::Hole(ci) => {
                    scratch.insert(ci, None);
                    touched.insert(ci / LEAF_CHUNKS);
                    report.sets += 1;
                }
                Op::Rehint(ci, old, new) => {
                    let cur = scratch
                        .get(&ci)
                        .copied()
                        .unwrap_or_else(|| st.content.get(&ci).copied());
                    match cur {
                        Some(mut e) if entry_hint(&e) == old => {
                            e.seg = new.seg;
                            e.off = new.off;
                            e.clen = new.clen;
                            scratch.insert(ci, Some(e));
                            touched.insert(ci / LEAF_CHUNKS);
                            report.rehints_applied += 1;
                        }
                        _ => report.rehints_skipped += 1,
                    }
                }
            }
        }
        if let Some(cap) = self.map.dirty_cap {
            let would_be = st.dirty.union(&touched).count() as u64;
            if would_be > cap {
                return Err(Error::MapFull);
            }
        }
        for (ci, e) in scratch {
            match e {
                Some(e) => st.content.insert(ci, e),
                None => st.content.remove(&ci),
            };
        }
        st.dirty.extend(touched);
        Ok(report)
    }
}

impl Drop for Txn<'_> {
    fn drop(&mut self) {
        lock(&self.map.st).txn_open = false;
    }
}

impl BlockMap for FlatMap {
    fn chunk_count(&self) -> u64 {
        lock(&self.st).chunk_count
    }

    fn get(&self, ci: ChunkNo) -> Result<Option<DataEntry>> {
        let st = lock(&self.st);
        if ci.0 >= st.chunk_count {
            return Err(Error::invalid("chunk past the end of the map"));
        }
        Ok(st.content.get(&ci.0).copied())
    }

    /// Everything is resident, so this never answers `Miss`.
    fn try_get_cached(&self, ci: ChunkNo) -> Cached {
        match lock(&self.st).content.get(&ci.0) {
            Some(e) => Cached::Hit(*e),
            None => Cached::Hole,
        }
    }

    fn begin_update(&self) -> Result<Box<dyn MapTxn + '_>> {
        let mut st = lock(&self.st);
        if st.txn_open {
            return Err(Error::Busy {
                what: "map transaction",
            });
        }
        if self
            .dirty_cap
            .is_some_and(|cap| st.dirty.len() as u64 >= cap)
        {
            return Err(Error::MapFull);
        }
        st.txn_open = true;
        Ok(Box::new(Txn {
            map: self,
            ops: Vec::new(),
        }))
    }

    fn seal(&self, packs: &mut dyn PackWrite) -> Result<SealResult> {
        let mut st = lock(&self.st);
        let mut leaves = st.leaves.clone();
        let (mut bytes, mut written) = (0u64, 0u64);
        let mut changed = st.size_changed;
        for &li in &st.dirty {
            let entries: Vec<(u64, DataEntry)> = st
                .content
                .range(li * LEAF_CHUNKS..(li + 1) * LEAF_CHUNKS)
                .map(|(c, e)| (*c, *e))
                .collect();
            if entries.is_empty() {
                changed |= leaves.remove(&li).is_some();
                continue;
            }
            let body = leaf_body(&entries);
            let hash = self.codec.chunk_id(ExtentKind::Node, &body).0;
            if leaves.get(&li).is_some_and(|(h, _)| *h == hash) {
                continue;
            }
            let loc = self.append(packs, &body, hash)?;
            bytes += u64::from(loc.len);
            written += 1;
            changed = true;
            leaves.insert(li, (hash, loc));
        }
        if !changed {
            st.dirty.clear();
            return Ok(SealResult {
                root: st.root,
                new_packs: Vec::new(),
                dirty_leaves: 0,
                bytes: 0,
            });
        }
        let hash = self.summary_hash(st.chunk_count, &leaves);
        let root = if leaves.is_empty() {
            MapRoot::empty(hash)
        } else {
            let loc = self.append(packs, &root_body(st.chunk_count, &leaves), hash)?;
            bytes += u64::from(loc.len);
            MapRoot {
                hash,
                pack: Some(loc.pack),
                off: loc.off,
                len: loc.len,
            }
        };
        st.dirty.clear();
        st.size_changed = false;
        st.leaves = leaves;
        st.root = root;
        let wrote = bytes > 0;
        Ok(SealResult {
            root,
            new_packs: if wrote { vec![packs.id()] } else { Vec::new() },
            dirty_leaves: written,
            bytes,
        })
    }

    fn grow(&self, new_chunk_count: u64) -> Result<()> {
        let mut st = lock(&self.st);
        if new_chunk_count < st.chunk_count {
            return Err(Error::invalid("cannot shrink"));
        }
        if new_chunk_count != st.chunk_count {
            st.chunk_count = new_chunk_count;
            st.size_changed = true;
        }
        Ok(())
    }

    fn root(&self) -> MapRoot {
        lock(&self.st).root
    }

    fn dirty_leaf_count(&self) -> u64 {
        lock(&self.st).dirty.len() as u64
    }

    fn walk_leaves_skipping(
        &self,
        root: &MapRoot,
        skip: &HashSet<NodeHash>,
        f: &mut dyn FnMut(ChunkNo, &DataEntry),
    ) -> Result<()> {
        if skip.contains(&root.hash) {
            return Ok(());
        }
        let Some((_, leaves)) = self.read_root(root)? else {
            return Ok(());
        };
        for (_, (hash, loc)) in leaves {
            if skip.contains(&hash) {
                continue;
            }
            for (ci, e) in self.read_leaf(hash, loc)? {
                f(ChunkNo(ci), &e);
            }
        }
        Ok(())
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
        if skip.contains(&root.hash) {
            return Ok(());
        }
        let Some((_, leaves)) = self.read_root(root)? else {
            return Ok(());
        };
        let pack = root.pack.ok_or(corrupt("non-empty root without a pack"))?;
        f(&NodeRef {
            hash: root.hash,
            level: 1,
            loc: NodeLoc {
                pack,
                off: root.off,
                len: root.len,
            },
        });
        for (_, (hash, loc)) in leaves {
            if !skip.contains(&hash) {
                f(&NodeRef {
                    hash,
                    level: 0,
                    loc,
                });
            }
        }
        Ok(())
    }
}
