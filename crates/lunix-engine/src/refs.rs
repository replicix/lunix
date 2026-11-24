//! The memory-only reference table of the cut (milestone refs, shadow stage;
//! `docs/reference/gc.md` "Condemn on cut").
//!
//! [`SegRefs`] counts, per segment, the entries of the in-memory map that
//! point into it (by hint, not by chunk id). [`RefMap`] wraps the map for the
//! cut only: every map transaction of a cut (the destage and the compaction
//! re-hints) goes through it, so the table follows the map without a walk.
//! A count that reaches zero is a segment no tip entry names; it is *ready*
//! `2 x (retain_last - 1)` cuts later, when no retained FULL names it either
//! (the collector's `keep_floor` rule).
//!
//! Nothing here is authoritative and nothing is persisted. The table is
//! checked against every mark: the mark counts the entries of its tip root by
//! hint ([`TipRefs`]) and [`SegRefs::rebase`] compares them with the counts as
//! they stood when the mark pinned its roots. A difference is counted
//! (`drift_segments`), logged and repaired; the first mark after a start is
//! the base (the table starts `Rebuilding`, recording deltas from an unknown
//! base).
//!
//! In `shadow` mode the table is maintained and compared, and the segments it
//! would offer for condemnation are counted and compared with what the marks
//! condemn (recall, and why an offer was not confirmed); no segment is condemned, listed or
//! deleted because of it.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lunix_format::commit::MapRoot;
use lunix_format::node::DataEntry;
use lunix_format::{CommitSeq, Hint, SegId};
use lunix_iface::gc::{MarkCondemned, RefsState, RefsStats, TipRefs};
use lunix_iface::map::{
    entry_hint, BlockMap, Cached, ChunkNo, MapTxn, NodeHash, NodeRef, SealResult, TxnReport,
};
use lunix_iface::store::PackWrite;
use lunix_iface::Result;

use crate::engine::lock;

/// The most segments one collector call is offered (and the shadow check
/// counts per step).
pub(crate) const OFFER_MAX: usize = 64;

/// How many differing segments a drift log line names.
const DRIFT_LOG: usize = 8;

/// A segment ready to be offered, with the cut in which its count reached
/// zero and when (the engine's monotonic clock).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Ready {
    pub(crate) seg: SegId,
    pub(crate) since: CommitSeq,
    pub(crate) at: Duration,
}

/// Counters of the table.
#[derive(Clone, Copy, Default, Debug)]
struct Counters {
    drift_segments: u64,
    rebases: u64,
    report_mismatches: u64,
    would_propose: u64,
    offered_idle: u64,
    offered_busy: u64,
    confirmed: u64,
    confirmed_lead: u64,
    lead_sum_ms: u64,
    lead_max_ms: u64,
    confirmed_no_lead: u64,
    unconfirmed_live: u64,
    unconfirmed_kept: u64,
    unconfirmed_listed: u64,
    revived: u64,
    early_by_mark: u64,
    missed: u64,
    lag_sum_ms: u64,
    lag_max_ms: u64,
    lag_count: u64,
    idle_wait_sum_ms: u64,
    idle_wait_max_ms: u64,
    idle_wait_count: u64,
}

