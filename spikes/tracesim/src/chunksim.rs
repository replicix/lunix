//! P0.6a: chunk-size simulation. See README for the modelling choices.
use crate::ds::*;
use crate::trace::Req;

pub const CHUNKS: [u64; 4] = [16 << 10, 64 << 10, 256 << 10, 1 << 20];
pub const TS_US: [u64; 3] = [10_000_000, 60_000_000, 300_000_000];
pub const FANOUTS: [u64; 3] = [256, 512, 1024];
const ENTRY: u64 = 48; // leaf entry and interior entry size, both 48 B per the plan

pub const H_RMW: &str = "set,vol,chunk,writes,write_gib,partial,partial_to_written,partial_to_new,rmw_inf,rmw_lru1,rmw_lru10,touched_chunks,span_chunks";
pub const H_MAP: &str = "set,vol,chunk,fanout,leaves,interior,map_bytes_full,map_bytes_half,span_map_bytes_full,mib_per_tib_span_full,mib_per_tib_touched_half";
pub const H_COMMIT: &str = "set,vol,chunk,interval_s,fanout,cuts,logical_bytes,data_r1,data_r05,map_r1,map_r05,mapwa_r05_p50,mapwa_r05_p90,mapwa_r05_max,mapwa_r1_p50,mapwa_r1_p90,mapwa_r1_max";

#[derive(Default)]
pub struct Out {
    pub rmw: Vec<String>,
    pub map: Vec<String>,
    pub commit: Vec<String>,
}

pub fn depth(f: u64, max_ci: u64) -> u32 {
    let (mut d, mut cap) = (1u32, f);
    while cap < max_ci + 1 {
        cap = cap.saturating_mul(f);
        d += 1;
    }
    d
}

/// Distinct leaves and interior nodes over a sorted, deduplicated chunk index list.
fn nodes_for(cis_sorted: &[u64], f: u64, d: u32) -> (u64, u64) {
    let mut cur: Vec<u64> = cis_sorted.iter().map(|x| x / f).collect();
    cur.dedup();
    let leaves = cur.len() as u64;
    let mut interior = 0;
    for _ in 1..d {
        cur = cur.iter().map(|x| x / f).collect();
        cur.dedup();
        interior += cur.len() as u64;
    }
    (leaves, interior)
}

struct Lru {
    l: DList,
    cap: usize,
}
impl Lru {
    fn new(cap: usize) -> Self {
        Lru { l: DList::new(), cap: cap.max(1) }
    }
    /// Returns true on hit; on miss the chunk is fetched and inserted.
    fn access(&mut self, k: u64) -> bool {
        if let Some(i) = self.l.idx(k) {
            self.l.move_front(i);
            return true;
        }
        if self.l.len() >= self.cap {
            self.l.pop_back();
        }
        self.l.push_front(k);
        false
    }
}

#[derive(Default)]
struct Cut {
    dirty: FxSet<u64>,
    logical: u64,
    next: u64,
    cuts: u64,
    tot_logical: u64,
    tot_d1: u64,
    tot_d05: u64,
    // per fanout
    m1: [u64; 3],
    m05: [u64; 3],
    wa05: [Vec<f64>; 3],
    wa1: [Vec<f64>; 3],
}

impl Cut {
    fn cut(&mut self, c: u64, max_ci: u64) {
        if self.dirty.is_empty() {
            self.logical = 0;
            return;
        }
        let n = self.dirty.len() as u64;
        let (d1, d05) = (n * c, n * c / 2);
        let mut cis: Vec<u64> = self.dirty.iter().copied().collect();
        cis.sort_unstable();
        for (fi, &f) in FANOUTS.iter().enumerate() {
            let (leaves, interior) = nodes_for(&cis, f, depth(f, max_ci));
            let node = f * ENTRY;
            let m1 = (leaves + interior) * node;
            let m05 = leaves * node / 2 + interior * node;
            self.m1[fi] += m1;
            self.m05[fi] += m05;
            self.wa1[fi].push(m1 as f64 / d1 as f64);
            self.wa05[fi].push(m05 as f64 / d05 as f64);
        }
        self.cuts += 1;
        self.tot_logical += self.logical;
        self.tot_d1 += d1;
        self.tot_d05 += d05;
        self.dirty.clear();
        self.logical = 0;
    }
}

