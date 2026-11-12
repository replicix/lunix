use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::Mutex;

use lunix_format::commit::MapRoot;
use lunix_format::node::{DataEntry, NodeLoc};
use lunix_format::{Hint, IdLen, IdPrefix, LineageIdx, ObjId};
use lunix_iface::contract::map::MapFixture;
use lunix_iface::map::*;
use lunix_iface::store::{PackRead, PackStore, PackWrite};
use lunix_iface::{Error, Result};

use super::lock;
use super::store::TStore;

const LEAF: u64 = 512;

fn enc_entry(ci: u64, e: &DataEntry, out: &mut Vec<u8>) {
    out.extend_from_slice(&ci.to_le_bytes());
    out.push(e.lineage.0);
    out.extend_from_slice(&e.seg.raw().to_le_bytes());
    out.extend_from_slice(&e.off.to_le_bytes());
    out.extend_from_slice(&e.clen.to_le_bytes());
    let p = e.prefix.as_bytes();
    out.push(p.len() as u8);
    out.extend_from_slice(p);
}

struct Rd<'a>(&'a [u8]);

impl<'a> Rd<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.0.len() < n {
            return Err(Error::Corrupt {
                what: "trivial node",
                detail: "short",
            });
        }
        let (a, b) = self.0.split_at(n);
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
}

fn dec_entry(r: &mut Rd<'_>, id_len: IdLen) -> Result<(u64, DataEntry)> {
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

fn leaf_bytes(entries: &[(u64, DataEntry)]) -> Vec<u8> {
    let mut out = (entries.len() as u32).to_le_bytes().to_vec();
    for (ci, e) in entries {
        enc_entry(*ci, e, &mut out);
    }
    out
}

type Child = (u64, NodeHash, NodeLoc);

fn root_hash(chunk_count: u64, children: &BTreeMap<u64, (NodeHash, NodeLoc)>) -> NodeHash {
    let mut b = chunk_count.to_le_bytes().to_vec();
    for (li, (h, _)) in children {
        b.extend_from_slice(&li.to_le_bytes());
        b.extend_from_slice(h);
    }
    super::hash32(&b)
}

struct State {
    chunk_count: u64,
    committed: BTreeMap<u64, DataEntry>,
    dirty: BTreeSet<u64>,
    root_dirty: bool,
    leaves: BTreeMap<u64, (NodeHash, NodeLoc)>,
    root: MapRoot,
    txn_open: bool,
}

pub struct TMap {
    store: TStore,
    id_len: IdLen,
    st: Mutex<State>,
}

impl TMap {
    pub fn new(store: TStore, chunk_count: u64, id_len: IdLen) -> Self {
        let root = MapRoot::empty(root_hash(chunk_count, &BTreeMap::new()));
        Self {
            store,
            id_len,
            st: Mutex::new(State {
                chunk_count,
                committed: BTreeMap::new(),
                dirty: BTreeSet::new(),
                root_dirty: false,
                leaves: BTreeMap::new(),
                root,
                txn_open: false,
            }),
        }
    }

    pub fn open(store: TStore, root: &MapRoot, id_len: IdLen) -> Self {
        let m = Self::new(store, 0, id_len);
        let (count, children) = m
            .children(root)
            .expect("read root")
            .expect("non-empty root");
        let mut st = lock(&m.st);
        st.chunk_count = count;
        st.root = *root;
        for (li, h, loc) in children {
            let mut b = Vec::new();
            m.store.read_node(loc, &mut b).expect("leaf");
            let mut r = Rd(&b);
            for _ in 0..r.u32().expect("count") {
                let (ci, e) = dec_entry(&mut r, id_len).expect("entry");
                st.committed.insert(ci, e);
            }
            st.leaves.insert(li, (h, loc));
        }
        drop(st);
        m
    }

    fn children(&self, root: &MapRoot) -> Result<Option<(u64, Vec<Child>)>> {
        let Some(pack) = root.pack else {
            return Ok(None);
        };
        let mut b = Vec::new();
        self.store.read_node(
            NodeLoc {
                pack,
                off: root.off,
                len: root.len,
            },
            &mut b,
        )?;
        let mut r = Rd(&b);
        let count = r.u64()?;
        let n = r.u32()?;
        let mut out = Vec::new();
        for _ in 0..n {
            let li = r.u64()?;
            let h: NodeHash = r.take(32)?.try_into().expect("32");
            let pack = ObjId::from_raw(r.u64()?)?;
            let (off, len) = (r.u32()?, r.u32()?);
            out.push((li, h, NodeLoc { pack, off, len }));
        }
        Ok(Some((count, out)))
    }
}

enum Op {
    Set(u64, DataEntry),
    Hole(u64),
    Rehint(u64, Hint, Hint),
}

struct Txn<'a> {
    map: &'a TMap,
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
        let mut st = lock(&self.map.st);
        let mut rep = TxnReport::default();
        for op in std::mem::take(&mut self.ops) {
            match op {
                Op::Set(ci, e) => {
                    st.committed.insert(ci, e);
                    st.dirty.insert(ci / LEAF);
                    rep.sets += 1;
                }
                Op::Hole(ci) => {
                    st.committed.remove(&ci);
                    st.dirty.insert(ci / LEAF);
                    rep.sets += 1;
                }
                Op::Rehint(ci, old, new) => match st.committed.get_mut(&ci) {
                    Some(e) if entry_hint(e) == old => {
                        e.seg = new.seg;
                        e.off = new.off;
                        e.clen = new.clen;
                        st.dirty.insert(ci / LEAF);
                        rep.rehints_applied += 1;
                    }
                    _ => rep.rehints_skipped += 1,
                },
            }
        }
        Ok(rep)
    }
}

