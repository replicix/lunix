//! `FlatMap` (on `MemPacks`) passes the map contract suite.

use std::sync::Arc;

use lunix_format::commit::MapRoot;
use lunix_format::IdLen;
use lunix_iface::contract::map::MapFixture;
use lunix_iface::crypto::ExtentCodec;
use lunix_iface::map::{BlockMap, ChunkNo};
use lunix_iface::store::{PackRead, PackStore, PackWrite};
use lunix_testkit::{FlatMap, MemPacks, PlainCodec};

struct Fix {
    codec: Arc<dyn ExtentCodec>,
    packs: MemPacks,
    map: FlatMap,
    id_len: IdLen,
}

impl Fix {
    fn new(chunks: u64) -> Self {
        let codec: Arc<dyn ExtentCodec> = Arc::new(PlainCodec::new());
        let packs = MemPacks::new();
        let id_len = IdLen::Sixteen;
        let map = FlatMap::new(codec.clone(), Arc::new(packs.clone()), chunks, id_len);
        Self {
            codec,
            packs,
            map,
            id_len,
        }
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
        let reads: Arc<dyn PackRead> = Arc::new(self.packs.clone());
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

lunix_iface::contract::map!(Fix::new);

#[test]
fn reads_past_the_end_are_invalid() {
    let f = Fix::new(10);
    assert!(f.map().get(ChunkNo(10)).is_err());
}
