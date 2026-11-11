use crate::gen::{idbytes, Rng};
use crate::leaf::*;
use crate::map::*;
use crate::model::*;
use crate::store::*;
use std::sync::Arc;

fn tmp(name: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!("radixmap-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    p
}
fn cfg(fan: usize, nchunks: u64, leaf: Kind, wrap: Wrap) -> Cfg {
    Cfg {
        fan,
        leaf: LeafFmt {
            kind: leaf,
            t: if leaf == Kind::Runs { 16 } else { 32 },
            wrap,
            fan,
        },
        interior_wrap: Wrap::Zstd1,
        nchunks,
    }
}
fn ent(r: &mut Rng) -> Ent {
    Ent {
        id: idbytes(r.u()),
        seg: (1 << 40) | (1 + r.below(100)),
        off: r.below(1 << 24) as u32,
        clen: 256 + r.below(65000) as u32,
    }
}
fn store(name: &str) -> (Arc<PackStore>, std::path::PathBuf) {
    let d = tmp(name);
    (PackStore::new(d.clone(), None), d)
}
fn same_prefix(a: &Ent, b: &Ent, t: usize) -> bool {
    a.seg == b.seg && a.off == b.off && a.clen == b.clen && a.id[..t] == b.id[..t]
}

#[test]
fn leaf_codecs_roundtrip() {
    let mut r = Rng(1);
    for fan in [16usize, 256, 512, 1024] {
        for (kind, t, wrap) in [
            (Kind::Runs, 16, Wrap::None),
            (Kind::Runs, 32, Wrap::None),
            (Kind::Rows, 32, Wrap::Lz4),
            (Kind::Rows, 32, Wrap::Zstd1),
        ] {
            let f = LeafFmt { kind, t, wrap, fan };
            for trial in 0..20 {
                let m = Model {
                    fan,
                    frag: if trial % 2 == 0 { 0.3 } else { 0.0 },
                    seed: trial,
                    chunk: 65536,
                    seg_span: 5000,
                };
                let mut l = Vec::new();
                m.leaf(trial, &mut l);
                // punch holes (runs of holes and isolated ones)
                for _ in 0..(trial as usize % 5) * 7 {
                    let i = r.below(fan as u64) as usize;
                    let n = 1 + r.below(9) as usize;
                    for e in l.iter_mut().skip(i).take(n) {
                        *e = Ent::default();
                    }
                }
                let mut raw = Vec::new();
                f.enc_raw(&l, &mut raw);
                let back = wrap_dec(wrap, &wrap_enc(wrap, &raw), fan * 64).unwrap();
                assert_eq!(back, raw);
                let mut d = vec![Ent::default(); fan];
                f.dec_raw(&raw, &mut d);
                for i in 0..fan {
                    if l[i].hole() {
                        assert!(d[i].hole());
                        assert_eq!(f.lk(&raw, i).seg, 0);
                    } else {
                        assert!(same_prefix(&l[i], &d[i], t), "dec {fan} {kind:?} {i}");
                        let k = f.lk(&raw, i);
                        assert_eq!(
                            (k.seg, k.off, k.clen),
                            (l[i].seg, l[i].off, l[i].clen),
                            "lk {fan} {kind:?} {i}"
                        );
                        assert_eq!(k.idp, u64::from_le_bytes(l[i].id[..8].try_into().unwrap()));
                    }
                }
            }
        }
    }
}

/// The synthetic model must land in the leafenc measured range (294-352 MiB/TiB
/// at 16 B ids = 9.2-11.0 KiB per leaf) and agree with leafenc's own generator.
#[test]
fn model_matches_leafenc_sizes() {
    let f = LeafFmt {
        kind: Kind::Runs,
        t: 16,
        wrap: Wrap::None,
        fan: 512,
    };
    let size = |frag: f64| {
        let m = Model::new(512, 1 << 24, frag);
        let (mut l, mut raw, mut tot) = (Vec::new(), Vec::new(), 0usize);
        for i in 0..400 {
            m.leaf(i * 80, &mut l);
            raw.clear();
            f.enc_raw(&l, &mut raw);
            tot += raw.len();
        }
        tot as f64 / 400.0 / 1024.0
    };
    let (seq, frag) = (size(0.0), size(0.3));
    eprintln!("model leaf KiB: seq {seq:.2} frag {frag:.2}");
    assert!((8.8..10.0).contains(&seq), "seq {seq}");
    assert!((10.3..11.6).contains(&frag), "frag {frag}");
    // leafenc's real generator (workload b) through the same codec
    let v = crate::gen::gen('b', 512 * 2048, 65536, 7);
    let (mut raw, mut tot) = (Vec::new(), 0usize);
    for c in v.ents.chunks(512) {
        let l: Vec<Ent> = c
            .iter()
            .map(|e| Ent {
                id: if e.hole() { [0; 32] } else { idbytes(e.idseed) },
                seg: e.seg,
                off: e.off,
                clen: e.clen,
            })
            .collect();
        raw.clear();
        f.enc_raw(&l, &mut raw);
        tot += raw.len();
    }
    let g = tot as f64 / 2048.0 / 1024.0;
    eprintln!("leafenc gen('b') leaf KiB: {g:.2}");
    assert!((g / frag - 1.0).abs() < 0.20, "model {frag} vs leafenc {g}");
}

fn build_by_sets(
    c: Cfg,
    ents: &[(u64, Ent)],
    seal_every: usize,
    name: &str,
) -> (Child, Arc<PackStore>) {
    let (s, _d) = store(name);
    let mut m = Map::new_empty(c, s.clone(), 1 << 20);
    for (n, (ci, e)) in ents.iter().enumerate() {
        m.set(*ci, *e).unwrap();
        if (n + 1) % seal_every == 0 {
            let mut w = s.new_writer(false);
            m.seal(&mut w).unwrap();
        }
    }
    let mut w = s.new_writer(false);
    m.seal(&mut w).unwrap();
    (m.root, s)
}

#[test]
fn root_is_history_independent() {
    for kind in [Kind::Runs, Kind::Rows] {
        let c = cfg(16, 4096, kind, Wrap::None);
        let mut r = Rng(5);
        let mut v: Vec<(u64, Ent)> = (0..2500).map(|_| (r.below(4096), ent(&mut r))).collect();
        // last write wins: make the final value per index unique so every order agrees
        v.sort_by_key(|x| x.0);
        v.dedup_by_key(|x| x.0);
        let mut shuffled = v.clone();
        for i in (1..shuffled.len()).rev() {
            shuffled.swap(i, r.below(i as u64 + 1) as usize);
        }
        let (a, _) = build_by_sets(c, &v, usize::MAX, "hi-a");
        let (b, _) = build_by_sets(c, &shuffled, usize::MAX, "hi-b");
        let (d, _) = build_by_sets(c, &shuffled, 137, "hi-c");
        assert_eq!(a.hash, b.hash);
        assert_eq!(a.hash, d.hash);
        // different packs/locations, same root hash
        assert_ne!(b.loc, d.loc);
    }
}

#[test]
fn builder_equals_sets() {
    let c = cfg(16, 16 * 16 * 16 * 3 / 2, Kind::Runs, Wrap::None); // 6144 chunks, 384 leaves
    let m = Model {
        fan: 16,
        frag: 0.3,
        seed: 3,
        chunk: 65536,
        seg_span: 100,
    };
    let (s, _) = store("bld");
    let mut map = Map::new_empty(c, s.clone(), 1 << 20);
    let mut w = s.new_writer(false);
    let mut b = Builder::new(&mut map, &mut w);
    let (mut l, mut raw) = (Vec::new(), Vec::new());
    let pop = c.nleaves() * 2 / 3;
    for li in 0..c.nleaves() {
        if li < pop {
            m.leaf(li, &mut l);
            b.push_leaf(li, Some(&l), false, &mut raw).unwrap();
        } else {
            b.push_leaf(li, None, false, &mut raw).unwrap();
        }
    }
    let (root, _) = b.finish().unwrap();
    let mut v = Vec::new();
    for li in (0..pop).rev() {
        m.leaf(li, &mut l);
        for (j, e) in l.iter().enumerate() {
            v.push((li * 16 + j as u64, *e));
        }
    }
    let (r2, _) = build_by_sets(c, &v, usize::MAX, "bld2");
    assert_eq!(root.hash, r2.hash);
}

#[test]
fn old_root_stays_readable_after_seal() {
    let c = cfg(16, 4096, Kind::Runs, Wrap::None);
    let (s, _d) = store("old");
    let mut m = Map::new_empty(c, s.clone(), 1 << 20);
    let mut r = Rng(9);
    let first: Vec<(u64, Ent)> = (0..1000).map(|i| (i * 4, ent(&mut r))).collect();
    for (ci, e) in &first {
        m.set(*ci, *e).unwrap();
    }
    let mut w = s.new_writer(false);
    m.seal(&mut w).unwrap();
    let r1 = m.root;
    for (ci, _) in first.iter().take(500) {
        m.set(*ci, ent(&mut r)).unwrap();
    }
    m.set(1, ent(&mut r)).unwrap();
    let mut w = s.new_writer(false);
    m.seal(&mut w).unwrap();
    assert_ne!(r1.hash, m.root.hash);
    // fresh handle on the old root (cold, nothing cached) sees the old values
    let mut old = Map::open(c, s.clone(), r1, 1 << 20);
    for (ci, e) in &first {
        let l = old.get(*ci).unwrap().unwrap();
        assert_eq!((l.seg, l.off, l.clen), (e.seg, e.off, e.clen));
    }
    assert!(old.get(1).unwrap().is_none());
    assert!(m.get(1).unwrap().is_some());
}

#[test]
fn diff_equals_bruteforce_1m() {
    let c = cfg(512, 1 << 20, Kind::Runs, Wrap::None);
    assert_eq!(c.depth(), 3);
    let (s, _d) = store("diff");
    let mut m = Map::new_empty(c, s.clone(), 64 << 20);
    let mut r = Rng(11);
    let mm = Model {
        fan: 512,
        frag: 0.3,
        seed: 4,
        chunk: 65536,
        seg_span: 4000,
    };
    let mut l = Vec::new();
    for li in 0..c.nleaves() - 100 {
        mm.leaf(li, &mut l);
        for (j, e) in l.iter().enumerate() {
            m.set(li * 512 + j as u64, *e).unwrap();
        }
    }
    let mut w = s.new_writer(false);
    m.seal(&mut w).unwrap();
    let a = m.root;
    for _ in 0..700 {
        m.set(r.below(1 << 20), ent(&mut r)).unwrap();
    }
    for _ in 0..50 {
        m.set(r.below(1 << 20), Ent::default()).unwrap(); // hole punches
    }
    for _ in 0..30 {
        m.set((c.nleaves() - 50) * 512 + r.below(512 * 20), ent(&mut r))
            .unwrap(); // fill into the empty tail
    }
    let mut w = s.new_writer(false);
    m.seal(&mut w).unwrap();
    let b = m.root;
    let (ranges, reads) = m.diff(a, b).unwrap();
    // brute force over every chunk
    let mut ma = Map::open(c, s.clone(), a, usize::MAX);
    let mut mb = Map::open(c, s.clone(), b, usize::MAX);
    let mut bf = std::collections::BTreeSet::new();
    for ci in 0..1u64 << 20 {
        if ma.get(ci).unwrap() != mb.get(ci).unwrap() {
            bf.insert(ci / 512);
        }
    }
    let got: std::collections::BTreeSet<u64> = ranges.iter().flat_map(|r| r.0..r.1).collect();
    assert_eq!(got, bf);
    let k = got.len() as u64;
    assert!(reads <= 2 * k * c.depth() as u64, "reads {reads} k {k}");
    // identical roots: empty diff, no reads
    assert_eq!(m.diff(b, b).unwrap(), (vec![], 0));
}

#[test]
fn empty_subtrees_are_shared() {
    // 10 TiB of holes at 64 KiB chunks
    let c = cfg(512, 168 * (1 << 20) + (1 << 19), Kind::Runs, Wrap::None);
    assert_eq!(c.depth(), 4);
    let (s, _d) = store("empty");
    let mut m = Map::new_empty(c, s.clone(), 1 << 20);
    let mut r = Rng(1);
    let mut w = s.new_writer(false);
    let st = m.seal(&mut w).unwrap();
    assert_eq!(st.stored(), 0);
    m.set(123_456_789, ent(&mut r)).unwrap();
    m.set(5, Ent::default()).unwrap(); // hole into hole: nothing to write
    let mut w = s.new_writer(false);
    let st = m.seal(&mut w).unwrap();
    assert_eq!(
        (st.leaves, st.interior),
        (1, 3),
        "a handful of nodes: leaf + one per interior level"
    );
    assert!(m.get(123_456_789).unwrap().is_some());
    assert!(m.get(123_456_790).unwrap().is_none());
    assert!(m.get(0).unwrap().is_none());
}

#[test]
fn cold_open_reads_depth_nodes() {
    let c = cfg(512, 1 << 21, Kind::Runs, Wrap::None); // depth 3
    let (s, _d) = store("cold");
    let mut m = Map::new_empty(c, s.clone(), 1 << 20);
    let mut r = Rng(2);
    for ci in (0..1u64 << 21).step_by(3) {
        m.set(ci, ent(&mut r)).unwrap();
    }
    let mut w = s.new_writer(false);
    m.seal(&mut w).unwrap();
    let root = m.root;
    s.reset_counters();
    let mut o = Map::open(c, s.clone(), root, 1 << 20);
    assert!(o.get(3 * 1000).unwrap().is_some());
    assert_eq!(gets(&s), c.depth() as u64);
    // same leaf again: no further GETs
    o.get(3 * 1000 + 3).unwrap();
    assert_eq!(gets(&s), c.depth() as u64);
}

#[test]
fn lru_is_bounded() {
    let mut l = Lru::new(10 * (100 + 64));
    for i in 0..100u8 {
        let mut k = [0u8; 32];
        k[0] = i;
        l.put(k, Arc::new(vec![i; 100]));
        assert!(l.bytes <= 10 * 164);
    }
    assert_eq!(l.len(), 10);
    let mut k = [0u8; 32];
    k[0] = 99;
    assert!(l.get(&k).is_some());
    k[0] = 0;
    assert!(l.get(&k).is_none());
}
