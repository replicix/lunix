//! Steps 7 and 8, in LBA order: append the sealed extents to segments, seal a
//! segment when it is full and the last one at the end of the cut, and collect
//! the map updates.

use std::collections::HashMap;
use std::time::Instant;

use lunix_format::node::DataEntry;
use lunix_format::{ChunkId, Hint, IdLen, SegId, WriteSeq};
use lunix_iface::journal::WriteSeqRange;
use lunix_iface::map::ChunkNo;
use lunix_iface::store::{SegmentStore, SegmentWriter};
use lunix_iface::{Error, Result};

use crate::outcome::DestagedSegment;
use crate::seal::{Sealer, Slot};
use crate::stats::{codec_slot, ns, DestageStats};
use crate::worker::{entry_for, Action, ChunkResult, SealSlot, Sealed};

/// One map update.
pub(crate) enum MapOp {
    Set(ChunkNo, DataEntry),
    Hole(ChunkNo),
}

/// The segment being appended to.
struct Current {
    writer: Box<dyn SegmentWriter>,
    seg: SegId,
    writes: WriteSeqRange,
    entries: Vec<(ChunkId, Hint)>,
    extent_bytes: u64,
}

/// A broken invariant of the seal collection: a bug, which a debug build
/// refuses loudly and a release build turns into a failed cut (the one place
/// the durability rule depends on).
fn invariant(msg: &'static str) -> Error {
    if cfg!(debug_assertions) {
        panic!("{msg}");
    }
    Error::invalid(msg)
}

/// A segment handed to the sealer, kept until its seal is collected.
struct Pending {
    seg: SegId,
    writes: WriteSeqRange,
    entries: Vec<(ChunkId, Hint)>,
    extent_bytes: u64,
}

pub(crate) struct Packer<'a> {
    segments: &'a dyn SegmentStore,
    id_len: IdLen,
    cur: Option<Current>,
    sealer: Sealer,
    pending: Vec<Pending>,
    /// A seal failure `submit` handed to the caller.
    error_reported: bool,
    /// Seal errors after the first.
    more_errors: Vec<String>,
    done: Vec<DestagedSegment>,
    /// Ids appended in this cut, for dedup against the cut's own extents (the
    /// index only learns them after the commit).
    appended: HashMap<ChunkId, Hint>,
    pub(crate) ops: Vec<MapOp>,
    pub(crate) stats: DestageStats,
}

impl<'a> Packer<'a> {
    pub(crate) fn new(
        segments: &'a dyn SegmentStore,
        id_len: IdLen,
        max_seals_in_flight: usize,
    ) -> Self {
        Self {
            segments,
            id_len,
            cur: None,
            sealer: Sealer::new(max_seals_in_flight),
            pending: Vec::new(),
            error_reported: false,
            more_errors: Vec::new(),
            done: Vec::new(),
            appended: HashMap::new(),
            ops: Vec::new(),
            stats: DestageStats::default(),
        }
    }

    /// Takes one finished chunk. `take_slot` fetches a sealed extent from its
    /// slot, waiting for the producer.
    pub(crate) fn push(
        &mut self,
        r: ChunkResult,
        take_slot: &dyn Fn(&SealSlot) -> Result<Sealed>,
    ) -> Result<()> {
        let ci = ChunkNo(r.chunk);
        match r.action {
            Action::Hole => self.ops.push(MapOp::Hole(ci)),
            Action::Adopt { id, hint } => {
                self.ops
                    .push(MapOp::Set(ci, entry_for(&id, &hint, self.id_len)));
            }
            Action::Store { id, slot } => {
                let hint = if let Some(h) = self.appended.get(&id) {
                    self.stats.dedup_in_cut += 1;
                    *h
                } else {
                    let sealed = take_slot(&slot)?;
                    let h = self.append(&id, sealed, r.seq)?;
                    self.appended.insert(id, h);
                    h
                };
                self.ops
                    .push(MapOp::Set(ci, entry_for(&id, &hint, self.id_len)));
            }
        }
        Ok(())
    }

    fn append(&mut self, id: &ChunkId, s: Sealed, seq: (WriteSeq, WriteSeq)) -> Result<Hint> {
        let t = Instant::now();
        if self.cur.is_none() {
            let writer = self.segments.open_segment()?;
            self.cur = Some(Current {
                seg: writer.id(),
                writer,
                writes: WriteSeqRange {
                    first: seq.0,
                    last: seq.1,
                },
                entries: Vec::new(),
                extent_bytes: 0,
            });
            self.stats.segments += 1;
        }
        let Some(cur) = self.cur.as_mut() else {
            return Err(Error::invalid("segment writer missing"));
        };
        let hint = cur.writer.append_extent(id, None, &s.extent)?;
        lunix_iface::crashpoint!("destage.after_segment_append");
        cur.entries.push((*id, hint));
        cur.extent_bytes += s.extent.len() as u64;
        cur.writes.first = cur.writes.first.min(seq.0);
        cur.writes.last = cur.writes.last.max(seq.1);
        let full = cur.writer.is_full();
        let slot = codec_slot(s.codec);
        self.stats.unique_stored += 1;
        self.stats.ulen_by_codec[slot] += u64::from(s.ulen);
        self.stats.clen_by_codec[slot] += u64::from(s.clen);
        self.stats.chunks_by_codec[slot] += 1;
        self.stats.extent_bytes += s.extent.len() as u64;
        // A full segment is sealed now so that the next extent opens a new one.
        if full {
            self.seal_current()?;
        }
        self.stats.append_ns += ns(t.elapsed());
        Ok(hint)
    }

