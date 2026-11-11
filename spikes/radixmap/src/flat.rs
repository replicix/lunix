//! Flat-table baseline (ADR-8's rejected alternative): one 48-byte row per
//! chunk, stored in fixed-size blocks, LZ4-compressed per block, BLAKE3 per
//! block, root = hash of the block-hash list ("manifest").
//!   (a) full seal  : compress + hash every block.
//!   (b) block-delta: only blocks containing edits are recompressed/rehashed;
//!                    the manifest (32 B per block) is rehashed and rewritten.
//! Blocks are regenerated from the synthetic model on demand (the 10 TiB table
//! is 8 GB); regeneration time is reported separately and excluded from seal
//! time, because a real system would read the block from its local copy.
use crate::leaf::Ent;
use crate::model::Model;
use std::time::{Duration, Instant};

pub const ROW: usize = 48;

pub struct Flat {
    pub model: Model,
    pub nchunks: u64,
    pub rpb: u64, // rows per block
    pub nblocks: u64,
    pub manifest: Vec<[u8; 32]>,
    pub root: [u8; 32],
}
#[derive(Default, Debug, Clone)]
pub struct FlatStats {
    pub blocks: u64,
    pub raw: u64,
    pub stored: u64,
    pub manifest_bytes: u64,
    pub t_total: Duration,
    pub t_comp: Duration,
    pub t_hash: Duration,
    pub t_manifest: Duration,
    pub t_gen: Duration,
}

fn put_row(e: &Ent, o: &mut [u8]) {
    o.fill(0);
    if e.hole() {
        return;
    }
    o[..32].copy_from_slice(&e.id);
    o[32..40].copy_from_slice(&e.seg.to_le_bytes());
    o[40..44].copy_from_slice(&e.off.to_le_bytes());
    o[44..48].copy_from_slice(&e.clen.to_le_bytes());
}
fn manifest_root(m: &[[u8; 32]]) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    for x in m {
        h.update(x);
    }
    *h.finalize().as_bytes()
}

