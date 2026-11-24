//! Compaction (plan M17 rule 4): rewrite the live extents of low-live
//! segments into new segments and hand the moves to the engine.
//!
//! Order of one output segment, the relocation rule of the plan:
//!
//! 1. each live extent is read, opened with its full id (R1) in its stored
//!    form, and **re-sealed** through `ExtentCodec::seal`, which draws a fresh
//!    salt (ADR-11); the chunk id is unchanged;
//! 2. the output is sealed (`gc.compact.after_segment_seal`) and the ledger
//!    trued up from the headroom it was charged against;
//! 3. only then is the index told (`relocate`, which keeps the entry in the
//!    memtable until a durable commit names the output, D3), and each move
//!    whose old copy was current becomes a [`Moved`] record
//!    (`gc.compact.after_relocate`); the engine applies them with
//!    `MapTxn::rehint` at its next cut and names the output there.
//!
//! One output segment is open at a time (bounded scratch).

use std::collections::{BTreeSet, VecDeque};
use std::time::Duration;

use lunix_format::{ChunkId, ExtentKind, Hint, SegId};
use lunix_iface::crypto::{OpenExpect, OpenOutput};
use lunix_iface::gc::{GcView, Moved};
use lunix_iface::space::SpaceKind;
use lunix_iface::store::SegmentWriter;
use lunix_iface::{ErrorKind, Result};

use crate::footers::{CacheBytes, FooterCache};
use crate::mark::{LiveExtent, Liveness};
use crate::points::{point, Points};

/// A segment compaction may rewrite.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Candidate {
    /// The segment.
    pub seg: SegId,
    /// Its file size.
    pub size: u64,
    /// Its live bytes.
    pub live: u64,
    /// Its age in object ids: the tip's high-water mark minus its id.
    pub age: u64,
}

impl Candidate {
    /// The live fraction `u = live / size`.
    pub fn utilization(&self) -> f64 {
        if self.size == 0 {
            return 1.0;
        }
        self.live as f64 / self.size as f64
    }

    /// LFS cost-benefit, `(1 - u) · age / (1 + u)`.
    pub fn score(&self) -> f64 {
        let u = self.utilization();
        (1.0 - u) * self.age as f64 / (1.0 + u)
    }
}

/// What an output costs per live extent beyond its payload (extent header and
/// footer entry, rounded up) and per segment (footer header, trailer).
pub const OUTPUT_COST_PER_EXTENT: u64 = 128;
/// See [`OUTPUT_COST_PER_EXTENT`].
pub const OUTPUT_COST_PER_SEGMENT: u64 = 512;

/// Bytes a compaction must free at least: what the source holds beyond what
/// its output would.
pub const MIN_GAIN: u64 = 1024;

/// Whether to compact a segment of `size` bytes holding `extents` live
/// extents of `live` payload bytes: its live fraction `live / size` is below
/// `threshold`, and an output for those extents (payload, per-extent and
/// per-segment overhead) is at least [`MIN_GAIN`] smaller than the source. A
/// small segment whose dead bytes are mostly container overhead is no
/// candidate: its output would be as large, and it would be rewritten at
/// every pass on an idle volume.
pub fn worth_compacting(size: u64, live: u64, extents: u64, threshold: f64) -> bool {
    if size == 0 || live == 0 {
        return false;
    }
    let out = live
        .saturating_add(extents.saturating_mul(OUTPUT_COST_PER_EXTENT))
        .saturating_add(OUTPUT_COST_PER_SEGMENT);
    (live as f64) < threshold * size as f64 && size >= out.saturating_add(MIN_GAIN)
}

/// Sorts candidates best first: higher score, then lower id.
pub fn rank(cands: &mut [Candidate]) {
    cands.sort_by(|a, b| b.score().total_cmp(&a.score()).then(a.seg.cmp(&b.seg)));
}

/// The compaction candidates of a mark: segments with live bytes, worth
/// compacting at `threshold` ([`worth_compacting`]: the live fraction counts
/// an output's overhead), an id at or below `high_water_mark`, and not
/// excluded by the caller (condemned, unnamed output, named by the tip FULL:
/// younger than one commit interval). Segments holding a live extent that no
/// confirmed index entry names are left out: `relocate` could not move it, so
/// the segment would be picked again every pass. Ranked.
pub fn candidates(
    l: &Liveness,
    threshold: f64,
    high_water_mark: u64,
    exclude: &dyn Fn(SegId) -> bool,
) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = l
        .segments
        .iter()
        .filter(|s| {
            s.seg.raw() <= high_water_mark && !exclude(s.seg) && !l.unindexed.contains(&s.seg)
        })
        .filter_map(|s| {
            let live = l.live_bytes(s.seg);
            let c = Candidate {
                seg: s.seg,
                size: s.size_bytes,
                live,
                age: high_water_mark.saturating_sub(s.seg.raw()),
            };
            let extents = l.live_in(s.seg).len() as u64;
            worth_compacting(c.size, live, extents, threshold).then_some(c)
        })
        .collect();
    rank(&mut out);
    out
}

