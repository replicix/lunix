//! Mark: walk every root of a pinned root set and compute which extents,
//! segments and map packs are live (plan M17 rules 1, 3 and 12).
//!
//! # Roots
//!
//! The root set `R` is pinned when the mark starts and walked exactly: the
//! map roots of GC's retained FULLs ([`retained_fulls`]: every FULL from the
//! lowest `chain_floor` any retained FULL records up to the tip, N12), plus
//! the commits that snapshot, reader and hold pins name (read even when empty, so no GC
//! version can delete a future pin's base). Objects are listed at the same
//! moment; only objects in those listings are ever candidates, so an object
//! created later is never judged by a root set that predates it.
//!
//! # Liveness by chunk id
//!
//! A leaf entry resolves to an extent through its hint when the hint's
//! segment exists and its authenticated footer holds, at exactly that
//! `(off, clen)`, a chunk id that starts with the entry's id prefix; else
//! through the index by prefix (a stale hint left by compaction). The extent
//! that is **live** for a live id is the index's current copy of that id,
//! confirmed against the footer of the copy's segment; an older duplicate is
//! dead. When the index has no usable entry, the hinted extent is live. On a
//! lean volume (`id_len = 0`) the hinted extent of every entry is live
//! (hint-only liveness: there is nothing to resolve a stale hint with).
//!
//! When a hint points into segment `S` but the live copy is in another
//! segment `T`, `S` can only be deleted while `T` is durable: the mark records
//! the highest such `T` per `S` (`deps`, the delete-time index invariant of
//! rule 5).
//!
//! # Shared subtrees and packs
//!
//! Pack liveness uses a **full** `walk_nodes` of every root (two roots can
//! store the same node in two packs, M5 note). The leaf walk skips every node
//! hash an earlier root already reported, and walks the remaining leaves of a
//! root `leaves_per_unit` at a time through `walk_leaves_skipping` (the other
//! leaves stay in the skip set).
//!
//! # Units and their bounds
//!
//! | unit | work |
//! |---|---|
//! | init | the root set, the pins, four listings (sorted) |
//! | nodes | one full `walk_nodes` of one root: one callback per stored node (interior nodes are resident in the map) |
//! | leaves | at most `leaves_per_unit` new leaves (`× fanout` entries) resolved, plus one skip-set lookup per leaf of the root during the traversal |
//! | finish | sort and deduplicate the live extents of segments until `finish_per_unit` extents are done (a segment is at most 16 MiB of extents) |
//!
//! # Memory
//!
//! The live-extent set is a **sorted vector per segment** of [`LiveExtent`]
//! (24 bytes per live extent: the hint and the first chunk number that
//! references it, for compaction's LBA ordering). Beyond it a mark holds the
//! node hashes of the roots (32 bytes per stored node), one batch of leaf
//! entries, the per-segment totals and a bounded footer cache.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::time::Duration;

use lunix_format::commit::{Commit, Full, MapRoot};
use lunix_format::node::DataEntry;
use lunix_format::{CommitSeq, Hint, IdLen, IdPrefix, PackId, SegId};
use lunix_iface::gc::GcView;
use lunix_iface::index::RunRef;
use lunix_iface::map::{entry_hint, NodeHash};
use lunix_iface::store::{CommitInfo, PackInfo, SegmentInfo};
use lunix_iface::{Error, ErrorKind, Result};

use crate::config::GcConfig;
use crate::footers::{CacheBytes, FooterCache};

/// One root of a mark.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Root {
    /// The commit.
    pub seq: CommitSeq,
    /// Its map root.
    pub root: MapRoot,
}

/// A live extent and the lowest chunk number (in walk order) that reaches it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct LiveExtent {
    /// Where it is.
    pub hint: Hint,
    /// A chunk number that references it (compaction orders by it).
    pub ci: u64,
}