impl Flat {
    pub fn new(model: Model, nchunks: u64, block_bytes: usize) -> Flat {
        let rpb = (block_bytes / ROW) as u64;
        Flat {
            model,
            nchunks,
            rpb,
            nblocks: nchunks.div_ceil(rpb),
            manifest: Vec::new(),
            root: [0; 32],
        }
    }
    /// Regenerate block `b` (rows from the model; `populated` = number of
    /// populated leading chunks, rest are holes).
    pub fn gen_block(
        &self,
        b: u64,
        populated: u64,
        buf: &mut Vec<u8>,
        scratch: &mut Vec<Ent>,
        last: &mut u64,
    ) {
        let fan = self.model.fan as u64;
        let c0 = b * self.rpb;
        let c1 = ((b + 1) * self.rpb).min(self.nchunks);
        buf.clear();
        buf.resize((c1 - c0) as usize * ROW, 0);
        let mut c = c0;
        while c < c1 {
            let li = c / fan;
            if c >= populated {
                break;
            }
            if *last != li {
                self.model.leaf(li, scratch);
                *last = li;
            }
            let upto = ((li + 1) * fan).min(c1).min(populated);
            while c < upto {
                put_row(
                    &scratch[(c % fan) as usize],
                    &mut buf[(c - c0) as usize * ROW..(c - c0 + 1) as usize * ROW],
                );
                c += 1;
            }
        }
    }
    pub fn full_seal(&mut self, populated: u64) -> FlatStats {
        let mut st = FlatStats::default();
        let t0 = Instant::now();
        let (mut buf, mut sc, mut last) = (Vec::new(), Vec::new(), u64::MAX);
        self.manifest.clear();
        for b in 0..self.nblocks {
            let t = Instant::now();
            self.gen_block(b, populated, &mut buf, &mut sc, &mut last);
            st.t_gen += t.elapsed();
            let t = Instant::now();
            let c = lz4_flex::block::compress_prepend_size(&buf);
            st.t_comp += t.elapsed();
            let t = Instant::now();
            self.manifest.push(*blake3::hash(&buf).as_bytes());
            st.t_hash += t.elapsed();
            st.raw += buf.len() as u64;
            st.stored += c.len() as u64;
            st.blocks += 1;
        }
        let t = Instant::now();
        self.root = manifest_root(&self.manifest);
        st.t_manifest = t.elapsed();
        st.manifest_bytes = self.nblocks * 32;
        st.t_total = t0.elapsed() - st.t_gen;
        st
    }
    /// Block-delta seal. `edits` must be sorted by chunk index. Returns the new
    /// manifest/root (self is updated) and stats; also the list of changed
    /// block ids for the diff comparison.
    pub fn delta_seal(
        &mut self,
        populated: u64,
        edits: &mut dyn Iterator<Item = (u64, Ent)>,
    ) -> (FlatStats, Vec<u64>) {
        let mut st = FlatStats::default();
        let t0 = Instant::now();
        let (mut buf, mut sc, mut last) = (Vec::new(), Vec::new(), u64::MAX);
        let mut cur: Option<u64> = None;
        let mut changed = Vec::new();
        let mut pending: Vec<(u64, Ent)> = Vec::new();
        let flush = |b: u64,
                     pend: &mut Vec<(u64, Ent)>,
                     me: &mut Flat,
                     st: &mut FlatStats,
                     buf: &mut Vec<u8>,
                     sc: &mut Vec<Ent>,
                     last: &mut u64,
                     changed: &mut Vec<u64>| {
            let t = Instant::now();
            me.gen_block(b, populated, buf, sc, last);
            st.t_gen += t.elapsed();
            for (ci, e) in pend.drain(..) {
                let r = (ci - b * me.rpb) as usize * ROW;
                put_row(&e, &mut buf[r..r + ROW]);
            }
            let t = Instant::now();
            let c = lz4_flex::block::compress_prepend_size(buf);
            st.t_comp += t.elapsed();
            let t = Instant::now();
            me.manifest[b as usize] = *blake3::hash(buf).as_bytes();
            st.t_hash += t.elapsed();
            st.raw += buf.len() as u64;
            st.stored += c.len() as u64;
            st.blocks += 1;
            changed.push(b);
        };
        for (ci, e) in edits {
            let b = ci / self.rpb;
            if cur != Some(b) {
                if let Some(pb) = cur {
                    flush(
                        pb,
                        &mut pending,
                        self,
                        &mut st,
                        &mut buf,
                        &mut sc,
                        &mut last,
                        &mut changed,
                    );
                }
                cur = Some(b);
            }
            pending.push((ci, e));
        }
        if let Some(pb) = cur {
            flush(
                pb,
                &mut pending,
                self,
                &mut st,
                &mut buf,
                &mut sc,
                &mut last,
                &mut changed,
            );
        }
        let t = Instant::now();
        self.root = manifest_root(&self.manifest);
        st.t_manifest = t.elapsed();
        st.manifest_bytes = self.nblocks * 32;
        st.t_total = t0.elapsed() - st.t_gen;
        (st, changed)
    }
    /// Diff against an older manifest: compare the lists, then (as a flat
    /// table must) read both versions of each changed block, decompress and
    /// compare rows. `old_blocks`/`new_blocks` regenerate the rows. Returns
    /// (time of manifest compare, time of block comparison, blocks read, rows differing).
    pub fn diff(
        &self,
        old: &[[u8; 32]],
        changed_hint: &[u64],
        populated: u64,
        patched: &dyn Fn(u64, &mut Vec<u8>),
    ) -> (Duration, Duration, u64, u64) {
        let t = Instant::now();
        let ch: Vec<u64> = (0..self.nblocks)
            .filter(|&b| old[b as usize] != self.manifest[b as usize])
            .collect();
        let t_man = t.elapsed();
        debug_assert_eq!(ch.len(), changed_hint.len());
        let (mut a, mut sc, mut last) = (Vec::new(), Vec::new(), u64::MAX);
        let mut b = Vec::new();
        let mut t_blk = Duration::ZERO;
        let mut rows = 0u64;
        for &blk in &ch {
            self.gen_block(blk, populated, &mut a, &mut sc, &mut last);
            patched(blk, &mut b);
            let (ca, cb) = (
                lz4_flex::block::compress_prepend_size(&a),
                lz4_flex::block::compress_prepend_size(&b),
            );
            let t = Instant::now();
            let da = lz4_flex::block::decompress_size_prepended(&ca).unwrap();
            let db = lz4_flex::block::decompress_size_prepended(&cb).unwrap();
            rows += da
                .chunks_exact(ROW)
                .zip(db.chunks_exact(ROW))
                .filter(|(x, y)| x != y)
                .count() as u64;
            t_blk += t.elapsed();
        }
        (t_man, t_blk, ch.len() as u64, rows)
    }
}