pub fn sim_volume(set: &str, vol: &str, reqs: &[Req], out: &mut Out) {
    if reqs.is_empty() {
        return;
    }
    let t0 = reqs[0].t;
    for &c in CHUNKS.iter() {
        // Pre-pass: working set (distinct touched chunks) and extent.
        let mut touched: FxSet<u64> = FxSet::default();
        let mut max_ci = 0;
        for r in reqs {
            for ch in r.off / c..=(r.off + r.len - 1) / c {
                touched.insert(ch);
                max_ci = max_ci.max(ch);
            }
        }
        let w = touched.len();
        let cap1 = ((w as f64 * 0.01).ceil() as usize).max(1);
        let cap10 = ((w as f64 * 0.10).ceil() as usize).max(1);
        let mut lru1 = Lru::new(cap1);
        let mut lru10 = Lru::new(cap10);
        let mut inf: FxSet<u64> = FxSet::default();
        let mut written: FxSet<u64> = FxSet::default();
        let (mut writes, mut wbytes, mut partial, mut p_old, mut p_new) = (0u64, 0u64, 0u64, 0u64, 0u64);
        let (mut r_inf, mut r_1, mut r_10) = (0u64, 0u64, 0u64);
        let mut cuts: Vec<Cut> = TS_US.iter().map(|&t| Cut { next: t0 + t, ..Default::default() }).collect();

        for r in reqs {
            for (ti, ct) in cuts.iter_mut().enumerate() {
                if r.t >= ct.next {
                    ct.cut(c, max_ci);
                    ct.next = t0 + ((r.t - t0) / TS_US[ti] + 1) * TS_US[ti];
                }
            }
            let (s, e) = (r.off, r.off + r.len);
            for ch in s / c..=(e - 1) / c {
                if !r.w {
                    // Reads populate the model caches (fetch on miss).
                    inf.insert(ch);
                    lru1.access(ch);
                    lru10.access(ch);
                    continue;
                }
                let full = ch * c >= s && (ch + 1) * c <= e;
                if !full {
                    partial += 1;
                    if written.contains(&ch) {
                        p_old += 1;
                        if inf.insert(ch) {
                            r_inf += 1;
                        }
                        if !lru1.access(ch) {
                            r_1 += 1;
                        }
                        if !lru10.access(ch) {
                            r_10 += 1;
                        }
                    } else {
                        p_new += 1;
                    }
                }
                written.insert(ch);
                for ct in cuts.iter_mut() {
                    ct.dirty.insert(ch);
                }
            }
            if r.w {
                writes += 1;
                wbytes += r.len;
                for ct in cuts.iter_mut() {
                    ct.logical += r.len;
                }
            }
        }
        for ct in cuts.iter_mut() {
            ct.cut(c, max_ci);
        }

        out.rmw.push(format!(
            "{set},{vol},{c},{writes},{:.6},{partial},{p_old},{p_new},{r_inf},{r_1},{r_10},{w},{}",
            wbytes as f64 / (1u64 << 30) as f64,
            max_ci + 1
        ));

        // Map size (static) per fanout.
        let mut tv: Vec<u64> = touched.iter().copied().collect();
        tv.sort_unstable();
        let tib = (1u64 << 40) as f64;
        let mib = (1u64 << 20) as f64;
        for &f in FANOUTS.iter() {
            let d = depth(f, max_ci);
            let (leaves, interior) = nodes_for(&tv, f, d);
            let node = f * ENTRY;
            let full = (leaves + interior) * node;
            let half = leaves * node / 2 + interior * node;
            // Fully populated span of the volume.
            let span = max_ci + 1;
            let mut n = (span + f - 1) / f;
            let mut sp = n;
            for _ in 1..d {
                n = (n + f - 1) / f;
                sp += n;
            }
            let span_bytes = sp * node;
            out.map.push(format!(
                "{set},{vol},{c},{f},{leaves},{interior},{full},{half},{span_bytes},{:.3},{:.3}",
                span_bytes as f64 / mib / (span as f64 * c as f64 / tib),
                half as f64 / mib / (w as f64 * c as f64 / tib)
            ));
        }

        for (ti, ct) in cuts.iter().enumerate() {
            for (fi, &f) in FANOUTS.iter().enumerate() {
                out.commit.push(format!(
                    "{set},{vol},{c},{},{f},{},{},{},{},{},{},{:.6},{:.6},{:.6},{:.6},{:.6},{:.6}",
                    TS_US[ti] / 1_000_000,
                    ct.cuts,
                    ct.tot_logical,
                    ct.tot_d1,
                    ct.tot_d05,
                    ct.m1[fi],
                    ct.m05[fi],
                    pct(&ct.wa05[fi], 0.5),
                    pct(&ct.wa05[fi], 0.9),
                    pct(&ct.wa05[fi], 1.0),
                    pct(&ct.wa1[fi], 0.5),
                    pct(&ct.wa1[fi], 0.9),
                    pct(&ct.wa1[fi], 1.0),
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trace::{align, Req};

    fn rq(t_s: u64, w: bool, off: u64, len: u64) -> Req {
        let (o, l) = align(off, len).unwrap();
        Req { t: t_s * 1_000_000, off: o, len: l, w }
    }

    #[test]
    fn offset_math() {
        assert_eq!(align(1, 1), Some((0, 4096)));
        assert_eq!(align(4095, 2), Some((0, 8192)));
        assert_eq!(align(4096, 4096), Some((4096, 4096)));
        assert_eq!(align(0, 0), None);
        assert_eq!(align(u64::MAX - 10, 100), None);
        // chunk range of a 16 KiB-chunk write spanning two chunks
        let c = 16384u64;
        let r = rq(0, true, 12288, 8192);
        assert_eq!((r.off / c, (r.off + r.len - 1) / c), (0, 1));
        assert_eq!(depth(512, 0), 1);
        assert_eq!(depth(512, 511), 1);
        assert_eq!(depth(512, 512), 2);
        // 8 TiB at 64 KiB chunks = 2^27 chunks = 512^3: exactly 3 levels; one more chunk is 4.
        assert_eq!(depth(512, (1 << 27) - 1), 3);
        assert_eq!(depth(512, 1 << 27), 4);
    }

    /// Hand-computed 10-request fixture (see the comments in the list) at C = 16 KiB.
    fn fixture() -> Vec<Req> {
        vec![
            rq(0, true, 0, 16384),        // chunk0 full
            rq(1, true, 4096, 4096),      // chunk0 partial, written -> cold fetch (inf miss)
            rq(2, true, 8192, 4096),      // chunk0 partial, written -> cached
            rq(3, true, 16384 + 4096, 4096), // chunk1 partial, never written
            rq(4, false, 32768, 4096),    // read chunk2
            rq(5, true, 32768, 4096),     // chunk2 partial, only read before -> new
            rq(6, true, 12288, 8192),     // chunk0 partial (hit) + chunk1 partial written (miss)
            rq(12, true, 0, 32768),       // chunks 0,1 full
            rq(13, true, 65536, 4096),    // chunk4 partial, new
            rq(14, false, 0, 4096),
        ]
    }

    #[test]
    fn hand_fixture() {
        let mut o = Out::default();
        sim_volume("t", "v", &fixture(), &mut o);
        let r16: Vec<&str> = o.rmw[0].split(',').collect();
        // writes=8 partial=7 to_written=4 to_new=3 rmw_inf=2 touched={0,1,2,4}=4 span=5
        assert_eq!(&r16[3], &"8");
        assert_eq!(&r16[5..9], &["7", "4", "3", "2"]);
        assert_eq!(&r16[11..], &["4", "5"]);
        // T = 10 s, fanout 512: two cuts (t<10: dirty {0,1,2}; t>=12: dirty {0,1,4}).
        let row = o.commit.iter().find(|l| l.starts_with("t,v,16384,10,512,")).unwrap();
        let f: Vec<&str> = row.split(',').collect();
        assert_eq!(&f[5..11], &["2", "77824", "98304", "49152", "49152", "24576"]);
    }

    #[test]
    fn deterministic() {
        let (mut a, mut b) = (Out::default(), Out::default());
        sim_volume("t", "v", &fixture(), &mut a);
        sim_volume("t", "v", &fixture(), &mut b);
        assert_eq!(a.rmw, b.rmw);
        assert_eq!(a.map, b.map);
        assert_eq!(a.commit, b.commit);
    }
}
