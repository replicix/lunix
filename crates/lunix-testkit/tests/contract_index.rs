//! `MemIndex` passes the index contract suite.

use std::collections::HashMap;
use std::sync::Arc;

use lunix_format::segment::{ContainerKind, FooterBody, FooterEntry};
use lunix_format::{ChunkId, Codec, Hint, SegId, WriteSeq};
use lunix_iface::contract::index::IndexFixture;
use lunix_iface::index::ChunkIndex;
use lunix_iface::store::{IdAllocator, SegmentFooters};
use lunix_iface::{Error, Result};
use lunix_testkit::{MemIds, MemIndex};

/// Footers built straight from the (id, hint) pairs the test names.
#[derive(Default)]
struct Footers(HashMap<SegId, FooterBody>);

impl SegmentFooters for Footers {
    fn segment_footer(&self, seg: SegId) -> Result<FooterBody> {
        self.0.get(&seg).cloned().ok_or(Error::not_found("segment"))
    }
}

struct Fix {
    ids: Arc<MemIds>,
    index: MemIndex,
    footers: Footers,
}

impl Fix {
    fn new() -> Self {
        let ids = Arc::new(MemIds::new());
        Self {
            index: MemIndex::new(ids.clone()),
            ids,
            footers: Footers::default(),
        }
    }
}

impl IndexFixture for Fix {
    fn index(&self) -> &dyn ChunkIndex {
        &self.index
    }

    fn footers(&self) -> &dyn SegmentFooters {
        &self.footers
    }

    fn add_segment(&mut self, seg: SegId, entries: &[(ChunkId, Hint)]) {
        let mut es: Vec<FooterEntry> = entries
            .iter()
            .map(|(id, h)| FooterEntry {
                chunk_id: *id,
                off: h.off,
                clen: h.clen,
                ulen: h.clen,
                codec: Codec::Raw,
            })
            .collect();
        es.sort_by_key(|e| (e.chunk_id, e.off));
        self.footers.0.insert(
            seg,
            FooterBody {
                container_kind: ContainerKind::Segment,
                object_id: seg,
                extents_len: 1 << 20,
                write_seq_lo: WriteSeq(0),
                write_seq_hi: WriteSeq(0),
                seg_hash: [0; 32],
                entries: es,
                dict_ids: Vec::new(),
            },
        );
    }

    fn reopen(&mut self) {
        let image = self.index.image();
        self.index = MemIndex::from_image(self.ids.clone() as Arc<dyn IdAllocator>, &image);
    }
}

lunix_iface::contract::index!(Fix::new);
