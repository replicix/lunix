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

pub(crate) struct Packer<'a> {
    segments: &'a dyn SegmentStore,
    id_len: IdLen,
    cur: Option<Current>,
    done: Vec<DestagedSegment>,
    /// Ids appended in this cut, for dedup against the cut's own extents (the
    /// index only learns them after the commit).
    appended: HashMap<ChunkId, Hint>,
    pub(crate) ops: Vec<MapOp>,
    pub(crate) stats: DestageStats,
}

impl<'a> Packer<'a> {
    pub(crate) fn new(segments: &'a dyn SegmentStore, id_len: IdLen) -> Self {
        Self {
            segments,
            id_len,
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

    /// Seals the segment being written, if any (its file is durable and named
    /// `segments/<id>` when this returns).
    pub(crate) fn seal_current(&mut self) -> Result<()> {
        let Some(cur) = self.cur.take() else {
            return Ok(());
        };
        let sealed = cur.writer.seal(Some(cur.writes))?;
        lunix_iface::crashpoint!("destage.after_segment_seal");
        self.done.push(DestagedSegment {
            seg: cur.seg,
            writes: cur.writes,
            entries: cur.entries,
            extent_bytes: cur.extent_bytes,
            sealed,
        });
        Ok(())
    }

    /// The map updates, consumed once the whole cut is packed.
    pub(crate) fn take_ops(&mut self) -> Vec<MapOp> {
        std::mem::take(&mut self.ops)
    }

    /// Ends the pack. A segment still being written is abandoned (the store
    /// removes the file); the sealed ones are returned.
    pub(crate) fn finish(self) -> Vec<DestagedSegment> {
        self.done
    }
}
