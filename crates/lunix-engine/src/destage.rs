//! Step C3, destage of a cut: the seam to `lunix-destage` (M9) and a minimal
//! in-crate stand-in.
//!
//! `lunix-iface` has no destage trait and the engine depends on no
//! implementation crate, so the engine defines [`Destager`]; M12 wires M9's
//! pipeline behind it. [`SimpleDestager`] is the stand-in the engine is built
//! and tested with: single-threaded, raw codec only, but with the same
//! obligations: whole-chunk assembly with an R1 base read for partial chunks,
//! zero chunks as holes, dedup against the cut and the index (a `Hinted` hit
//! is confirmed first, D17), one map transaction applied atomically, index
//! inserts held back for the caller. Like M9 it seals full segments during
//! the cut and leaves the last one open for C4.

use std::collections::HashMap;
use std::time::Instant;

use lunix_format::node::DataEntry;
use lunix_format::{ChunkId, Codec, ExtentKind, Hint, IdLen, IdPrefix, Level, LineageIdx};
use lunix_iface::crypto::{EncodingDesc, ExtentCodec};
use lunix_iface::index::{ChunkIndex, Confirm};
use lunix_iface::journal::{Journal, WriteSeqRange};
use lunix_iface::map::{BlockMap, ChunkNo};
use lunix_iface::overlay::{OverlayCut, PieceSource};
use lunix_iface::space::SpaceGate;
use lunix_iface::store::{SealedSegment, SegmentStore, SegmentWriter};
use lunix_iface::{Error, Result};

use crate::read::ChunkReader;

const BLOCK: usize = 4096;

/// What a destage call may use.
pub struct DestageCtx<'a> {
    /// Blocking journal reads of the cut's records.
    pub journal: &'a dyn Journal,
    /// The map, updated in memory through one transaction.
    pub map: &'a dyn BlockMap,
    /// Dedup lookups (`get_for_dedup`); never inserted into here.
    pub index: &'a dyn ChunkIndex,
    /// Where new segments are opened.
    pub segments: &'a dyn SegmentStore,
    /// Ids and sealing.
    pub codec: &'a dyn ExtentCodec,
    /// Verified reads (R1) for partial-chunk bases and dedup confirmation.
    pub reader: &'a dyn ChunkReader,
    /// The space gate, for `ReadOnly` and `fatal_io` on a host EIO only: the
    /// ledger reservations of a cut are the coordinator's (C2, C4).
    pub space: &'a dyn SpaceGate,
    /// The volume chunk size.
    pub chunk_size: u32,
    /// The map id-prefix length.
    pub id_len: IdLen,
    /// The `write_seq` range of the cut, for the segment footers.
    pub writes: Option<WriteSeqRange>,
}

/// What a destage produced, for C4 and the counters.
#[derive(Default)]
pub struct DestageOutcome {
    /// Segments written and **not sealed** (C4 seals them, in this order).
    pub writers: Vec<Box<dyn SegmentWriter>>,
    /// Segments the pipeline already sealed during the cut (M9 seals full
    /// ones as it goes). Durable but unnamed until C6, like the rest.
    pub sealed: Vec<SealedSegment>,
    /// Index entries for the new extents, inserted by the caller once the
    /// commit naming their segments is durable.
    pub index_inserts: Vec<(ChunkId, Hint)>,
    /// Bytes of the chunks assembled.
    pub bytes_in: u64,
    /// Chunks in the cut.
    pub chunks_total: u64,
    /// Chunks that became holes (all zero, or covered by hole pieces).
    pub chunks_zero: u64,
    /// Chunks adopted by dedup.
    pub dedup_hits: u64,
    /// Sum of `ulen` of stored chunks.
    pub stored_ulen: u64,
    /// Sum of `clen` of stored chunks.
    pub stored_clen: u64,
    /// Partial chunks that read their base.
    pub cold_rmw: u64,
    /// Nanoseconds per stage: hash, dedup, compress, seal, append.
    pub stage_ns: [u64; 5],
}