fn ms(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

/// An offer the shadow check made and no mark has judged yet.
#[derive(Clone, Copy, Debug)]
struct Offer {
    /// The tip at the offer: only a mark that began at or after it judges it.
    tip: CommitSeq,
    /// When the offer was made (the engine's monotonic clock).
    at: Duration,
    /// The first moment the collector was between passes at or after the
    /// offer (a proposal is only taken then); `None` while it was not.
    idle_at: Option<Duration>,
}

/// The reference table.
#[derive(Debug)]
pub(crate) struct SegRefs {
    state: RefsState,
    /// A map transaction since the last snapshot was not applied as
    /// predicted: the mark that began before it must not make the table
    /// `Ready` (its snapshot is older than the failure).
    tainted: bool,
    /// Entries of the in-memory map per segment, by hint. Signed: while
    /// `Rebuilding` they are deltas from an unknown base.
    counts: HashMap<SegId, i64>,
    /// Reached zero in a map transaction since the last committed cut.
    went_zero: BTreeSet<SegId>,
    /// Zero since the cut committed as this sequence, and when.
    zero_since: BTreeMap<SegId, (CommitSeq, Duration)>,
    /// `counts` as they stood when the current mark pinned its roots, with
    /// whether the table was `Ready` then.
    snapshot: Option<Snapshot>,
    /// The tip of the last mark that rebased the table, and whether the
    /// table was `Ready` (and untainted) when it began: what a judgement of
    /// that mark's condemns needs.
    judged: Option<(CommitSeq, bool)>,
    /// Segments offered in shadow mode and not yet judged.
    offered: BTreeMap<SegId, Offer>,
    /// Segments ready at the last offer.
    ready_last: u64,
    /// When the table was created (the engine's monotonic clock).
    born: Duration,
    /// Time from `born` to the first call that found a segment ready.
    first_ready: Option<Duration>,
    /// Time from `born` to the first mark that condemned a segment.
    first_mark_condemn: Option<Duration>,
    c: Counters,
}

#[derive(Debug)]
struct Snapshot {
    tip: CommitSeq,
    ready: bool,
    counts: HashMap<SegId, i64>,
}

impl SegRefs {
    pub(crate) fn new(now: Duration) -> Self {
        Self {
            born: now,
            first_ready: None,
            first_mark_condemn: None,
            state: RefsState::Rebuilding,
            tainted: false,
            counts: HashMap::new(),
            went_zero: BTreeSet::new(),
            zero_since: BTreeMap::new(),
            snapshot: None,
            judged: None,
            offered: BTreeMap::new(),
            ready_last: 0,
            c: Counters::default(),
        }
    }

    /// Applies the net count changes of one committed map transaction.
    pub(crate) fn apply(&mut self, deltas: &HashMap<SegId, i64>) {
        for (&seg, &d) in deltas {
            if d == 0 {
                continue;
            }
            let c = self.counts.entry(seg).or_insert(0);
            *c += d;
            if *c == 0 {
                self.counts.remove(&seg);
                if self.state != RefsState::Rebuilding {
                    self.went_zero.insert(seg);
                }
            } else {
                self.went_zero.remove(&seg);
                self.zero_since.remove(&seg);
                if self.offered.remove(&seg).is_some() {
                    self.c.revived += 1;
                }
            }
        }
    }

    /// The cut committed as `seq`: the segments whose count reached zero in
    /// it (and stayed there) are zero since `seq`.
    pub(crate) fn seal_cut(&mut self, seq: CommitSeq, now: Duration) {
        for seg in std::mem::take(&mut self.went_zero) {
            if !self.counts.contains_key(&seg) {
                self.zero_since.entry(seg).or_insert((seq, now));
            }
        }
    }

    /// Segments zero for at least `lag` cuts at `tip`, not in `skip`, oldest
    /// first, at most `max`. Nothing unless the table is `Ready`.
    pub(crate) fn ready(
        &self,
        tip: CommitSeq,
        lag: u64,
        skip: &HashSet<SegId>,
        max: usize,
    ) -> Vec<Ready> {
        if self.state != RefsState::Ready || self.zero_since.is_empty() {
            return Vec::new();
        }
        let mut v: Vec<Ready> = self
            .zero_since
            .iter()
            .filter(|(s, (k, _))| k.0.saturating_add(lag) <= tip.0 && !skip.contains(s))
            .map(|(&seg, &(since, at))| Ready { seg, since, at })
            .collect();
        v.sort_unstable_by_key(|r| (r.since, r.seg));
        v.truncate(max);
        v
    }

    /// Shadow mode: the offer is counted and remembered for the judgement of
    /// the next mark, never acted on.
    ///
    /// `idle`: the collector is between passes at this call, so an offer
    /// would be taken now; else the offered segments wait for the first
    /// call that finds it idle (their wait is measured).
    pub(crate) fn shadow_offer(
        &mut self,
        ready: &[Ready],
        tip: CommitSeq,
        now: Duration,
        idle: bool,
    ) {
        self.ready_last = ready.len() as u64;
        if !ready.is_empty() && self.first_ready.is_none() {
            self.first_ready = Some(now.saturating_sub(self.born));
        }
        if idle {
            // Offers that were made during a pass meet the idle collector
            // now: their wait is the time since the offer.
            for o in self.offered.values_mut() {
                if o.idle_at.is_none() {
                    o.idle_at = Some(now);
                    let w = ms(now.saturating_sub(o.at));
                    self.c.idle_wait_sum_ms = self.c.idle_wait_sum_ms.saturating_add(w);
                    self.c.idle_wait_max_ms = self.c.idle_wait_max_ms.max(w);
                    self.c.idle_wait_count += 1;
                }
            }
        }
        for r in ready {
            if self.zero_since.remove(&r.seg).is_none() {
                continue;
            }
            self.offered.insert(
                r.seg,
                Offer {
                    tip,
                    at: now,
                    idle_at: idle.then_some(now),
                },
            );
            self.c.would_propose += 1;
            if idle {
                self.c.offered_idle += 1;
            } else {
                self.c.offered_busy += 1;
            }
            let lag = ms(now.saturating_sub(r.at));
            self.c.lag_sum_ms = self.c.lag_sum_ms.saturating_add(lag);
            self.c.lag_max_ms = self.c.lag_max_ms.max(lag);
            self.c.lag_count += 1;
        }
    }

    /// A mark pinned its roots at `tip` (the map is at the tip): keep the
    /// counts as they are now. Whatever failed before this moment is in the
    /// snapshot and cancels in `counts - snapshot + mark`.
    pub(crate) fn mark_started(&mut self, tip: CommitSeq) {
        self.tainted = false;
        self.snapshot = Some(Snapshot {
            tip,
            ready: self.state == RefsState::Ready,
            counts: self.counts.clone(),
        });
    }

    /// The mark finished with the entry counts of its tip root. Compares
    /// them with the snapshot, repairs the counts and makes the table
    /// `Ready`, unless a map transaction failed since the snapshot (the
    /// table then stays `Drifted` or `Rebuilding`: the next mark repairs
    /// it). Returns the number of segments that differed (0 for the first
    /// base: a `Rebuilding` table has nothing to compare). Ignored when no
    /// snapshot of that tip exists.
    pub(crate) fn rebase(&mut self, r: &TipRefs) -> u64 {
        let Some(snap) = self.snapshot.take() else {
            return 0;
        };
        if snap.tip != r.tip {
            return 0;
        }
        let refs: HashMap<SegId, i64> = r
            .refs
            .iter()
            .map(|&(s, n)| (s, i64::try_from(n).unwrap_or(i64::MAX)))
            .collect();
        let mut diff = 0u64;
        if self.state != RefsState::Rebuilding {
            let mut first = Vec::new();
            let keys: BTreeSet<SegId> = snap.counts.keys().chain(refs.keys()).copied().collect();
            for s in keys {
                let (a, b) = (
                    snap.counts.get(&s).copied().unwrap_or(0),
                    refs.get(&s).copied().unwrap_or(0),
                );
                if a != b {
                    diff += 1;
                    if first.len() < DRIFT_LOG {
                        first.push((s.raw(), a, b));
                    }
                }
            }
            if diff > 0 {
                tracing::warn!(
                    tip = r.tip.0,
                    differing = diff,
                    first = ?first,
                    "the reference table differed from a mark (segment, table, mark); repaired"
                );
            }
        }
        // counts = counts - snapshot + mark: the deltas since the mark began
        // are kept.
        for (s, v) in &snap.counts {
            *self.counts.entry(*s).or_insert(0) -= v;
        }
        for (s, v) in &refs {
            *self.counts.entry(*s).or_insert(0) += v;
        }
        self.counts.retain(|_, v| *v != 0);
        if diff > 0 {
            self.c.drift_segments += diff;
            self.zero_since.clear();
            self.went_zero.clear();
        }
        self.judged = Some((r.tip, snap.ready && !self.tainted));
        if !self.tainted {
            self.state = RefsState::Ready;
        }
        self.c.rebases += 1;
        diff
    }

    /// The mark whose tip root was just rebased decided about segments
    /// (`c`) at `now`: the offers that began at or before its tip are judged
    /// against them, and the condemns the table had not offered are counted.
    pub(crate) fn judge(&mut self, c: &MarkCondemned, now: Duration) {
        let dead: BTreeSet<SegId> = c.segs.iter().copied().collect();
        let live: BTreeSet<SegId> = c.live.iter().copied().collect();
        let listed: BTreeSet<SegId> = c.listed.iter().copied().collect();
        let mut confirmed = BTreeSet::new();
        let judged: Vec<SegId> = self
            .offered
            .iter()
            .filter(|&(_, o)| o.tip <= c.tip)
            .map(|(&s, _)| s)
            .collect();
        for s in judged {
            let Some(o) = self.offered.remove(&s) else {
                continue;
            };
            if dead.contains(&s) {
                self.c.confirmed += 1;
                confirmed.insert(s);
                match o.idle_at {
                    Some(t) => {
                        let lead = ms(now.saturating_sub(t));
                        self.c.confirmed_lead += 1;
                        self.c.lead_sum_ms = self.c.lead_sum_ms.saturating_add(lead);
                        self.c.lead_max_ms = self.c.lead_max_ms.max(lead);
                    }
                    None => self.c.confirmed_no_lead += 1,
                }
            } else if live.contains(&s) {
                self.c.unconfirmed_live += 1;
            } else if listed.contains(&s) {
                self.c.unconfirmed_listed += 1;
            } else {
                self.c.unconfirmed_kept += 1;
            }
        }
        // The mark condemns these now: the table's own zeros for them are
        // done with (not offered later and then called unconfirmed).
        let was_ready = self.judged.is_some_and(|(t, ready)| t == c.tip && ready);
        for s in &dead {
            let known = self.zero_since.remove(s).is_some();
            self.went_zero.remove(s);
            if confirmed.contains(s) {
                continue;
            }
            if known {
                self.c.early_by_mark += 1;
            } else if was_ready {
                self.c.missed += 1;
            }
        }
        if !dead.is_empty() && self.first_mark_condemn.is_none() {
            // Wall time from attach: the monotonic clock of the engine.
            self.first_mark_condemn = Some(now.saturating_sub(self.born));
        }
    }

    /// A map transaction did not do what the table predicted, or could not
    /// be predicted: the table offers nothing and keeps no zero until a
    /// mark that began after this moment repairs it. A failure while the
    /// table is `Rebuilding` keeps it so.
    fn drifted(&mut self, mismatch: bool) {
        if mismatch {
            self.c.report_mismatches += 1;
        }
        self.tainted = true;
        self.zero_since.clear();
        self.went_zero.clear();
        if self.state == RefsState::Ready {
            self.state = RefsState::Drifted;
        }
    }

    /// The figures for the status.
    pub(crate) fn stats(&self) -> RefsStats {
        RefsStats {
            state: self.state,
            segments: self.counts.len() as u64,
            zero_waiting: self.zero_since.len() as u64,
            ready: self.ready_last,
            drift_segments: self.c.drift_segments,
            rebases: self.c.rebases,
            report_mismatches: self.c.report_mismatches,
            shadow_would_propose: self.c.would_propose,
            shadow_offered_idle: self.c.offered_idle,
            shadow_offered_busy: self.c.offered_busy,
            shadow_confirmed: self.c.confirmed,
            shadow_confirmed_lead: self.c.confirmed_lead,
            shadow_lead_ms_sum: self.c.lead_sum_ms,
            shadow_lead_ms_max: self.c.lead_max_ms,
            shadow_confirmed_no_lead: self.c.confirmed_no_lead,
            shadow_unconfirmed_live: self.c.unconfirmed_live,
            shadow_unconfirmed_kept: self.c.unconfirmed_kept,
            shadow_unconfirmed_listed: self.c.unconfirmed_listed,
            shadow_revived: self.c.revived,
            shadow_early_by_mark: self.c.early_by_mark,
            shadow_missed: self.c.missed,
            first_mark_condemn_ms: self.first_mark_condemn.map(ms),
            condemn_lag_ms_sum: self.c.lag_sum_ms,
            condemn_lag_ms_max: self.c.lag_max_ms,
            condemn_lag_count: self.c.lag_count,
            idle_wait_ms_sum: self.c.idle_wait_sum_ms,
            idle_wait_ms_max: self.c.idle_wait_max_ms,
            idle_wait_count: self.c.idle_wait_count,
            idle_waiting: self
                .offered
                .values()
                .filter(|o| o.idle_at.is_none())
                .count() as u64,
            first_ready_ms: self.first_ready.map(ms),
        }
    }

    /// The count of one segment (tests).
    #[cfg(test)]
    pub(crate) fn count(&self, seg: SegId) -> i64 {
        self.counts.get(&seg).copied().unwrap_or(0)
    }

    /// The table state (tests).
    #[cfg(test)]
    pub(crate) fn state(&self) -> RefsState {
        self.state
    }
}

/// One recorded operation of a wrapped transaction.
#[derive(Clone, Copy, Debug)]
enum Op {
    Set(ChunkNo, Hint),
    Hole(ChunkNo),
    Rehint(ChunkNo, Hint, Hint),
}

/// The map as the cut sees it: every transaction is forwarded to the real
/// map at once and recorded; at commit the table gets the count changes the
/// transaction made.
pub(crate) struct RefMap {
    inner: Arc<dyn BlockMap>,
    refs: Arc<Mutex<SegRefs>>,
}

impl RefMap {
    pub(crate) fn new(inner: Arc<dyn BlockMap>, refs: Arc<Mutex<SegRefs>>) -> Self {
        Self { inner, refs }
    }
}

struct RefTxn<'a> {
    inner: Box<dyn MapTxn + 'a>,
    owner: &'a RefMap,
    ops: Vec<Op>,
}

