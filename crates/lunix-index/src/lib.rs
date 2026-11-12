//! The chunk index: chunk id to the extent that holds it (DESIGN section 7
//! "Chunk index", ADR-14), as an LSM of checksummed runs plus a memtable.
//!
//! # Shape
//!
//! * **Memtable**: ids inserted since the last [`persist`](ChunkIndex::persist),
//!   built from sealed segments in this process, so its hits are
//!   [`Confirm::Verified`]. It also holds relocated entries until the segment
//!   they name is named by a durable commit.
//! * **Runs** `index/<run:016x>` (on-disk-formats.md section 17): immutable,
//!   sorted, checksummed and never MAC'd. Each has a RAM filter (a Bloom filter
//!   keyed by `xxh3`) and a block directory, about 2.4 bytes per entry in all.
//!   A hit from a run is [`Confirm::Hinted`] (D17).
//! * **Manifest** `index/manifest` (+ `manifest.prev`): the runs in recency
//!   order, the condemned segments and the dropped segments, replaced
//!   atomically. It is local only and not a frozen format.
//!
//! # The persist rule
//!
//! `persist(upto)` writes a run with, for every id, its newest known location
//! in a segment `<= upto`. A relocated entry whose new segment is above `upto`
//! stays in memory and the run gets the **previous** location instead (kept in
//! a per-id chain beside the memtable), so a crash before the new segment is
//! named by a commit leaves the old entry current and loses nothing. The
//! caller keeps the invariant of the trait: every existing segment `<= upto`
//! is named by a durable commit.
//!
//! # Recovery
//!
//! [`LsmIndex::open`] validates every run end to end. If the newest manifest or
//! one of its runs is damaged it falls back to the previous generation (and in
//! the worst case to an empty index) and says so in [`OpenReport`]; the caller
//! then runs [`catch_up`](ChunkIndex::catch_up) with the segment ids named by
//! the retained commit chain, which restores every entry of those segments from
//! their footers. The condemned set is taken from the newest manifest that
//! decodes, but a damaged manifest can lose a flag: the GC's own condemned
//! list (`state/gc/condemned`) is authoritative and must be re-applied by M17
//! after open.
//!
//! # Limits
//!
//! `rebuild` and `iter_entries` hold the memtable (rebuild) or a sorted copy of
//! it (iter) in RAM; compaction and run writing stream. After
//! `persist`, entries live only in runs, so later hits are `Hinted` even for
//! chunks this process just wrote.

#![forbid(unsafe_code)]

mod bloom;
mod manifest;
mod merge;
mod run;

use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::hash::{BuildHasherDefault, Hasher};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};

use lunix_format::index_run::IndexEntry;
use lunix_format::{ChunkId, Hint, IdLen, IdPrefix, ObjId, RunId, SegId};
use lunix_iface::crashpoint;
use lunix_iface::index::{CatchUpReport, ChunkIndex, Confirm, DedupHit, IndexedChunk, RunRef};
use lunix_iface::store::{IdAllocator, SegmentFooters};
use lunix_iface::{Error, ErrorKind, Result};

use manifest::{Manifest, RunMeta};
use merge::{Merge, Source};
use run::{run_path, sync_dir, Run, RunWriter};

/// Tuning that tests lower; the defaults are the documented ones.
#[derive(Clone, Copy, Debug)]
pub struct IndexOptions {
    /// Runs of one size level that are tolerated before the adjacent ones are
    /// merged into one (size-tiered compaction). Documented value: 8.
    pub max_runs_per_level: usize,
}

impl Default for IndexOptions {
    fn default() -> Self {
        Self {
            max_runs_per_level: 8,
        }
    }
}

/// What [`LsmIndex::open`] found.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct OpenReport {
    /// A manifest file (current or previous) decoded.
    pub manifest_found: bool,
    /// The generation now in force.
    pub generation: u64,
    /// The newest manifest or one of its runs was unusable and an older
    /// generation (or nothing) was used. The caller must `catch_up`.
    pub fell_back: bool,
    /// Runs that were named but failed validation.
    pub discarded_runs: Vec<RunId>,
}

/// Hashes a chunk id with `xxh3`: ids are already hashes, but a volume with a
/// weak id function must not be able to degrade the table.
#[derive(Default)]
struct IdHasher(u64);

