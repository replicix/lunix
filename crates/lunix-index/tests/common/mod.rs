#![allow(dead_code)]

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use lunix_format::segment::{ContainerKind, FooterBody, FooterEntry};
use lunix_format::{ChunkId, Codec, Hint, IdLen, ObjId, SegId, WriteSeq};
use lunix_iface::store::{IdAllocator, SegmentFooters};
use lunix_iface::{Error, Result};
use lunix_index::{IndexOptions, LsmIndex};

pub fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// A counter standing in for the store's allocator. Segments and runs share it,
/// like in a volume; tests that need segment ids use `seg()` below instead.
#[derive(Debug)]
pub struct Ids(AtomicU64);

impl Ids {
    /// Starts well above the segment ids tests use so runs never collide.
    pub fn new() -> Arc<Self> {
        Arc::new(Self(AtomicU64::new(1_000_000)))
    }
}

impl IdAllocator for Ids {
    fn allocate(&self) -> Result<ObjId> {
        Ok(ObjId::new(0, self.0.fetch_add(1, Ordering::SeqCst) + 1)?)
    }
    fn high_water(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
    fn observe(&self, raw: u64) {
        self.0.fetch_max(raw & ObjId::MAX_COUNTER, Ordering::SeqCst);
    }
}

pub fn seg(n: u64) -> SegId {
    ObjId::new(0, n).expect("id")
}

/// A well-spread chunk id from a number.
pub fn cid(n: u64) -> ChunkId {
    ChunkId(*blake3::hash(&n.to_le_bytes()).as_bytes())
}

pub fn hint(s: u64, off: u32) -> Hint {
    Hint {
        seg: seg(s),
        off,
        clen: 500,
    }
}

pub fn open(root: &Path, ids: &Arc<Ids>) -> LsmIndex {
    LsmIndex::open(root, ids.clone(), IdLen::Sixteen).expect("open")
}

pub fn open_with(root: &Path, ids: &Arc<Ids>, max_runs: usize) -> LsmIndex {
    LsmIndex::open_with(
        root,
        ids.clone(),
        IdLen::Sixteen,
        IndexOptions {
            max_runs_per_level: max_runs,
        },
    )
    .expect("open")
}

/// Footers by segment, standing in for the store.
#[derive(Default)]
pub struct Footers(pub Mutex<HashMap<SegId, FooterBody>>);

impl Footers {
    pub fn add(&self, seg: SegId, entries: &[(ChunkId, Hint)]) {
        let entries = entries
            .iter()
            .map(|(id, h)| FooterEntry {
                chunk_id: *id,
                off: h.off,
                clen: h.clen,
                ulen: 16384,
                codec: Codec::Raw,
            })
            .collect();
        let body = FooterBody {
            container_kind: ContainerKind::Segment,
            object_id: seg,
            extents_len: 0,
            write_seq_lo: WriteSeq(0),
            write_seq_hi: WriteSeq(0),
            seg_hash: [0; 32],
            entries,
            dict_ids: Vec::new(),
        };
        lock(&self.0).insert(seg, body);
    }

    pub fn remove(&self, seg: SegId) {
        lock(&self.0).remove(&seg);
    }
}

impl SegmentFooters for Footers {
    fn segment_footer(&self, seg: SegId) -> Result<FooterBody> {
        lock(&self.0)
            .get(&seg)
            .cloned()
            .ok_or(Error::not_found("segment"))
    }
}