/// Picks candidates in rank order until `budget` live bytes are taken.
pub(crate) fn pick(cands: &[Candidate], budget: u64) -> Vec<Candidate> {
    let mut taken = 0u64;
    let mut out = Vec::new();
    for c in cands {
        if taken >= budget {
            break;
        }
        taken = taken.saturating_add(c.live);
        out.push(*c);
    }
    out
}

/// A compaction in progress.
pub(crate) struct Compactor {
    queue: VecDeque<LiveExtent>,
    pub(crate) sources: Vec<Candidate>,
    /// The next source whose live extents are not queued yet.
    next_source: usize,
    /// Refill the queue to about this many extents (bounded scratch).
    queue_cap: usize,
    writer: Option<Box<dyn SegmentWriter>>,
    reserved: u64,
    pending: Vec<(ChunkId, Hint, Hint)>,
    footers: FooterCache,
    pub(crate) started: Duration,
    pub(crate) bytes_read: u64,
    /// The rate cap's window: the rate it counts under (`None` before the
    /// first check), when it began and the bytes read by then. A change of
    /// the rate (write pressure comes and goes) starts a new window, so
    /// bytes read while the rate was higher are not charged to the lower
    /// one afterwards.
    pub(crate) window: Option<(u64, Duration, u64)>,
    pub(crate) bytes_written: u64,
    raw: Vec<u8>,
    stored: Vec<u8>,
    sealed: Vec<u8>,
    /// Source segments with an extent that did not open (`Auth`, `Corrupt`,
    /// `Unsupported`): skipped, reported, and never picked again by this
    /// process.
    pub(crate) bad: Vec<SegId>,
}

impl std::fmt::Debug for Compactor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Compactor")
            .field("queue", &self.queue.len())
            .field("sources", &self.sources.len())
            .field("open", &self.writer.as_ref().map(|w| w.id()))
            .finish_non_exhaustive()
    }
}

impl Compactor {
    /// A compaction of `sources`. Their live extents are queued a group of
    /// sources at a time (about `queue_cap` extents, plus at most one
    /// segment's), each group ordered by chunk number (LBA locality, map
    /// order). `footer_cache` must hold the footers of all sources of one
    /// group (the queue interleaves their extents).
    pub(crate) fn new(
        sources: Vec<Candidate>,
        started: Duration,
        queue_cap: usize,
        footer_cache: CacheBytes,
    ) -> Self {
        Self {
            queue: VecDeque::new(),
            sources,
            next_source: 0,
            queue_cap: queue_cap.max(1),
            writer: None,
            reserved: 0,
            pending: Vec::new(),
            footers: FooterCache::new(footer_cache),
            started,
            bytes_read: 0,
            window: None,
            bytes_written: 0,
            raw: Vec::new(),
            stored: Vec::new(),
            sealed: Vec::new(),
            bad: Vec::new(),
        }
    }

    pub(crate) fn is_done(&self) -> bool {
        self.queue.is_empty() && self.writer.is_none() && self.next_source >= self.sources.len()
    }

    fn refill(&mut self, l: &Liveness) {
        while self.queue.len() < self.queue_cap {
            let Some(c) = self.sources.get(self.next_source) else {
                break;
            };
            self.queue.extend(l.live_in(c.seg).iter().copied());
            self.next_source += 1;
        }
        self.queue
            .make_contiguous()
            .sort_unstable_by_key(|e| (e.ci, e.hint.seg, e.hint.off));
    }

    pub(crate) fn heap_bytes(&self) -> usize {
        self.queue.capacity() * std::mem::size_of::<LiveExtent>()
            + self.pending.capacity() * std::mem::size_of::<(ChunkId, Hint, Hint)>()
            + self.footers.heap_bytes()
            + self.raw.capacity()
            + self.stored.capacity()
            + self.sealed.capacity()
    }

    /// Abandons the open output (its file is removed by the store or the
    /// open-time sweep) and returns the headroom charged for it.
    pub(crate) fn abandon(&mut self, view: &GcView<'_>) {
        if self.writer.take().is_some() && self.reserved > 0 {
            view.space.release(SpaceKind::Segments, self.reserved);
        }
        self.reserved = 0;
        self.pending.clear();
        self.queue.clear();
        self.next_source = self.sources.len();
    }

