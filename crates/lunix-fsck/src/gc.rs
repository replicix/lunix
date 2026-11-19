//! The GC-specific checks of `lunix-fsck` (plan M17.B "fsck integration";
//! `docs/reference/gc.md`).
//!
//! * **The condemned list** (`state/gc/condemned`, body version 1) is parsed.
//!   A listed object that still exists is the note `gc_pending` (a pass will
//!   delete it after its grace and a recheck, or un-condemn it when a root
//!   still references it); a listed object that is gone is fine and not
//!   reported. A body that does not decode is `condemned_invalid` (a warning;
//!   the list is a hint and GC re-marks).
//! * **Missing files.** A file that no retained root resolves to may be
//!   missing; one a retained root needs is already an error of the map walk
//!   and the resolution (`map_node_missing`, `map_entry_unresolved`), after
//!   hint and index fallback. Nothing here repeats them.
//! * **Liveness.** `lunix-gc`'s mark ([`lunix_gc::liveness`]) runs over the
//!   locked volume (the index is fsck's scratch copy) and fsck recounts the
//!   live bytes of every segment: a mismatch in any segment is the error
//!   `gc_liveness`, as is a pack fsck's walk from disk reaches that the mark
//!   does not count live. The recount is a second writing of the mark's resolution
//!   rule over fsck's own footer parse and a full walk of every root (the
//!   mark walks with a skip set, 32 leaves per unit); it shares with the mark
//!   the root set (the mark's own), the index and the map walker, so a wrong
//!   root set, a wrong index or a wrong `walk_leaves` is not caught here.
//!   A mark, recount or dry run that fails is the warning `gc_unchecked`.
//! * **The dry run.** `Collector::dry_run` of a fresh collector reports what
//!   a pass would condemn, delete and compact now (`gc_dry_run`, a note).
//! * **`--repair --gc`** finishes the work offline through the same
//!   collector, over the real index: mark, condemn, the grace floor (10 s
//!   plus twice `read_timeout`), recheck, delete, until a pass frees nothing.
//!   The grace runs on a manual clock that the repair advances: fsck holds
//!   the volume lock, so no cut, destage or read is in flight that the grace
//!   could protect. Compaction is off (its moves need a cut to name the
//!   output). The repair is refused when the check pass before it found any
//!   error (as `--delete-orphans` is: `rollback`, `fork` and every other
//!   error), unless the chain verified, the tip walk is complete, the index
//!   opens and every entry of every root resolves, and when fsck's recount
//!   and the mark over the real index disagree. Its result is `gc_repaired`.
//!
//! `GcContext` gives read access to everything the other checks learned and
//! to the locked volume.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::Arc;
use std::time::Duration;

use lunix_format::commit::Commit;
use lunix_format::gcfile::Condemned;
use lunix_format::local::SpaceSettings;
use lunix_format::node::DataEntry;
use lunix_format::paths::local as lpath;
use lunix_format::{ChunkId, Hint, IdLen, ObjId};
use lunix_gc::mark::Root;
use lunix_gc::{grace_floor, CondemnedList, FloorSource, Gc, GcConfig, Liveness, ObjectRef};
use lunix_iface::clock::{Clock, ManualClock, SystemClock};
use lunix_iface::gc::{Collector, GcBudget, GcParams, GcPassSummary, GcState, GcView, TipInfo};
use lunix_iface::index::ChunkIndex;
use lunix_iface::map::{entry_hint, NodeSource};
use lunix_iface::space::{
    Ladder, LadderStep, LadderStepKind, ReserveOutcome, SpaceEvents, SpaceGate, SpaceKind,
    SpaceSnapshot,
};
use lunix_iface::store::{CommitStore, VolumeDir};
use lunix_iface::wake::WakeFd;
use lunix_iface::{Error, ErrorKind, Result};
use lunix_index::LsmIndex;
use lunix_map::{MapConfig, RadixMap};

use crate::code;
use crate::facts::Facts;
use crate::indexcheck;
use crate::report::{Finding, Sink};
use crate::session::Session;

