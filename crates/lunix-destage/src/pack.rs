//! Steps 7 and 8, in LBA order: append the sealed extents to segments, roll a
//! full segment, and collect the map updates.

use std::collections::HashMap;
use std::time::Instant;

use lunix_format::node::DataEntry;
use lunix_format::{ChunkId, Hint, IdLen, WriteSeq};
use lunix_iface::journal::WriteSeqRange;
use lunix_iface::map::ChunkNo;
use lunix_iface::store::{SegmentStore, SegmentWriter};
use lunix_iface::{Error, Result};

use crate::outcome::DestagedSegment;
use crate::stats::{codec_slot, ns, DestageStats};
use crate::worker::{entry_for, Action, ChunkResult, SealSlot, Sealed};

/// One map update.
pub(crate) enum MapOp {
    Set(ChunkNo, DataEntry),
    Hole(ChunkNo),
}

pub(crate) struct Packer<'a> {
    segments: &'a dyn SegmentStore,
    id_len: IdLen,
    writer: Option<Box<dyn SegmentWriter>>,
    cur: Option<DestagedSegment>,
    done: Vec<DestagedSegment>,
    /// Ids appended in this cut, for dedup against the cut's own extents (the
    /// index only learns them after the segment is sealed).
    appended: HashMap<ChunkId, Hint>,
    pub(crate) ops: Vec<MapOp>,
    pub(crate) stats: DestageStats,
}

impl<'a> Packer<'a> {
    pub(crate) fn new(segments: &'a dyn SegmentStore, id_len: IdLen) -> Self {
        Self {
            segments,
            id_len,
            writer: None,
            cur: None,
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
        if self.writer.is_none() {
            let w = self.segments.open_segment()?;
            self.cur = Some(DestagedSegment {
                seg: w.id(),
                writes: WriteSeqRange {
                    first: seq.0,
                    last: seq.1,
                },
                entries: Vec::new(),
                extent_bytes: 0,
                sealed: None,
            });
            self.writer = Some(w);
            self.stats.segments += 1;
        }
        let (Some(w), Some(cur)) = (self.writer.as_mut(), self.cur.as_mut()) else {
            return Err(Error::invalid("segment writer without a record"));
        };
        let hint = w.append_extent(id, None, &s.extent)?;
        lunix_iface::crashpoint!("destage.after_segment_append");
        cur.entries.push((*id, hint));
        cur.extent_bytes += s.extent.len() as u64;
        cur.writes.first = cur.writes.first.min(seq.0);
        cur.writes.last = cur.writes.last.max(seq.1);
        let slot = codec_slot(s.codec);
        self.stats.unique_stored += 1;
        self.stats.ulen_by_codec[slot] += u64::from(s.ulen);
        self.stats.clen_by_codec[slot] += u64::from(s.clen);
        self.stats.chunks_by_codec[slot] += 1;
        self.stats.extent_bytes += s.extent.len() as u64;
        // A full segment is sealed now so that the next extent opens a new one.
        if w.is_full() {
            self.seal_current()?;
        }
        self.stats.append_ns += ns(t.elapsed());
        Ok(hint)
    }

    fn seal_current(&mut self) -> Result<()> {
        let (Some(w), Some(mut cur)) = (self.writer.take(), self.cur.take()) else {
            return Ok(());
        };
        cur.sealed = Some(w.seal(Some(cur.writes))?);
        self.done.push(cur);
        Ok(())
    }

    /// The map updates, consumed once the whole cut is packed.
    pub(crate) fn take_ops(&mut self) -> Vec<MapOp> {
        std::mem::take(&mut self.ops)
    }

    /// Ends the pack: the segments so far and the still-open writer.
    pub(crate) fn finish(mut self) -> (Vec<DestagedSegment>, Option<Box<dyn SegmentWriter>>) {
        if let Some(cur) = self.cur.take() {
            self.done.push(cur);
        }
        (self.done, self.writer)
    }
}