impl Hasher for IdHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        self.0 = xxhash_rust::xxh3::xxh3_64_with_seed(bytes, self.0);
    }

    // The slice length prefix carries no information for a fixed-size id.
    fn write_usize(&mut self, _: usize) {}
}

type Mem = HashMap<ChunkId, Hint, BuildHasherDefault<IdHasher>>;

#[derive(Default)]
struct Inner {
    mem: Mem,
    /// Superseded locations of ids in `mem`, oldest first (see the persist rule).
    shadow: HashMap<ChunkId, Vec<Hint>>,
    /// Oldest first.
    runs: Vec<Arc<Run>>,
    condemned: BTreeSet<SegId>,
    dropped: BTreeSet<SegId>,
}

/// The index over a directory. See the crate docs.
pub struct LsmIndex {
    dir: PathBuf,
    ids: Arc<dyn IdAllocator>,
    id_len: IdLen,
    opts: IndexOptions,
    /// Serialises every operation that writes files or replaces the manifest,
    /// and holds the manifest as it is on disk. Lock order: this, then `inner`.
    committed: Mutex<Manifest>,
    inner: RwLock<Inner>,
    report: OpenReport,
}

impl std::fmt::Debug for LsmIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LsmIndex")
            .field("dir", &self.dir)
            .field("report", &self.report)
            .finish_non_exhaustive()
    }
}

fn write_synced(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut f = fs::File::create(path)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    Ok(())
}

/// Replaces the manifest durably, keeping the generation it replaces as
/// `manifest.prev`.
fn swap_manifest(dir: &Path, committed: &mut Manifest, mut new: Manifest) -> Result<()> {
    new.generation = committed.generation.checked_add(1).ok_or(Error::Refused {
        why: "manifest generation exhausted",
    })?;
    let tmp = dir.join("manifest.tmp");
    write_synced(&tmp, &new.encode())?;
    crashpoint!("index.before_manifest_swap");
    let prev_tmp = dir.join("manifest.prev.tmp");
    write_synced(&prev_tmp, &committed.encode())?;
    fs::rename(&prev_tmp, dir.join("manifest.prev"))?;
    fs::rename(&tmp, dir.join("manifest"))?;
    sync_dir(dir)?;
    crashpoint!("index.after_manifest_swap");
    *committed = new;
    Ok(())
}

fn meta_of(r: &Run) -> RunMeta {
    RunMeta {
        id: r.id,
        size: r.size,
        entries: r.entries,
    }
}

fn run_ref(r: &Run) -> RunRef {
    RunRef {
        id: r.id,
        size_bytes: r.size,
        entries: r.entries,
    }
}

fn hint_of(e: &IndexEntry) -> Hint {
    Hint {
        seg: e.seg,
        off: e.off,
        clen: e.clen,
    }
}

fn entry_of(id: ChunkId, h: Hint) -> IndexEntry {
    IndexEntry {
        chunk_id: id,
        seg: h.seg,
        off: h.off,
        clen: h.clen,
    }
}

/// The newest location of `id` across the memtable and the runs.
fn resolve(inner: &Inner, id: &ChunkId) -> Result<Option<(Hint, Confirm)>> {
    if let Some(h) = inner.mem.get(id) {
        return Ok(Some((*h, Confirm::Verified)));
    }
    for run in inner.runs.iter().rev() {
        if let Some(e) = run.lookup(id)? {
            if inner.dropped.contains(&e.seg) {
                return Ok(None);
            }
            return Ok(Some((hint_of(&e), Confirm::Hinted)));
        }
    }
    Ok(None)
}

/// Records `id` at `hint`; a different previous memtable location becomes a
/// persist fallback.
fn put(inner: &mut Inner, id: ChunkId, hint: Hint) {
    if let Some(old) = inner.mem.insert(id, hint) {
        if old != hint {
            inner.shadow.entry(id).or_default().push(old);
        }
    }
}

