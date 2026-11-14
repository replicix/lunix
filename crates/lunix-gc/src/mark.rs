//! Mark: walk every root of a pinned root set and compute which extents,
//! segments and map packs are live (plan M17 rules 1, 3 and 12).
//!
//! # Roots
//!
//! The root set `R` is pinned when the mark starts and walked exactly: the
//! map roots of the retained FULL commits of the view, plus the commits that
//! snapshot, reader and hold pins name (read even when empty, so no GC
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
//! root in at most `max_leaf_batches` batches through `walk_leaves_skipping`
//! (the other leaves are put in the skip set), so one budgeted unit of work is
//! about `1/64` of a root.
//!
//! # Memory
//!
//! The live-extent set is a **sorted vector** of [`LiveExtent`] (24 bytes per
//! live extent: the hint and the first chunk number that references it, for
//! compaction's LBA ordering). Beyond it a mark holds the node hashes of the
//! roots (32 bytes per stored node), one batch of leaf entries, the
//! per-segment totals and a bounded footer cache.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::time::Duration;

use lunix_format::commit::{Commit, MapRoot};
use lunix_format::node::DataEntry;
use lunix_format::{CommitSeq, Hint, IdLen, IdPrefix, PackId, SegId};
use lunix_iface::gc::GcView;
use lunix_iface::index::RunRef;
use lunix_iface::map::{entry_hint, NodeHash};
use lunix_iface::store::{CommitInfo, PackInfo, SegmentInfo};
use lunix_iface::{Error, ErrorKind, Result};

use crate::config::GcConfig;
use crate::footers::FooterCache;

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
    /// Live extents, sorted by `(seg, off)`, one per hint.
    pub live: Vec<LiveExtent>,
    /// Live bytes (sum of `clen` of the live extents) per segment, only
    /// segments with a live extent.
    pub seg_live: BTreeMap<SegId, u64>,
    /// Packs holding a node of some root.
    pub live_packs: BTreeSet<PackId>,
    /// For a segment that hints point into while the live copy of the id is
    /// elsewhere: the highest segment holding such a copy.
    pub deps: BTreeMap<SegId, SegId>,
    /// Entries that resolved to no extent at all (already unreadable).
    pub unresolved: u64,
    /// Sealed segments when the mark started.
    pub segments: Vec<SegmentInfo>,
    /// Sealed map packs when the mark started.
    pub packs: Vec<PackInfo>,
    /// Index run files when the mark started.
    pub runs: Vec<RunRef>,
    /// Commit objects when the mark started.
    pub commits: Vec<CommitInfo>,
    /// When the mark started, on the view's monotonic clock.
    pub started: Duration,
}

impl Liveness {
    /// Live bytes of `seg` (0 when nothing in it is live).
    pub fn live_bytes(&self, seg: SegId) -> u64 {
        self.seg_live.get(&seg).copied().unwrap_or(0)
    }

    /// The live extents of `seg`, by offset.
    pub fn live_in(&self, seg: SegId) -> &[LiveExtent] {
        let lo = self.live.partition_point(|e| e.hint.seg < seg);
        let hi = self.live.partition_point(|e| e.hint.seg <= seg);
        self.live.get(lo..hi).unwrap_or(&[])
    }

    /// Total live bytes.
    pub fn total_live(&self) -> u64 {
        self.seg_live.values().sum()
    }

    pub(crate) fn heap_bytes(&self) -> usize {
        self.live.capacity() * std::mem::size_of::<LiveExtent>()
            + (self.seg_live.len() + self.deps.len()) * 48
            + self.live_packs.len() * 32
            + self.segments.capacity() * std::mem::size_of::<SegmentInfo>()
            + self.packs.capacity() * std::mem::size_of::<PackInfo>()
            + self.runs.capacity() * std::mem::size_of::<RunRef>()
            + self.commits.capacity() * std::mem::size_of::<CommitInfo>()
    }
}