impl Drop for Txn<'_> {
    fn drop(&mut self) {
        lock(&self.map.st).txn_open = false;
    }
}

impl BlockMap for TMap {
    fn chunk_count(&self) -> u64 {
        lock(&self.st).chunk_count
    }
    fn get(&self, ci: ChunkNo) -> Result<Option<DataEntry>> {
        Ok(lock(&self.st).committed.get(&ci.0).copied())
    }
    fn try_get_cached(&self, ci: ChunkNo) -> Cached {
        match lock(&self.st).committed.get(&ci.0) {
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
        st.txn_open = true;
        Ok(Box::new(Txn {
            map: self,
            ops: Vec::new(),
        }))
    }
    fn seal(&self, packs: &mut dyn PackWrite) -> Result<SealResult> {
        let mut st = lock(&self.st);
        if st.dirty.is_empty() && !st.root_dirty {
            return Ok(SealResult {
                root: st.root,
                new_packs: Vec::new(),
                dirty_leaves: 0,
                bytes: 0,
            });
        }
        let (mut bytes, dirty) = (0u64, std::mem::take(&mut st.dirty));
        for &li in &dirty {
            let entries: Vec<(u64, DataEntry)> = st
                .committed
                .range(li * LEAF..(li + 1) * LEAF)
                .map(|(c, e)| (*c, *e))
                .collect();
            if entries.is_empty() {
                st.leaves.remove(&li);
                continue;
            }
            let b = leaf_bytes(&entries);
            let h = super::hash32(&b);
            let loc = packs.append_node(&h, &b)?;
            bytes += b.len() as u64;
            st.leaves.insert(li, (h, loc));
        }
        st.root_dirty = false;
        let hash = root_hash(st.chunk_count, &st.leaves);
        let root = if st.leaves.is_empty() {
            MapRoot::empty(hash)
        } else {
            let mut b = st.chunk_count.to_le_bytes().to_vec();
            b.extend_from_slice(&(st.leaves.len() as u32).to_le_bytes());
            for (li, (h, loc)) in &st.leaves {
                b.extend_from_slice(&li.to_le_bytes());
                b.extend_from_slice(h);
                b.extend_from_slice(&loc.pack.raw().to_le_bytes());
                b.extend_from_slice(&loc.off.to_le_bytes());
                b.extend_from_slice(&loc.len.to_le_bytes());
            }
            let loc = packs.append_node(&hash, &b)?;
            bytes += b.len() as u64;
            MapRoot {
                hash,
                pack: Some(loc.pack),
                off: loc.off,
                len: loc.len,
            }
        };
        st.root = root;
        Ok(SealResult {
            root,
            new_packs: vec![packs.id()],
            dirty_leaves: dirty.len() as u64,
            bytes,
        })
    }
    fn grow(&self, new_chunk_count: u64) -> Result<()> {
        let mut st = lock(&self.st);
        if new_chunk_count < st.chunk_count {
            return Err(Error::invalid("cannot shrink"));
        }
        st.chunk_count = new_chunk_count;
        st.root_dirty = true;
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
        let Some((_, children)) = self.children(root)? else {
            return Ok(());
        };
        for (_, h, loc) in children {
            if skip.contains(&h) {
                continue;
            }
            let mut b = Vec::new();
            self.store.read_node(loc, &mut b)?;
            let mut r = Rd(&b);
            for _ in 0..r.u32()? {
                let (ci, e) = dec_entry(&mut r, self.id_len)?;
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
        let Some((_, children)) = self.children(root)? else {
            return Ok(());
        };
        let pack = root.pack.expect("non-empty");
        f(&NodeRef {
            hash: root.hash,
            level: 1,
            loc: NodeLoc {
                pack,
                off: root.off,
                len: root.len,
            },
        });
        for (_, h, loc) in children {
            if !skip.contains(&h) {
                f(&NodeRef {
                    hash: h,
                    level: 0,
                    loc,
                });
            }
        }
        Ok(())
    }
}

pub struct MFix {
    store: TStore,
    map: TMap,
    id_len: IdLen,
}

impl MFix {
    pub fn new(chunks: u64) -> Self {
        let store = TStore::new();
        let id_len = IdLen::Sixteen;
        Self {
            map: TMap::new(store.clone(), chunks, id_len),
            store,
            id_len,
        }
    }
}

impl MapFixture for MFix {
    fn map(&self) -> &dyn BlockMap {
        &self.map
    }
    fn open_pack(&self) -> Box<dyn PackWrite> {
        self.store.open_pack().expect("pack")
    }
    fn reopen(&mut self, root: &MapRoot) {
        self.map = TMap::open(self.store.clone(), root, self.id_len);
    }
    fn id_len(&self) -> IdLen {
        self.id_len
    }
}