fn apply_drop(inner: &mut Inner, seg: SegId) {
    inner.dropped.insert(seg);
    let hit: Vec<ChunkId> = inner
        .mem
        .iter()
        .filter(|(id, h)| {
            h.seg == seg
                || inner
                    .shadow
                    .get(id)
                    .is_some_and(|c| c.iter().any(|x| x.seg == seg))
        })
        .map(|(id, _)| *id)
        .collect();
    for id in hit {
        let mut chain = inner.shadow.remove(&id).unwrap_or_default();
        chain.retain(|h| h.seg != seg);
        if inner.mem.get(&id).is_some_and(|h| h.seg == seg) {
            match chain.pop() {
                Some(h) => {
                    inner.mem.insert(id, h);
                }
                None => {
                    inner.mem.remove(&id);
                }
            }
        }
        if !chain.is_empty() {
            inner.shadow.insert(id, chain);
        }
    }
}

fn level(entries: u64) -> u32 {
    const BASE: u64 = 4096;
    match entries / BASE {
        0 => 0,
        n => n.ilog(8) + 1,
    }
}

/// The oldest window of more than `max` adjacent runs of one level.
fn find_window(runs: &[Arc<Run>], max: usize) -> Option<(usize, usize)> {
    let mut i = 0;
    while i < runs.len() {
        let l = level(runs[i].entries);
        let mut j = i + 1;
        while j < runs.len() && level(runs[j].entries) == l {
            j += 1;
        }
        if j - i > max.max(1) {
            return Some((i, j));
        }
        i = j;
    }
    None
}

fn open_runs(dir: &Path, m: &Manifest) -> (Vec<Arc<Run>>, Vec<RunId>) {
    let mut good = Vec::new();
    let mut bad = Vec::new();
    for meta in &m.runs {
        match Run::open(dir, meta.id) {
            Ok(r) if r.entries == meta.entries && r.size == meta.size => good.push(Arc::new(r)),
            _ => bad.push(meta.id),
        }
    }
    (good, bad)
}

impl LsmIndex {
    /// Opens (creating if needed) the index under `root/index/`.
    ///
    /// `ids` is the volume's object-id allocator (runs share its counter).
    /// `id_len` is the volume's `id_len`; with `Zero` prefix lookups are
    /// `Unsupported`.
    ///
    /// # Errors
    /// `Io` for directory failures. Damaged manifests and runs are not errors:
    /// see [`OpenReport`].
    pub fn open(root: &Path, ids: Arc<dyn IdAllocator>, id_len: IdLen) -> Result<Self> {
        Self::open_with(root, ids, id_len, IndexOptions::default())
    }

    /// [`open`](Self::open) with explicit [`IndexOptions`].
    pub fn open_with(
        root: &Path,
        ids: Arc<dyn IdAllocator>,
        id_len: IdLen,
        opts: IndexOptions,
    ) -> Result<Self> {
        let dir = root.join("index");
        fs::create_dir_all(&dir)?;
        for ent in fs::read_dir(&dir)? {
            let ent = ent?;
            if ent.file_name().to_string_lossy().ends_with(".tmp") {
                fs::remove_file(ent.path())?;
            }
        }
        let mut manifest_files_bad = false;
        let mut cands: Vec<Manifest> = Vec::new();
        for name in ["manifest", "manifest.prev"] {
            match fs::read(dir.join(name)) {
                Ok(b) => match Manifest::decode(&b) {
                    Ok(m) => cands.push(m),
                    Err(_) => manifest_files_bad = true,
                },
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        cands.sort_by_key(|m| std::cmp::Reverse(m.generation));
        cands.dedup_by_key(|m| m.generation);

        let mut report = OpenReport {
            manifest_found: !cands.is_empty(),
            fell_back: manifest_files_bad,
            ..OpenReport::default()
        };
        let (committed, runs) = match cands.first() {
            None => (Manifest::default(), Vec::new()),
            Some(newest) => {
                let (good, bad) = open_runs(&dir, newest);
                if bad.is_empty() {
                    (newest.clone(), good)
                } else {
                    report.fell_back = true;
                    report.discarded_runs.extend(&bad);
                    let (good, bad2) = match cands.get(1) {
                        Some(prev) => open_runs(&dir, prev),
                        None => (Vec::new(), Vec::new()),
                    };
                    let (good, dropped_extra) = if bad2.is_empty() {
                        (
                            good,
                            cands.get(1).map(|p| p.dropped.clone()).unwrap_or_default(),
                        )
                    } else {
                        report.discarded_runs.extend(&bad2);
                        (Vec::new(), BTreeSet::new())
                    };
                    let mut dropped = newest.dropped.clone();
                    dropped.extend(dropped_extra);
                    (
                        Manifest {
                            generation: newest.generation,
                            runs: good.iter().map(|r| meta_of(r)).collect(),
                            condemned: newest.condemned.clone(),
                            dropped,
                        },
                        good,
                    )
                }
            }
        };
        let mut committed = committed;
        if report.fell_back {
            // Put a clean manifest on disk at once so that the damaged one can
            // never become `manifest.prev` of a later swap.
            let same = committed.clone();
            swap_manifest(&dir, &mut committed, same)?;
        }
        report.generation = committed.generation;
        let inner = Inner {
            condemned: committed.condemned.clone(),
            dropped: committed.dropped.clone(),
            runs,
            ..Inner::default()
        };
        Ok(Self {
            dir,
            ids,
            id_len,
            opts,
            committed: Mutex::new(committed),
            inner: RwLock::new(inner),
            report,
        })
    }

    /// What `open` found.
    pub fn open_report(&self) -> &OpenReport {
        &self.report
    }

    /// Heap bytes of the RAM filters and block directories of all runs (the
    /// quantity the 16 bytes per chunk budget is about).
    pub fn filter_ram_bytes(&self) -> usize {
        self.rd().runs.iter().map(|r| r.ram_bytes()).sum()
    }

    /// Entries in the memtable (not yet in a run).
    pub fn memtable_len(&self) -> usize {
        self.rd().mem.len()
    }

    fn rd(&self) -> RwLockReadGuard<'_, Inner> {
        self.inner.read().unwrap_or_else(|p| p.into_inner())
    }

    fn wr(&self) -> RwLockWriteGuard<'_, Inner> {
        self.inner.write().unwrap_or_else(|p| p.into_inner())
    }