/// Reads the root set: the retained FULLs, then the pinned commits.
///
/// # Errors
/// `Invalid` when the view retains no FULL; a pin whose commit cannot be read
/// or is not a FULL fails the mark (GC never runs past a pin it cannot
/// resolve).
pub fn root_set(view: &GcView<'_>) -> Result<(Vec<Root>, BTreeSet<CommitSeq>)> {
    let mut roots: Vec<Root> = view
        .retained
        .iter()
        .filter_map(|c| match c {
            Commit::Full(f) => Some(Root {
                seq: f.common.commit_seq,
                root: f.map_root,
            }),
            _ => None,
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
        batches: Vec<Vec<NodeHash>>,
        next: usize,
        root_hashes: Vec<NodeHash>,
    },
    Finish,
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
}

impl Mark {
    pub(crate) fn new(cfg: GcConfig) -> Self {
        Self {
            footers: FooterCache::new(cfg.footer_cache),
            cfg,
            stage: Stage::Init,
            root_idx: 0,
            seen: HashSet::new(),
            skip: HashSet::new(),
            refs: Vec::new(),
            existing: BTreeSet::new(),
            out: Liveness::default(),
        }
    }

    /// Bytes of the live-extent set being built.
    pub(crate) fn live_set_bytes(&self) -> usize {
        self.out.live.capacity() * std::mem::size_of::<LiveExtent>()
    }

    /// The result, once [`is_done`](Self::is_done).
    pub(crate) fn into_liveness(self) -> Liveness {
        self.out
    }

    pub(crate) fn heap_bytes(&self) -> usize {
        let batch = match &self.stage {
            Stage::Leaves {
                batches,
                root_hashes,
                ..
            } => (batches.iter().map(Vec::capacity).sum::<usize>() + root_hashes.capacity()) * 32,
            _ => 0,
        };
        (self.seen.capacity() + self.skip.capacity()) * 40
            + self.refs.capacity() * std::mem::size_of::<(u64, DataEntry)>()
            + self.footers.heap_bytes()
            + self.existing.len() * 24
            + batch
            + self.out.heap_bytes()
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
                self.out.packs = view.packs.list_packs()?;
                self.out.runs = view.index.all_runs()?;
                self.out.commits = view.commits.list()?;
                self.existing = self.out.segments.iter().map(|s| s.seg).collect();
                self.stage = Stage::Nodes;
            }
            Stage::Nodes => {
                let Some(root) = self.out.roots.get(self.root_idx).copied() else {
                    self.stage = Stage::Finish;
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
                let per = new_leaves
                    .len()
                    .div_ceil(self.cfg.max_leaf_batches.max(1))
                    .max(self.cfg.min_leaf_batch.max(1));
                let batches: Vec<Vec<NodeHash>> =
                    new_leaves.chunks(per).map(<[NodeHash]>::to_vec).collect();
                self.skip = self.seen.clone();
                self.skip.extend(new_leaves.iter().copied());
                self.stage = Stage::Leaves {
                    batches,
                    next: 0,
                    root_hashes,
                };
            }
            Stage::Leaves {
                batches,
                next,
                root_hashes,
            } => {
                let root = self
                    .out
                    .roots
                    .get(self.root_idx)
                    .copied()
                    .ok_or(Error::invalid("mark root index out of range"))?;
                if let Some(batch) = batches.get(next) {
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
                    self.resolve(view)?;
                    self.stage = Stage::Leaves {
                        batches,
                        next: next + 1,
                        root_hashes,
                    };
                } else {
                    self.seen.extend(root_hashes);
                    self.skip = HashSet::new();
                    self.root_idx += 1;
                    self.stage = Stage::Nodes;
                }
            }
            Stage::Finish => {
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
        self.out.live.push(LiveExtent { hint, ci });
    }

    fn dep(&mut self, from: SegId, on: SegId) {
        if from != on && self.existing.contains(&from) {
            let d = self.out.deps.entry(from).or_insert(on);
            *d = (*d).max(on);
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
                    // No usable index entry: the hinted copy is the one.
                    _ => self.live(hint, ci),
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
            }
        }
        Ok(())
    }

    fn finish(&mut self) {
        let live = &mut self.out.live;
        live.sort_unstable_by_key(|l| (l.hint.seg, l.hint.off, l.hint.clen, l.ci));
        live.dedup_by(|b, a| a.hint == b.hint);
        live.shrink_to_fit();
        let mut seg_live = BTreeMap::new();
        for l in live.iter() {
            *seg_live.entry(l.hint.seg).or_insert(0u64) += u64::from(l.hint.clen);
        }
        self.out.seg_live = seg_live;
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
