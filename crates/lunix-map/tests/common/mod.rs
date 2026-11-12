//! Shared fixtures: codecs, a pack-backed map, counting and corrupting sources.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use lunix_format::commit::MapRoot;
use lunix_format::node::{DataEntry, NodeLoc};
use lunix_format::{ChunkId, CryptoMode, ExtentKind, IdLen};
use lunix_iface::contract::map::MapFixture;
use lunix_iface::crypto::{EncodingDesc, ExtentCodec, OpenExpect, OpenedMeta};
use lunix_iface::map::{BlockMap, ChunkNo, NodeSource};
use lunix_iface::store::{PackRead, PackStore, PackWrite};
use lunix_iface::Result;
use lunix_map::{MapConfig, RadixMap};
use lunix_testkit::strategies::data_entry;
use lunix_testkit::{MemPacks, PlainCodec};

pub const ID_LEN: IdLen = IdLen::Sixteen;

/// `PlainCodec` framing with real unkeyed BLAKE3 node hashes, so that the
/// empty-hash constants of on-disk-formats.md section 23.7 apply and hashing
/// costs what it costs in production.
pub struct Blake3Codec(pub PlainCodec);

impl Blake3Codec {
    pub fn arc() -> Arc<dyn ExtentCodec> {
        Arc::new(Self(PlainCodec::unkeyed()))
    }
}

impl ExtentCodec for Blake3Codec {
    fn mode(&self) -> CryptoMode {
        ExtentCodec::mode(&self.0)
    }

    fn chunk_id(&self, _kind: ExtentKind, data: &[u8]) -> ChunkId {
        ChunkId(*blake3::hash(data).as_bytes())
    }

    fn seal(
        &self,
        kind: ExtentKind,
        id: &ChunkId,
        desc: &EncodingDesc,
        compressed: &[u8],
        out: &mut Vec<u8>,
    ) -> Result<()> {
        ExtentCodec::seal(&self.0, kind, id, desc, compressed, out)
    }

    fn open(
        &self,
        kind: ExtentKind,
        bytes: &[u8],
        expect: &OpenExpect<'_>,
        out: &mut Vec<u8>,
    ) -> Result<OpenedMeta> {
        ExtentCodec::open(&self.0, kind, bytes, expect, out)
    }
}

pub fn plain() -> Arc<dyn ExtentCodec> {
    Arc::new(PlainCodec::new())
}

/// A map on in-memory packs.
pub struct Fix {
    pub codec: Arc<dyn ExtentCodec>,
    pub packs: MemPacks,
    pub map: RadixMap,
    pub cfg: MapConfig,
}

impl Fix {
    pub fn new(fanout: u16, chunks: u64) -> Self {
        Self::with(plain(), MapConfig::new(fanout, ID_LEN), chunks)
    }

    pub fn with(codec: Arc<dyn ExtentCodec>, cfg: MapConfig, chunks: u64) -> Self {
        let packs = MemPacks::new();
        let map =
            RadixMap::new(codec.clone(), Arc::new(packs.clone()), chunks, cfg).expect("new map");
        Self {
            codec,
            packs,
            map,
            cfg,
        }
    }

    pub fn open_at(&self, root: &MapRoot, chunks: u64) -> RadixMap {
        RadixMap::open(
            self.codec.clone(),
            Arc::new(self.packs.clone()),
            root,
            chunks,
            self.cfg,
        )
        .expect("open")
    }

    /// Seals into a fresh pack, seals the pack, and returns the result.
    pub fn seal(&self) -> lunix_iface::map::SealResult {
        let mut pack = self.packs.open_pack().expect("pack");
        let res = self.map.seal(&mut *pack).expect("seal");
        assert_eq!(res.new_packs.is_empty(), pack.is_empty());
        assert_eq!(res.bytes, pack.len());
        if !pack.is_empty() {
            pack.seal().expect("seal pack");
        }
        assert_eq!(self.map.root(), res.root);
        assert_eq!(self.map.dirty_leaf_count(), 0);
        res
    }
}

impl MapFixture for Fix {
    fn map(&self) -> &dyn BlockMap {
        &self.map
    }
    fn open_pack(&self) -> Box<dyn PackWrite> {
        self.packs.open_pack().expect("open pack")
    }
    fn reopen(&mut self, root: &MapRoot) {
        self.map = self.open_at(root, self.map.chunk_count());
    }
    fn id_len(&self) -> IdLen {
        ID_LEN
    }
}

pub fn set_all(map: &dyn BlockMap, cis: impl IntoIterator<Item = u64>, version: u32) {
    let mut txn = map.begin_update().expect("begin");
    for ci in cis {
        txn.set(ChunkNo(ci), data_entry(ci, version, ID_LEN))
            .expect("set");
    }
    txn.commit().expect("commit");
}

pub fn leaves(map: &dyn BlockMap, root: &MapRoot) -> BTreeMap<u64, DataEntry> {
    let mut out = BTreeMap::new();
    map.walk_leaves(root, &mut |ci, e| {
        assert!(out.insert(ci.0, *e).is_none(), "each chunk once");
    })
    .expect("walk");
    out
}

/// Counts node reads.
pub struct Counting {
    pub inner: MemPacks,
    pub reads: AtomicU64,
}

impl Counting {
    pub fn new(inner: MemPacks) -> Arc<Self> {
        Arc::new(Self {
            inner,
            reads: AtomicU64::new(0),
        })
    }
    pub fn count(&self) -> u64 {
        self.reads.load(Ordering::Relaxed)
    }
}

impl NodeSource for Counting {
    fn load_node(&self, loc: NodeLoc, out: &mut Vec<u8>) -> Result<()> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        self.inner.read_node(loc, out)
    }
}

pub struct SplitMix(pub u64);

impl SplitMix {
    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    pub fn below(&mut self, n: u64) -> u64 {
        ((u128::from(self.next()) * u128::from(n)) >> 64) as u64
    }
}