    fn writer(&self) -> MutexGuard<'_, Manifest> {
        self.committed.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn chunk(inner: &Inner, id: ChunkId, hint: Hint) -> IndexedChunk {
        IndexedChunk {
            id,
            hint,
            condemned: inner.condemned.contains(&hint.seg),
        }
    }

    /// Merges the oldest window of too many same-level runs, repeatedly.
    /// Returns the ids of the runs it created.
    fn compact_locked(&self, committed: &mut Manifest) -> Result<Vec<RunId>> {
        let mut made = Vec::new();
        loop {
            let (window, runs) = {
                let g = self.rd();
                (
                    find_window(&g.runs, self.opts.max_runs_per_level),
                    g.runs.clone(),
                )
            };
            let Some((i, j)) = window else {
                return Ok(made);
            };
            let all = i == 0 && j == runs.len();
            // Dropped entries are purged only when no older run can hold an
            // older copy of the same id that the purge would resurrect.
            let purge = i == 0;
            let dropped = committed.dropped.clone();
            let mut covered = BTreeSet::new();
            let mut expected = 0u64;
            for r in &runs[i..j] {
                covered.extend(r.covered.iter().copied());
                expected = expected.saturating_add(r.entries);
            }
            if purge {
                covered.retain(|s| !dropped.contains(s));
            }
            let sources: Vec<Source> = runs[i..j]
                .iter()
                .map(|r| Box::new(r.iter_from(None)) as Source)
                .collect();
            let id = self.ids.allocate()?;
            let mut w = RunWriter::create(&self.dir, id, expected)?;
            for e in Merge::new(sources)? {
                let e = e?;
                if purge && dropped.contains(&e.seg) {
                    continue;
                }
                w.push(&e)?;
            }
            let covered: Vec<SegId> = covered.into_iter().collect();
            let new_run = w.finish(&covered)?.map(Arc::new);
            let mut next = committed.clone();
            next.runs.splice(i..j, new_run.iter().map(|r| meta_of(r)));
            if all {
                next.dropped.clear();
            }
            swap_manifest(&self.dir, committed, next)?;
            let mut g = self.wr();
            g.runs.splice(i..j, new_run.iter().cloned());
            if all {
                g.dropped.clear();
            }
            drop(g);
            if let Some(r) = &new_run {
                made.push(r.id);
            }
        }
    }

