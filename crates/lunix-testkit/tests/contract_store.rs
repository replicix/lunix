//! `MemStore` passes the store contract suite (and its packs pass the map
//! contract suite, next to `FlatMap`'s own run on `MemPacks`).

use std::sync::Arc;

use lunix_format::commit::MapRoot;
use lunix_format::{ChunkId, Codec, ExtentKind, IdLen, Level};
use lunix_iface::contract::map::MapFixture;
use lunix_iface::contract::store::StoreFixture;
use lunix_iface::crypto::{EncodingDesc, ExtentCodec};
use lunix_iface::map::BlockMap;
use lunix_iface::store::{
    CheckpointStore, CommitStore, GcStateStore, PackRead, PackStore, PackWrite, SegmentStore,
    VolumeDir,
};
use lunix_testkit::{FlatMap, MemStore, PlainCodec};

struct Fix {
    store: MemStore,
    codec: PlainCodec,
}

impl Fix {
    fn new() -> Self {
        Self {
            store: MemStore::new(),
            codec: PlainCodec::new(),
        }
    }

    fn extent(&self, kind: ExtentKind, id: &ChunkId, payload_len: usize) -> Vec<u8> {
        let payload = vec![0x5au8; payload_len];
        let desc = EncodingDesc {
            codec: Codec::Raw,
            level: Level(0),
            ulen: u32::try_from(payload_len).expect("small"),
            dict_id: None,
        };
        let mut out = Vec::new();
        ExtentCodec::seal(&self.codec, kind, id, &desc, &payload, &mut out).expect("seal");
        out
    }
}

impl StoreFixture for Fix {
    fn segments(&self) -> &dyn SegmentStore {
        &self.store
    }
    fn packs(&self) -> &dyn PackStore {
        &self.store
    }
    fn commits(&self) -> &dyn CommitStore {
        &self.store
    }
    fn checkpoint(&self) -> &dyn CheckpointStore {
        &self.store
    }
    fn gc_state(&self) -> &dyn GcStateStore {
        &self.store
    }
    fn volume(&self) -> &dyn VolumeDir {
        &self.store
    }
    fn data_extent(&self, id: &ChunkId, payload_len: usize) -> Vec<u8> {
        self.extent(ExtentKind::Data, id, payload_len)
    }
    fn node_extent(&self, hash: &[u8; 32], payload_len: usize) -> Vec<u8> {
        self.extent(ExtentKind::Node, &ChunkId(*hash), payload_len)
    }
    fn reopen(&mut self) {
        let image = self.store.close();
        self.store = MemStore::from_image(&image);
    }
}

lunix_iface::contract::store!(Fix::new);

/// `FlatMap` on `MemStore`'s packs.
struct MapFix {
    codec: Arc<dyn ExtentCodec>,
    store: MemStore,
    map: FlatMap,
    id_len: IdLen,
}

impl MapFix {
    fn new(chunks: u64) -> Self {
        let codec: Arc<dyn ExtentCodec> = Arc::new(PlainCodec::new());
        let store = MemStore::new();
        let id_len = IdLen::Sixteen;
        let map = FlatMap::new(codec.clone(), Arc::new(store.clone()), chunks, id_len);
        Self {
            codec,
            store,
            map,
            id_len,
        }
    }
}

impl MapFixture for MapFix {
    fn map(&self) -> &dyn BlockMap {
        &self.map
    }
    fn open_pack(&self) -> Box<dyn PackWrite> {
        self.store.open_pack().expect("open pack")
    }
    fn reopen(&mut self, root: &MapRoot) {
        let reads: Arc<dyn PackRead> = Arc::new(self.store.clone());
        self.map = FlatMap::open(
            self.codec.clone(),
            reads,
            root,
            self.map.chunk_count(),
            self.id_len,
        )
        .expect("reopen");
    }
    fn id_len(&self) -> IdLen {
        self.id_len
    }
}

lunix_iface::contract::map!(contract_map_on_mem_store, MapFix::new);
