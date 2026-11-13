//! Step 1: build the chunk from the overlay's pieces, reading the base chunk
//! when they cover it only partly.

use lunix_format::WriteSeq;
use lunix_iface::map::ChunkNo;
use lunix_iface::overlay::{CutChunk, PieceSource};
use lunix_iface::{Error, Result};

use crate::config::BLOCK;
use crate::deps::DestageDeps;
use crate::read::read_base;
use crate::stats::DestageStats;

/// What assembling produced.
pub(crate) enum Assembled {
    /// The hole pieces cover the whole chunk: a map hole, nothing was read.
    Hole,
    /// `buf` holds the chunk (`chunk_size` bytes).
    Data,
}

/// The facts about a cut chunk's pieces that the later stages need.
pub(crate) struct PieceFacts {
    /// Blocks the pieces cover (excluding `PieceSource::None`).
    pub covered: u64,
    /// True when every covering piece is a hole.
    pub all_holes: bool,
    /// Lowest and highest `write_seq` among the pieces.
    pub seq: (WriteSeq, WriteSeq),
}

/// Validates the pieces against their chunk (a violation is a bug in the
/// overlay, not a data error) and summarises them.
pub(crate) fn check_pieces(
    cc: &CutChunk,
    chunk_blocks: u64,
    cut_seq: WriteSeq,
) -> Result<PieceFacts> {
    let bad = |why: &'static str| Error::Invalid { why };
    let base = cc
        .chunk
        .checked_mul(chunk_blocks)
        .ok_or_else(|| bad("chunk number overflows"))?;
    let end = base
        .checked_add(chunk_blocks)
        .ok_or_else(|| bad("chunk number overflows"))?;
    let mut next = base;
    let mut covered = 0u64;
    let mut all_holes = true;
    let mut lo = WriteSeq(u64::MAX);
    let mut hi = WriteSeq(0);
    for p in &cc.pieces {
        let pend = p.range.end().ok_or_else(|| bad("piece range overflows"))?;
        if p.range.blocks == 0 || p.range.lba < next || pend > end {
            return Err(bad("cut piece outside its chunk, empty or out of order"));
        }
        next = pend;
        match p.source {
            PieceSource::None => continue,
            PieceSource::Hole => {}
            PieceSource::Journal(_) => all_holes = false,
        }
        // The cut holds exactly the records up to `cut_seq`; one above it
        // would put a later write into a commit that does not own it.
        if p.write_seq == WriteSeq(0) || p.write_seq > cut_seq {
            return Err(bad("piece write_seq outside 1 ..= cut_seq"));
        }
        covered += p.range.blocks;
        lo = lo.min(p.write_seq);
        hi = hi.max(p.write_seq);
    }
    if covered == 0 {
        return Err(bad("cut chunk without data pieces"));
    }
    Ok(PieceFacts {
        covered,
        all_holes,
        seq: (lo, hi),
    })
}

/// Assembles the chunk into `buf` (resized to `chunk_size`). `raw` and
/// `scratch` are reusable buffers for the base read.
pub(crate) fn assemble(
    deps: &DestageDeps<'_>,
    cc: &CutChunk,
    facts: &PieceFacts,
    buf: &mut Vec<u8>,
    raw: &mut Vec<u8>,
    stats: &mut DestageStats,
) -> Result<Assembled> {
    let chunk_blocks = deps.params.chunk_blocks();
    let full = facts.covered == chunk_blocks;
    if full && facts.all_holes {
        return Ok(Assembled::Hole);
    }
    let size = deps.params.chunk_size as usize;
    if full {
        buf.clear();
        buf.resize(size, 0);
    } else {
        match deps.map.get(ChunkNo(cc.chunk))? {
            Some(entry) => {
                read_base(deps, &entry, raw, buf, stats)?;
                stats.cold_rmw += 1;
            }
            None => {
                buf.clear();
                buf.resize(size, 0);
            }
        }
    }
    let base = cc.chunk * chunk_blocks;
    for p in &cc.pieces {
        let off =
            usize::try_from((p.range.lba - base) * BLOCK).map_err(|_| Error::invalid("offset"))?;
        let len = usize::try_from(p.range.blocks * BLOCK).map_err(|_| Error::invalid("length"))?;
        let Some(dst) = buf.get_mut(off..off + len) else {
            return Err(Error::invalid("piece outside the chunk buffer"));
        };
        match p.source {
            PieceSource::None => {}
            PieceSource::Hole => dst.fill(0),
            PieceSource::Journal(loc) => deps.journal.read_at(loc, dst)?,
        }
    }
    Ok(Assembled::Data)
}