/// The result of a mark.
#[derive(Clone, Debug, Default)]
pub struct Liveness {
    /// The pinned root set, in walk order.
    pub roots: Vec<Root>,
    /// Commits pins name.
    pub pinned: BTreeSet<CommitSeq>,
    /// Live extents per segment, sorted by offset, one per hint.
    pub live: BTreeMap<SegId, Vec<LiveExtent>>,
    /// Live bytes (sum of `clen` of the live extents) per segment, only
    /// segments with a live extent.
    pub seg_live: BTreeMap<SegId, u64>,
    /// Packs holding a node of some root.
    pub live_packs: BTreeSet<PackId>,
    /// For a segment that hints point into while the live copy of the id is
    /// elsewhere: the segments holding such copies.
    pub deps: BTreeMap<SegId, BTreeSet<SegId>>,
    /// Segments holding a live extent that no confirmed index entry names
    /// (compaction could not relocate it, so they are no candidates).
    pub unindexed: BTreeSet<SegId>,
    /// Entries that resolved to no extent at all (already unreadable).
    pub unresolved: u64,
    /// Live extents in total.
    pub live_count: u64,
    /// Sealed segments when the mark started (ascending).
    pub segments: Vec<SegmentInfo>,
    /// Sealed map packs when the mark started (ascending).
    pub packs: Vec<PackInfo>,
    /// Index run files when the mark started (ascending).
    pub runs: Vec<RunRef>,
    /// Commit objects when the mark started (ascending).
    pub commits: Vec<CommitInfo>,
    /// When the mark started, on the view's monotonic clock.
    pub started: Duration,
    /// Map entries per segment, by hint, of the first root (the tip): what
    /// the engine's reference table is checked against. One count per
    /// segment with an entry; the other roots are not counted.
    pub tip_refs: BTreeMap<SegId, u64>,
    /// The segments hinted at by the entries of the other roots (retained
    /// FULLs and pins; `GcConfig::tip_refs` only). Leaves a root shares with
    /// an earlier root are skipped by the walk, so a segment named only
    /// through shared leaves is in `tip_refs`, not here. The engine's shadow
    /// check asserts that no offered segment is in it.
    pub older_refs: BTreeSet<SegId>,
}

impl Liveness {
    /// Live bytes of `seg` (0 when nothing in it is live).
    pub fn live_bytes(&self, seg: SegId) -> u64 {
        self.seg_live.get(&seg).copied().unwrap_or(0)
    }

    /// The live extents of `seg`, by offset.
    pub fn live_in(&self, seg: SegId) -> &[LiveExtent] {
        self.live.get(&seg).map_or(&[], Vec::as_slice)
    }

    /// The size of a listed segment.
    pub fn segment_size(&self, seg: SegId) -> Option<u64> {
        let i = self.segments.binary_search_by_key(&seg, |x| x.seg).ok()?;
        self.segments.get(i).map(|x| x.size_bytes)
    }

    /// The size of a listed pack.
    pub fn pack_size(&self, pack: PackId) -> Option<u64> {
        let i = self.packs.binary_search_by_key(&pack, |x| x.pack).ok()?;
        self.packs.get(i).map(|x| x.size_bytes)
    }

    /// A listed run.
    pub fn run(&self, run: lunix_format::RunId) -> Option<RunRef> {
        let i = self.runs.binary_search_by_key(&run, |x| x.id).ok()?;
        self.runs.get(i).copied()
    }

    /// The size of a listed commit object.
    pub fn commit_size(&self, seq: CommitSeq) -> Option<u64> {
        let i = self.commits.binary_search_by_key(&seq, |x| x.seq).ok()?;
        self.commits.get(i).map(|x| x.size_bytes)
    }

    /// Bytes of the live-extent set.
    pub fn live_set_bytes(&self) -> usize {
        usize::try_from(self.live_count)
            .unwrap_or(usize::MAX)
            .saturating_mul(std::mem::size_of::<LiveExtent>())
    }

    /// Total live bytes.
    pub fn total_live(&self) -> u64 {
        self.seg_live.values().sum()
    }

    /// Heap beyond the live-extent set.
    pub(crate) fn heap_bytes(&self) -> usize {
        (self.live.len()
            + self.seg_live.len()
            + self.deps.len()
            + self.unindexed.len()
            + self.tip_refs.len()
            + self.older_refs.len())
            * 48
            + self.live_packs.len() * 32
            + self.segments.capacity() * std::mem::size_of::<SegmentInfo>()
            + self.packs.capacity() * std::mem::size_of::<PackInfo>()
            + self.runs.capacity() * std::mem::size_of::<RunRef>()
            + self.commits.capacity() * std::mem::size_of::<CommitInfo>()
    }
}

