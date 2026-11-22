//! [`MemIndex`]: the chunk index in memory, with persisted runs.
//!
//! It models the parts of the LSM the callers can observe:
//!
//! * a **live table** (id to hint) plus a set of **dirty** entries that are in
//!   the memtable only: new inserts, relocations and entries ingested by
//!   `catch_up` or `rebuild`;
//! * `persist(upto_seg)` writes the dirty entries whose segment is
//!   `<= upto_seg` as one **run** (an id from the shared allocator, named in the
//!   manifest at once); relocated entries to a higher segment stay in memory;
//! * the **manifest** names runs and holds the condemned set and the dropped
//!   segments, all durable the moment they are written;
//! * [`MemIndex::image`] and [`MemIndex::from_image`] model a crash and
//!   reopen: the memtable is lost, the live table is rebuilt from the runs the
//!   manifest names (newest run wins), and every hit from it is
//!   [`Confirm::Hinted`]. Hits from entries inserted in this process are
//!   [`Confirm::Verified`].
//!
//! Fault points (see [`Faults`]): `index.persist.before_run`,
//! `index.persist.before_manifest` (the run file exists but the manifest does
//! not name it, so `all_runs` lists it and `runs` does not),
//! `index.persist.after`, `index.condemn.before`, `index.drop.before`,
//! `index.delete_run.before`.

use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use lunix_format::{ChunkId, Hint, IdPrefix, RunId, SegId};
use lunix_iface::index::{CatchUpReport, ChunkIndex, Confirm, DedupHit, IndexedChunk, RunRef};
use lunix_iface::store::{IdAllocator, SegmentFooters};
use lunix_iface::{Error, ErrorKind, Result};

use crate::faults::Faults;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

const RUN_FIXED: u64 = 64;
const RUN_ENTRY: u64 = 48;

#[derive(Clone, Copy, Debug)]
struct Live {
    hint: Hint,
    confirm: Confirm,
    dirty: bool,
}

#[derive(Clone, Debug)]
struct RunFile {
    entries: Vec<(ChunkId, Hint)>,
}

impl RunFile {
    fn size(&self) -> u64 {
        RUN_FIXED + RUN_ENTRY * self.entries.len() as u64
    }
}

/// What survives a crash of a [`MemIndex`].
#[derive(Clone, Debug, Default)]
pub struct IndexImage {
    files: BTreeMap<u64, RunFile>,
    manifest: Vec<u64>,
    condemned: BTreeSet<u64>,
    dropped: BTreeSet<u64>,
}

#[derive(Default)]
struct State {
    live: HashMap<ChunkId, Live>,
    image: IndexImage,
}

/// The in-memory chunk index.
pub struct MemIndex {
    ids: Arc<dyn IdAllocator>,
    state: Mutex<State>,
    faults: Faults,
    /// Calls of the batch updates, for tests that count them.
    batches: BatchCalls,
}

#[derive(Default)]
struct BatchCalls {
    condemn: AtomicU64,
    retire: AtomicU64,
    uncondemn: AtomicU64,
    /// Calls of the single-segment `mark_condemned`, `unmark_condemned` and
    /// `drop_segment`.
    singles: AtomicU64,
}

impl std::fmt::Debug for MemIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let st = lock(&self.state);
        f.debug_struct("MemIndex")
            .field("entries", &st.live.len())
            .field("runs", &st.image.manifest)
            .finish()
    }
}

fn run_id(raw: u64) -> Result<RunId> {
    Ok(RunId::from_raw(raw)?)
}

impl MemIndex {
    /// An empty index; run ids come from `ids` (share the store's allocator).
    pub fn new(ids: Arc<dyn IdAllocator>) -> Self {
        Self::from_image(ids, &IndexImage::default())
    }

    /// How many times `mark_condemned_all` and `retire_segments` were called.
    pub fn batch_calls(&self) -> (u64, u64) {
        (
            self.batches.condemn.load(Ordering::Relaxed),
            self.batches.retire.load(Ordering::Relaxed),
        )
    }

    /// How many times `unmark_condemned_all` was called.
    pub fn uncondemn_calls(&self) -> u64 {
        self.batches.uncondemn.load(Ordering::Relaxed)
    }

    /// How many times the single-segment `mark_condemned`, `unmark_condemned`
    /// and `drop_segment` were called (by anyone, the batch methods of this
    /// type excluded).
    pub fn single_calls(&self) -> u64 {
        self.batches.singles.load(Ordering::Relaxed)
    }

