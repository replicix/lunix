//! Deterministic, random-access synthetic volume: leaf `idx` is a pure function
//! of `(seed, idx)`. It follows the leafenc workload `b-frag` (the worst case
//! for leaf size): chunks are first laid out sequentially in segments, then a
//! fraction `frag` is "overwritten" into unrelated segments so that almost every
//! overwritten entry is its own run. Reuses leafenc's `Rng`, `clen` model,
//! `idbytes` and segment geometry (`gen.rs` is a verbatim copy).
//!
//! Why a pure function: a 10 TiB volume has ~3.6 GB of encoded leaves, over the
//! 2 GiB disk budget, so the 10 TiB base leaves are *regenerated on read* by the
//! node source (still hash-verified like any other node) instead of stored.
use crate::gen::{clen, idbytes, ClenModel, Rng, HDR, SEG_BYTES};
use crate::leaf::Ent;

pub const SEGBASE: u64 = 1 << 40;

#[derive(Clone, Copy, Debug)]
pub struct Model {
    pub fan: usize,
    /// fraction of entries that are "overwritten" (own run, random segment)
    pub frag: f64,
    pub seed: u64,
    pub chunk: u32,
    /// segment-number span used for overwritten entries (roughly the number of
    /// segments the volume has)
    pub seg_span: u64,
}

impl Model {
    pub fn new(fan: usize, nchunks: u64, frag: f64) -> Self {
        let nleaves = nchunks.div_ceil(fan as u64);
        Model {
            fan,
            frag,
            seed: 0x5eed_0007,
            chunk: 65536,
            seg_span: (nleaves * 4).max(1024),
        }
    }

    /// Fill `out` with the `fan` entries of leaf `idx`.
    pub fn leaf(&self, idx: u64, out: &mut Vec<Ent>) {
        out.clear();
        let mut r = Rng(self.seed ^ idx.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        let mut seg = SEGBASE | (1 + idx * 4);
        let mut off = 0u64;
        let first = idx * self.fan as u64;
        for j in 0..self.fan as u64 {
            let c = clen(&mut r, self.chunk, ClenModel::Typical);
            let sz = (HDR + c) as u64;
            if off + sz > SEG_BYTES {
                seg += 1;
                off = 0;
            }
            let mut e = Ent {
                id: idbytes(first + j + 1),
                seg,
                off: off as u32,
                clen: c,
            };
            off += sz;
            if self.frag > 0.0 && r.f() < self.frag {
                let c2 = clen(&mut r, self.chunk, ClenModel::Typical);
                e.id = idbytes((1u64 << 62) | (first + j));
                e.seg = SEGBASE | (1 + r.below(self.seg_span));
                e.off = r.below(SEG_BYTES - (HDR + c2) as u64) as u32;
                e.clen = c2;
            }
            out.push(e);
        }
    }
}

/// Allocator for newly destaged chunks: contiguous within a segment, ids unique.
pub struct NewAlloc {
    seg: u64,
    off: u64,
    n: u64,
    r: Rng,
    chunk: u32,
}
impl NewAlloc {
    pub fn new(seed: u64, chunk: u32) -> Self {
        NewAlloc {
            seg: SEGBASE | (1 << 30),
            off: 0,
            n: 0,
            r: Rng(seed),
            chunk,
        }
    }
    pub fn put(&mut self) -> Ent {
        let c = clen(&mut self.r, self.chunk, ClenModel::Typical);
        let sz = (HDR + c) as u64;
        if self.off + sz > SEG_BYTES {
            self.seg += 1;
            self.off = 0;
        }
        self.n += 1;
        let e = Ent {
            id: idbytes((3u64 << 62) | self.n),
            seg: self.seg,
            off: self.off as u32,
            clen: c,
        };
        self.off += sz;
        e
    }
}