/// The lowest `chain_floor` any retained FULL of the view records (the tip's
/// own floor when the view retains none).
pub fn keep_floor(view: &GcView<'_>) -> CommitSeq {
    view.retained
        .iter()
        .filter_map(|c| match c {
            Commit::Full(f) => Some(f.chain_floor),
            _ => None,
        })
        .chain(std::iter::once(view.tip.chain_floor))
        .min()
        .unwrap_or(view.tip.chain_floor)
}

/// GC's retained set (N12, option (b), lead decision of 2026-11-14): every
/// FULL from [`keep_floor`] up to the tip, ascending. These are roots and
/// what they name is never condemned, so after an offline `fsck --repair
/// --truncate-tip` the predecessor's own retained window is complete (both
/// its roots readable, every object it names present).
///
/// The view carries the commits from the tip's `chain_floor` up; the ones
/// below are read from the commit store, walking down from the tip's
/// `chain_floor`. A missing commit ends that walk (after a truncation the
/// floor commit's own floor may already be gone: nothing below it can be
/// kept); a commit that exists but does not open fails the call.
///
/// # Errors
/// Store and codec errors other than `NotFound`.
pub fn retained_fulls(view: &GcView<'_>) -> Result<Vec<Full>> {
    let mut out: Vec<Full> = view
        .retained
        .iter()
        .filter_map(|c| match c {
            Commit::Full(f) => Some(f.clone()),
            _ => None,
        })
        .collect();
    let have: BTreeSet<CommitSeq> = out.iter().map(|f| f.common.commit_seq).collect();
    let low = keep_floor(view);
    let mut seq = view.tip.chain_floor;
    while seq > low {
        seq = CommitSeq(seq.0 - 1);
        if have.contains(&seq) {
            continue;
        }
        let bytes = match view.commits.get(seq) {
            Ok(b) => b,
            Err(e) if e.kind() == ErrorKind::NotFound => break,
            Err(e) => return Err(e),
        };
        if let Commit::Full(f) = view.commit_codec.open_commit(seq, &bytes)? {
            out.push(f);
        }
    }
    out.sort_by_key(|f| f.common.commit_seq);
    Ok(out)
}

/// Reads the root set: GC's retained FULLs ([`retained_fulls`]), then the
/// pinned commits.
///
/// # Errors
/// `Invalid` when the view retains no FULL; a retained FULL below the tip's
/// `chain_floor` that exists but does not open, or a pin whose commit cannot
/// be read or is not a FULL, fails the mark (GC never runs past a root it
/// cannot resolve).
pub fn root_set(view: &GcView<'_>) -> Result<(Vec<Root>, BTreeSet<CommitSeq>)> {
    if !view.retained.iter().any(|c| matches!(c, Commit::Full(_))) {
        return Err(Error::invalid("the view retains no FULL commit"));
    }
    let mut roots: Vec<Root> = retained_fulls(view)?
        .iter()
        .map(|f| Root {
            seq: f.common.commit_seq,
            root: f.map_root,
        })
        .collect();
    if roots.is_empty() {
        return Err(Error::invalid("the view retains no FULL commit"));
    }
    // The tip first: later roots mostly share its subtrees.
    roots.sort_by_key(|r| std::cmp::Reverse(r.seq));
    let mut pinned = BTreeSet::new();
    for pin in view.pins.roots()? {
        pinned.insert(pin.commit_seq);
        if roots.iter().any(|r| r.seq == pin.commit_seq) {
            continue;
        }
        let bytes = view.commits.get(pin.commit_seq)?;
        match view.commit_codec.open_commit(pin.commit_seq, &bytes)? {
            Commit::Full(f) => roots.push(Root {
                seq: pin.commit_seq,
                root: f.map_root,
            }),
            _ => {
                return Err(Error::Unsupported {
                    what: "a pin on a commit that is not a FULL",
                })
            }
        }
    }
    Ok((roots, pinned))
}