/// The destage pipeline (M9).
pub trait Destager: Send + Sync {
    /// Destages `cut` (C3): every chunk of it ends up in the map, through one
    /// transaction committed before returning. On an error nothing was
    /// applied to the map; segments already written are abandoned (dropped)
    /// and become orphans of the open-time sweep at worst.
    ///
    /// # Errors
    /// `MapFull` when the cut needs more dirty leaves than the map allows;
    /// store, journal and codec errors.
    fn destage(&self, cut: &OverlayCut, ctx: &DestageCtx<'_>) -> Result<DestageOutcome>;
}

/// The in-crate stand-in for M9 (raw codec, one thread).
#[derive(Clone, Copy, Debug, Default)]
pub struct SimpleDestager;

fn nanos(t: Instant) -> u64 {
    u64::try_from(t.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

impl Destager for SimpleDestager {
    fn destage(&self, cut: &OverlayCut, ctx: &DestageCtx<'_>) -> Result<DestageOutcome> {
        let cs = ctx.chunk_size as usize;
        let chunk_blocks = (cs / BLOCK) as u64;
        let mut out = DestageOutcome::default();
        let mut txn = ctx.map.begin_update()?;
        let mut writer: Option<Box<dyn SegmentWriter>> = None;
        let mut in_cut: HashMap<ChunkId, Hint> = HashMap::new();
        let mut buf = vec![0u8; cs];
        let mut extent = Vec::with_capacity(cs + 256);
        for chunk in &cut.chunks {
            out.chunks_total += 1;
            let ci = ChunkNo(chunk.chunk);
            let base_lba = chunk.chunk * chunk_blocks;
            let covered: u64 = chunk.pieces.iter().map(|p| p.range.blocks).sum();
            let all_holes = chunk
                .pieces
                .iter()
                .all(|p| matches!(p.source, PieceSource::Hole));
            if covered == chunk_blocks && all_holes {
                txn.set_hole(ci)?;
                out.chunks_zero += 1;
                continue;
            }
            // Assemble: the base for a partial chunk (R1 read), then patch.
            if covered < chunk_blocks {
                match ctx.map.get(ci)? {
                    Some(e) => {
                        ctx.reader.read_entry(&e, &mut buf)?;
                        out.cold_rmw += 1;
                    }
                    None => buf.iter_mut().for_each(|b| *b = 0),
                }
            }
            for p in &chunk.pieces {
                let start = usize::try_from((p.range.lba - base_lba) * BLOCK as u64)
                    .map_err(|_| Error::invalid("piece offset"))?;
                let len = usize::try_from(p.range.blocks * BLOCK as u64)
                    .map_err(|_| Error::invalid("piece length"))?;
                let dst = buf
                    .get_mut(start..start + len)
                    .ok_or(Error::invalid("piece outside its chunk"))?;
                match p.source {
                    PieceSource::Journal(loc) => ctx.journal.read_at(loc, dst)?,
                    PieceSource::Hole => dst.fill(0),
                    PieceSource::None => {
                        return Err(Error::invalid("a cut piece without a source"));
                    }
                }
            }
            out.bytes_in += cs as u64;
            if buf.iter().all(|&b| b == 0) {
                txn.set_hole(ci)?;
                out.chunks_zero += 1;
                continue;
            }
            let t = Instant::now();
            let id = ctx.codec.chunk_id(ExtentKind::Data, &buf);
            out.stage_ns[0] += nanos(t);
            let t = Instant::now();
            let mut hit = in_cut.get(&id).copied();
            if hit.is_none() {
                if let Some(h) = ctx.index.get_for_dedup(&id)? {
                    let ok = match h.confirm {
                        Confirm::Verified => true,
                        Confirm::Hinted => ctx.reader.confirm(&h.hint, &id)?,
                    };
                    if ok {
                        hit = Some(h.hint);
                    }
                }
            }
            out.stage_ns[1] += nanos(t);
            let hint = match hit {
                Some(h) => {
                    out.dedup_hits += 1;
                    h
                }
                None => {
                    let t = Instant::now();
                    let desc = EncodingDesc {
                        codec: Codec::Raw,
                        level: Level(0),
                        ulen: ctx.chunk_size,
                        dict_id: None,
                    };
                    extent.clear();
                    ctx.codec
                        .seal(ExtentKind::Data, &id, &desc, &buf, &mut extent)?;
                    out.stage_ns[3] += nanos(t);
                    let t = Instant::now();
                    // A full segment is sealed now, as M9 does; the last one
                    // stays open for C4.
                    if writer.as_ref().is_some_and(|w| w.is_full()) {
                        if let Some(w) = writer.take() {
                            out.sealed.push(w.seal(ctx.writes)?);
                        }
                    }
                    if writer.is_none() {
                        writer = Some(ctx.segments.open_segment()?);
                    }
                    let h = match writer.as_mut() {
                        Some(w) => w.append_extent(&id, None, &extent)?,
                        None => return Err(Error::invalid("no open segment")),
                    };
                    out.stage_ns[4] += nanos(t);
                    out.stored_ulen += u64::from(ctx.chunk_size);
                    out.stored_clen += u64::from(h.clen);
                    out.index_inserts.push((id, h));
                    in_cut.insert(id, h);
                    h
                }
            };
            txn.set(
                ci,
                DataEntry {
                    lineage: LineageIdx(0),
                    seg: hint.seg,
                    off: hint.off,
                    clen: hint.clen,
                    prefix: IdPrefix::of(&id, ctx.id_len),
                },
            )?;
        }
        out.writers.extend(writer.take());
        txn.commit()?;
        Ok(out)
    }
}