/// What a transaction will do to the counts and to the report.
struct Predicted {
    deltas: HashMap<SegId, i64>,
    sets: u64,
    rehints_applied: u64,
}

impl RefTxn<'_> {
    /// Replays the recorded operations against the map as it is before the
    /// transaction (the pending operations of the transaction on top), in
    /// order, the way the map applies them at commit.
    fn predict(&self) -> Result<Predicted> {
        let mut pending: HashMap<u64, Option<Hint>> = HashMap::new();
        let mut p = Predicted {
            deltas: HashMap::new(),
            sets: 0,
            rehints_applied: 0,
        };
        let current = |pending: &HashMap<u64, Option<Hint>>, ci: ChunkNo| -> Result<Option<Hint>> {
            match pending.get(&ci.0) {
                Some(h) => Ok(*h),
                None => Ok(self.owner.inner.get(ci)?.map(|e| entry_hint(&e))),
            }
        };
        for &op in &self.ops {
            match op {
                Op::Set(ci, new) => {
                    if let Some(old) = current(&pending, ci)? {
                        *p.deltas.entry(old.seg).or_insert(0) -= 1;
                    }
                    *p.deltas.entry(new.seg).or_insert(0) += 1;
                    pending.insert(ci.0, Some(new));
                    p.sets += 1;
                }
                Op::Hole(ci) => {
                    if let Some(old) = current(&pending, ci)? {
                        *p.deltas.entry(old.seg).or_insert(0) -= 1;
                    }
                    pending.insert(ci.0, None);
                    p.sets += 1;
                }
                Op::Rehint(ci, old, new) => {
                    if current(&pending, ci)? == Some(old) {
                        *p.deltas.entry(old.seg).or_insert(0) -= 1;
                        *p.deltas.entry(new.seg).or_insert(0) += 1;
                        pending.insert(ci.0, Some(new));
                        p.rehints_applied += 1;
                    }
                }
            }
        }
        Ok(p)
    }
}

impl MapTxn for RefTxn<'_> {
    fn set(&mut self, ci: ChunkNo, entry: DataEntry) -> Result<()> {
        self.inner.set(ci, entry)?;
        self.ops.push(Op::Set(ci, entry_hint(&entry)));
        Ok(())
    }

    fn set_hole(&mut self, ci: ChunkNo) -> Result<()> {
        self.inner.set_hole(ci)?;
        self.ops.push(Op::Hole(ci));
        Ok(())
    }

    fn rehint(&mut self, ci: ChunkNo, old: Hint, new: Hint) -> Result<()> {
        self.inner.rehint(ci, old, new)?;
        self.ops.push(Op::Rehint(ci, old, new));
        Ok(())
    }

    fn commit(self: Box<Self>) -> Result<TxnReport> {
        // The prediction reads the map before the transaction is applied. A
        // read error only costs the table its prediction (the next mark
        // repairs it); the transaction itself must not fail for it.
        let this = *self;
        let predicted = this.predict();
        let refs = Arc::clone(&this.owner.refs);
        let report = this.inner.commit()?;
        let mut t = lock(&refs);
        match predicted {
            Ok(p) => {
                if p.sets != report.sets || p.rehints_applied != report.rehints_applied {
                    tracing::warn!(
                        predicted_sets = p.sets,
                        sets = report.sets,
                        predicted_rehints = p.rehints_applied,
                        rehints = report.rehints_applied,
                        "reference table: a map transaction report differed from the prediction"
                    );
                    t.drifted(true);
                }
                // Applied although it is known to be wrong when the report
                // differed: the table is `Drifted` and offers nothing, and
                // the next mark repairs the counts either way.
                t.apply(&p.deltas);
            }
            Err(e) => {
                tracing::warn!(error = %e, "reference table: a map transaction could not be predicted");
                t.drifted(false);
            }
        }
        Ok(report)
    }
}

impl BlockMap for RefMap {
    fn chunk_count(&self) -> u64 {
        self.inner.chunk_count()
    }