    /// Opens a crash image: the live table is what the named runs hold.
    pub fn from_image(ids: Arc<dyn IdAllocator>, image: &IndexImage) -> Self {
        let mut live = HashMap::new();
        for raw in &image.manifest {
            if let Some(f) = image.files.get(raw) {
                for (id, hint) in &f.entries {
                    live.insert(
                        *id,
                        Live {
                            hint: *hint,
                            confirm: Confirm::Hinted,
                            dirty: false,
                        },
                    );
                }
            }
        }
        live.retain(|_, l| !image.dropped.contains(&l.hint.seg.raw()));
        // Runs on disk raise the allocator floor, as a directory scan would.
        if let Some(&max) = image.files.keys().next_back() {
            ids.observe(max);
        }
        Self {
            ids,
            state: Mutex::new(State {
                live,
                image: image.clone(),
            }),
            faults: Faults::default(),
            batches: BatchCalls::default(),
        }
    }

    /// What a crash right now leaves (the memtable is lost). The index stays
    /// usable; callers drop it and open the image.
    pub fn image(&self) -> IndexImage {
        lock(&self.state).image.clone()
    }

    /// Plants an entry as if it had been read from a persisted run: `Hinted`,
    /// not dirty, and **not** in any run (a reopen loses it). For the dedup
    /// confirmation cases, where a run entry names the wrong extent.
    pub fn insert_hinted(&self, id: ChunkId, hint: Hint) {
        lock(&self.state).live.insert(
            id,
            Live {
                hint,
                confirm: Confirm::Hinted,
                dirty: false,
            },
        );
    }

    /// The fault points of this index.
    pub fn faults(&self) -> &Faults {
        &self.faults
    }

    /// Entries in the memtable only.
    pub fn dirty_entries(&self) -> usize {
        lock(&self.state).live.values().filter(|l| l.dirty).count()
    }

    fn view(st: &State, id: &ChunkId, l: &Live) -> IndexedChunk {
        IndexedChunk {
            id: *id,
            hint: l.hint,
            condemned: st.image.condemned.contains(&l.hint.seg.raw()),
        }
    }
}

impl ChunkIndex for MemIndex {
    fn get_for_dedup(&self, id: &ChunkId) -> Result<Option<DedupHit>> {
        self.faults.alive()?;
        let st = lock(&self.state);
        Ok(st.live.get(id).and_then(|l| {
            (!st.image.condemned.contains(&l.hint.seg.raw())).then_some(DedupHit {
                hint: l.hint,
                confirm: l.confirm,
            })
        }))
    }

    fn get(&self, id: &ChunkId) -> Result<Option<IndexedChunk>> {
        self.faults.alive()?;
        let st = lock(&self.state);
        Ok(st.live.get(id).map(|l| Self::view(&st, id, l)))
    }

    fn get_by_prefix(&self, prefix: &IdPrefix) -> Result<Vec<IndexedChunk>> {
        self.faults.alive()?;
        if prefix.as_bytes().is_empty() {
            return Err(Error::Unsupported {
                what: "prefix lookup on an id_len 0 volume",
            });
        }
        let st = lock(&self.state);
        let mut found: Vec<_> = st
            .live
            .iter()
            .filter(|(id, _)| prefix.matches(id))
            .map(|(id, l)| Self::view(&st, id, l))
            .collect();
        found.sort_by_key(|c| c.id);
        Ok(found)
    }

    fn insert(&self, id: ChunkId, hint: Hint) -> Result<()> {
        self.faults.alive()?;
        lock(&self.state).live.insert(
            id,
            Live {
                hint,
                confirm: Confirm::Verified,
                dirty: true,
            },
        );
        Ok(())
    }