    /// Copies one extent, or seals the output when it is full or the queue is
    /// empty. A seal adds the output to `outputs` (unnamed outputs, never
    /// condemned) and the moves to `outbox` one by one, so an error part-way
    /// loses none. Returns the output's id and size when one was sealed.
    pub(crate) fn unit(
        &mut self,
        view: &GcView<'_>,
        l: &Liveness,
        points: &Points,
        outbox: &mut Vec<Moved>,
        outputs: &mut BTreeSet<SegId>,
    ) -> Result<Option<(SegId, u64)>> {
        let drained = self.queue.is_empty() && self.next_source >= self.sources.len();
        let full = self.writer.as_ref().is_some_and(|w| w.is_full());
        if full || (drained && self.writer.is_some()) {
            return self.seal(view, points, outbox, outputs).map(Some);
        }
        if self.queue.is_empty() {
            self.refill(l);
            return Ok(None);
        }
        let Some(ext) = self.queue.pop_front() else {
            return Ok(None);
        };
        if self.bad.contains(&ext.hint.seg) {
            return Ok(None);
        }
        self.copy(view, ext.hint)?;
        Ok(None)
    }

    fn copy(&mut self, view: &GcView<'_>, old: Hint) -> Result<()> {
        let Some(id) = self
            .footers
            .get(view.segments, old.seg)?
            .and_then(|f| f.id_at(&old))
        else {
            // Gone since the mark (or never there): nothing to move.
            return Ok(());
        };
        // Only the index's current copy is moved: anything else could not be
        // relocated, and the copy would be an orphan.
        if view.index.get(&id)?.is_none_or(|c| c.hint != old) {
            return Ok(());
        }
        self.raw.clear();
        match view.segments.read_extent(&old, &mut self.raw) {
            Ok(()) => {}
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        }
        self.bytes_read += self.raw.len() as u64;
        self.stored.clear();
        let opened = view.extents.open(
            ExtentKind::Data,
            &self.raw,
            &OpenExpect {
                chunk_size: view.params.chunk_size,
                chunk_id: Some(&id),
                id_prefix: None,
                verify_id: false,
                output: OpenOutput::Stored,
            },
            &mut self.stored,
        );
        let meta = match opened {
            Ok(m) => m,
            Err(e)
                if matches!(
                    e.kind(),
                    ErrorKind::Auth | ErrorKind::Corrupt | ErrorKind::Unsupported
                ) =>
            {
                tracing::warn!(error = %e, seg = ?old.seg, off = old.off, "gc compaction: an extent does not open; its segment is skipped");
                self.bad.push(old.seg);
                return Ok(());
            }
            Err(e) => return Err(e),
        };
        self.sealed.clear();
        view.extents.seal(
            ExtentKind::Data,
            &id,
            &meta.desc,
            &self.stored,
            &mut self.sealed,
        )?;
        let len = self.sealed.len() as u64;
        view.space.reserve_headroom(SpaceKind::Segments, len)?;
        self.reserved += len;
        let writer = match &mut self.writer {
            Some(w) => w,
            None => self.writer.insert(view.segments.open_segment()?),
        };
        let new = writer.append_extent(&id, meta.desc.dict_id, &self.sealed)?;
        self.pending.push((id, old, new));
        Ok(())
    }