    fn get(&self, ci: ChunkNo) -> Result<Option<DataEntry>> {
        self.inner.get(ci)
    }

    fn try_get_cached(&self, ci: ChunkNo) -> Cached {
        self.inner.try_get_cached(ci)
    }

    fn begin_update(&self) -> Result<Box<dyn MapTxn + '_>> {
        Ok(Box::new(RefTxn {
            inner: self.inner.begin_update()?,
            owner: self,
            ops: Vec::new(),
        }))
    }

    fn seal(&self, packs: &mut dyn PackWrite) -> Result<SealResult> {
        self.inner.seal(packs)
    }

    fn grow(&self, new_chunk_count: u64) -> Result<()> {
        self.inner.grow(new_chunk_count)
    }

    fn root(&self) -> MapRoot {
        self.inner.root()
    }

    fn dirty_leaf_count(&self) -> u64 {
        self.inner.dirty_leaf_count()
    }

    fn walk_leaves_skipping(
        &self,
        root: &MapRoot,
        skip: &HashSet<NodeHash>,
        f: &mut dyn FnMut(ChunkNo, &DataEntry),
    ) -> Result<()> {
        self.inner.walk_leaves_skipping(root, skip, f)
    }

    fn walk_nodes(&self, root: &MapRoot, f: &mut dyn FnMut(&NodeRef)) -> Result<()> {
        self.inner.walk_nodes(root, f)
    }

    fn walk_nodes_skipping(
        &self,
        root: &MapRoot,
        skip: &HashSet<NodeHash>,
        f: &mut dyn FnMut(&NodeRef),
    ) -> Result<()> {
        self.inner.walk_nodes_skipping(root, skip, f)
    }
}

/// Measurement support (feature `testsupport`): the cut's table wrapper over
/// an empty `map`, already `Ready` (rebased on the empty map).
#[cfg(feature = "testsupport")]
pub(crate) fn bench_ref_map(map: Arc<dyn BlockMap>) -> Arc<dyn BlockMap> {
    let mut t = SegRefs::new(Duration::ZERO);
    t.mark_started(CommitSeq(0));
    t.rebase(&TipRefs {
        tip: CommitSeq(0),
        refs: Vec::new(),
    });
    Arc::new(RefMap::new(map, Arc::new(Mutex::new(t))))
}

