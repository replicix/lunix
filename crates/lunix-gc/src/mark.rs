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
//! rule 5). When the hinted segment is already gone there is no `S` to hold
//! the dependency; the copy's segment goes to `orphan_deps`. A copy that a
//! durable FULL does not name yet (above `named_high`, or an unnamed output)
//! is *pending*: the mark then reads the footers of the dead segments and
//! records, per segment that holds the id of a pending copy, the copy's
//! segment (`orphan_holds`), and only those segments wait for it.
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
use lunix_format::{ChunkId, CommitSeq, Hint, IdLen, IdPrefix, PackId, SegId};
use lunix_iface::gc::GcView;
use lunix_iface::index::RunRef;
use lunix_iface::map::{entry_hint, NodeHash};
use lunix_iface::store::{CommitInfo, PackInfo, SegmentInfo};
use lunix_iface::{Error, ErrorKind, Result};

use crate::config::GcConfig;
use crate::footers::{CacheBytes, FooterCache};

#[cfg(test)]
thread_local! {
    /// Per test thread: the mark leaves the pin roots out of its root set
    /// (the negative control of the unfiltered proposal oracle).
    pub(crate) static DROP_PIN_ROOTS: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

/// Test-only: leaves the pin roots out of the root set while alive.
#[cfg(test)]
pub(crate) struct DropPinRoots;

#[cfg(test)]
impl DropPinRoots {
    pub(crate) fn on() -> Self {
        DROP_PIN_ROOTS.with(|c| c.set(true));
        Self
    }
}

#[cfg(test)]
impl Drop for DropPinRoots {
    fn drop(&mut self) {
        DROP_PIN_ROOTS.with(|c| c.set(false));
    }
}

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
    /// Segments holding the live copy of an entry whose hinted segment no
    /// longer exists (a re-hint lost at a restart, the source deleted). No
    /// segment's `deps` records them, so the delete rule holds every segment
    /// delete while one of them is not yet durable.
    pub orphan_deps: BTreeSet<SegId>,
    /// For a segment without a live extent whose footer holds the chunk id of
    /// a *pending* orphan copy (the copy's segment is above the `named_high`
    /// the mark was given, or an unnamed output): the segments holding those
    /// copies. The delete rule of `deps`, for the one segment that can still
    /// be the only durable holder of the id.
    pub orphan_holds: BTreeMap<SegId, BTreeSet<SegId>>,
    /// The mark ran the `Hold` stage (some orphan copy was pending), the
    /// footers it read, and the footers that could not be read (those
    /// segments are held for every pending copy).
    pub hold_ran: bool,
    /// See `hold_ran`.
    pub hold_footers: u64,
    /// See `hold_ran`.
    pub hold_failed: u64,
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
            + self.orphan_deps.len()
            + self.orphan_holds.len()
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
        // Test control: the commit stays, its map is not a root.
        #[cfg(test)]
        if DROP_PIN_ROOTS.with(std::cell::Cell::get) {
            continue;
        }
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
    /// Footers of the dead segments, read for the pending orphan ids.
    Hold {
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
    /// The highest id a durable FULL is known to name when the mark began
    /// (`u64::MAX`: every copy counts as durable).
    named_high: u64,
    /// The unnamed compaction outputs when the mark began.
    outputs: BTreeSet<SegId>,
    /// Chunk ids whose index copy is a pending orphan copy, with the copy's
    /// segment.
    orphan_ids: BTreeMap<ChunkId, SegId>,
    /// Copies of ids dropped because `orphan_ids` is full: every dead segment
    /// is held for them (the old global hold, bounded memory).
    orphan_overflow: BTreeSet<SegId>,
    /// `MAX_ORPHAN_IDS` (a field so a test can lower it).
    max_orphan_ids: usize,
}

/// Pending orphan ids a mark keeps (about 100 bytes each). Beyond it the
/// copies' segments are held for every dead segment instead.
const MAX_ORPHAN_IDS: usize = 1 << 16;

/// Footers read per unit of the `Hold` stage.
const HOLD_FOOTERS_PER_UNIT: usize = 32;

impl Mark {
    /// A mark. `named_high` and `outputs` are the collector's durability
    /// facts: an orphan copy in a segment above `named_high` or in `outputs`
    /// is pending and makes the segments holding its id wait. Pass
    /// `u64::MAX` and an empty set when no delete is judged.
    pub(crate) fn new(cfg: GcConfig, named_high: u64, outputs: BTreeSet<SegId>) -> Self {
        Self {
            named_high,
            outputs,
            orphan_ids: BTreeMap::new(),
            orphan_overflow: BTreeSet::new(),
            max_orphan_ids: MAX_ORPHAN_IDS,
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
            Stage::Finish { segs, .. } | Stage::Hold { segs, .. } => segs.capacity() * 8,
            _ => 0,
        };
        (self.seen.capacity() + self.skip.capacity()) * 40
            + self.refs.capacity() * std::mem::size_of::<(u64, DataEntry)>()
            + self.footers.heap_bytes()
            + self.existing.len() * 24
            + (self.orphan_ids.len() + self.orphan_overflow.len()) * 100
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
                if self.orphan_ids.is_empty() && self.orphan_overflow.is_empty() {
                    self.finish();
                    self.stage = Stage::Done;
                    return Ok(true);
                }
                // The segments the judge may delete (`live_bytes == 0`); an
                // unnamed output is kept by its own rule and needs no hold.
                let out = &self.out;
                let outputs = &self.outputs;
                let segs: Vec<SegId> = out
                    .segments
                    .iter()
                    .map(|s| s.seg)
                    .filter(|&s| out.live_bytes(s) == 0 && !outputs.contains(&s))
                    .collect();
                self.out.hold_ran = true;
                self.stage = Stage::Hold { segs, next: 0 };
            }
            Stage::Hold { segs, mut next } => {
                let end = segs.len().min(next + HOLD_FOOTERS_PER_UNIT);
                while next < end {
                    let seg = segs[next];
                    next += 1;
                    self.hold_segment(view, seg);
                }
                if next < segs.len() {
                    self.stage = Stage::Hold { segs, next };
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

    fn dep(&mut self, from: SegId, on: SegId, id: ChunkId) {
        if from == on {
            return;
        }
        if self.existing.contains(&from) {
            self.out.deps.entry(from).or_default().insert(on);
        } else {
            // The hinted segment is gone: nothing can carry the dependency.
            self.orphan_dep(on, id);
        }
    }

    /// The live copy of `id` is in `on`, and no segment can carry the
    /// dependency (the hinted one is gone, or does not hold the id).
    fn orphan_dep(&mut self, on: SegId, id: ChunkId) {
        self.out.orphan_deps.insert(on);
        if on.raw() > self.named_high || self.outputs.contains(&on) {
            if self.orphan_ids.len() >= self.max_orphan_ids && !self.orphan_ids.contains_key(&id) {
                self.orphan_overflow.insert(on);
            } else {
                self.orphan_ids.insert(id, on);
            }
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
                        self.dep(hint.seg, c.hint.seg, id);
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
                // The hinted segment does not hold the id at the hint (it is
                // gone, or its footer lacks it): it cannot carry a
                // dependency, whether it exists or not.
                if c.hint.seg != hint.seg {
                    self.orphan_dep(c.hint.seg, c.id);
                }
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

    /// Records the pending orphan copies whose ids `seg` holds. A footer that
    /// cannot be read holds the segment for every pending copy: the mark does
    /// not fail for a dead segment it could not prove clear.
    fn hold_segment(&mut self, view: &GcView<'_>, seg: SegId) {
        self.out.hold_footers += 1;
        let mut held: BTreeSet<SegId> = self.orphan_overflow.clone();
        match self.footers.get(view.segments, seg) {
            Ok(Some(f)) => held.extend(f.ids().filter_map(|id| self.orphan_ids.get(id).copied())),
            Ok(None) => return,
            Err(e) => {
                self.out.hold_failed += 1;
                tracing::warn!(
                    seg = seg.raw(),
                    error = %e,
                    "gc mark: a dead segment's footer cannot be read; it is held for every pending orphan copy"
                );
                held.extend(self.orphan_ids.values().copied());
            }
        }
        if !held.is_empty() {
            self.out.orphan_holds.insert(seg, held);
        }
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
    liveness_with(view, cfg, u64::MAX, BTreeSet::new())
}

/// [`liveness`] with the collector's durability facts (see [`Mark::new`]).
pub(crate) fn liveness_with(
    view: &GcView<'_>,
    cfg: GcConfig,
    named_high: u64,
    outputs: BTreeSet<SegId>,
) -> Result<Liveness> {
    let mut m = Mark::new(cfg, named_high, outputs);
    m.run(view)?;
    Ok(m.into_liveness())
}

#[cfg(test)]
mod tests {
    use lunix_format::ExtentKind;
    use lunix_iface::map::ChunkNo;

    use super::*;
    use crate::tests::sim::{data_of, Sim, SimCfg};

    /// N9: a stale hint whose segment exists but does not hold the id (a
    /// damaged or foreign hint) cannot carry a dependency: the copy goes to
    /// `orphan_deps` and no `deps` entry is made for the wrong segment.
    #[test]
    fn a_hinted_segment_that_exists_but_does_not_hold_the_id_records_an_orphan_dep() {
        const CS: u32 = 16 << 10;
        let mut cfg = SimCfg::new(CS, IdLen::Sixteen);
        cfg.segment_target = u64::from(CS) * 8;
        let mut s = Sim::new(cfg);
        for i in 0..16 {
            s.write(i, 200 + i);
        }
        s.commit().expect("commit");
        let hint_of = |ci: u64| {
            let e = s.map.get(ChunkNo(ci)).expect("get").expect("populated");
            lunix_iface::map::entry_hint(&e)
        };
        let (y, z) = (hint_of(1).seg, hint_of(12).seg);
        assert_ne!(y, z, "two segments");
        let id = s.codec.chunk_id(ExtentKind::Data, &data_of(201, CS));
        let prefix = IdPrefix::of(&id, IdLen::Sixteen);

        let mut m = Mark::new(s.cfg.gc, 0, BTreeSet::new());
        m.existing = [y, z].into_iter().collect();
        // The entry hints `z` at an offset where `z` holds nothing of the id.
        let bogus = Hint {
            seg: z,
            off: 1,
            clen: 1,
        };
        s.with_view(|v| m.stale(v, &prefix, bogus, 0))
            .expect("stale");
        assert!(m.out.deps.is_empty(), "z cannot carry the dependency");
        assert!(m.out.orphan_deps.contains(&y), "{:?}", m.out.orphan_deps);
        assert_eq!(m.orphan_ids.get(&id), Some(&y), "y is above named_high 0");
    }

    // ----------------------------------------------------- the Hold stage

    use lunix_format::segment::FooterBody;
    use lunix_format::{Codec, Level};
    use lunix_iface::crypto::EncodingDesc;
    use lunix_iface::store::{SegmentInfo, SegmentStore, SegmentWriter};

    const CS: u32 = 16 << 10;

    /// A segment store whose `footer` fails for one segment.
    struct Failing<'a> {
        inner: &'a dyn SegmentStore,
        bad: SegId,
    }

    impl SegmentStore for Failing<'_> {
        fn open_segment(&self) -> Result<Box<dyn SegmentWriter>> {
            self.inner.open_segment()
        }
        fn read_extent(&self, hint: &Hint, out: &mut Vec<u8>) -> Result<()> {
            self.inner.read_extent(hint, out)
        }
        fn locate(&self, hint: &Hint) -> Result<lunix_iface::store::ExtentLocation> {
            self.inner.locate(hint)
        }
        fn footer(&self, seg: SegId) -> Result<FooterBody> {
            if seg == self.bad {
                return Err(Error::invalid("torn footer"));
            }
            self.inner.footer(seg)
        }
        fn list(&self) -> Result<Vec<SegmentInfo>> {
            self.inner.list()
        }
        fn size(&self, seg: SegId) -> Result<Option<u64>> {
            self.inner.size(seg)
        }
        fn delete(&self, seg: SegId) -> Result<u64> {
            self.inner.delete(seg)
        }
    }

    /// `n` chunks, one commit each: one segment per chunk (seed `100 + i`).
    fn volume(n: u64) -> Sim {
        let mut cfg = SimCfg::new(CS, IdLen::Sixteen);
        cfg.chunk_count = n.max(8);
        cfg.segment_target = u64::from(CS) * 2;
        let mut s = Sim::new(cfg);
        for i in 0..n {
            s.write(i, 100 + i);
            s.commit().expect("commit");
        }
        s
    }

    fn seg_of(s: &Sim, ci: u64) -> SegId {
        let e = s.map.get(ChunkNo(ci)).expect("get").expect("populated");
        lunix_iface::map::entry_hint(&e).seg
    }

    fn id_of(s: &Sim, ci: u64) -> ChunkId {
        s.codec.chunk_id(ExtentKind::Data, &data_of(100 + ci, CS))
    }

    /// A mark at the start of its `Hold` stage: every listed segment is dead
    /// for it, `pending` are its pending orphan ids.
    fn at_hold(s: &Sim, pending: &[(ChunkId, SegId)]) -> Mark {
        let mut m = Mark::new(s.cfg.gc, 0, BTreeSet::new());
        let mut segs = SegmentStore::list(&s.store).expect("list");
        segs.sort_unstable_by_key(|x| x.seg);
        m.out.segments = segs;
        m.orphan_ids = pending.iter().copied().collect();
        m.stage = Stage::Finish {
            segs: Vec::new(),
            next: 0,
        };
        m
    }

    /// Runs the mark to the end; returns the number of `unit` calls.
    fn run_counting(s: &Sim, m: &mut Mark, store: Option<&dyn SegmentStore>) -> usize {
        let mut units = 0;
        s.with_view(|v| {
            let view = GcView {
                segments: store.unwrap_or(v.segments),
                ..*v
            };
            loop {
                units += 1;
                if m.unit(&view).expect("unit") {
                    break;
                }
            }
        });
        units
    }

    /// An unreadable footer holds the segment for every pending copy (and is
    /// counted), the readable ones only for the ids they hold.
    #[test]
    fn an_unreadable_footer_holds_the_segment_for_every_pending_copy() {
        let s = volume(6);
        let (c1, c2) = (seg_of(&s, 5), seg_of(&s, 4));
        let mut m = at_hold(&s, &[(id_of(&s, 0), c1), (id_of(&s, 1), c2)]);
        let bad = seg_of(&s, 3);
        let failing = Failing {
            inner: &s.store,
            bad,
        };
        run_counting(&s, &mut m, Some(&failing));
        let h = &m.out.orphan_holds;
        assert_eq!(
            h[&bad],
            [c1, c2].into_iter().collect(),
            "all pending copies"
        );
        assert_eq!(h[&seg_of(&s, 0)], [c1].into_iter().collect());
        assert_eq!(h[&seg_of(&s, 1)], [c2].into_iter().collect());
        assert!(
            !h.contains_key(&seg_of(&s, 2)),
            "a readable, unrelated segment"
        );
        assert_eq!((m.out.hold_footers, m.out.hold_failed), (6, 1));
        assert!(m.out.hold_ran);
    }

    /// A second dead segment that holds the same pending id (an older
    /// duplicate) is held as well.
    #[test]
    fn two_dead_holders_of_one_pending_id_are_both_held() {
        let s = volume(4);
        let data = data_of(100, CS);
        let id = id_of(&s, 0);
        let mut ext = Vec::new();
        s.codec
            .seal(
                ExtentKind::Data,
                &id,
                &EncodingDesc {
                    codec: Codec::Raw,
                    level: Level(0),
                    ulen: CS,
                    dict_id: None,
                },
                &data,
                &mut ext,
            )
            .expect("seal");
        let mut w = s.store.open_segment().expect("open");
        w.append_extent(&id, None, &ext).expect("append");
        let dup = w.seal(None).expect("seal segment").seg;
        let first = seg_of(&s, 0);
        assert_ne!(first, dup);
        let copy = seg_of(&s, 3);
        let mut m = at_hold(&s, &[(id, copy)]);
        run_counting(&s, &mut m, None);
        let want: BTreeSet<SegId> = [copy].into_iter().collect();
        assert_eq!(m.out.orphan_holds.get(&first), Some(&want));
        assert_eq!(m.out.orphan_holds.get(&dup), Some(&want));
        assert_eq!(m.out.orphan_holds.len(), 2);
    }

    /// More dead segments than one `Hold` unit reads: the stage spans units
    /// and a holder in the last one is still found.
    #[test]
    fn the_hold_stage_spans_units() {
        let n = 3 * HOLD_FOOTERS_PER_UNIT as u64 + 5;
        let s = volume(n);
        let copy = seg_of(&s, 1);
        let last = n - 1;
        let mut m = at_hold(
            &s,
            &[
                (id_of(&s, 0), copy),
                (id_of(&s, last), copy),
                (id_of(&s, 40), copy),
            ],
        );
        let units = run_counting(&s, &mut m, None);
        // One Finish unit, then ceil(n / 32) Hold units.
        let hold_units = (n as usize).div_ceil(HOLD_FOOTERS_PER_UNIT);
        assert!(hold_units >= 3);
        assert_eq!(units, 1 + hold_units);
        let h = &m.out.orphan_holds;
        for ci in [0, 40, last] {
            assert!(h.contains_key(&seg_of(&s, ci)), "chunk {ci}");
        }
        assert_eq!(h.len(), 3);
        assert_eq!(m.out.hold_footers, n);
    }

    /// Review N11: past the bound the ids are not kept (memory), their copies
    /// hold every dead segment instead.
    #[test]
    fn past_the_bound_the_copies_hold_every_dead_segment() {
        let s = volume(5);
        let mut m = at_hold(&s, &[]);
        m.max_orphan_ids = 1;
        let (c1, c2) = (seg_of(&s, 4), seg_of(&s, 3));
        m.orphan_dep(c1, id_of(&s, 0));
        m.orphan_dep(c2, id_of(&s, 1));
        assert_eq!(m.orphan_ids.len(), 1);
        assert_eq!(m.orphan_overflow, [c2].into_iter().collect());
        assert!(m.heap_bytes() >= 2 * 100);
        run_counting(&s, &mut m, None);
        let h = &m.out.orphan_holds;
        assert_eq!(h[&seg_of(&s, 0)], [c1, c2].into_iter().collect());
        assert_eq!(h[&seg_of(&s, 2)], [c2].into_iter().collect());
    }
}
