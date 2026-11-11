//! P0.6b: cache policy replay. Each request is expanded to the chunks it
//! touches. Reads are the measured accesses. Writes are pinned dirty (outside
//! the evictable set, reads of them are overlay hits) until the next cut
//! (60 s of trace clock), then inserted into the clean cache as an access.
use crate::ds::*;
use crate::policies::*;
use crate::trace::Req;
use std::collections::BTreeSet;

pub const FRACS_PM: [u32; 6] = [5, 10, 20, 50, 100, 250]; // per mille of the working set
pub const CUT_US: u64 = 60_000_000;
pub const H_CACHE: &str = "set,vol,chunk,policy,frac_pm,capacity,read_chunk_accesses,miss_req,miss_bytes,gets_per_1000_reads";
pub const H_SCAN: &str = "set,vol,chunk,policy,frac_pm,capacity,scan_chunks,miss_req";

/// (chunk, bytes of the request inside the chunk, is_read, trace time)
pub struct Acc {
    pub ch: u64,
    pub bytes: u32,
    pub read: bool,
    pub t: u64,
}

pub fn expand(reqs: &[Req], c: u64) -> (Vec<Acc>, usize, u64) {
    let mut v = Vec::new();
    let mut ws: FxSet<u64> = FxSet::default();
    let mut nreads = 0u64;
    for r in reqs {
        let (s, e) = (r.off, r.off + r.len);
        for ch in s / c..=(e - 1) / c {
            let b = e.min((ch + 1) * c) - s.max(ch * c);
            ws.insert(ch);
            v.push(Acc { ch, bytes: b as u32, read: !r.w, t: r.t });
        }
        nreads += !r.w as u64;
    }
    (v, ws.len(), nreads)
}

pub struct Stats {
    pub reads: u64,
    pub miss: u64,
    pub bytes: u64,
    pub miss_bytes: u64,
}

/// Replay; if `scan` is Some((at_index, n)) inject n never-reused read chunks
/// before access `at_index`. Statistics cover original accesses only.
pub fn replay(acc: &[Acc], cache: &mut dyn Cache, scan: Option<(usize, usize)>) -> Stats {
    let mut st = Stats { reads: 0, miss: 0, bytes: 0, miss_bytes: 0 };
    let mut dirty: BTreeSet<u64> = BTreeSet::new();
    let mut next_cut = acc.first().map(|a| a.t + CUT_US).unwrap_or(0);
    for (i, a) in acc.iter().enumerate() {
        if let Some((at, n)) = scan {
            if i == at {
                for j in 0..n as u64 {
                    cache.access((1u64 << 62) + j);
                }
            }
        }
        if a.t >= next_cut {
            for &d in dirty.iter() {
                cache.access(d);
            }
            dirty.clear();
            next_cut += CUT_US * ((a.t - next_cut) / CUT_US + 1);
        }
        if !a.read {
            dirty.insert(a.ch);
            continue;
        }
        st.reads += 1;
        st.bytes += a.bytes as u64;
        let hit = dirty.contains(&a.ch) || cache.access(a.ch);
        if !hit {
            st.miss += 1;
            st.miss_bytes += a.bytes as u64;
        }
    }
    st
}

pub fn sim_volume(set: &str, vol: &str, reqs: &[Req], c: u64, scan_test: bool, rows: &mut Vec<String>, scan_rows: &mut Vec<String>) {
    let (acc, w, nreads) = expand(reqs, c);
    if w == 0 {
        return;
    }
    for &pm in FRACS_PM.iter() {
        let cap = ((w as u64 * pm as u64).div_ceil(1000) as usize).max(16);
        for p in POLICIES {
            let mut cache = make(p, cap);
            let s = replay(&acc, cache.as_mut(), None);
            rows.push(format!(
                "{set},{vol},{c},{p},{pm},{cap},{},{:.6},{:.6},{:.3}",
                s.reads,
                s.miss as f64 / s.reads.max(1) as f64,
                s.miss_bytes as f64 / s.bytes.max(1) as f64,
                s.miss as f64 * 1000.0 / nreads.max(1) as f64
            ));
            if scan_test {
                let mut cache = make(p, cap);
                let n = 2 * cap;
                let s = replay(&acc, cache.as_mut(), Some((acc.len() / 2, n)));
                scan_rows.push(format!("{set},{vol},{c},{p},{pm},{cap},{n},{:.6}", s.miss as f64 / s.reads.max(1) as f64));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trace::{align, Req};

    #[test]
    fn replay_counts_and_pinning() {
        let c = 65536;
        let mk = |t: u64, w: bool, ch: u64| {
            let (o, l) = align(ch * c, c).unwrap();
            Req { t: t * 1_000_000, off: o, len: l, w }
        };
        // read 1 (miss), read 1 (hit), write 2 (pinned), read 2 (overlay hit),
        // after the cut chunk 2 is a clean entry -> read 2 hits via cache.
        let reqs = vec![mk(0, false, 1), mk(1, false, 1), mk(2, true, 2), mk(3, false, 2), mk(70, false, 2), mk(71, false, 9)];
        let (acc, w, n) = expand(&reqs, c);
        assert_eq!((w, n), (3, 5));
        let mut cache = make("lru", 16);
        let s = replay(&acc, cache.as_mut(), None);
        assert_eq!((s.reads, s.miss), (5, 2)); // chunk 1 first read, chunk 9
    }

    #[test]
    fn deterministic_rows() {
        let reqs: Vec<Req> = (0..2000u64)
            .map(|i| {
                let x = mix(i);
                let (o, l) = align((x % 500) * 65536, 4096 + (x >> 20) % 70000).unwrap();
                Req { t: i * 1_000_000, off: o, len: l, w: x % 3 == 0 }
            })
            .collect();
        let run = || {
            let (mut a, mut b) = (Vec::new(), Vec::new());
            sim_volume("t", "v", &reqs, 65536, true, &mut a, &mut b);
            (a, b)
        };
        assert_eq!(run(), run());
    }
}