#[derive(Debug)]
enum Stage {
    Init,
    Nodes,
    Leaves {
        new_leaves: Vec<NodeHash>,
        next: usize,
        root_hashes: Vec<NodeHash>,
    },
    Finish {
        segs: Vec<SegId>,
        next: usize,
    },
    Done,
}

/// A resumable mark.
#[derive(Debug)]
pub(crate) struct Mark {
    cfg: GcConfig,
    stage: Stage,
    root_idx: usize,
    /// Node hashes of the roots already walked.
    seen: HashSet<NodeHash>,
    /// `seen` plus the not-yet-walked leaves of the current root.
    skip: HashSet<NodeHash>,
    refs: Vec<(u64, DataEntry)>,
    footers: FooterCache,
    existing: BTreeSet<SegId>,
    out: Liveness,
    /// The tip the roots were pinned at, until the collector takes it.
    init_tip: Option<CommitSeq>,
}

impl Mark {
    pub(crate) fn new(cfg: GcConfig) -> Self {
        Self {
            footers: FooterCache::new(CacheBytes::new(cfg.footer_cache_bytes)),
            cfg,
            stage: Stage::Init,
            root_idx: 0,
            seen: HashSet::new(),
            skip: HashSet::new(),
            refs: Vec::new(),
            existing: BTreeSet::new(),
            out: Liveness::default(),
            init_tip: None,
        }
    }

    /// The tip the mark pinned its roots at, once, after the unit that did it.
    pub(crate) fn take_init_tip(&mut self) -> Option<CommitSeq> {
        self.init_tip.take()
    }

    /// The result, once the mark is done.
    pub(crate) fn into_liveness(self) -> Liveness {
        self.out
    }

    pub(crate) fn heap_bytes(&self) -> usize {
        let batch = match &self.stage {
            Stage::Leaves {
                new_leaves,
                root_hashes,
                ..
            } => (new_leaves.capacity() + root_hashes.capacity()) * 32,
            Stage::Finish { segs, .. } => segs.capacity() * 8,
            _ => 0,
        };
        (self.seen.capacity() + self.skip.capacity()) * 40
            + self.refs.capacity() * std::mem::size_of::<(u64, DataEntry)>()
            + self.footers.heap_bytes()
            + self.existing.len() * 24
            + batch
            + self.out.heap_bytes()
    }

    /// Adds the walked batch of the tip root to `out.tip_refs`, by hint. No
    /// root was walked before the tip, so every leaf of it is reported here.
    /// Neighbouring chunks mostly name one segment: runs are summed first.
    fn count_tip_refs(&mut self) {
        let mut run: Option<(SegId, u64)> = None;
        for (_, e) in &self.refs {
            run = match run {
                Some((seg, n)) if seg == e.seg => Some((seg, n + 1)),
                Some((seg, n)) => {
                    *self.out.tip_refs.entry(seg).or_default() += n;
                    Some((e.seg, 1))
                }
                None => Some((e.seg, 1)),
            };
        }
        if let Some((seg, n)) = run {
            *self.out.tip_refs.entry(seg).or_default() += n;
        }
    }

    /// Adds the segments the walked batch of a root after the tip names to
    /// `out.older_refs`, by hint.
    fn note_older_refs(&mut self) {
        let mut last: Option<SegId> = None;
        for (_, e) in &self.refs {
            if last != Some(e.seg) {
                self.out.older_refs.insert(e.seg);
                last = Some(e.seg);
            }
        }
    }