/// Input of [`check_gc`].
pub struct GcContext<'a> {
    pub(crate) session: &'a Session,
    pub(crate) facts: &'a Facts,
    /// `--repair --gc` was given: finish a pending pass instead of only
    /// reporting it.
    pub(crate) repair: bool,
}

/// What the GC hook found.
pub struct GcCheck {
    /// The findings (notes such as `gc_pending` included).
    pub findings: Vec<Finding>,
    /// What a pass would do now, when the dry run ran (the report's
    /// `gc_dry_run`; all zero on a volume without GC work).
    pub dry_run: Option<GcPassSummary>,
}

/// The GC-specific checks, or with `repair` the offline pass.
pub fn check_gc(ctx: &GcContext<'_>) -> GcCheck {
    let mut out = Vec::new();
    let mut dry_run = None;
    if ctx.repair {
        repair(ctx, &mut out);
        return GcCheck {
            findings: out,
            dry_run,
        };
    }
    let list = decode_list(ctx.facts, &mut out);
    let marked = if ready(ctx.facts) {
        compare_and_dry_run(ctx, &mut out, &mut dry_run)
    } else {
        None
    };
    if let Some(list) = &list {
        pending(ctx, list, marked.as_ref(), &mut out);
    }
    GcCheck {
        findings: out,
        dry_run,
    }
}

