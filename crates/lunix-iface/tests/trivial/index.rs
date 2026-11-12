use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};

use lunix_format::segment::{ContainerKind, FooterBody, FooterEntry};
use lunix_format::{ChunkId, Codec, Hint, IdPrefix, ObjId, SegId, WriteSeq};
use lunix_iface::contract::index::IndexFixture;
use lunix_iface::index::*;
use lunix_iface::store::SegmentFooters;
use lunix_iface::{Error, Result};

use super::lock;

#[derive(Default)]
pub struct Persisted {
    entries: HashMap<ChunkId, Hint>,
    condemned: HashSet<SegId>,
    runs: Vec<RunRef>,
    next_run: u64,
}

pub struct TIndex {
    persisted: Arc<Mutex<Persisted>>,
    mem: Mutex<HashMap<ChunkId, Hint>>,
}

impl TIndex {
    fn new(persisted: Arc<Mutex<Persisted>>) -> Self {
        Self {
            persisted,
            mem: Mutex::new(HashMap::new()),
        }
    }

    fn current(&self, id: &ChunkId) -> Option<(Hint, Confirm)> {
        if let Some(h) = lock(&self.mem).get(id) {
            return Some((*h, Confirm::Verified));
        }
        lock(&self.persisted)
            .entries
            .get(id)
            .map(|h| (*h, Confirm::Hinted))
    }

    fn all(&self) -> BTreeMap<ChunkId, Hint> {
        let mut m: BTreeMap<ChunkId, Hint> = lock(&self.persisted)
            .entries
            .iter()
            .map(|(k, v)| (*k, *v))
            .collect();
        m.extend(lock(&self.mem).iter().map(|(k, v)| (*k, *v)));
        m
    }

    fn chunk(&self, id: ChunkId, hint: Hint) -> IndexedChunk {
        let condemned = lock(&self.persisted).condemned.contains(&hint.seg);
        IndexedChunk {
            id,
            hint,
            condemned,
        }
    }
}