    /// Does one unit of work. Returns `true` when the mark is complete.
    pub(crate) fn unit(&mut self, view: &GcView<'_>) -> Result<bool> {
        match std::mem::replace(&mut self.stage, Stage::Done) {
            Stage::Init => {
                self.out.started = view.clock.monotonic();
                let (roots, pinned) = root_set(view)?;
                self.out.roots = roots;
                self.out.pinned = pinned;
                self.out.segments = view.segments.list()?;
                self.out.segments.sort_unstable_by_key(|x| x.seg);
                self.out.packs = view.packs.list_packs()?;
                self.out.packs.sort_unstable_by_key(|x| x.pack);
                self.out.runs = view.index.all_runs()?;
                self.out.runs.sort_unstable_by_key(|x| x.id);
                self.out.commits = view.commits.list()?;
                self.out.commits.sort_unstable_by_key(|x| x.seq);
                self.existing = self.out.segments.iter().map(|s| s.seg).collect();
                self.init_tip = Some(view.tip.commit_seq);
                self.stage = Stage::Nodes;
            }
            Stage::Nodes => {
                let Some(root) = self.out.roots.get(self.root_idx).copied() else {
                    let segs: Vec<SegId> = self.out.live.keys().copied().collect();
                    self.stage = Stage::Finish { segs, next: 0 };
                    return Ok(false);
                };
                let mut root_hashes = Vec::new();
                let mut new_leaves = Vec::new();
                let mut fresh = HashSet::new();
                let live_packs = &mut self.out.live_packs;
                let seen = &self.seen;
                view.map.walk_nodes(&root.root, &mut |n| {
                    live_packs.insert(n.loc.pack);
                    root_hashes.push(n.hash);
                    if n.level == 0 && !seen.contains(&n.hash) && fresh.insert(n.hash) {
                        new_leaves.push(n.hash);
                    }
                })?;
                if let Some(p) = root.root.pack {
                    self.out.live_packs.insert(p);
                }
                self.skip = self.seen.clone();
                self.skip.extend(new_leaves.iter().copied());
                self.stage = Stage::Leaves {
                    new_leaves,
                    next: 0,
                    root_hashes,
                };
            }
            Stage::Leaves {
                new_leaves,
                next,
                root_hashes,
            } => {
                let root = self
                    .out
                    .roots
                    .get(self.root_idx)
                    .copied()
                    .ok_or(Error::invalid("mark root index out of range"))?;
                let per = self.cfg.leaves_per_unit.max(1);
                if let Some(batch) = new_leaves.get(next..).filter(|b| !b.is_empty()) {
                    let batch = &batch[..batch.len().min(per)];
                    for h in batch {
                        self.skip.remove(h);
                    }
                    let refs = &mut self.refs;
                    refs.clear();
                    let walked =
                        view.map
                            .walk_leaves_skipping(&root.root, &self.skip, &mut |ci, e| {
                                refs.push((ci.0, *e));
                            });
                    self.skip.extend(batch.iter().copied());
                    walked?;
                    if self.cfg.tip_refs {
                        if self.root_idx == 0 {
                            self.count_tip_refs();
                        } else {
                            self.note_older_refs();
                        }
                    }
                    self.resolve(view)?;
                    let n = batch.len();
                    self.stage = Stage::Leaves {
                        new_leaves,
                        next: next + n,
                        root_hashes,
                    };
                } else {
                    self.seen.extend(root_hashes);
                    self.skip = HashSet::new();
                    self.root_idx += 1;
                    self.stage = Stage::Nodes;
                }
            }
            Stage::Finish { segs, mut next } => {
                let mut done = 0usize;
                while let Some(&seg) = segs.get(next) {
                    done += self.finish_segment(seg);
                    next += 1;
                    if done >= self.cfg.finish_per_unit {
                        break;
                    }
                }
                if next < segs.len() {
                    self.stage = Stage::Finish { segs, next };
                    return Ok(false);
                }
                self.finish();
                self.stage = Stage::Done;
                return Ok(true);
            }
            Stage::Done => return Ok(true),
        }
        Ok(false)
    }

    /// Runs the mark to the end (dry run, urgent pass, fsck).
    pub(crate) fn run(&mut self, view: &GcView<'_>) -> Result<()> {
        while !self.unit(view)? {}
        Ok(())
    }

    fn live(&mut self, hint: Hint, ci: u64) {
        self.out
            .live
            .entry(hint.seg)
            .or_default()
            .push(LiveExtent { hint, ci });
        self.out.live_count += 1;
    }

    fn dep(&mut self, from: SegId, on: SegId) {
        if from != on && self.existing.contains(&from) {
            self.out.deps.entry(from).or_default().insert(on);
        }
    }

    /// Confirms an index copy against the footer of its segment.
    fn confirm(&mut self, view: &GcView<'_>, id: &lunix_format::ChunkId, h: &Hint) -> Result<bool> {
        Ok(self
            .footers
            .get(view.segments, h.seg)?
            .is_some_and(|f| f.id_at(h).as_ref() == Some(id)))
    }