/// What the table's mark-start snapshot and `ready()` cost for a table of
/// `segments` segments of which `zeros` wait in `zero_since`: nanoseconds per
/// call, averaged over `reps` calls.
#[cfg(feature = "testsupport")]
pub(crate) fn bench_costs(segments: usize, zeros: usize, reps: u32) -> (u64, u64) {
    let mut t = SegRefs::new(Duration::ZERO);
    t.mark_started(CommitSeq(0));
    t.rebase(&TipRefs {
        tip: CommitSeq(0),
        refs: Vec::new(),
    });
    let ids: Vec<SegId> = (1..=segments as u64)
        .filter_map(|n| SegId::new(0, n).ok())
        .collect();
    let all: HashMap<SegId, i64> = ids.iter().map(|&s| (s, 1)).collect();
    t.apply(&all);
    let gone: HashMap<SegId, i64> = ids.iter().take(zeros).map(|&s| (s, -1)).collect();
    t.apply(&gone);
    t.seal_cut(CommitSeq(1), Duration::ZERO);
    let reps = reps.max(1);
    let t0 = std::time::Instant::now();
    for i in 0..reps {
        t.mark_started(CommitSeq(u64::from(i) + 2));
    }
    let clone_ns = u64::try_from(t0.elapsed().as_nanos() / u128::from(reps)).unwrap_or(u64::MAX);
    let t0 = std::time::Instant::now();
    let skip = HashSet::new();
    for _ in 0..reps {
        std::hint::black_box(t.ready(CommitSeq(u64::MAX / 2), 0, &skip, OFFER_MAX));
    }
    let ready_ns = u64::try_from(t0.elapsed().as_nanos() / u128::from(reps)).unwrap_or(u64::MAX);
    (clone_ns, ready_ns)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lunix_format::{IdLen, IdPrefix};
    use lunix_testkit::{FlatMap, MemPacks, PlainCodec};
    use proptest::prelude::*;

    fn seg(n: u64) -> SegId {
        SegId::new(0, n).expect("segment id")
    }

    fn seq(n: u64) -> CommitSeq {
        CommitSeq(n)
    }

    fn at(ms: u64) -> Duration {
        Duration::from_millis(ms)
    }

    fn hint(s: u64, off: u32) -> Hint {
        Hint {
            seg: seg(s),
            off,
            clen: 100,
        }
    }

    fn entry(s: u64, off: u32) -> DataEntry {
        DataEntry {
            lineage: lunix_format::LineageIdx(0),
            seg: seg(s),
            off,
            clen: 100,
            prefix: IdPrefix::from_slice(IdLen::Sixteen, &[7; 16]).expect("prefix"),
        }
    }

    fn flat(chunks: u64, cap: Option<u64>) -> Arc<dyn BlockMap> {
        let m = FlatMap::new(
            Arc::new(PlainCodec::new()),
            Arc::new(MemPacks::new()),
            chunks,
            IdLen::Sixteen,
        );
        Arc::new(match cap {
            Some(c) => m.with_dirty_leaf_cap(c),
            None => m,
        })
    }

    fn ready_table() -> SegRefs {
        let mut t = SegRefs::new(Duration::ZERO);
        t.state = RefsState::Ready;
        t
    }

    fn deltas(v: &[(u64, i64)]) -> HashMap<SegId, i64> {
        v.iter().map(|&(s, d)| (seg(s), d)).collect()
    }

    /// The entries per segment of a whole map, counted chunk by chunk.
    fn counted(m: &dyn BlockMap) -> BTreeMap<SegId, i64> {
        let mut out = BTreeMap::new();
        for ci in 0..m.chunk_count() {
            if let Some(e) = m.get(ChunkNo(ci)).expect("get") {
                *out.entry(e.seg).or_insert(0) += 1;
            }
        }
        out
    }

    fn table(t: &Mutex<SegRefs>) -> BTreeMap<SegId, i64> {
        lock(t).counts.iter().map(|(&s, &n)| (s, n)).collect()
    }

    fn wrapped(chunks: u64, cap: Option<u64>) -> (Arc<dyn BlockMap>, RefMap, Arc<Mutex<SegRefs>>) {
        let inner = flat(chunks, cap);
        let refs = Arc::new(Mutex::new(SegRefs::new(Duration::ZERO)));
        let m = RefMap::new(Arc::clone(&inner), Arc::clone(&refs));
        (inner, m, refs)
    }

    // ------------------------------------------------------------ SegRefs

    #[test]
    fn a_zero_is_ready_after_two_cuts_for_two_retained_fulls() {
        for (retain, lag) in [(1u64, 0u64), (2, 2), (3, 4)] {
            let mut t = ready_table();
            t.apply(&deltas(&[(1, 1), (2, 1)]));
            t.seal_cut(seq(10), at(0));
            assert!(t.zero_since.is_empty(), "nothing went to zero");
            t.apply(&deltas(&[(1, -1)]));
            t.seal_cut(seq(11), at(5));
            assert_eq!(2 * (retain - 1), lag);
            assert!(
                t.ready(seq(10 + lag), lag, &HashSet::new(), 9).is_empty(),
                "retain {retain}: not yet at {}",
                10 + lag
            );
            let r = t.ready(seq(11 + lag), lag, &HashSet::new(), 9);
            assert_eq!(
                r,
                vec![Ready {
                    seg: seg(1),
                    since: seq(11),
                    at: at(5)
                }],
                "retain {retain}"
            );
        }
    }

    #[test]
    fn a_revival_clears_a_zero_and_a_dip_inside_one_transaction_is_not_one() {
        let mut t = ready_table();
        t.apply(&deltas(&[(1, 1), (2, 1)]));
        t.seal_cut(seq(1), at(0));
        // Gone and back in one cut: two transactions, so it is recorded
        // while it is at zero and cleared when it comes back.
        t.apply(&deltas(&[(1, -1)]));
        t.apply(&deltas(&[(1, 1)]));
        t.seal_cut(seq(2), at(1));
        assert!(t.zero_since.is_empty() && t.went_zero.is_empty());
        // Zero in cut 3, revived in cut 4 before it is ready.
        t.apply(&deltas(&[(2, -1)]));
        t.seal_cut(seq(3), at(2));
        assert_eq!(t.zero_since.len(), 1);
        t.apply(&deltas(&[(2, 1)]));
        assert!(t.zero_since.is_empty(), "a new entry revives it");
        // One transaction whose net change is zero records nothing.
        t.apply(&deltas(&[(2, 0)]));
        t.seal_cut(seq(4), at(3));
        assert!(t.zero_since.is_empty());
    }

    #[test]
    fn skip_and_max_are_honoured_and_the_oldest_cut_goes_first() {
        let mut t = ready_table();
        t.apply(&deltas(&[(1, 1), (2, 1), (3, 1), (4, 1)]));
        t.seal_cut(seq(1), at(0));
        t.apply(&deltas(&[(3, -1)]));
        t.seal_cut(seq(2), at(1));
        t.apply(&deltas(&[(1, -1), (2, -1), (4, -1)]));
        t.seal_cut(seq(3), at(2));
        let skip: HashSet<SegId> = [seg(2)].into_iter().collect();
        let r = t.ready(seq(9), 2, &skip, 2);
        let segs: Vec<SegId> = r.iter().map(|x| x.seg).collect();
        assert_eq!(
            segs,
            vec![seg(3), seg(1)],
            "cut 2 before cut 3, not 2, max 2"
        );
    }

    #[test]
    fn a_rebuilding_or_drifted_table_offers_nothing_and_records_no_zero() {
        let mut t = SegRefs::new(Duration::ZERO);
        t.apply(&deltas(&[(1, 1)]));
        t.apply(&deltas(&[(1, -1)]));
        t.seal_cut(seq(1), at(0));
        assert!(t.went_zero.is_empty() && t.zero_since.is_empty());
        assert!(t.ready(seq(99), 0, &HashSet::new(), 9).is_empty());
        let mut t = ready_table();
        t.apply(&deltas(&[(1, 1)]));
        t.apply(&deltas(&[(1, -1)]));
        t.seal_cut(seq(1), at(0));
        t.drifted(true);
        assert_eq!(t.state(), RefsState::Drifted);
        assert!(t.ready(seq(99), 0, &HashSet::new(), 9).is_empty());
        assert_eq!(t.stats().report_mismatches, 1);
    }

    fn tip_refs(tip: u64, v: &[(u64, u64)]) -> TipRefs {
        TipRefs {
            tip: seq(tip),
            refs: v.iter().map(|&(s, n)| (seg(s), n)).collect(),
        }
    }

    #[test]
    fn the_first_mark_is_the_base_and_a_matching_mark_is_no_drift() {
        let mut t = SegRefs::new(Duration::ZERO);
        // Deltas from an unknown base: the table says 2 and 5, the real
        // counts are 7 and 5 + 4.
        t.apply(&deltas(&[(1, 2), (2, 5)]));
        t.mark_started(seq(5));
        // A cut after the mark began.
        t.apply(&deltas(&[(2, -1), (3, 1)]));
        assert_eq!(t.rebase(&tip_refs(5, &[(1, 7), (2, 9)])), 0, "no base yet");
        assert_eq!(t.state(), RefsState::Ready);
        assert_eq!(t.count(seg(1)), 7);
        assert_eq!(t.count(seg(2)), 8, "the delta since the mark began is kept");
        assert_eq!(t.count(seg(3)), 1);
        // Next mark, equal.
        t.mark_started(seq(6));
        assert_eq!(t.rebase(&tip_refs(6, &[(1, 7), (2, 8), (3, 1)])), 0);
        assert_eq!(t.stats().drift_segments, 0);
        assert_eq!(t.stats().rebases, 2);
    }

    #[test]
    fn a_differing_mark_is_counted_repaired_and_clears_the_zeros() {
        let mut t = ready_table();
        t.apply(&deltas(&[(1, 3), (2, 1), (3, 2)]));
        t.apply(&deltas(&[(2, -1)]));
        t.seal_cut(seq(1), at(0));
        assert_eq!(t.zero_since.len(), 1);
        t.mark_started(seq(1));
        t.apply(&deltas(&[(1, 1)]));
        // The mark found 4 in segment 1 and 0 in 3 (the table said 3 and 2).
        let d = t.rebase(&tip_refs(1, &[(1, 4)]));
        assert_eq!(d, 2);
        assert_eq!(
            t.count(seg(1)),
            5,
            "4 from the mark plus the delta after it"
        );
        assert_eq!(t.count(seg(3)), 0);
        assert_eq!(t.stats().drift_segments, 2);
        assert!(
            t.zero_since.is_empty(),
            "the zeros are not trusted any more"
        );
        assert_eq!(t.state(), RefsState::Ready);
    }

    #[test]
    fn a_drifted_table_is_repaired_by_the_next_mark_and_a_snapshot_of_another_tip_is_ignored() {
        let mut t = ready_table();
        t.apply(&deltas(&[(1, 1)]));
        t.drifted(true);
        t.mark_started(seq(3));
        assert_eq!(t.rebase(&tip_refs(4, &[(1, 5)])), 0, "another tip: ignored");
        assert_eq!(t.count(seg(1)), 1);
        assert_eq!(t.state(), RefsState::Drifted);
        assert_eq!(t.rebase(&tip_refs(3, &[(1, 5)])), 0, "snapshot is gone");
        t.mark_started(seq(3));
        assert_eq!(t.rebase(&tip_refs(3, &[(1, 5)])), 1);
        assert_eq!(t.count(seg(1)), 5);
        assert_eq!(t.state(), RefsState::Ready);
    }

    fn mc(tip: u64, segs: &[u64], live: &[u64], listed: &[u64]) -> MarkCondemned {
        let v = |x: &[u64]| x.iter().map(|&n| seg(n)).collect::<Vec<_>>();
        MarkCondemned {
            tip: seq(tip),
            segs: v(segs),
            live: v(live),
            listed: v(listed),
        }
    }

    /// B1: a failure after the mark pinned its roots is not repaired by that
    /// mark (its snapshot is older): the table stays `Drifted`, keeps no
    /// zero and offers nothing; the next mark makes it `Ready`.
    #[test]
    fn a_failure_during_a_mark_is_not_erased_by_that_marks_rebase() {
        for report_mismatch in [true, false] {
            let mut t = ready_table();
            t.apply(&deltas(&[(1, 1), (2, 1)]));
            t.seal_cut(seq(1), at(0));
            t.mark_started(seq(1));
            // A cut after the snapshot whose prediction failed: segment 2
            // lost its entry, the table never heard of it (or heard wrong).
            t.apply(&deltas(&[(1, -1)]));
            t.seal_cut(seq(2), at(1));
            t.drifted(report_mismatch);
            assert!(t.zero_since.is_empty(), "a zero is not trusted any more");
            // The mark saw the tip as it was at the snapshot: no difference.
            assert_eq!(t.rebase(&tip_refs(1, &[(1, 1), (2, 1)])), 0);
            assert_eq!(t.state(), RefsState::Drifted, "{report_mismatch}");
            assert!(t.ready(seq(99), 0, &HashSet::new(), 9).is_empty());
            assert_eq!(t.judged, Some((seq(1), false)));
            // The next mark began after the failure: its snapshot holds the
            // bad count, the mark finds the difference and repairs it.
            t.mark_started(seq(2));
            assert_eq!(t.rebase(&tip_refs(2, &[])), 1);
            assert_eq!(t.state(), RefsState::Ready);
            assert_eq!(t.count(seg(2)), 0);
        }
    }

    #[test]
    fn a_failure_while_rebuilding_keeps_the_table_rebuilding() {
        let mut t = SegRefs::new(Duration::ZERO);
        t.apply(&deltas(&[(1, 2)]));
        t.mark_started(seq(1));
        t.drifted(false);
        assert_eq!(t.rebase(&tip_refs(1, &[(1, 3)])), 0);
        assert_eq!(t.state(), RefsState::Rebuilding, "not a base");
        t.mark_started(seq(1));
        t.rebase(&tip_refs(1, &[(1, 3)]));
        assert_eq!(t.state(), RefsState::Ready);
    }

    /// S2: why an offer was not confirmed, and what the mark condemned
    /// while the table already knew the zero.
    #[test]
    fn an_unconfirmed_offer_is_live_by_id_listed_or_kept_and_an_early_mark_is_not_a_miss() {
        let mut t = ready_table();
        t.apply(&deltas(&[(1, 1), (2, 1), (3, 1), (4, 1), (5, 1), (6, 1)]));
        t.seal_cut(seq(1), at(0));
        t.apply(&deltas(&[
            (1, -1),
            (2, -1),
            (3, -1),
            (4, -1),
            (5, -1),
            (6, -1),
        ]));
        t.seal_cut(seq(2), at(10));
        // 1..4 are offered (idle), 5 and 6 stay in `zero_since` (cap).
        let r: Vec<Ready> = t
            .ready(seq(4), 2, &HashSet::new(), 9)
            .into_iter()
            .take(4)
            .collect();
        t.shadow_offer(&r, seq(4), at(100), true);
        t.mark_started(seq(4));
        t.rebase(&tip_refs(4, &[]));
        // The mark: condemns 1 and 5; 2 is live by id; 3 is listed; 4 kept.
        t.judge(&mc(4, &[1, 5], &[2], &[3]), at(450));
        let s = t.stats();
        assert_eq!(s.shadow_confirmed, 1);
        assert_eq!((s.shadow_confirmed_lead, s.shadow_lead_ms_sum), (1, 350));
        assert_eq!(s.shadow_lead_ms_max, 350);
        assert_eq!(s.shadow_unconfirmed_live, 1);
        assert_eq!(s.shadow_unconfirmed_listed, 1);
        assert_eq!(s.shadow_unconfirmed_kept, 1);
        assert_eq!(s.shadow_early_by_mark, 1, "5 was known, not yet offered");
        assert_eq!(s.shadow_missed, 0, "a zero the table knew is not a miss");
        assert_eq!(s.shadow_offered_idle, 4);
        assert_eq!(s.first_mark_condemn_ms, Some(450));
        // 5 is listed now: it is not offered later; 6 still is.
        let r = t.ready(seq(9), 2, &HashSet::new(), 9);
        assert_eq!(r.iter().map(|x| x.seg).collect::<Vec<_>>(), vec![seg(6)]);
    }

    /// S1: a revival and a judgement remove the idle-wait entry.
    #[test]
    fn a_revived_or_judged_offer_is_no_longer_waiting_for_idle() {
        let mut t = ready_table();
        t.apply(&deltas(&[(1, 1), (2, 1)]));
        t.seal_cut(seq(1), at(0));
        t.apply(&deltas(&[(1, -1), (2, -1)]));
        t.seal_cut(seq(2), at(10));
        let r = t.ready(seq(4), 2, &HashSet::new(), 9);
        t.shadow_offer(&r, seq(4), at(100), false);
        assert_eq!(t.stats().idle_waiting, 2);
        assert_eq!(t.stats().shadow_offered_busy, 2);
        t.apply(&deltas(&[(1, 1)]));
        assert_eq!(t.stats().idle_waiting, 1, "revived");
        t.mark_started(seq(4));
        t.rebase(&tip_refs(4, &[(1, 1)]));
        t.judge(&mc(4, &[2], &[], &[]), at(200));
        let s = t.stats();
        assert_eq!(s.idle_waiting, 0, "judged");
        assert_eq!((s.idle_wait_count, s.shadow_confirmed_no_lead), (0, 1));
    }

    #[test]
    fn the_shadow_offer_is_judged_by_a_mark_that_began_at_or_after_it() {
        let mut t = ready_table();
        t.apply(&deltas(&[(1, 1), (2, 1), (3, 1), (4, 1)]));
        t.seal_cut(seq(1), at(0));
        t.apply(&deltas(&[(1, -1), (2, -1), (3, -1)]));
        t.seal_cut(seq(2), at(100));
        let r = t.ready(seq(4), 2, &HashSet::new(), 9);
        assert_eq!(r.len(), 3);
        t.shadow_offer(&r, seq(4), at(350), false);
        let s = t.stats();
        assert_eq!(s.shadow_would_propose, 3);
        assert_eq!((s.condemn_lag_ms_sum, s.condemn_lag_ms_max), (750, 250));
        assert_eq!(s.zero_waiting, 0, "offered segments leave the waiting set");
        // Segment 3 is revived before a mark judges it.
        t.apply(&deltas(&[(3, 1)]));
        assert_eq!(t.stats().shadow_revived, 1);
        // A mark that began before the offer judges nothing.
        t.mark_started(seq(3));
        t.rebase(&tip_refs(3, &[(4, 1), (3, 1)]));
        t.judge(&mc(3, &[], &[], &[]), at(400));
        assert_eq!(t.offered.len(), 2);
        // One at the offer's tip: 1 condemned, 2 not; 9 is the mark's own.
        t.mark_started(seq(4));
        t.rebase(&tip_refs(4, &[(4, 1), (3, 1)]));
        t.judge(&mc(4, &[1, 9], &[], &[]), at(500));
        let s = t.stats();
        assert_eq!((s.shadow_confirmed, s.shadow_unconfirmed_kept), (1, 1));
        assert_eq!(s.shadow_confirmed_no_lead, 1, "offered during a pass");
        assert_eq!(s.shadow_missed, 1, "segment 9 was the mark's alone");
        assert!(t.offered.is_empty());
    }

    #[test]
    fn the_wait_for_an_idle_collector_and_the_first_ready_time_are_measured() {
        let mut t = SegRefs::new(at(1_000));
        t.state = RefsState::Ready;
        t.apply(&deltas(&[(1, 1), (2, 1)]));
        t.seal_cut(seq(1), at(1_100));
        t.shadow_offer(&[], seq(1), at(1_200), true);
        assert_eq!(t.stats().first_ready_ms, None);
        t.apply(&deltas(&[(1, -1), (2, -1)]));
        t.seal_cut(seq(2), at(1_300));
        let r = t.ready(seq(4), 2, &HashSet::new(), 9);
        // The collector is mid-pass at the offer, idle 700 ms later.
        t.shadow_offer(&r, seq(4), at(2_000), false);
        assert_eq!(t.stats().first_ready_ms, Some(1_000));
        assert_eq!(t.stats().idle_waiting, 2);
        t.shadow_offer(&[], seq(4), at(2_500), false);
        assert_eq!(t.stats().idle_wait_count, 0);
        t.shadow_offer(&[], seq(4), at(2_700), true);
        let s = t.stats();
        assert_eq!(
            (
                s.idle_waiting,
                s.idle_wait_count,
                s.idle_wait_ms_sum,
                s.idle_wait_ms_max
            ),
            (0, 2, 1_400, 700)
        );
    }

    // ------------------------------------------------------------- RefMap

    fn commit(m: &RefMap, ops: &[Op]) -> Result<TxnReport> {
        let mut txn = m.begin_update()?;
        for &op in ops {
            match op {
                Op::Set(ci, h) => txn.set(ci, entry(h.seg.raw(), h.off))?,
                Op::Hole(ci) => txn.set_hole(ci)?,
                Op::Rehint(ci, o, n) => txn.rehint(ci, o, n)?,
            }
        }
        txn.commit()
    }

    #[test]
    fn a_set_over_a_hole_over_an_entry_and_a_hole_are_counted() {
        let (inner, m, refs) = wrapped(64, None);
        let r = commit(
            &m,
            &[
                Op::Set(ChunkNo(1), hint(1, 0)),
                Op::Set(ChunkNo(2), hint(1, 1)),
            ],
        )
        .expect("commit");
        assert_eq!(r.sets, 2);
        assert_eq!(table(&refs), counted(&*inner));
        // Over an entry: the old segment loses one, the new gains one.
        commit(&m, &[Op::Set(ChunkNo(1), hint(2, 0))]).expect("commit");
        assert_eq!(table(&refs), counted(&*inner));
        assert_eq!(lock(&refs).count(seg(1)), 1);
        // A hole over an entry, and over a hole.
        commit(&m, &[Op::Hole(ChunkNo(2)), Op::Hole(ChunkNo(9))]).expect("commit");
        assert_eq!(table(&refs), counted(&*inner));
        assert_eq!(lock(&refs).count(seg(1)), 0);
        assert_eq!(lock(&refs).stats().report_mismatches, 0);
    }

    #[test]
    fn a_rehint_that_applies_and_one_that_is_skipped_are_predicted() {
        let (inner, m, refs) = wrapped(64, None);
        commit(
            &m,
            &[
                Op::Set(ChunkNo(1), hint(1, 0)),
                Op::Set(ChunkNo(2), hint(1, 1)),
            ],
        )
        .expect("commit");
        let r = commit(
            &m,
            &[
                Op::Rehint(ChunkNo(1), hint(1, 0), hint(5, 0)),
                // The entry's hint is not `old`: skipped.
                Op::Rehint(ChunkNo(2), hint(1, 99), hint(5, 1)),
                // A hole: skipped.
                Op::Rehint(ChunkNo(3), hint(1, 0), hint(5, 2)),
            ],
        )
        .expect("commit");
        assert_eq!((r.rehints_applied, r.rehints_skipped), (1, 2));
        assert_eq!(table(&refs), counted(&*inner));
        assert_eq!(lock(&refs).count(seg(5)), 1);
        assert_eq!(lock(&refs).stats().report_mismatches, 0);
    }

    #[test]
    fn two_operations_on_one_chunk_in_one_transaction_see_each_other() {
        let (inner, m, refs) = wrapped(64, None);
        commit(&m, &[Op::Set(ChunkNo(1), hint(1, 0))]).expect("commit");
        commit(
            &m,
            &[
                Op::Set(ChunkNo(1), hint(2, 0)),
                // Applies to the entry the first operation wrote.
                Op::Rehint(ChunkNo(1), hint(2, 0), hint(3, 0)),
                Op::Hole(ChunkNo(4)),
                Op::Set(ChunkNo(4), hint(2, 5)),
                Op::Hole(ChunkNo(4)),
            ],
        )
        .expect("commit");
        assert_eq!(table(&refs), counted(&*inner));
        assert_eq!(counted(&*inner).len(), 1);
        assert_eq!(lock(&refs).stats().report_mismatches, 0);
    }

    #[test]
    fn a_dropped_transaction_and_a_failed_commit_record_nothing() {
        let (inner, m, refs) = wrapped(2048, Some(2));
        commit(&m, &[Op::Set(ChunkNo(1), hint(1, 0))]).expect("commit");
        let before = table(&refs);
        {
            let mut txn = m.begin_update().expect("begin");
            txn.set(ChunkNo(2), entry(2, 0)).expect("set");
            // Dropped.
        }
        assert_eq!(table(&refs), before);
        // Three leaves with a cap of two: the commit fails MapFull.
        let e = commit(
            &m,
            &[
                Op::Set(ChunkNo(3), hint(3, 0)),
                Op::Set(ChunkNo(600), hint(3, 1)),
                Op::Set(ChunkNo(1500), hint(3, 2)),
            ],
        )
        .expect_err("map full");
        assert_eq!(e.kind(), lunix_iface::ErrorKind::MapFull);
        assert_eq!(table(&refs), before);
        assert_eq!(table(&refs), counted(&*inner));
        // The transaction is closed again.
        commit(&m, &[Op::Set(ChunkNo(5), hint(1, 7))]).expect("commit");
        assert_eq!(table(&refs), counted(&*inner));
    }

    #[derive(Clone, Debug)]
    enum Raw {
        Set(u8, u8, u8),
        Hole(u8),
        /// A re-hint of what the entry has now to another segment.
        Move(u8, u8),
        /// A re-hint from a hint that is not there.
        Stale(u8, u8),
        Drop,
    }

    fn raw() -> impl Strategy<Value = Raw> {
        prop_oneof![
            4 => (0u8..40, 1u8..7, any::<u8>()).prop_map(|(c, s, o)| Raw::Set(c, s, o)),
            1 => (0u8..40).prop_map(Raw::Hole),
            3 => (0u8..40, 1u8..7).prop_map(|(c, s)| Raw::Move(c, s)),
            1 => (0u8..40, 1u8..7).prop_map(|(c, s)| Raw::Stale(c, s)),
            1 => Just(Raw::Drop),
        ]
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        #[test]
        fn the_table_equals_a_count_of_the_map_after_every_commit(
            txns in proptest::collection::vec(proptest::collection::vec(raw(), 0..12), 1..16)
        ) {
            let (inner, m, refs) = wrapped(40, None);
            for ops in txns {
                let mut txn = m.begin_update().expect("begin");
                let mut dropped = false;
                for op in ops {
                    match op {
                        Raw::Set(c, s, o) => txn.set(ChunkNo(u64::from(c)), entry(u64::from(s), u32::from(o))).expect("set"),
                        Raw::Hole(c) => txn.set_hole(ChunkNo(u64::from(c))).expect("hole"),
                        Raw::Move(c, s) => {
                            let ci = ChunkNo(u64::from(c));
                            let old = inner.get(ci).expect("get").map(|e| entry_hint(&e)).unwrap_or(hint(1, 0));
                            txn.rehint(ci, old, hint(u64::from(s), 4242)).expect("rehint");
                        }
                        Raw::Stale(c, s) => txn
                            .rehint(ChunkNo(u64::from(c)), hint(99, 1), hint(u64::from(s), 7))
                            .expect("rehint"),
                        Raw::Drop => dropped = true,
                    }
                }
                if dropped {
                    drop(txn);
                } else {
                    txn.commit().expect("commit");
                }
                prop_assert_eq!(table(&refs), counted(&*inner));
            }
            prop_assert_eq!(lock(&refs).stats().report_mismatches, 0);
        }
    }

    // ---------------------------------------- negative controls (S3)

    /// A map that misbehaves under the wrapper: it drops every re-hint
    /// silently (the real map never sees it) or reports one `set` too many.
    /// The table must notice and the next mark must repair it.
    struct Lying {
        inner: Arc<dyn BlockMap>,
        drop_rehints: bool,
        extra_sets: u64,
    }

    struct LyingTxn<'a> {
        inner: Box<dyn MapTxn + 'a>,
        drop_rehints: bool,
        extra_sets: u64,
    }

    impl MapTxn for LyingTxn<'_> {
        fn set(&mut self, ci: ChunkNo, entry: DataEntry) -> Result<()> {
            self.inner.set(ci, entry)
        }
        fn set_hole(&mut self, ci: ChunkNo) -> Result<()> {
            self.inner.set_hole(ci)
        }
        fn rehint(&mut self, ci: ChunkNo, old: Hint, new: Hint) -> Result<()> {
            if self.drop_rehints {
                return Ok(());
            }
            self.inner.rehint(ci, old, new)
        }
        fn commit(self: Box<Self>) -> Result<TxnReport> {
            let extra = self.extra_sets;
            let mut r = self.inner.commit()?;
            r.sets += extra;
            Ok(r)
        }
    }

    impl BlockMap for Lying {
        fn chunk_count(&self) -> u64 {
            self.inner.chunk_count()
        }
        fn get(&self, ci: ChunkNo) -> Result<Option<DataEntry>> {
            self.inner.get(ci)
        }
        fn try_get_cached(&self, ci: ChunkNo) -> Cached {
            self.inner.try_get_cached(ci)
        }
        fn begin_update(&self) -> Result<Box<dyn MapTxn + '_>> {
            Ok(Box::new(LyingTxn {
                inner: self.inner.begin_update()?,
                drop_rehints: self.drop_rehints,
                extra_sets: self.extra_sets,
            }))
        }
        fn seal(&self, packs: &mut dyn PackWrite) -> Result<SealResult> {
            self.inner.seal(packs)
        }
        fn grow(&self, n: u64) -> Result<()> {
            self.inner.grow(n)
        }
        fn root(&self) -> MapRoot {
            self.inner.root()
        }
        fn dirty_leaf_count(&self) -> u64 {
            self.inner.dirty_leaf_count()
        }
        fn walk_leaves_skipping(
            &self,
            root: &MapRoot,
            skip: &HashSet<NodeHash>,
            f: &mut dyn FnMut(ChunkNo, &DataEntry),
        ) -> Result<()> {
            self.inner.walk_leaves_skipping(root, skip, f)
        }
        fn walk_nodes(&self, root: &MapRoot, f: &mut dyn FnMut(&NodeRef)) -> Result<()> {
            self.inner.walk_nodes(root, f)
        }
        fn walk_nodes_skipping(
            &self,
            root: &MapRoot,
            skip: &HashSet<NodeHash>,
            f: &mut dyn FnMut(&NodeRef),
        ) -> Result<()> {
            self.inner.walk_nodes_skipping(root, skip, f)
        }
    }

    fn lying(
        drop_rehints: bool,
        extra_sets: u64,
    ) -> (Arc<dyn BlockMap>, RefMap, Arc<Mutex<SegRefs>>) {
        let real = flat(64, None);
        let liar: Arc<dyn BlockMap> = Arc::new(Lying {
            inner: Arc::clone(&real),
            drop_rehints,
            extra_sets,
        });
        let refs = Arc::new(Mutex::new(SegRefs::new(Duration::ZERO)));
        // The table has a base: it was rebased on an empty map.
        {
            let mut t = lock(&refs);
            t.mark_started(seq(0));
            t.rebase(&tip_refs(0, &[]));
            assert_eq!(t.state(), RefsState::Ready);
        }
        (real, RefMap::new(liar, Arc::clone(&refs)), refs)
    }

    /// The real map misses a re-hint the table counted as applied: the
    /// report differs at once (`Drifted`, nothing offered), and the next
    /// mark finds exactly the two segments the re-hint touched and repairs
    /// the table.
    #[test]
    fn a_dropped_rehint_is_found_at_the_commit_and_repaired_by_the_next_mark() {
        let (real, m, refs) = lying(true, 0);
        commit(&m, &[Op::Set(ChunkNo(1), hint(1, 0))]).expect("commit");
        assert_eq!(lock(&refs).stats().report_mismatches, 0);
        commit(&m, &[Op::Rehint(ChunkNo(1), hint(1, 0), hint(2, 0))]).expect("commit");
        let t = lock(&refs).stats();
        assert_eq!((t.report_mismatches, t.state), (1, RefsState::Drifted));
        assert_ne!(table(&refs), counted(&*real), "the table is wrong now");
        assert!(lock(&refs).ready(seq(99), 0, &HashSet::new(), 9).is_empty());
        // The next mark: the real counts.
        let truth: Vec<(SegId, u64)> = counted(&*real)
            .into_iter()
            .map(|(s, n)| (s, n as u64))
            .collect();
        lock(&refs).mark_started(seq(1));
        let diff = lock(&refs).rebase(&TipRefs {
            tip: seq(1),
            refs: truth,
        });
        assert_eq!(diff, 2, "exactly the two segments of the re-hint");
        assert_eq!(table(&refs), counted(&*real));
        assert_eq!(lock(&refs).state(), RefsState::Ready);
        assert_eq!(lock(&refs).stats().drift_segments, 2);
    }

    /// A wrong report (one `sets` too many) is a mismatch and offers nothing
    /// until a mark began after it.
    #[test]
    fn a_wrong_report_makes_the_table_drifted_and_silent() {
        let (_real, m, refs) = lying(false, 1);
        commit(&m, &[Op::Set(ChunkNo(1), hint(1, 0)), Op::Hole(ChunkNo(1))]).expect("commit");
        let t = lock(&refs).stats();
        assert_eq!((t.report_mismatches, t.state), (1, RefsState::Drifted));
        assert!(lock(&refs).ready(seq(99), 0, &HashSet::new(), 9).is_empty());
    }
}
