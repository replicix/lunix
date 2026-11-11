//! Synthetic map generation: destage model + workloads.
pub const FAN: usize = 512;
pub const HDR: u32 = 64; // extent header + AEAD tag overhead modelled inside a segment
pub const SEG_BYTES: u64 = 16 << 20;

#[derive(Clone, Copy, Default, PartialEq, Debug)]
pub struct E {
    pub idseed: u64, // stands for the 32-byte chunk_id (expanded by idbytes)
    pub seg: u64,    // 0 = hole
    pub off: u32,
    pub clen: u32,
    pub slot: u16,
}
impl E {
    #[inline]
    pub fn hole(&self) -> bool {
        self.seg == 0
    }
}

pub fn splitmix(x: &mut u64) -> u64 {
    *x = x.wrapping_add(0x9E3779B97F4A7C15);
    let mut z = *x;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
    z ^ (z >> 31)
}
/// Stand-in for a BLAKE3 digest: 32 incompressible, deterministic bytes.
pub fn idbytes(seed: u64) -> [u8; 32] {
    let mut s = seed ^ 0xA5A5_5A5A_1234_5678;
    let mut o = [0u8; 32];
    for k in 0..4 {
        o[k * 8..k * 8 + 8].copy_from_slice(&splitmix(&mut s).to_le_bytes());
    }
    o
}

pub struct Rng(pub u64);
impl Rng {
    pub fn u(&mut self) -> u64 {
        splitmix(&mut self.0)
    }
    pub fn f(&mut self) -> f64 {
        (self.u() >> 11) as f64 / (1u64 << 53) as f64
    }
    pub fn below(&mut self, n: u64) -> u64 {
        ((self.u() as u128 * n as u128) >> 64) as u64
    }
    pub fn nrm(&mut self) -> f64 {
        let mut s = 0.0;
        for _ in 0..12 {
            s += self.f();
        }
        s - 6.0
    }
    pub fn geo(&mut self, mean: f64) -> u64 {
        let p = 1.0 / mean;
        let u = self.f().max(1e-12);
        1 + (u.ln() / (1.0 - p).ln()) as u64
    }
}

#[derive(Clone, Copy, PartialEq)]
pub enum ClenModel {
    Typical,
    Mixed,
}
pub fn clen(r: &mut Rng, chunk: u32, m: ClenModel) -> u32 {
    match m {
        ClenModel::Typical => {
            if r.f() < 0.08 {
                chunk
            } else {
                let f = (r.nrm() * 0.15 + 0.45).clamp(0.06, 1.0);
                ((f * chunk as f64) as u32).clamp(256, chunk)
            }
        }
        ClenModel::Mixed => {
            if r.f() < 0.25 {
                chunk
            } else {
                let lo = chunk / 16;
                lo + r.below((chunk - lo) as u64) as u32
            }
        }
    }
}

pub struct Alloc {
    pub seg: u64,
    off: u64,
    slot: u16,
    next_id: u64,
    pub footers: Vec<Vec<(u32, u32)>>, // index = counter-1 ; (off, clen) per slot
}
impl Alloc {
    pub fn new() -> Self {
        Alloc { seg: (1u64 << 40) | 1, off: 0, slot: 0, next_id: 0, footers: vec![Vec::new()] }
    }
    pub fn put(&mut self, clen: u32) -> E {
        let sz = (HDR + clen) as u64;
        if self.off + sz > SEG_BYTES {
            self.seg += 1;
            self.off = 0;
            self.slot = 0;
            self.footers.push(Vec::new());
        }
        self.next_id += 1;
        let e = E { idseed: self.next_id, seg: self.seg, off: self.off as u32, clen, slot: self.slot };
        self.footers.last_mut().unwrap().push((e.off, clen));
        self.off += sz;
        self.slot += 1;
        e
    }
}

pub struct Volume {
    pub ents: Vec<E>,
    pub footers: Vec<Vec<(u32, u32)>>,
    pub name: &'static str,
}

fn fill_seq(n: usize, chunk: u32, m: ClenModel, r: &mut Rng, a: &mut Alloc) -> Vec<E> {
    (0..n).map(|_| a.put(clen(r, chunk, m))).collect()
}

fn overwrite(v: &mut [E], frac: f64, batch: usize, chunk: u32, r: &mut Rng, a: &mut Alloc) {
    let mut idx: Vec<u32> = (0..v.len() as u32).filter(|_| r.f() < frac).collect();
    for i in (1..idx.len()).rev() {
        let j = r.below(i as u64 + 1) as usize;
        idx.swap(i, j);
    }
    for b in idx.chunks_mut(batch) {
        b.sort_unstable();
        for &i in b.iter() {
            v[i as usize] = a.put(clen(r, chunk, ClenModel::Typical));
        }
    }
}

pub fn gen(w: char, tib_chunks: usize, chunk: u32, seed: u64) -> Volume {
    let mut r = Rng(seed);
    let mut a = Alloc::new();
    let n = tib_chunks;
    let (ents, name) = match w {
        'a' => (fill_seq(n, chunk, ClenModel::Typical, &mut r, &mut a), "a-seq"),
        'b' => {
            let mut v = fill_seq(n, chunk, ClenModel::Typical, &mut r, &mut a);
            // 30% of chunks rewritten randomly, destaged in 1 GiB batches, each sorted by LBA
            let batch = ((1u64 << 30) / chunk as u64) as usize;
            overwrite(&mut v, 0.30, batch, chunk, &mut r, &mut a);
            (v, "b-frag")
        }
        'c' => {
            // 8 VM disks (n/8 chunks each) cloned from a base image of the same size
            let per = n / 8;
            let base = fill_seq(per, chunk, ClenModel::Typical, &mut r, &mut a);
            let mut v = vec![E::default(); n];
            for k in 0..8 {
                let mut pending: Vec<usize> = Vec::new();
                let permuted = k % 2 == 1;
                let mut i = 0usize;
                while i < per {
                    let l = (r.geo(if permuted { 12.0 } else { 24.0 }) as usize).min(per - i);
                    let unique = r.f() < if permuted { 0.20 } else { 0.15 };
                    if unique {
                        for j in 0..l {
                            pending.push(k * per + i + j);
                        }
                    } else {
                        let bs = if permuted { r.below((per - l + 1) as u64) as usize } else { i };
                        for j in 0..l {
                            v[k * per + i + j] = base[bs + j];
                        }
                    }
                    i += l;
                }
                pending.sort_unstable();
                for p in pending {
                    v[p] = a.put(clen(&mut r, chunk, ClenModel::Typical));
                }
            }
            (v, "c-clone")
        }
        'd' => (fill_seq(n, chunk, ClenModel::Mixed, &mut r, &mut a), "d-mixed"),
        'e' => {
            let mut v = vec![E::default(); n];
            let mut i = 0;
            let mut data = r.f() < 0.5;
            while i < n {
                let l = (r.geo(64.0) as usize).min(n - i);
                if data {
                    for j in 0..l {
                        v[i + j] = a.put(clen(&mut r, chunk, ClenModel::Typical));
                    }
                }
                i += l;
                data = !data;
            }
            (v, "e-sparse")
        }
        _ => panic!("workload"),
    };
    Volume { ents, footers: a.footers, name }
}
