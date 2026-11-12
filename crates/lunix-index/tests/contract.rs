//! The `lunix_iface::contract` index suite against the real index.

mod common;

use std::sync::Arc;

use lunix_format::{ChunkId, Hint, SegId};
use lunix_iface::contract::index::IndexFixture;
use lunix_iface::index::ChunkIndex;
use lunix_iface::store::SegmentFooters;
use lunix_index::LsmIndex;
use tempfile::TempDir;

use common::{open, Footers, Ids};

struct Fix {
    dir: TempDir,
    ids: Arc<Ids>,
    index: Option<LsmIndex>,
    footers: Footers,
}

impl Fix {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let ids = Ids::new();
        let index = Some(open(dir.path(), &ids));
        Self {
            dir,
            ids,
            index,
            footers: Footers::default(),
        }
    }
}

impl IndexFixture for Fix {
    fn index(&self) -> &dyn ChunkIndex {
        self.index.as_ref().expect("open")
    }
    fn footers(&self) -> &dyn SegmentFooters {
        &self.footers
    }
    fn add_segment(&mut self, seg: SegId, entries: &[(ChunkId, Hint)]) {
        self.footers.add(seg, entries);
    }
    fn reopen(&mut self) {
        // A crash: nothing but the files survives.
        self.index = None;
        self.index = Some(open(self.dir.path(), &self.ids));
    }
}

lunix_iface::contract::index!(Fix::new);