/// The ids of segments, packs and index runs the condemned list names, so
/// that the orphan check does not report them. A body that does not decode
/// names nothing.
pub fn condemned_ids(list: &Condemned) -> HashSet<ObjId> {
    CondemnedList::from_file(list)
        .map(|l| {
            l.entries
                .keys()
                .filter_map(|o| match *o {
                    ObjectRef::Segment(id) | ObjectRef::Pack(id) | ObjectRef::Run(id) => Some(id),
                    ObjectRef::Commit(_) => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

const LIST: &str = "state/gc/condemned";

fn decode_list(facts: &Facts, out: &mut Vec<Finding>) -> Option<CondemnedList> {
    let file = facts.condemned.as_ref()?;
    let list = CondemnedList::from_file(file);
    if list.is_none() {
        out.push(Finding::new(
            code::CONDEMNED_INVALID,
            LIST,
            format!(
                "the body (version {}) does not decode: ignored, GC re-marks",
                file.body_ver
            ),
        ));
    }
    list
}

/// What a listed object is now.
enum Listed {
    Gone,
    Pending,
    Referenced,
}

/// What the retained commits and roots keep, as far as this run knows.
struct Kept {
    /// Objects a retained commit names.
    named: HashSet<ObjId>,
    /// Commits on disk.
    commits: HashSet<u64>,
}

impl Kept {
    fn new(ctx: &GcContext<'_>) -> Self {
        let mut named = HashSet::new();
        for r in ctx.facts.retained.iter().chain(&ctx.facts.gc_extra) {
            if let Commit::Full(f) = &r.commit {
                named.extend(f.new_segments.iter().copied());
                named.extend(f.map_packs.iter().copied());
                named.extend(f.index_packs.iter().copied());
                named.extend(f.map_root.pack);
            }
        }
        let commits = CommitStore::list(&ctx.session.vol)
            .map(|l| l.iter().map(|c| c.seq.0).collect())
            .unwrap_or_default();
        Self { named, commits }
    }
}

fn listed_state(ctx: &GcContext<'_>, obj: ObjectRef, kept: &Kept, l: Option<&Liveness>) -> Listed {
    let f = ctx.facts;
    let live = |yes: bool| {
        if yes {
            Listed::Referenced
        } else {
            Listed::Pending
        }
    };
    match obj {
        ObjectRef::Segment(id) if f.segments.contains_key(&id) => live(
            kept.named.contains(&id)
                || f.referenced_segments.contains(&id)
                || l.is_some_and(|l| l.live_bytes(id) > 0),
        ),
        ObjectRef::Pack(id) if f.packs.contains_key(&id) => {
            live(kept.named.contains(&id) || l.is_some_and(|l| l.live_packs.contains(&id)))
        }
        ObjectRef::Run(id) if ctx.session.dir.join(lpath::index_run(id)).exists() => {
            live(kept.named.contains(&id))
        }
        ObjectRef::Commit(seq) if kept.commits.contains(&seq.0) => {
            // GC keeps every commit from the lowest chain_floor of its
            // retained set (and the rollback floor window) up.
            live(seq >= f.gc_floor || f.retained.iter().any(|r| r.seq == seq))
        }
        _ => Listed::Gone,
    }
}

fn object_name(obj: ObjectRef) -> String {
    match obj {
        ObjectRef::Segment(id) => lpath::segment_sealed(id),
        ObjectRef::Pack(id) => lpath::map_pack_sealed(id),
        ObjectRef::Run(id) => lpath::index_run(id),
        ObjectRef::Commit(seq) => lpath::commit(seq),
    }
}

fn pending(
    ctx: &GcContext<'_>,
    list: &CondemnedList,
    marked: Option<&Liveness>,
    out: &mut Vec<Finding>,
) {
    let kept = Kept::new(ctx);
    for (&obj, e) in &list.entries {
        let why = match listed_state(ctx, obj, &kept, marked) {
            Listed::Gone => continue,
            Listed::Pending => format!(
                "condemned ({} bytes): a pass deletes it after its grace and a recheck",
                e.bytes
            ),
            Listed::Referenced => format!(
                "condemned ({} bytes) but a retained commit names or references it: the recheck un-condemns it",
                e.bytes
            ),
        };
        out.push(Finding::new(code::GC_PENDING, object_name(obj), why));
    }
}

/// The checks GC's figures depend on passed far enough to compare them.
fn ready(f: &Facts) -> bool {
    f.tip.is_some() && f.chain_ok && f.index_ok && f.ratios.is_some()
}

fn params(ctx: &GcContext<'_>) -> GcParams {
    let json = ctx.facts.json.clone().unwrap_or_default();
    GcParams {
        retain_last: json.retain_last,
        grace: Duration::from_millis(json.gc_grace_ms),
        read_timeout: Duration::from_millis(json.read_timeout_ms),
        rate_bytes_per_s: json.gc_rate,
        live_threshold: json.gc_live_threshold,
        chunk_size: ctx.session.desc.chunk_size,
        id_len: ctx.session.desc.id_len,
    }
}

/// The ledger of an offline run: there is none (startup accounting recounts
/// the files at the next mount), so every charge succeeds and every release
/// is a no-op.
struct OfflineSpace;

impl Ladder for OfflineSpace {
    fn register(&self, _kind: LadderStepKind, _step: Box<dyn LadderStep>) {}
}

impl SpaceEvents for OfflineSpace {
    fn fatal_io(&self, _what: &str) {}

    fn is_read_only(&self) -> bool {
        false
    }
}

impl SpaceGate for OfflineSpace {
    fn reserve(
        &self,
        _kind: SpaceKind,
        _bytes: u64,
        _wake: Option<&WakeFd>,
    ) -> Result<ReserveOutcome> {
        Ok(ReserveOutcome::Reserved)
    }

    fn reserve_since(
        &self,
        _kind: SpaceKind,
        _bytes: u64,
        _wake: Option<&WakeFd>,
        _arrived: std::time::Duration,
        _class: lunix_iface::space::ReserveClass,
    ) -> Result<ReserveOutcome> {
        Ok(ReserveOutcome::Reserved)
    }

    fn reserve_headroom(&self, _kind: SpaceKind, _bytes: u64) -> Result<()> {
        Ok(())
    }

    fn release(&self, _kind: SpaceKind, _bytes: u64) {}

    fn transfer(&self, _from: SpaceKind, _to: SpaceKind, _out: u64, _in: u64) -> Result<()> {
        Ok(())
    }

    fn tick(&self) -> usize {
        0
    }

    fn snapshot(&self) -> SpaceSnapshot {
        SpaceSnapshot::default()
    }

    fn reconfigure(&self, _settings: &SpaceSettings) -> Result<()> {
        Ok(())
    }
}

/// Everything a [`GcView`] borrows besides the session, owned for one call.
struct Parts {
    retained: Vec<Commit>,
    tip: TipInfo,
    map: RadixMap,
    clock: ManualClock,
    params: GcParams,
}

impl Parts {
    fn new(ctx: &GcContext<'_>) -> Result<Self> {
        let s = ctx.session;
        let f = ctx.facts;
        let tip = f.tip.as_ref().ok_or(Error::invalid("no tip"))?;
        // The tip's map, read-only, over the volume's packs (any root can be
        // walked through it).
        let source: Arc<dyn NodeSource> = Arc::new(s.vol.clone());
        let map = RadixMap::open(
            s.crypto.extents().clone(),
            source,
            &tip.full.map_root,
            tip.full.size / u64::from(s.desc.chunk_size),
            MapConfig::new(s.desc.map_fanout, s.desc.id_len).with_cache_max(32 << 20),
        )?;
        Ok(Self {
            retained: f.retained.iter().map(|r| r.commit.clone()).collect(),
            tip: TipInfo {
                commit_seq: tip.seq,
                map_root: tip.full.map_root,
                chain_floor: tip.full.chain_floor,
                high_water_mark: f.high_water,
            },
            map,
            clock: ManualClock::new(SystemClock.now()),
            params: params(ctx),
        })
    }

    fn view<'a>(&'a self, s: &'a Session, index: &'a dyn ChunkIndex) -> GcView<'a> {
        GcView {
            retained: &self.retained,
            tip: self.tip,
            pins: &s.vol,
            map: &self.map,
            index,
            segments: &s.vol,
            packs: &s.vol,
            commits: &s.vol,
            commit_codec: s.commit_codec(),
            extents: s.extents(),
            state: &s.vol,
            space: &OfflineSpace,
            clock: &self.clock,
            params: self.params,
        }
    }
}

fn floor_source(s: &Session) -> Arc<dyn FloorSource> {
    Arc::new(s.vol.clone())
}

/// Returns `lunix-gc`'s mark when it ran.
fn compare_and_dry_run(
    ctx: &GcContext<'_>,
    out: &mut Vec<Finding>,
    dry: &mut Option<GcPassSummary>,
) -> Option<Liveness> {
    let s = ctx.session;
    // The scratch copy: a dry run and a mark change nothing, and fsck never
    // writes to the volume's index without a repair flag. Its damage was
    // reported by the index check already.
    let mut reported = Sink::default();
    let iv = indexcheck::open_view(s, &mut reported)?;
    let parts = match Parts::new(ctx) {
        Ok(p) => p,
        Err(e) => {
            out.push(Finding::new(
                code::GC_UNCHECKED,
                "map/",
                format!("no GC figures: the tip's map does not open: {e}"),
            ));
            return None;
        }
    };
    let view = parts.view(s, &iv.idx);
    let gc_live = match lunix_gc::liveness(&view, GcConfig::default()) {
        Ok(l) => l,
        Err(e) => {
            out.push(Finding::new(
                code::GC_UNCHECKED,
                "gc",
                format!("no GC figures: the mark failed: {e}"),
            ));
            return None;
        }
    };
    match recount(&view, ctx.facts, &gc_live.roots) {
        Ok(mine) => {
            compare(&mine, &gc_live, out);
            compare_packs(ctx.facts, &gc_live, out);
        }
        Err(e) => out.push(Finding::new(
            code::GC_UNCHECKED,
            "gc",
            format!("no liveness recount: {e}"),
        )),
    }
    let mut gc = Gc::new(floor_source(s), GcConfig::default());
    match gc.dry_run(&view) {
        Ok(sum) => {
            if let Some(text) = summary_text(&sum) {
                out.push(Finding::new(code::GC_DRY_RUN, "gc", text));
            }
            *dry = Some(sum);
        }
        Err(e) => out.push(Finding::new(
            code::GC_UNCHECKED,
            "gc",
            format!("no dry run: {e}"),
        )),
    }
    Some(gc_live)
}

fn summary_text(s: &GcPassSummary) -> Option<String> {
    let deleted = s.segments_deleted + s.packs_deleted + s.runs_deleted + s.commits_deleted;
    if deleted == 0 && s.segments_condemned == 0 && s.segments_compacted == 0 {
        return None;
    }
    Some(format!(
        "a pass would condemn {} segments and delete {} segments, {} packs, {} index runs and {} commits ({} bytes), and compact {} segments ({} live bytes)",
        s.segments_condemned,
        s.segments_deleted,
        s.packs_deleted,
        s.runs_deleted,
        s.commits_deleted,
        s.bytes_freed,
        s.segments_compacted,
        s.bytes_moved
    ))
}

/// The chunk id fsck's own footer parse holds at exactly `h`.
fn footer_id(facts: &Facts, h: &Hint) -> Option<ChunkId> {
    let c = facts.segments.get(&h.seg)?;
    if !c.good {
        return None;
    }
    let x = c.find(h.off)?;
    (x.clen == h.clen).then_some(x.id)
}

/// fsck's recount of the live bytes per segment (gc.md "The live copy of a
/// chunk"), from the roots GC pinned, fsck's decoded footers and the view's
/// index.
fn recount(view: &GcView<'_>, facts: &Facts, roots: &[Root]) -> Result<BTreeMap<ObjId, u64>> {
    let lean = view.params.id_len == IdLen::Zero;
    let mut live: BTreeMap<ObjId, Vec<(u32, u32)>> = BTreeMap::new();
    // Resolved in the walk's callback (no per-root entry list); the first
    // error stops the counting and is returned after the walk.
    let mut failed: Option<Error> = None;
    for root in roots {
        view.map.walk_leaves(&root.root, &mut |_, e| {
            if failed.is_some() {
                return;
            }
            if let Err(err) = resolve_one(view, facts, lean, e, &mut live) {
                failed = Some(err);
            }
        })?;
        if let Some(err) = failed.take() {
            return Err(err);
        }
        // Keep the per-segment vectors small between roots.
        for v in live.values_mut() {
            v.sort_unstable();
            v.dedup();
        }
    }
    Ok(live
        .into_iter()
        .map(|(seg, v)| (seg, v.iter().map(|&(_, clen)| u64::from(clen)).sum()))
        .collect())
}

/// One entry of [`recount`]: the extents it keeps live.
fn resolve_one(
    view: &GcView<'_>,
    facts: &Facts,
    lean: bool,
    e: &DataEntry,
    live: &mut BTreeMap<ObjId, Vec<(u32, u32)>>,
) -> Result<()> {
    let mut add = |h: Hint| live.entry(h.seg).or_default().push((h.off, h.clen));
    let hint = entry_hint(e);
    if lean {
        if facts.segments.contains_key(&hint.seg) {
            add(hint);
        }
        return Ok(());
    }
    match footer_id(facts, &hint).filter(|id| e.prefix.matches(id)) {
        Some(id) => match view.index.get(&id)? {
            Some(c) if c.hint == hint => add(hint),
            Some(c) if footer_id(facts, &c.hint) == Some(id) => add(c.hint),
            _ => add(hint),
        },
        None => {
            let cands = match view.index.get_by_prefix(&e.prefix) {
                Ok(c) => c,
                Err(err) if err.kind() == ErrorKind::Unsupported => Vec::new(),
                Err(err) => return Err(err),
            };
            let mut found = false;
            for c in cands {
                if footer_id(facts, &c.hint) == Some(c.id) {
                    found = true;
                    add(c.hint);
                }
            }
            if !found && facts.segments.contains_key(&hint.seg) {
                add(hint);
            }
        }
    }
    Ok(())
}

fn compare(mine: &BTreeMap<ObjId, u64>, gc: &Liveness, out: &mut Vec<Finding>) {
    let segs: BTreeSet<ObjId> = mine.keys().chain(gc.seg_live.keys()).copied().collect();
    for seg in segs {
        let a = mine.get(&seg).copied().unwrap_or(0);
        let b = gc.live_bytes(seg);
        if a != b {
            out.push(Finding::new(
                code::GC_LIVENESS,
                lpath::segment_sealed(seg),
                format!("fsck counts {a} live bytes, lunix-gc's mark {b}"),
            ));
        }
    }
}

/// Every pack fsck's own walk (from disk) reached from the retained roots and
/// GC's retained set below `chain_floor` must be live in GC's mark (M17.B
/// stack review B-1: a mark that walked a resident copy of a node missed the
/// older root's pack). GC may keep more (pins); it may not keep less.
fn compare_packs(facts: &Facts, gc: &Liveness, out: &mut Vec<Finding>) {
    let Some(w) = &facts.walk else { return };
    let mut missed: Vec<ObjId> = w
        .used_packs
        .iter()
        .filter(|p| !gc.live_packs.contains(*p))
        .copied()
        .collect();
    missed.sort_unstable();
    for p in missed {
        out.push(Finding::new(
            code::GC_LIVENESS,
            lpath::map_pack_sealed(p),
            "a retained root reaches this pack (fsck's walk from disk), lunix-gc's mark does not: a pass would delete it",
        ));
    }
}

/// Passes an offline repair runs at most; the second normally frees nothing.
const MAX_PASSES: usize = 4;
/// Steps one pass may take before the repair gives up.
const MAX_STEPS: usize = 100_000;

fn refuse(out: &mut Vec<Finding>, why: impl Into<String>) {
    out.push(Finding::new(code::REPAIR_REFUSED, "--gc", why.into()));
}

fn repair(ctx: &GcContext<'_>, out: &mut Vec<Finding>) {
    let s = ctx.session;
    let f = ctx.facts;
    // Like --delete-orphans: nothing is deleted past an error (DESIGN 4.4
    // step 3: nothing changes before R4 passes; M17.B review A-1).
    if !f.error_codes.is_empty() {
        return refuse(
            out,
            format!(
                "the check found errors ({}): nothing is collected on such a volume",
                f.error_codes.join(", ")
            ),
        );
    }
    if f.tip.is_none() || !f.chain_ok {
        return refuse(out, "the retained chain does not verify");
    }
    if f.ratios.is_none() {
        return refuse(out, "the walk of the tip's map did not complete");
    }
    if f.pins_rejected {
        return refuse(
            out,
            "state/pins is rejected, so the rollback floor is unknown",
        );
    }
    let idx = match LsmIndex::open(&s.dir, s.vol.ids(), s.desc.id_len) {
        Ok(i) => i,
        Err(e) => return refuse(out, format!("the index does not open: {e}")),
    };
    let mut parts = match Parts::new(ctx) {
        Ok(p) => p,
        Err(e) => return refuse(out, format!("the tip's map does not open: {e}")),
    };
    // Offline: the grace floor on a manual clock, and no compaction (its
    // moves would need a cut to name the output segment).
    parts.params.grace = grace_floor(&parts.params);
    parts.params.live_threshold = f64::MIN_POSITIVE;
    let cfg = GcConfig {
        pass_interval: Duration::ZERO,
        compact_per_pass: 0,
        ..GcConfig::default()
    };
    let view = parts.view(s, &idx);
    let marked = match lunix_gc::liveness(&view, cfg) {
        Ok(l) if l.unresolved > 0 => {
            return refuse(
                out,
                format!(
                    "{} map entries resolve to no extent: repair the volume first",
                    l.unresolved
                ),
            );
        }
        Ok(l) => l,
        Err(e) => return refuse(out, format!("the mark failed: {e}")),
    };
    // The same comparison as the check, over the index the pass will use.
    let mut differ = Vec::new();
    match recount(&view, f, &marked.roots) {
        Ok(mine) => compare(&mine, &marked, &mut differ),
        Err(e) => return refuse(out, format!("the liveness recount failed: {e}")),
    }
    if let Some(d) = differ.first() {
        return refuse(
            out,
            format!(
                "fsck's liveness recount and lunix-gc's mark differ ({}: {})",
                d.object, d.message
            ),
        );
    }
    drop(marked);
    let mut gc = Gc::new(floor_source(s), cfg);
    match run_passes(&mut gc, &view, &parts.clock) {
        Ok(freed) => out.push(Finding::new(
            code::GC_REPAIRED,
            "gc",
            format!(
                "--repair --gc: {} offline passes freed {freed} bytes; the condemned list holds {} entries",
                gc.stats().passes,
                gc.condemned().len()
            ),
        )),
        Err(e) => refuse(out, format!("the offline pass stopped: {e}")),
    }
}

/// Runs passes until one frees nothing (at most [`MAX_PASSES`]); returns the
/// bytes freed.
fn run_passes(gc: &mut Gc, view: &GcView<'_>, clock: &ManualClock) -> Result<u64> {
    let budget = GcBudget {
        max_duration: Duration::from_secs(3600),
        max_bytes: 0,
    };
    let mut total = 0u64;
    for _ in 0..MAX_PASSES {
        let mut freed = 0u64;
        let mut finished = false;
        for _ in 0..MAX_STEPS {
            let p = gc.step(view, budget)?;
            if !p.moved.is_empty() {
                return Err(Error::invalid(
                    "the offline pass moved extents (compaction is off)",
                ));
            }
            freed = freed.saturating_add(p.freed);
            if p.done || p.state == GcState::Idle {
                finished = true;
                break;
            }
            if p.state == GcState::Grace {
                clock.advance(view.params.grace.saturating_add(Duration::from_secs(1)));
            }
        }
        if !finished {
            return Err(Error::invalid(
                "a pass did not finish within its step limit",
            ));
        }
        total = total.saturating_add(freed);
        if freed == 0 {
            break;
        }
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dry_run_that_finds_nothing_says_nothing() {
        assert!(summary_text(&GcPassSummary::default()).is_none());
        let s = GcPassSummary {
            segments_deleted: 1,
            bytes_freed: 10,
            ..GcPassSummary::default()
        };
        let t = summary_text(&s).expect("text");
        assert!(t.contains("delete 1 segments"), "{t}");
    }

    #[test]
    fn liveness_mismatches_are_errors_per_segment() {
        let seg = ObjId::from_raw(7).expect("id");
        let other = ObjId::from_raw(9).expect("id");
        let mut gc = Liveness::default();
        gc.seg_live.insert(seg, 100);
        gc.seg_live.insert(other, 5);
        let mut mine = BTreeMap::new();
        mine.insert(seg, 100);
        let mut out = Vec::new();
        compare(&mine, &gc, &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].code, code::GC_LIVENESS);
        assert!(
            out[0].object.contains("0000000000000009"),
            "{}",
            out[0].object
        );
        mine.insert(other, 5);
        out.clear();
        compare(&mine, &gc, &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn condemned_ids_skip_commits_and_a_body_that_does_not_decode() {
        let mut l = CondemnedList::default();
        let e = lunix_gc::CondemnedEntry {
            condemned_at: lunix_format::TimeMs(1),
            bytes: 3,
        };
        let a = ObjId::from_raw(4).expect("id");
        let b = ObjId::from_raw(5).expect("id");
        l.entries.insert(ObjectRef::Segment(a), e);
        l.entries.insert(ObjectRef::Pack(b), e);
        l.entries
            .insert(ObjectRef::Commit(lunix_format::CommitSeq(2)), e);
        let file = l.to_file().expect("file");
        assert_eq!(condemned_ids(&file), [a, b].into_iter().collect());
        let bad = Condemned {
            body_ver: 1,
            body: vec![1, 2, 3],
        };
        assert!(condemned_ids(&bad).is_empty());
    }
}