impl ChunkIndex for TIndex {
    fn get_for_dedup(&self, id: &ChunkId) -> Result<Option<DedupHit>> {
        Ok(self
            .current(id)
            .filter(|(h, _)| !lock(&self.persisted).condemned.contains(&h.seg))
            .map(|(hint, confirm)| DedupHit { hint, confirm }))
    }
    fn get(&self, id: &ChunkId) -> Result<Option<IndexedChunk>> {
        Ok(self.current(id).map(|(h, _)| self.chunk(*id, h)))
    }
    fn get_by_prefix(&self, prefix: &IdPrefix) -> Result<Vec<IndexedChunk>> {
        Ok(self
            .all()
            .into_iter()
            .filter(|(id, _)| prefix.matches(id))
            .map(|(id, h)| self.chunk(id, h))
            .collect())
    }
    fn insert(&self, id: ChunkId, hint: Hint) -> Result<()> {
        lock(&self.mem).insert(id, hint);
        Ok(())
    }
    fn relocate(&self, id: &ChunkId, old: &Hint, new: &Hint) -> Result<bool> {
        if self.current(id).map(|(h, _)| h) != Some(*old) {
            return Ok(false);
        }
        lock(&self.mem).insert(*id, *new);
        Ok(true)
    }
    fn mark_condemned(&self, seg: SegId) -> Result<()> {
        lock(&self.persisted).condemned.insert(seg);
        Ok(())
    }
    fn unmark_condemned(&self, seg: SegId) -> Result<()> {
        lock(&self.persisted).condemned.remove(&seg);
        Ok(())
    }
    fn condemned(&self) -> Result<Vec<SegId>> {
        let mut v: Vec<SegId> = lock(&self.persisted).condemned.iter().copied().collect();
        v.sort();
        Ok(v)
    }
    fn drop_segment(&self, seg: SegId) -> Result<()> {
        lock(&self.mem).retain(|_, h| h.seg != seg);
        lock(&self.persisted).entries.retain(|_, h| h.seg != seg);
        Ok(())
    }
    fn persist(&self, upto_seg: SegId) -> Result<Vec<RunRef>> {
        let mut mem = lock(&self.mem);
        let mut p = lock(&self.persisted);
        let moved: Vec<(ChunkId, Hint)> = mem
            .iter()
            .filter(|(_, h)| h.seg <= upto_seg)
            .map(|(k, v)| (*k, *v))
            .collect();
        if moved.is_empty() {
            return Ok(Vec::new());
        }
        for (k, v) in &moved {
            mem.remove(k);
            p.entries.insert(*k, *v);
        }
        p.next_run += 1;
        let run = RunRef {
            id: ObjId::new(0, p.next_run)?,
            size_bytes: 48 * moved.len() as u64,
            entries: moved.len() as u64,
        };
        p.runs.push(run);
        Ok(vec![run])
    }
    fn runs(&self) -> Result<Vec<RunRef>> {
        Ok(lock(&self.persisted).runs.clone())
    }
    fn all_runs(&self) -> Result<Vec<RunRef>> {
        Ok(lock(&self.persisted).runs.clone())
    }
    fn catch_up(&self, segments: &[SegId], footers: &dyn SegmentFooters) -> Result<CatchUpReport> {
        let mut rep = CatchUpReport::default();
        for &seg in segments {
            match footers.segment_footer(seg) {
                Ok(f) => {
                    for e in f.entries {
                        if self.current(&e.chunk_id).is_none() {
                            self.insert(
                                e.chunk_id,
                                Hint {
                                    seg,
                                    off: e.off,
                                    clen: e.clen,
                                },
                            )?;
                            rep.inserted += 1;
                        }
                    }
                }
                Err(e) if e.kind() == lunix_iface::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        let segs: HashSet<SegId> = self.all().values().map(|h| h.seg).collect();
        for seg in segs {
            if let Err(e) = footers.segment_footer(seg) {
                if e.kind() != lunix_iface::ErrorKind::NotFound {
                    return Err(e);
                }
                let before = self.all().len();
                self.drop_segment(seg)?;
                rep.dropped += (before - self.all().len()) as u64;
            }
        }
        Ok(rep)
    }
    fn rebuild(&self, segments: &[SegId], footers: &dyn SegmentFooters) -> Result<()> {
        lock(&self.mem).clear();
        lock(&self.persisted).entries.clear();
        for &seg in segments {
            for e in footers.segment_footer(seg)?.entries {
                self.insert(
                    e.chunk_id,
                    Hint {
                        seg,
                        off: e.off,
                        clen: e.clen,
                    },
                )?;
            }
        }
        Ok(())
    }
    fn delete_run(&self, run: RunRef) -> Result<u64> {
        let mut p = lock(&self.persisted);
        let before = p.runs.len();
        p.runs.retain(|r| r.id != run.id);
        Ok(if p.runs.len() < before {
            run.size_bytes
        } else {
            0
        })
    }
    fn iter_entries(&self, f: &mut dyn FnMut(&IndexedChunk)) -> Result<()> {
        for (id, h) in self.all() {
            f(&self.chunk(id, h));
        }
        Ok(())
    }
}

#[derive(Default)]
pub struct Footers(Mutex<HashMap<SegId, FooterBody>>);

impl SegmentFooters for Footers {
    fn segment_footer(&self, seg: SegId) -> Result<FooterBody> {
        lock(&self.0)
            .get(&seg)
            .cloned()
            .ok_or(Error::not_found("segment"))
    }
}

pub struct IFix {
    persisted: Arc<Mutex<Persisted>>,
    index: TIndex,
    footers: Footers,
}

impl IFix {
    pub fn new() -> Self {
        let persisted = Arc::new(Mutex::new(Persisted::default()));
        Self {
            index: TIndex::new(persisted.clone()),
            persisted,
            footers: Footers::default(),
        }
    }
}

impl IndexFixture for IFix {
    fn index(&self) -> &dyn ChunkIndex {
        &self.index
    }
    fn footers(&self) -> &dyn SegmentFooters {
        &self.footers
    }
    fn add_segment(&mut self, seg: SegId, entries: &[(ChunkId, Hint)]) {
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
        lock(&self.footers.0).insert(seg, body);
    }
    fn reopen(&mut self) {
        self.index = TIndex::new(self.persisted.clone());
    }
}
