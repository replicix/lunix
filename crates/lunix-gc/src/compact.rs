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

use crate::footers::FooterCache;
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

/// Sorts candidates best first: higher score, then lower id.
pub fn rank(cands: &mut [Candidate]) {
    cands.sort_by(|a, b| b.score().total_cmp(&a.score()).then(a.seg.cmp(&b.seg)));
}

/// The compaction candidates of a mark: segments with live bytes, a live
/// fraction below `threshold`, an id at or below `high_water_mark`, and not
/// excluded by the caller (condemned, unnamed output, named by the tip FULL:
/// younger than one commit interval). Ranked.
pub fn candidates(
    l: &Liveness,
    threshold: f64,
    high_water_mark: u64,
    exclude: &dyn Fn(SegId) -> bool,
) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = l
        .segments
        .iter()
        .filter(|s| s.seg.raw() <= high_water_mark && !exclude(s.seg))
        .filter_map(|s| {
            let live = l.live_bytes(s.seg);
            let c = Candidate {
                seg: s.seg,
                size: s.size_bytes,
                live,
                age: high_water_mark.saturating_sub(s.seg.raw()),
            };
            (live > 0 && c.utilization() < threshold).then_some(c)
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
    writer: Option<Box<dyn SegmentWriter>>,
    reserved: u64,
    pending: Vec<(ChunkId, Hint, Hint)>,
    footers: FooterCache,
    pub(crate) started: Duration,
    pub(crate) bytes_read: u64,
    pub(crate) bytes_written: u64,
    raw: Vec<u8>,
    stored: Vec<u8>,
    sealed: Vec<u8>,
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
    /// Queues the live extents of `sources`, ordered by chunk number (LBA
    /// locality, map order).
    pub(crate) fn new(l: &Liveness, sources: Vec<Candidate>, started: Duration) -> Self {
        let mut queue: Vec<LiveExtent> = sources
            .iter()
            .flat_map(|c| l.live_in(c.seg).iter().copied())
            .collect();
        queue.sort_unstable_by_key(|e| (e.ci, e.hint.seg, e.hint.off));
        Self {
            queue: queue.into(),
            sources,
            writer: None,
            reserved: 0,
            pending: Vec::new(),
            footers: FooterCache::new(8),
            started,
            bytes_read: 0,
            bytes_written: 0,
            raw: Vec::new(),
            stored: Vec::new(),
            sealed: Vec::new(),
        }
    }

    pub(crate) fn is_done(&self) -> bool {
        self.queue.is_empty() && self.writer.is_none()
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
    }

    /// Copies one extent, or seals the output when it is full or the queue is
    /// empty. A seal adds the output to `outputs` (unnamed outputs, never
    /// condemned) and the moves to `outbox` one by one, so an error part-way
    /// loses none. Returns the output's id and size when one was sealed.
    pub(crate) fn unit(
        &mut self,
        view: &GcView<'_>,
        points: &Points,
        outbox: &mut Vec<Moved>,
        outputs: &mut BTreeSet<SegId>,
    ) -> Result<Option<(SegId, u64)>> {
        let full = self.writer.as_ref().is_some_and(|w| w.is_full());
        if full || (self.queue.is_empty() && self.writer.is_some()) {
            return self.seal(view, points, outbox, outputs).map(Some);
        }
        let Some(ext) = self.queue.pop_front() else {
            return Ok(None);
        };
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
        self.raw.clear();
        match view.segments.read_extent(&old, &mut self.raw) {
            Ok(()) => {}
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        }
        self.bytes_read += self.raw.len() as u64;
        self.stored.clear();
        let meta = view.extents.open(
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
        )?;
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
        let sealed = match writer.seal(None) {
            Ok(s) => s,
            Err(e) => {
                view.space.release(SpaceKind::Segments, reserved);
                self.pending.clear();
                return Err(e);
            }
        };
        outputs.insert(sealed.seg);
        point!(points, "gc.compact.after_segment_seal");
        view.space.transfer(
            SpaceKind::Segments,
            SpaceKind::Segments,
            reserved,
            sealed.size_bytes,
        )?;
        self.bytes_written += sealed.size_bytes;
        let mut any = false;
        for (id, old, new) in std::mem::take(&mut self.pending) {
            if view.index.relocate(&id, &old, &new)? {
                outbox.push(Moved { id, old, new });
                any = true;
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