    fn resolve(&mut self, view: &GcView<'_>) -> Result<()> {
        let mut refs = std::mem::take(&mut self.refs);
        refs.sort_unstable_by_key(|(ci, e)| (e.seg, e.off, *ci));
        let lean = view.params.id_len == IdLen::Zero;
        for &(ci, ref e) in &refs {
            let hint = entry_hint(e);
            if lean {
                // Hint-only liveness: the hinted extent is kept whenever its
                // segment exists.
                if self.existing.contains(&hint.seg) {
                    self.live(hint, ci);
                } else {
                    self.out.unresolved += 1;
                }
                continue;
            }
            let at_hint = match self.footers.get(view.segments, hint.seg)? {
                Some(f) => f.id_at(&hint).filter(|id| e.prefix.matches(id)),
                None => None,
            };
            match at_hint {
                Some(id) => match view.index.get(&id)? {
                    Some(c) if c.hint == hint => self.live(hint, ci),
                    Some(c) if self.confirm(view, &id, &c.hint)? => {
                        self.live(c.hint, ci);
                        self.dep(hint.seg, c.hint.seg);
                    }
                    // No usable index entry: the hinted copy is the one, and
                    // compaction could not relocate it.
                    _ => {
                        self.live(hint, ci);
                        self.out.unindexed.insert(hint.seg);
                    }
                },
                None => self.stale(view, &e.prefix, hint, ci)?,
            }
        }
        refs.clear();
        self.refs = refs;
        Ok(())
    }

    /// A hint that does not land on its id: resolve through the index.
    fn stale(&mut self, view: &GcView<'_>, prefix: &IdPrefix, hint: Hint, ci: u64) -> Result<()> {
        let cands = match view.index.get_by_prefix(prefix) {
            Ok(c) => c,
            Err(e) if e.kind() == ErrorKind::Unsupported => Vec::new(),
            Err(e) => return Err(e),
        };
        let mut found = false;
        for c in cands {
            if self.confirm(view, &c.id, &c.hint)? {
                found = true;
                self.live(c.hint, ci);
                self.dep(hint.seg, c.hint.seg);
            }
        }
        if !found {
            self.out.unresolved += 1;
            if self.existing.contains(&hint.seg) {
                // Unresolvable but its segment exists: keep that segment.
                self.live(hint, ci);
                self.out.unindexed.insert(hint.seg);
            }
        }
        Ok(())
    }

    /// Sorts and deduplicates one segment's live extents; returns how many
    /// there were.
    fn finish_segment(&mut self, seg: SegId) -> usize {
        let Some(v) = self.out.live.get_mut(&seg) else {
            return 0;
        };
        let n = v.len();
        v.sort_unstable_by_key(|l| (l.hint.off, l.hint.clen, l.ci));
        v.dedup_by(|b, a| a.hint == b.hint);
        v.shrink_to_fit();
        let bytes: u64 = v.iter().map(|l| u64::from(l.hint.clen)).sum();
        let kept = v.len() as u64;
        self.out.live_count = self.out.live_count.saturating_sub(n as u64) + kept;
        self.out.seg_live.insert(seg, bytes);
        n
    }

    fn finish(&mut self) {
        // A segment that is live itself needs no dependency.
        let seg_live = &self.out.seg_live;
        self.out.deps.retain(|s, _| !seg_live.contains_key(s));
        self.seen = HashSet::new();
        self.skip = HashSet::new();
        self.refs = Vec::new();
        self.footers.clear();
        if self.out.unresolved > 0 {
            tracing::warn!(
                unresolved = self.out.unresolved,
                "gc mark: map entries that resolve to no extent (already unreadable)"
            );
        }
    }
}

/// A synchronous mark of the view's current roots, changing nothing: the
/// per-segment liveness `lunix gc --dry-run` and fsck (M17.B) compare with.
///
/// # Errors
/// As a mark step: store, index and map errors; an unreadable pin.
pub fn liveness(view: &GcView<'_>, cfg: GcConfig) -> Result<Liveness> {
    let mut m = Mark::new(cfg);
    m.run(view)?;
    Ok(m.into_liveness())
}