    fn seal(
        &mut self,
        view: &GcView<'_>,
        points: &Points,
        outbox: &mut Vec<Moved>,
        outputs: &mut BTreeSet<SegId>,
    ) -> Result<(SegId, u64)> {
        let Some(writer) = self.writer.take() else {
            return Err(lunix_iface::Error::invalid("no compaction output is open"));
        };
        let reserved = std::mem::take(&mut self.reserved);
        let id = writer.id();
        let sealed = match writer.seal(None) {
            Ok(s) => s,
            Err(e) => {
                // The seal may have happened (an error after the rename): the
                // file is then an orphan with no relocation, charged at its
                // size; otherwise nothing exists and the headroom goes back.
                self.pending.clear();
                let size = view
                    .segments
                    .list()
                    .ok()
                    .and_then(|l| l.into_iter().find(|x| x.seg == id))
                    .map(|x| x.size_bytes);
                match size {
                    Some(n) => {
                        if view
                            .space
                            .transfer(SpaceKind::Segments, SpaceKind::Segments, reserved, n)
                            .is_err()
                        {
                            tracing::warn!(
                                "gc compaction: ledger true-up of a failed seal refused"
                            );
                        }
                    }
                    None => view.space.release(SpaceKind::Segments, reserved),
                }
                return Err(e);
            }
        };
        outputs.insert(sealed.seg);
        point!(points, "gc.compact.after_segment_seal");
        if let Err(e) = view.space.transfer(
            SpaceKind::Segments,
            SpaceKind::Segments,
            reserved,
            sealed.size_bytes,
        ) {
            tracing::warn!(error = %e, "gc compaction: ledger true-up of an output refused");
        }
        self.bytes_written += sealed.size_bytes;
        let mut any = false;
        for (id, old, new) in std::mem::take(&mut self.pending) {
            match view.index.relocate(&id, &old, &new) {
                Ok(true) => {
                    outbox.push(Moved { id, old, new });
                    any = true;
                }
                Ok(false) => {}
                Err(e) => {
                    if !any {
                        outputs.remove(&sealed.seg);
                    }
                    return Err(e);
                }
            }
        }
        if !any {
            // No index entry and no map entry will point into it: no cut will
            // name it, so it is an ordinary orphan, not a pending output.
            outputs.remove(&sealed.seg);
        }
        point!(points, "gc.compact.after_relocate");
        Ok((sealed.seg, sealed.size_bytes))
    }
}

/// Segments named by the tip FULL: younger than one commit interval.
pub(crate) fn tip_segments(view: &GcView<'_>) -> BTreeSet<SegId> {
    view.retained
        .iter()
        .filter_map(|c| match c {
            lunix_format::commit::Commit::Full(f) if f.common.commit_seq == view.tip.commit_seq => {
                Some(f.new_segments.iter().copied())
            }
            _ => None,
        })
        .flatten()
        .collect()
}

/// The compaction rate cap: bytes read in the current window may not run
/// ahead of `rate` by more than one step's worth (`step_max`). The window
/// is `(rate, start, bytes read at its start)`; the first check opens it at
/// the run's `started`, and a different `rate` (write pressure came or went)
/// opens a new one now, so bytes read while the rate was higher are not
/// charged to a lower one. Returns the window and whether the run is over
/// its cap.
pub(crate) fn rate_cap(
    window: Option<(u64, Duration, u64)>,
    rate: u64,
    now: Duration,
    started: Duration,
    bytes_read: u64,
    step_max: u64,
) -> ((u64, Duration, u64), bool) {
    let w = match window {
        Some(w) if w.0 == rate => w,
        Some(_) => (rate, now, bytes_read),
        None => (rate, started, 0),
    };
    let elapsed_ms = now.saturating_sub(w.1).as_millis();
    let allowed = u128::from(rate)
        .saturating_mul(elapsed_ms)
        .checked_div(1000)
        .unwrap_or(0)
        .saturating_add(u128::from(step_max));
    (w, u128::from(bytes_read.saturating_sub(w.2)) >= allowed)
}

#[cfg(test)]
mod rate_tests {
    use super::*;

    const S: Duration = Duration::from_secs(1);

    #[test]
    fn the_cap_counts_from_the_start_at_a_steady_rate() {
        let (w, over) = rate_cap(None, 1000, S, Duration::ZERO, 0, 100);
        assert!(!over);
        // 1 s at 1000 B/s plus a step of 100: 1100 allowed.
        assert!(!rate_cap(Some(w), 1000, S, Duration::ZERO, 1099, 100).1);
        assert!(rate_cap(Some(w), 1000, S, Duration::ZERO, 1100, 100).1);
        assert_eq!(w.1, Duration::ZERO, "the first window starts with the run");
    }

    #[test]
    fn bytes_read_at_a_higher_rate_are_not_charged_to_a_lower_one() {
        // Urgent pressure: an uncapped rate, a long read-ahead.
        let (w, over) = rate_cap(None, u64::MAX, S, Duration::ZERO, 50_000, 100);
        assert!(!over);
        // Pressure ends at 2 s: the rate is 1000 B/s again. The old
        // accounting would see 50 000 read against 2 100 allowed and sit at
        // the cap for 48 s; the new window starts at the change.
        let (w2, over) = rate_cap(Some(w), 1000, 2 * S, Duration::ZERO, 50_000, 100);
        assert!(!over, "the run stalled after the pressure ended");
        assert_eq!(w2, (1000, 2 * S, 50_000));
        // And the new rate holds from there.
        assert!(rate_cap(Some(w2), 1000, 3 * S, Duration::ZERO, 51_100, 100).1);
        assert!(!rate_cap(Some(w2), 1000, 3 * S, Duration::ZERO, 51_099, 100).1);
    }
}