    fn relocate(&self, id: &ChunkId, old: &Hint, new: &Hint) -> Result<bool> {
        self.faults.alive()?;
        let mut st = lock(&self.state);
        match st.live.get_mut(id) {
            Some(l) if l.hint == *old => {
                *l = Live {
                    hint: *new,
                    confirm: Confirm::Verified,
                    dirty: true,
                };
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    fn mark_condemned(&self, seg: SegId) -> Result<()> {
        self.batches.singles.fetch_add(1, Ordering::Relaxed);
        self.faults.point("index.condemn.before")?;
        lock(&self.state).image.condemned.insert(seg.raw());
        Ok(())
    }

    fn mark_condemned_all(&self, segs: &[SegId]) -> Result<()> {
        self.batches.condemn.fetch_add(1, Ordering::Relaxed);
        if segs.is_empty() {
            return Ok(());
        }
        // One durable update: all or none.
        self.faults.point("index.condemn.before")?;
        let mut st = lock(&self.state);
        st.image.condemned.extend(segs.iter().map(|s| s.raw()));
        Ok(())
    }

    fn retire_segments(&self, segs: &[SegId]) -> Result<()> {
        self.batches.retire.fetch_add(1, Ordering::Relaxed);
        if segs.is_empty() {
            return Ok(());
        }
        // One durable update: the entries go and the flags clear together.
        self.faults.point("index.drop.before")?;
        let mut st = lock(&self.state);
        let gone: BTreeSet<SegId> = segs.iter().copied().collect();
        st.live.retain(|_, l| !gone.contains(&l.hint.seg));
        for seg in &gone {
            st.image.dropped.insert(seg.raw());
            st.image.condemned.remove(&seg.raw());
        }
        Ok(())
    }

    fn unmark_condemned_all(&self, segs: &[SegId]) -> Result<()> {
        self.batches.uncondemn.fetch_add(1, Ordering::Relaxed);
        if segs.is_empty() {
            return Ok(());
        }
        self.faults.point("index.condemn.before")?;
        let mut st = lock(&self.state);
        for s in segs {
            st.image.condemned.remove(&s.raw());
        }
        Ok(())
    }

    fn unmark_condemned(&self, seg: SegId) -> Result<()> {
        self.batches.singles.fetch_add(1, Ordering::Relaxed);
        self.faults.point("index.condemn.before")?;
        lock(&self.state).image.condemned.remove(&seg.raw());
        Ok(())
    }

    fn condemned(&self) -> Result<Vec<SegId>> {
        self.faults.alive()?;
        lock(&self.state)
            .image
            .condemned
            .iter()
            .map(|&r| Ok(SegId::from_raw(r)?))
            .collect()
    }

    fn drop_segment(&self, seg: SegId) -> Result<()> {
        self.batches.singles.fetch_add(1, Ordering::Relaxed);
        self.faults.point("index.drop.before")?;
        let mut st = lock(&self.state);
        st.live.retain(|_, l| l.hint.seg != seg);
        st.image.dropped.insert(seg.raw());
        Ok(())
    }

    fn persist(&self, upto_seg: SegId) -> Result<Vec<RunRef>> {
        self.faults.alive()?;
        let mut st = lock(&self.state);
        let mut entries: Vec<(ChunkId, Hint)> = st
            .live
            .iter()
            .filter(|(_, l)| l.dirty && l.hint.seg <= upto_seg)
            .map(|(id, l)| (*id, l.hint))
            .collect();
        if entries.is_empty() {
            return Ok(Vec::new());
        }
        entries.sort_by_key(|e| e.0);
        drop(st);
        self.faults.point("index.persist.before_run")?;
        let id = self.ids.allocate()?;
        let file = RunFile { entries };
        let run = RunRef {
            id,
            size_bytes: file.size(),
            entries: file.entries.len() as u64,
        };
        st = lock(&self.state);
        st.image.files.insert(id.raw(), file);
        drop(st);
        self.faults.point("index.persist.before_manifest")?;
        st = lock(&self.state);
        st.image.manifest.push(id.raw());
        for (cid, hint) in st
            .image
            .files
            .get(&id.raw())
            .map(|f| f.entries.clone())
            .unwrap_or_default()
        {
            if let Some(l) = st.live.get_mut(&cid) {
                if l.hint == hint {
                    l.dirty = false;
                }
            }
        }
        drop(st);
        self.faults.point("index.persist.after")?;
        Ok(vec![run])
    }

    fn runs(&self) -> Result<Vec<RunRef>> {
        self.faults.alive()?;
        let st = lock(&self.state);
        st.image
            .manifest
            .iter()
            .filter_map(|raw| st.image.files.get(raw).map(|f| (raw, f)))
            .map(|(&raw, f)| {
                Ok(RunRef {
                    id: run_id(raw)?,
                    size_bytes: f.size(),
                    entries: f.entries.len() as u64,
                })
            })
            .collect()
    }

    fn all_runs(&self) -> Result<Vec<RunRef>> {
        self.faults.alive()?;
        lock(&self.state)
            .image
            .files
            .iter()
            .map(|(&raw, f)| {
                Ok(RunRef {
                    id: run_id(raw)?,
                    size_bytes: f.size(),
                    entries: f.entries.len() as u64,
                })
            })
            .collect()
    }

    fn catch_up(&self, segments: &[SegId], footers: &dyn SegmentFooters) -> Result<CatchUpReport> {
        self.faults.alive()?;
        let mut report = CatchUpReport::default();
        // Entries of segments that no longer exist are dropped, whether or not
        // the caller names them.
        let held: BTreeSet<SegId> = lock(&self.state)
            .live
            .values()
            .map(|l| l.hint.seg)
            .collect();
        let mut gone = Vec::new();
        for seg in held {
            match footers.segment_footer(seg) {
                Ok(_) => {}
                Err(e) if e.kind() == ErrorKind::NotFound => gone.push(seg),
                Err(e) => return Err(e),
            }
        }
        {
            let mut st = lock(&self.state);
            for seg in &gone {
                let before = st.live.len();
                st.live.retain(|_, l| l.hint.seg != *seg);
                report.dropped += (before - st.live.len()) as u64;
                st.image.dropped.insert(seg.raw());
            }
        }
        let mut order: Vec<SegId> = segments.to_vec();
        order.sort_unstable();
        order.dedup();
        for seg in order {
            let footer = match footers.segment_footer(seg) {
                Ok(f) => f,
                Err(e) if e.kind() == ErrorKind::NotFound => continue,
                Err(e) => return Err(e),
            };
            let mut st = lock(&self.state);
            for e in &footer.entries {
                if let Entry::Vacant(slot) = st.live.entry(e.chunk_id) {
                    slot.insert(Live {
                        hint: Hint {
                            seg,
                            off: e.off,
                            clen: e.clen,
                        },
                        confirm: Confirm::Verified,
                        dirty: true,
                    });
                    report.inserted += 1;
                }
            }
        }
        Ok(report)
    }

    fn rebuild(&self, segments: &[SegId], footers: &dyn SegmentFooters) -> Result<()> {
        self.faults.alive()?;
        let mut order: Vec<SegId> = segments.to_vec();
        order.sort_unstable();
        order.dedup();
        let mut live = HashMap::new();
        for seg in order {
            let footer = match footers.segment_footer(seg) {
                Ok(f) => f,
                Err(e) if e.kind() == ErrorKind::NotFound => continue,
                Err(e) => return Err(e),
            };
            for e in &footer.entries {
                live.insert(
                    e.chunk_id,
                    Live {
                        hint: Hint {
                            seg,
                            off: e.off,
                            clen: e.clen,
                        },
                        confirm: Confirm::Verified,
                        dirty: false,
                    },
                );
            }
        }
        // As the real index: the new run first, then one manifest swap (an
        // interrupted rebuild never leaves an empty manifest); the old run
        // files stay for GC.
        let mut entries: Vec<(ChunkId, Hint)> = live.iter().map(|(id, l)| (*id, l.hint)).collect();
        entries.sort_by_key(|e| e.0);
        let run = if entries.is_empty() {
            None
        } else {
            let id = self.ids.allocate()?;
            lock(&self.state)
                .image
                .files
                .insert(id.raw(), RunFile { entries });
            Some(id.raw())
        };
        let mut st = lock(&self.state);
        st.live = live;
        st.image.manifest = run.into_iter().collect();
        st.image.dropped.clear();
        Ok(())
    }

    fn delete_run(&self, run: RunRef) -> Result<u64> {
        self.faults.point("index.delete_run.before")?;
        let mut st = lock(&self.state);
        st.image.manifest.retain(|&r| r != run.id.raw());
        Ok(st.image.files.remove(&run.id.raw()).map_or(0, |f| f.size()))
    }

    fn iter_entries(&self, f: &mut dyn FnMut(&IndexedChunk)) -> Result<()> {
        self.faults.alive()?;
        let st = lock(&self.state);
        let mut all: Vec<_> = st
            .live
            .iter()
            .map(|(id, l)| Self::view(&st, id, l))
            .collect();
        all.sort_by_key(|c| c.id);
        for c in &all {
            f(c);
        }
        Ok(())
    }
}
