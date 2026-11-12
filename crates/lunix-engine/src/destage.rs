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
//! inserts held back for the caller. Like M9 (after its review) it seals
//! every segment before the map transaction commits, so no map entry ever
//! names an unsealed segment, and deletes its segments when the cut fails.

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
use lunix_iface::{Error, ErrorKind, Result};

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
    /// The journal's fill is M9's back-pressure input (degrade compression
    /// under pressure): an adapter derives it from `journal.fill()`.
    /// The volume chunk size.
    pub chunk_size: u32,
    /// The map id-prefix length.
    pub id_len: IdLen,
    /// The `write_seq` range of the cut, for the segment footers.
    pub writes: Option<WriteSeqRange>,
}

/// What a destage produced, for C4 and the counters.
#[derive(Clone, Debug, Default)]
pub struct DestageOutcome {
    /// Every segment the cut wrote, sealed (durable) before the map
    /// transaction committed; unnamed until C6.
    pub segments: Vec<SealedSegment>,
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
    /// Extents whose tag, AEAD or id failed in the pipeline's own reads
    /// (base reads, dedup confirmation). Reads through `DestageCtx::reader`
    /// are already counted by the engine and must not be added here.
    pub extent_auth_failures: u64,
}

/// What the coordinator does about a failed destage (1:1 with M9's
/// `ErrorClass`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Retry {
    /// Transient (`Io` that is not a host EIO, `Busy`): fail this commit
    /// attempt, the next trigger retries.
    Later,
    /// No space: retry once the space ladder freed room.
    AfterSpace,
    /// `MapFull` with dirty leaves already in the map: cut smaller.
    AfterMapSeal,
    /// The cut alone exceeds the map's dirty-leaf cap: cut smaller.
    CutTooLarge,
    /// Retrying cannot help (a damaged or forged extent, a malformed cut):
    /// the engine stops committing; writes fail, reads continue.
    Never,
    /// The volume is read-only or shutting down: stop.
    Stop,
}

impl Retry {
    /// The class of `e` (M9's table). `map_dirty` tells the two `MapFull`s
    /// apart.
    pub fn classify(e: &Error, map_dirty: bool) -> Self {
        if matches!(e, Error::Io(io) if matches!(io.raw_os_error(), Some(5 | 30))) {
            return Self::Stop;
        }
        match e.kind() {
            ErrorKind::NoSpace => Self::AfterSpace,
            ErrorKind::MapFull if map_dirty => Self::AfterMapSeal,
            ErrorKind::MapFull => Self::CutTooLarge,
            ErrorKind::Io | ErrorKind::Busy => Self::Later,
            ErrorKind::ReadOnly | ErrorKind::Closed => Self::Stop,
            _ => Self::Never,
        }
    }
}

/// A failed destage. Nothing was applied to the map and the segments of the
/// cut were deleted; the overlay still holds the data.
#[derive(Debug, thiserror::Error)]
#[error("destage failed ({retry:?}, chunk {chunk:?}): {error}")]
pub struct DestageFailure {
    /// Why.
    #[source]
    pub error: Error,
    /// What to do about it.
    pub retry: Retry,
    /// The chunk the failure happened at, when there is one.
    pub chunk: Option<u64>,
}

/// The destage pipeline (M9).
pub trait Destager: Send + Sync {
    /// Destages `cut` (C3, which also covers C4's sealing): every segment it
    /// writes is sealed before one map transaction, holding every chunk of
    /// the cut, commits. On an error nothing was applied to the map and the
    /// segments it wrote are deleted (a crash leaves them to the open-time
    /// sweep). Index inserts are returned, never applied.
    ///
    /// # Errors
    /// A [`DestageFailure`] with its [`Retry`] class and the chunk: `MapFull`
    /// classes make the caller cut smaller, `Later`/`AfterSpace` fail the
    /// commit attempt, `Never`/`Stop` stop the engine's commits.
    fn destage(
        &self,
        cut: &OverlayCut,
        ctx: &DestageCtx<'_>,
    ) -> std::result::Result<DestageOutcome, DestageFailure>;
}

/// The in-crate stand-in for M9 (raw codec, one thread).
#[derive(Clone, Copy, Debug, Default)]
pub struct SimpleDestager;

fn nanos(t: Instant) -> u64 {
    u64::try_from(t.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

impl Destager for SimpleDestager {
    fn destage(
        &self,
        cut: &OverlayCut,
        ctx: &DestageCtx<'_>,
    ) -> std::result::Result<DestageOutcome, DestageFailure> {
        let mut out = DestageOutcome::default();
        let mut chunk = None;
        let map_dirty = ctx.map.dirty_leaf_count() > 0;
        let res = if ctx.space.is_read_only() {
            Err(Error::ReadOnly)
        } else {
            destage_into(cut, ctx, &mut out, &mut chunk)
        };
        match res {
            Ok(()) => Ok(out),
            Err(error) => {
                for s in &out.segments {
                    if let Err(d) = ctx.segments.delete(s.seg) {
                        tracing::warn!(error = %d, seg = %s.seg, "segment of a failed cut not deleted; the sweep or GC removes it");
                    }
                }
                let retry = Retry::classify(&error, map_dirty);
                // A MapFull from the final transaction is about the cut, not a chunk.
                let chunk = if error.kind() == ErrorKind::MapFull {
                    None
                } else {
                    chunk
                };
                Err(DestageFailure {
                    error,
                    retry,
                    chunk,
                })
            }
        }
    }
}

fn destage_into(
    cut: &OverlayCut,
    ctx: &DestageCtx<'_>,
    out: &mut DestageOutcome,
    current: &mut Option<u64>,
) -> Result<()> {
    {
        let cs = ctx.chunk_size as usize;
        let chunk_blocks = (cs / BLOCK) as u64;
        let mut txn = ctx.map.begin_update()?;
        let mut writer: Option<Box<dyn SegmentWriter>> = None;
        let mut in_cut: HashMap<ChunkId, Hint> = HashMap::new();
        let mut buf = vec![0u8; cs];
        let mut extent = Vec::with_capacity(cs + 256);
        for chunk in &cut.chunks {
            *current = Some(chunk.chunk);
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
                    // A full segment is sealed now; the last one before
                    // the map transaction commits.
                    if writer.as_ref().is_some_and(|w| w.is_full()) {
                        if let Some(w) = writer.take() {
                            out.segments.push(w.seal(ctx.writes)?);
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
        if let Some(w) = writer.take() {
            out.segments.push(w.seal(ctx.writes)?);
        }
        txn.commit()?;
        Ok(())
    }
}