    /// Hands the segment being written, if any, to the sealer: it runs
    /// beside the packing of the next segment. Its file is durable and named
    /// `segments/<id>` only once [`wait_sealed`](Self::wait_sealed) returned.
    ///
    /// # Errors
    /// The error of an earlier seal that failed.
    pub(crate) fn seal_current(&mut self) -> Result<()> {
        let Some(cur) = self.cur.take() else {
            return Ok(());
        };
        if let Err(e) = self.sealer.submit(cur.writer, cur.writes) {
            self.error_reported = true;
            return Err(e);
        }
        self.pending.push(Pending {
            seg: cur.seg,
            writes: cur.writes,
            entries: cur.entries,
            extent_bytes: cur.extent_bytes,
        });
        lunix_iface::crashpoint!("destage.seal_submitted");
        Ok(())
    }

    /// Waits for every seal handed out and collects the ones that succeeded
    /// (in segment order) into the sealed list, so a failed cut deletes them.
    /// A segment whose seal failed is deleted at once, best effort: the seal
    /// may have failed after the rename (the directory `fsync`), and nothing
    /// durable names it.
    ///
    /// # Errors
    /// The first seal error not reported before; later ones are kept for
    /// [`finish`](Self::finish). `Invalid` if a seal's outcome is missing,
    /// which would otherwise drop a segment the map names.
    pub(crate) fn wait_sealed(&mut self) -> Result<()> {
        let t = Instant::now();
        let slots = self.sealer.wait_all();
        self.stats.seal_wait_ns += ns(t.elapsed());
        let f = self.sealer.figures();
        self.stats.seals = f.seals;
        self.stats.seal_in_flight_max = f.in_flight_max;
        self.stats.seal_busy_ns = f.busy_ns;
        self.stats.seal_stall_ns = f.stall_ns;
        let mut first: Option<Error> = None;
        let mut note = |e: Error, more: &mut Vec<String>| {
            if first.is_none() {
                first = Some(e);
            } else {
                more.push(e.to_string());
            }
        };
        let pending = std::mem::take(&mut self.pending);
        if slots.len() != pending.len() {
            note(
                invariant("seal outcomes and segments differ in number"),
                &mut self.more_errors,
            );
        }
        for (p, slot) in pending.into_iter().zip(slots) {
            match slot {
                Slot::Done(sealed) => self.done.push(DestagedSegment {
                    seg: p.seg,
                    writes: p.writes,
                    entries: p.entries,
                    extent_bytes: p.extent_bytes,
                    sealed,
                }),
                Slot::Failed(e) => {
                    let _ = self.segments.delete(p.seg);
                    note(e, &mut self.more_errors);
                }
                Slot::Taken => {
                    // Its error was handed to the caller by `submit`.
                    let _ = self.segments.delete(p.seg);
                    if !self.error_reported {
                        note(invariant("a seal failure was lost"), &mut self.more_errors);
                    }
                }
                Slot::Pending => {
                    note(
                        invariant("a seal is pending after the wait"),
                        &mut self.more_errors,
                    );
                }
            }
        }
        match first {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// The map updates, consumed once the whole cut is packed.
    pub(crate) fn take_ops(&mut self) -> Vec<MapOp> {
        std::mem::take(&mut self.ops)
    }

    /// Ends the pack: waits for the seals still running (so that every file
    /// the cut sealed is in the list), and returns the sealed segments, the
    /// counters and the seal errors after the first. A segment still being
    /// written is abandoned (the store removes the file).
    pub(crate) fn finish(mut self) -> Finished {
        let _ = self.wait_sealed();
        Finished {
            segments: std::mem::take(&mut self.done),
            stats: std::mem::take(&mut self.stats),
            more_errors: std::mem::take(&mut self.more_errors),
        }
    }
}

/// What [`Packer::finish`] hands back.
pub(crate) struct Finished {
    pub(crate) segments: Vec<DestagedSegment>,
    pub(crate) stats: DestageStats,
    pub(crate) more_errors: Vec<String>,
}

impl Drop for Packer<'_> {
    /// A packer dropped without `finish` (a panic unwinding out of the cut):
    /// no seal thread outlives it, and what it sealed, which nothing names, is
    /// deleted (best effort; the open-time sweep removes the rest).
    fn drop(&mut self) {
        let _ = self.wait_sealed();
        for s in self.done.drain(..) {
            let _ = self.segments.delete(s.seg);
        }
    }
}