    /// Drops `segs` from lookups and records them in the manifest.
    fn drop_locked(&self, committed: &mut Manifest, segs: &[SegId]) -> Result<u64> {
        if segs.is_empty() {
            return Ok(0);
        }
        let mut removed = 0u64;
        {
            let g = self.rd();
            for &seg in segs {
                removed += g.mem.values().filter(|h| h.seg == seg).count() as u64;
                for r in g
                    .runs
                    .iter()
                    .filter(|r| r.covered.binary_search(&seg).is_ok())
                {
                    for e in r.iter_from(None) {
                        if e?.seg == seg {
                            removed += 1;
                        }
                    }
                }
            }
        }
        let mut next = committed.clone();
        next.dropped.extend(segs.iter().copied());
        swap_manifest(&self.dir, committed, next)?;
        let mut g = self.wr();
        for &seg in segs {
            apply_drop(&mut g, seg);
        }
        Ok(removed)
    }
}

impl ChunkIndex for LsmIndex {
    fn get_for_dedup(&self, id: &ChunkId) -> Result<Option<DedupHit>> {
        let g = self.rd();
        Ok(resolve(&g, id)?
            .filter(|(h, _)| !g.condemned.contains(&h.seg))
            .map(|(hint, confirm)| DedupHit { hint, confirm }))
    }

    fn get(&self, id: &ChunkId) -> Result<Option<IndexedChunk>> {
        let g = self.rd();
        Ok(resolve(&g, id)?.map(|(h, _)| Self::chunk(&g, *id, h)))
    }

    fn get_by_prefix(&self, prefix: &IdPrefix) -> Result<Vec<IndexedChunk>> {
        if self.id_len.is_empty() || prefix.id_len().is_empty() {
            return Err(Error::Unsupported {
                what: "prefix lookup on a volume with id_len = 0",
            });
        }
        let bytes = prefix.as_bytes();
        let (mut lo, mut hi) = ([0u8; 32], [0xffu8; 32]);
        lo[..bytes.len()].copy_from_slice(bytes);
        hi[..bytes.len()].copy_from_slice(bytes);
        let (lo, hi) = (ChunkId(lo), ChunkId(hi));
        let g = self.rd();
        let mut ids: BTreeSet<ChunkId> = g
            .mem
            .keys()
            .filter(|id| **id >= lo && **id <= hi)
            .copied()
            .collect();
        for r in &g.runs {
            for e in r.iter_from(Some(&lo)) {
                let e = e?;
                if e.chunk_id > hi {
                    break;
                }
                ids.insert(e.chunk_id);
            }
        }
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            // Only the newest copy is a candidate.
            if let Some((h, _)) = resolve(&g, &id)? {
                out.push(Self::chunk(&g, id, h));
            }
        }
        Ok(out)
    }

    fn insert(&self, id: ChunkId, hint: Hint) -> Result<()> {
        put(&mut self.wr(), id, hint);
        Ok(())
    }

    fn relocate(&self, id: &ChunkId, old: &Hint, new: &Hint) -> Result<bool> {
        let mut g = self.wr();
        match resolve(&g, id)? {
            Some((cur, _)) if cur == *old => {
                put(&mut g, *id, *new);
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    fn mark_condemned(&self, seg: SegId) -> Result<()> {
        let mut c = self.writer();
        if c.condemned.contains(&seg) {
            return Ok(());
        }
        let mut next = c.clone();
        next.condemned.insert(seg);
        swap_manifest(&self.dir, &mut c, next)?;
        self.wr().condemned.insert(seg);
        crashpoint!("index.after_condemn_flag");
        Ok(())
    }

    fn unmark_condemned(&self, seg: SegId) -> Result<()> {
        let mut c = self.writer();
        if !c.condemned.contains(&seg) {
            return Ok(());
        }
        let mut next = c.clone();
        next.condemned.remove(&seg);
        swap_manifest(&self.dir, &mut c, next)?;
        self.wr().condemned.remove(&seg);
        Ok(())
    }

    fn condemned(&self) -> Result<Vec<SegId>> {
        Ok(self.rd().condemned.iter().copied().collect())
    }

    fn drop_segment(&self, seg: SegId) -> Result<()> {
        let mut c = self.writer();
        self.drop_locked(&mut c, &[seg]).map(|_| ())
    }

    fn persist(&self, upto_seg: SegId) -> Result<Vec<RunRef>> {
        struct Pick {
            id: ChunkId,
            hint: Hint,
            /// Index in the chain, or `None` when the pick is the current entry.
            chain_idx: Option<usize>,
            cur: Hint,
            chain_len: usize,
        }
        let mut committed = self.writer();
        let mut picks: Vec<Pick> = {
            let g = self.rd();
            g.mem
                .iter()
                .filter_map(|(id, cur)| {
                    let chain = g.shadow.get(id).map_or(&[][..], Vec::as_slice);
                    let (hint, chain_idx) = if cur.seg <= upto_seg {
                        (*cur, None)
                    } else {
                        let k = chain.iter().rposition(|h| h.seg <= upto_seg)?;
                        (chain[k], Some(k))
                    };
                    Some(Pick {
                        id: *id,
                        hint,
                        chain_idx,
                        cur: *cur,
                        chain_len: chain.len(),
                    })
                })
                .collect()
        };
        picks.sort_unstable_by_key(|p| p.id);
        let mut made: Vec<RunId> = Vec::new();
        if !picks.is_empty() {
            let id = self.ids.allocate()?;
            let mut w = RunWriter::create(&self.dir, id, picks.len() as u64)?;
            let mut covered = BTreeSet::new();
            for p in &picks {
                w.push(&entry_of(p.id, p.hint))?;
                covered.insert(p.hint.seg);
            }
            let covered: Vec<SegId> = covered.into_iter().collect();
            let run = w
                .finish(&covered)?
                .ok_or(Error::invalid("persist produced an empty run"))?;
            let mut next = committed.clone();
            next.runs.push(meta_of(&run));
            swap_manifest(&self.dir, &mut committed, next)?;
            let mut g = self.wr();
            g.runs.push(Arc::new(run));
            for p in picks {
                let unchanged = g.mem.get(&p.id) == Some(&p.cur)
                    && g.shadow.get(&p.id).map_or(0, Vec::len) == p.chain_len;
                if !unchanged {
                    continue;
                }
                match p.chain_idx {
                    None => {
                        g.mem.remove(&p.id);
                        g.shadow.remove(&p.id);
                    }
                    Some(k) => {
                        if let Some(chain) = g.shadow.get_mut(&p.id) {
                            chain.drain(..=k);
                            if chain.is_empty() {
                                g.shadow.remove(&p.id);
                            }
                        }
                    }
                }
            }
            drop(g);
            made.push(id);
        }
        made.extend(self.compact_locked(&mut committed)?);
        let g = self.rd();
        Ok(g.runs
            .iter()
            .filter(|r| made.contains(&r.id))
            .map(|r| run_ref(r))
            .collect())
    }

    fn runs(&self) -> Result<Vec<RunRef>> {
        Ok(self.rd().runs.iter().map(|r| run_ref(r)).collect())
    }

    fn all_runs(&self) -> Result<Vec<RunRef>> {
        let mut out = Vec::new();
        for ent in fs::read_dir(&self.dir)? {
            let name = ent?.file_name();
            let Some(name) = name.to_str() else { continue };
            // `ObjId` parses exactly 16 lowercase hex digits; manifests and
            // temporary files never match.
            let Ok(id) = name.parse::<ObjId>() else {
                continue;
            };
            match run::peek(&self.dir, id) {
                Ok((size_bytes, entries)) => out.push(RunRef {
                    id,
                    size_bytes,
                    entries,
                }),
                Err(e) if e.kind() == ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        out.sort_by_key(|r| r.id);
        Ok(out)
    }

    fn catch_up(&self, segments: &[SegId], footers: &dyn SegmentFooters) -> Result<CatchUpReport> {
        let mut committed = self.writer();
        let mut report = CatchUpReport::default();
        let covered: BTreeSet<SegId> = self
            .rd()
            .runs
            .iter()
            .flat_map(|r| r.covered.iter().copied())
            .collect();
        let named: BTreeSet<SegId> = segments.iter().copied().collect();
        let mut present: BTreeSet<SegId> = BTreeSet::new();
        let mut missing: BTreeSet<SegId> = BTreeSet::new();
        for &seg in &named {
            if covered.contains(&seg) {
                continue;
            }
            match footers.segment_footer(seg) {
                Ok(fb) => {
                    if fb.object_id != seg {
                        return Err(Error::Corrupt {
                            what: "segment footer",
                            detail: "object id differs from the segment",
                        });
                    }
                    present.insert(seg);
                    let mut g = self.wr();
                    for e in &fb.entries {
                        let hint = Hint {
                            seg,
                            off: e.off,
                            clen: e.clen,
                        };
                        // Higher segment id is the newer copy; an existing
                        // entry in the same or a newer segment stands.
                        if resolve(&g, &e.chunk_id)?.is_none_or(|(h, _)| h.seg < seg) {
                            put(&mut g, e.chunk_id, hint);
                            report.inserted += 1;
                        }
                    }
                }
                Err(e) if e.kind() == ErrorKind::NotFound => {
                    missing.insert(seg);
                }
                Err(e) => return Err(e),
            }
        }
        // Existence check of every other segment the index points into.
        let referenced: BTreeSet<SegId> = {
            let g = self.rd();
            let mut s: BTreeSet<SegId> = covered.clone();
            for h in g.mem.values() {
                s.insert(h.seg);
            }
            for c in g.shadow.values() {
                s.extend(c.iter().map(|h| h.seg));
            }
            s
        };
        for seg in referenced {
            if present.contains(&seg) || missing.contains(&seg) {
                continue;
            }
            match footers.segment_footer(seg) {
                Ok(_) => {}
                Err(e) if e.kind() == ErrorKind::NotFound => {
                    missing.insert(seg);
                }
                Err(e) => return Err(e),
            }
        }
        let gone: Vec<SegId> = {
            let g = self.rd();
            missing
                .into_iter()
                .filter(|s| !g.dropped.contains(s))
                .collect()
        };
        report.dropped = self.drop_locked(&mut committed, &gone)?;
        Ok(report)
    }

    fn rebuild(&self, segments: &[SegId], footers: &dyn SegmentFooters) -> Result<()> {
        let mut committed = self.writer();
        let ordered: BTreeSet<SegId> = segments.iter().copied().collect();
        let mut mem = Mem::default();
        for &seg in &ordered {
            let fb = footers.segment_footer(seg)?;
            if fb.object_id != seg {
                return Err(Error::Corrupt {
                    what: "segment footer",
                    detail: "object id differs from the segment",
                });
            }
            for e in &fb.entries {
                // Ascending segment order: the newest copy overwrites.
                mem.insert(
                    e.chunk_id,
                    Hint {
                        seg,
                        off: e.off,
                        clen: e.clen,
                    },
                );
            }
        }
        let mut next = committed.clone();
        next.runs.clear();
        next.dropped.clear();
        swap_manifest(&self.dir, &mut committed, next)?;
        let mut g = self.wr();
        g.mem = mem;
        g.shadow.clear();
        g.runs.clear();
        g.dropped.clear();
        Ok(())
    }

    fn delete_run(&self, run: RunRef) -> Result<u64> {
        let mut c = self.writer();
        if c.runs.iter().any(|r| r.id == run.id) {
            let mut next = c.clone();
            next.runs.retain(|r| r.id != run.id);
            swap_manifest(&self.dir, &mut c, next)?;
            self.wr().runs.retain(|r| r.id != run.id);
        }
        let path = run_path(&self.dir, run.id);
        let size = match fs::metadata(&path) {
            Ok(m) => m.len(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(e.into()),
        };
        fs::remove_file(&path)?;
        sync_dir(&self.dir)?;
        Ok(size)
    }

    fn iter_entries(&self, f: &mut dyn FnMut(&IndexedChunk)) -> Result<()> {
        let (mem, runs, condemned, dropped) = {
            let g = self.rd();
            let mut mem: Vec<IndexEntry> = g.mem.iter().map(|(id, h)| entry_of(*id, *h)).collect();
            mem.sort_unstable_by_key(|e| e.chunk_id);
            (mem, g.runs.clone(), g.condemned.clone(), g.dropped.clone())
        };
        let mut sources: Vec<Source> = runs
            .iter()
            .map(|r| Box::new(r.iter_from(None)) as Source)
            .collect();
        sources.push(Box::new(mem.into_iter().map(Ok)));
        for e in Merge::new(sources)? {
            let e = e?;
            if dropped.contains(&e.seg) {
                continue;
            }
            f(&IndexedChunk {
                id: e.chunk_id,
                hint: hint_of(&e),
                condemned: condemned.contains(&e.seg),
            });
        }
        Ok(())
    }
}
