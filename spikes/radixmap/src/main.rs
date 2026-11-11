//! P0.7 radix Merkle map spike: bench driver. Throwaway measurement code.
mod flat;
#[allow(dead_code)]
mod gen;
mod leaf;
mod map;
mod model;
mod store;
#[cfg(test)]
mod tests;

use flat::Flat;
use gen::Rng;
use leaf::*;
use map::*;
use model::*;
use std::collections::HashSet;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use store::*;

// ---------------------------------------------------------------- args / env
struct Args(Vec<String>);
impl Args {
    fn s(&self, k: &str, d: &str) -> String {
        self.0
            .iter()
            .position(|a| a == k)
            .map(|i| self.0[i + 1].clone())
            .unwrap_or(d.to_string())
    }
    fn f(&self, k: &str, d: f64) -> f64 {
        self.s(k, &d.to_string()).parse().unwrap()
    }
    fn u(&self, k: &str, d: u64) -> u64 {
        self.s(k, &d.to_string()).parse().unwrap()
    }
    fn flag(&self, k: &str) -> bool {
        self.0.iter().any(|a| a == k)
    }
    fn list(&self, k: &str, d: &str) -> Vec<u64> {
        self.s(k, d)
            .split(',')
            .map(|x| x.parse().unwrap())
            .collect()
    }
}

struct Env {
    tib: f64,
    pct: u64,
    dir: PathBuf,
    virt: bool,
    model: Model,
    cfg: Cfg,
    leafname: String,
}
impl Env {
    fn tag(&self) -> String {
        format!(
            "t{}-f{}-{}-p{}-frag{}{}",
            self.tib,
            self.cfg.fan,
            self.leafname,
            self.pct,
            self.model.frag,
            if self.virt { "-virt" } else { "" }
        )
    }
    fn nleaves(&self) -> u64 {
        self.cfg.nleaves()
    }
    fn pop_leaves(&self) -> u64 {
        self.nleaves() * self.pct / 100
    }
    fn pop_chunks(&self) -> u64 {
        (self.pop_leaves() * self.cfg.fan as u64).min(self.cfg.nchunks)
    }
    fn store(&self) -> std::sync::Arc<PackStore> {
        let s = PackStore::new(self.dir.clone(), self.virt.then_some((self.model, self.cfg.leaf)));
        s.resume_ids();
        s
    }
    fn root_path(&self, suffix: &str) -> PathBuf {
        self.dir.join(format!("root-{}{}.txt", self.tag(), suffix))
    }
}
fn env(a: &Args) -> Env {
    let tib = a.f("--tib", 1.0);
    let fan = a.u("--fan", 512) as usize;
    let nchunks = (tib * (1u64 << 24) as f64) as u64;
    let leafname = a.s("--leaf", "rb16");
    let leaf = match leafname.as_str() {
        "rb16" => LeafFmt {
            kind: Kind::Runs,
            t: 16,
            wrap: Wrap::None,
            fan,
        },
        "rb32" => LeafFmt {
            kind: Kind::Runs,
            t: 32,
            wrap: Wrap::None,
            fan,
        },
        "row48" => LeafFmt {
            kind: Kind::Rows,
            t: 32,
            wrap: Wrap::None,
            fan,
        },
        "row48lz4" => LeafFmt {
            kind: Kind::Rows,
            t: 32,
            wrap: Wrap::Lz4,
            fan,
        },
        "row48z1" => LeafFmt {
            kind: Kind::Rows,
            t: 32,
            wrap: Wrap::Zstd1,
            fan,
        },
        x => panic!("leaf {x}"),
    };
    let iwrap = match a.s("--iwrap", "zstd1").as_str() {
        "none" => Wrap::None,
        "lz4" => Wrap::Lz4,
        _ => Wrap::Zstd1,
    };
    let cfg = Cfg {
        fan,
        leaf,
        interior_wrap: iwrap,
        nchunks,
    };
    let model = Model::new(fan, nchunks, a.f("--frag", 0.3));
    Env {
        tib,
        pct: a.u("--pct", 100),
        dir: PathBuf::from(a.s("--dir", "data")),
        virt: a.flag("--virtual"),
        model,
        cfg,
        leafname,
    }
}

// ---------------------------------------------------------------- helpers
fn hex(h: &Hash) -> String {
    h.iter().map(|b| format!("{b:02x}")).collect()
}
fn unhex(s: &str) -> Hash {
    let mut h = [0u8; 32];
    for i in 0..32 {
        h[i] = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap();
    }
    h
}
fn save_root(p: &PathBuf, c: &Child) {
    std::fs::write(
        p,
        format!(
            "{} {} {} {}\n",
            hex(&c.hash),
            c.loc.pack,
            c.loc.off,
            c.loc.len
        ),
    )
    .unwrap();
}
fn load_root(p: &PathBuf) -> Child {
    let s = std::fs::read_to_string(p)
        .unwrap_or_else(|_| panic!("missing root file {} (run build first)", p.display()));
    let f: Vec<&str> = s.split_whitespace().collect();
    Child {
        hash: unhex(f[0]),
        loc: NodeLoc {
            pack: f[1].parse().unwrap(),
            off: f[2].parse().unwrap(),
            len: f[3].parse().unwrap(),
        },
    }
}
fn proc_kb(key: &str) -> u64 {
    let s = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    s.lines()
        .find(|l| l.starts_with(key))
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}
fn rss_mib() -> f64 {
    proc_kb("VmRSS:") as f64 / 1024.0
}
fn hwm_mib() -> f64 {
    proc_kb("VmHWM:") as f64 / 1024.0
}
extern "C" {
    fn malloc_trim(pad: usize) -> i32;
}
/// Return freed heap to the OS, then reset the kernel's peak-RSS mark, so each
/// cell reports its own peak instead of the high-water mark of earlier cells.
fn reset_hwm() {
    unsafe {
        malloc_trim(0);
    }
    let _ = std::fs::write("/proc/self/clear_refs", "5");
}
fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// Bijection on [0, 2^k): xorshift-multiply rounds, used for random insertion order.
fn perm(i: u64, k: u32) -> u64 {
    let m = (1u64 << k) - 1;
    let mut x = i & m;
    for c in [
        0x9E37_79B9_7F4A_7C15u64,
        0xBF58_476D_1CE4_E5B9,
        0x94D0_49BB_1331_11EB,
    ] {
        x ^= x >> (k / 2 + 1);
        x = x.wrapping_mul(c | 1) & m;
        x ^= x >> (k / 3 + 1);
    }
    x
}

// ---------------------------------------------------------------- edits
enum Pos {
    List(Vec<u64>),  // sorted chunk indices
    Range(u64, u64), // first chunk, count
}
impl Pos {
    fn len(&self) -> u64 {
        match self {
            Pos::List(v) => v.len() as u64,
            Pos::Range(_, n) => *n,
        }
    }
    fn nth(&self, i: u64) -> u64 {
        match self {
            Pos::List(v) => v[i as usize],
            Pos::Range(s, _) => s + i,
        }
    }
}
struct Edits<'a> {
    pos: &'a Pos,
    i: u64,
    alloc: NewAlloc,
}
impl<'a> Edits<'a> {
    fn new(pos: &'a Pos, seed: u64) -> Self {
        Edits {
            pos,
            i: 0,
            alloc: NewAlloc::new(seed, 65536),
        }
    }
}
impl<'a> Iterator for Edits<'a> {
    type Item = (u64, Ent);
    fn next(&mut self) -> Option<(u64, Ent)> {
        if self.i >= self.pos.len() {
            return None;
        }
        let ci = self.pos.nth(self.i);
        self.i += 1;
        Some((ci, self.alloc.put()))
    }
}
struct Zipf {
    cdf: Vec<f64>,
}
impl Zipf {
    fn new(n: usize, theta: f64) -> Self {
        let mut c = Vec::with_capacity(n);
        let mut s = 0.0;
        for k in 1..=n {
            s += 1.0 / (k as f64).powf(theta);
            c.push(s);
        }
        c.iter_mut().for_each(|x| *x /= s);
        Zipf { cdf: c }
    }
    fn sample(&self, r: &mut Rng) -> usize {
        let u = r.f();
        self.cdf.partition_point(|&x| x < u).min(self.cdf.len() - 1)
    }
}
/// Pick the chunks to rewrite: `uniform` = D distinct leaves, one random chunk each;
/// `zipf` = D distinct leaves clustered by a Zipf(0.9) popularity over 1 GiB
/// regions (32 leaves), in short bursts, one random chunk each; `seq` = D
/// consecutive leaves rewritten completely.
fn positions(pattern: &str, d: u64, e: &Env, r: &mut Rng) -> Pos {
    let fan = e.cfg.fan as u64;
    let pop = e.pop_leaves();
    match pattern {
        "uniform" => {
            let mut s = HashSet::new();
            while (s.len() as u64) < d {
                s.insert(r.below(pop));
            }
            let mut v: Vec<u64> = s.into_iter().map(|l| l * fan + r.below(fan)).collect();
            v.sort_unstable();
            Pos::List(v)
        }
        "zipf" => {
            let regions = (pop / 32).max(1) as usize;
            let z = Zipf::new(regions, 0.9);
            let mut s = HashSet::new();
            let mut tries = 0u64;
            while (s.len() as u64) < d {
                tries += 1;
                if tries > 400 * d {
                    s.insert(r.below(pop)); // coupon-collector tail: fill uniformly
                    continue;
                }
                let reg = z.sample(r) as u64;
                let start = reg * 32 + r.below(32);
                let burst = r.geo(4.0);
                for l in start..(start + burst).min(pop) {
                    if (s.len() as u64) < d {
                        s.insert(l);
                    }
                }
            }
            let mut v: Vec<u64> = s.into_iter().map(|l| l * fan + r.below(fan)).collect();
            v.sort_unstable();
            Pos::List(v)
        }
        "seq" => {
            let start = r.below(pop - d + 1);
            Pos::Range(start * fan, d * fan)
        }
        x => panic!("pattern {x}"),
    }
}

// ---------------------------------------------------------------- commands
fn cmd_table() {
    println!("| volume | chunk | chunks | leaves | depth (levels incl. leaf) | L1 nodes | interior nodes total | interior resident (MiB, 24 KiB/node) | full table @48 B (GiB) |");
    println!("|---|---|---|---|---|---|---|---|---|");
    for tib in [1u64, 8, 10, 32] {
        for ck in [16u64, 64, 256] {
            let nchunks = tib * (1u64 << 40) / (ck << 10);
            let cfg = Cfg {
                fan: 512,
                leaf: LeafFmt {
                    kind: Kind::Runs,
                    t: 16,
                    wrap: Wrap::None,
                    fan: 512,
                },
                interior_wrap: Wrap::None,
                nchunks,
            };
            let d = cfg.depth();
            let leaves = cfg.nleaves();
            let mut inter = 0u64;
            let mut n = leaves;
            for _ in 1..d {
                n = n.div_ceil(512);
                inter += n;
            }
            let l1 = leaves.div_ceil(512);
            println!(
                "| {tib} TiB | {ck} KiB | {nchunks} | {leaves} | {d} | {l1} | {inter} | {:.1} | {:.2} |",
                inter as f64 * 24.0 / 1024.0,
                nchunks as f64 * 48.0 / (1u64 << 30) as f64
            );
        }
    }
}

const CSV_BUILD: &str = "tag,order,virtual,depth,chunks,leaves_written,wall_s,wall_ex_gen_s,gen_s,enc_s,hash_s,comp_s,write_s,leaf_raw_mib,leaf_stored_mib,leaf_stored_mib_per_tib,interior_nodes,interior_raw_mib,interior_stored_mib,root8,peak_rss_mib,set_s,seal_s,overlay_mib";

fn cmd_build(a: &Args) {
    let e = env(a);
    let store = e.store();
    let order = a.s("--order", "seq");
    let virt = e.virt;
    let t_all = Instant::now();
    reset_hwm();
    if a.flag("--header") {
        println!("{CSV_BUILD}");
    }
    let mut map = Map::new_empty(e.cfg, store.clone(), 64 << 20);
    let mut w = store.new_writer(false);
    let (root, st, gen_s, set_s, seal_s, ov_mib);
    if order == "seq" {
        let mut ents = Vec::new();
        let mut raw = Vec::new();
        let mut tg = Duration::ZERO;
        let mut b = Builder::new(&mut map, &mut w);
        for li in 0..e.nleaves() {
            if li < e.pop_leaves() {
                let t = Instant::now();
                e.model.leaf(li, &mut ents);
                tg += t.elapsed();
                b.push_leaf(li, Some(&ents), virt, &mut raw).unwrap();
            } else {
                b.push_leaf(li, None, virt, &mut raw).unwrap();
            }
        }
        let (r, s) = b.finish().unwrap();
        root = r;
        st = s;
        gen_s = tg.as_secs_f64();
        set_s = 0.0;
        seal_s = 0.0;
        ov_mib = 0.0;
    } else {
        // random insertion order into the mutable overlay, then one seal.
        let n = e.cfg.nchunks;
        assert!(
            n.is_power_of_two(),
            "random order needs a power-of-two chunk count"
        );
        let k = n.trailing_zeros();
        let tg = Instant::now();
        let mut all: Vec<Ent> = Vec::with_capacity(n as usize);
        let mut ents = Vec::new();
        for li in 0..e.nleaves() {
            e.model.leaf(li, &mut ents);
            all.extend_from_slice(&ents);
        }
        gen_s = tg.elapsed().as_secs_f64();
        let cnt = n * e.pct / 100;
        let t = Instant::now();
        for i in 0..cnt {
            let ci = perm(i, k);
            map.set(ci, all[ci as usize]).unwrap();
        }
        set_s = t.elapsed().as_secs_f64();
        ov_mib = map.overlay.len() as f64 * e.cfg.fan as f64 * 48.0 / 1048576.0;
        drop(all);
        let t = Instant::now();
        let s = map.seal(&mut w).unwrap();
        seal_s = t.elapsed().as_secs_f64();
        root = map.root;
        st = s;
    }
    let wall = t_all.elapsed().as_secs_f64();
    let tib = e.tib;
    println!(
        "{},{order},{virt},{},{},{},{:.2},{:.2},{:.2},{:.2},{:.2},{:.2},{:.2},{:.1},{:.1},{:.1},{},{:.2},{:.2},{},{:.0},{:.2},{:.2},{:.0}",
        e.tag(),
        e.cfg.depth(),
        e.cfg.nchunks,
        st.leaves,
        wall,
        wall - gen_s,
        gen_s,
        st.t_enc.as_secs_f64(),
        st.t_hash.as_secs_f64(),
        st.t_comp.as_secs_f64(),
        st.t_write.as_secs_f64(),
        st.leaf_raw as f64 / 1048576.0,
        st.leaf_stored as f64 / 1048576.0,
        st.leaf_stored as f64 / 1048576.0 / (tib * e.pct as f64 / 100.0),
        st.interior,
        st.int_raw as f64 / 1048576.0,
        st.int_stored as f64 / 1048576.0,
        &hex(&root.hash)[..16],
        hwm_mib(),
        set_s,
        seal_s,
        ov_mib
    );
    let suffix = if order == "seq" { "" } else { "-rand" };
    save_root(&e.root_path(suffix), &root);
    if order != "seq" && !a.flag("--keep") {
        w.remove();
    }
}

const CSV_SEAL: &str = "system,tag,pattern,D,rep,chunks_dirtied,set_ms,seal_ms,enc_ms,hash_ms,comp_ms,write_ms,hash_share,leaves,interior_nodes,raw_mib,stored_mib,leaf_stored_kib_per_leaf,amp_stored_over_dirty_data,peak_rss_mib,virt_regen_ms_in_set";

fn cmd_seal(a: &Args) {
    let e = env(a);
    let base = load_root(&e.root_path(""));
    let store = e.store();
    let mut map = Map::open(e.cfg, store.clone(), base, 256 << 20);
    let t = Instant::now();
    let nint = map.warm_interior().unwrap();
    eprintln!(
        "warm: {nint} interior nodes, {:.2} s",
        t.elapsed().as_secs_f64()
    );
    let base_interior = map.interior.clone();
    let patterns: Vec<String> = a
        .s("--pattern", "uniform,zipf,seq")
        .split(',')
        .map(|s| s.to_string())
        .collect();
    let ds = a.list("--d", "1,10,100,1000,10000,100000");
    let reps = a.u("--reps", 3);
    let discard_above = a.u("--discard-above", 10000);
    let with_flat = a.flag("--flat");
    let mut flats: Vec<(Flat, Vec<[u8; 32]>)> = Vec::new();
    if with_flat {
        for bb in a.list("--flat-blocks", "1048576,65536") {
            let mut f = Flat::new(e.model, e.cfg.nchunks, bb as usize);
            let s = f.full_seal(e.pop_chunks());
            println!(
                "flat-full,{},{}B,blocks={},raw_mib={:.0},stored_mib={:.0},seal_s={:.2},comp_s={:.2},hash_s={:.2},manifest_kib={:.0},gen_s_excluded={:.2}",
                e.tag(),
                bb,
                s.blocks,
                s.raw as f64 / 1048576.0,
                s.stored as f64 / 1048576.0,
                s.t_total.as_secs_f64(),
                s.t_comp.as_secs_f64(),
                s.t_hash.as_secs_f64(),
                s.manifest_bytes as f64 / 1024.0,
                s.t_gen.as_secs_f64()
            );
            let m0 = f.manifest.clone();
            flats.push((f, m0));
        }
    }
    println!("{CSV_SEAL}");
    for pattern in &patterns {
        for &d in &ds {
            for rep in 0..reps {
                let mut r = Rng(0xD1_0000 + rep * 7919 + d);
                let pos = positions(pattern, d, &e, &mut r);
                let seed = 1000 + rep;
                map.root = base;
                map.overlay.clear();
                map.interior = base_interior.clone();
                store.reset_counters();
                reset_hwm();
                let t = Instant::now();
                for (ci, ent) in Edits::new(&pos, seed) {
                    map.set(ci, ent).unwrap();
                }
                let set_ms = ms(t.elapsed());
                let virt_ms = store.virt_ns.load(std::sync::atomic::Ordering::Relaxed) as f64 / 1e6;
                let mut w = store.new_writer(d > discard_above);
                let st = map.seal(&mut w).unwrap();
                let rss = hwm_mib();
                let nd = pos.len();
                println!(
                    "radix,{},{pattern},{d},{rep},{nd},{:.1},{:.2},{:.2},{:.2},{:.2},{:.2},{:.3},{},{},{:.2},{:.2},{:.2},{:.3},{:.0},{:.0}",
                    e.tag(),
                    set_ms,
                    ms(st.t_total),
                    ms(st.t_enc),
                    ms(st.t_hash),
                    ms(st.t_comp),
                    ms(st.t_write),
                    st.t_hash.as_secs_f64() / st.t_total.as_secs_f64().max(1e-9),
                    st.leaves,
                    st.interior,
                    st.raw() as f64 / 1048576.0,
                    st.stored() as f64 / 1048576.0,
                    st.leaf_stored as f64 / 1024.0 / st.leaves.max(1) as f64,
                    st.stored() as f64 / (nd as f64 * 65536.0),
                    rss,
                    virt_ms
                );
                w.remove();
                drop(pos);
                if with_flat {
                    let pos = positions(pattern, d, &e, &mut Rng(0xD1_0000 + rep * 7919 + d));
                    for (f, m0) in flats.iter_mut() {
                        f.manifest = m0.clone();
                        let mut it = Edits::new(&pos, seed);
                        let (fs, _) = f.delta_seal(e.pop_chunks(), &mut it);
                        println!(
                            "flat{},{},{pattern},{d},{rep},{nd},0,{:.2},0,{:.2},{:.2},{:.2},{:.3},{},0,{:.2},{:.2},0,{:.3},0,0",
                            f.rpb * 48,
                            e.tag(),
                            ms(fs.t_total),
                            ms(fs.t_hash),
                            ms(fs.t_comp),
                            ms(fs.t_manifest),
                            fs.t_hash.as_secs_f64() / fs.t_total.as_secs_f64().max(1e-9),
                            fs.blocks,
                            fs.raw as f64 / 1048576.0,
                            (fs.stored + fs.manifest_bytes) as f64 / 1048576.0,
                            (fs.stored + fs.manifest_bytes) as f64 / (nd as f64 * 65536.0)
                        );
                    }
                }
            }
        }
    }
}

fn cmd_diff(a: &Args) {
    let e = env(a);
    let base = load_root(&e.root_path(""));
    let store = e.store();
    let mut map = Map::open(e.cfg, store.clone(), base, 256 << 20);
    map.warm_interior().unwrap();
    let base_interior = map.interior.clone();
    let ks = a.list("--k", "1,100,1000,10000");
    let depth = e.cfg.depth();
    let mut flats = Vec::new();
    for bb in a.list("--flat-blocks", "1048576,65536") {
        let mut f = Flat::new(e.model, e.cfg.nchunks, bb as usize);
        f.full_seal(e.pop_chunks());
        let m0 = f.manifest.clone();
        flats.push((f, m0));
    }
    println!("system,tag,k,rep,depth,diff_ms_warm,nodes_read,bound_2kd,ranges,differing_leaves_expected,entries_diff_ms_first,entries_diff_ms_second,entries_differing,flat_manifest_cmp_ms,flat_block_cmp_ms,flat_blocks_read,flat_rows_differing");
    for &k in &ks {
        for rep in 0..a.u("--reps", 3) {
            let mut r = Rng(0xD2_0000 + rep * 31 + k);
            let pos = positions("uniform", k, &e, &mut r);
            map.root = base;
            map.overlay.clear();
            map.interior = base_interior.clone();
            for (ci, ent) in Edits::new(&pos, 77 + rep) {
                map.set(ci, ent).unwrap();
            }
            let mut w = store.new_writer(false);
            map.seal(&mut w).unwrap();
            let newroot = map.root;
            let t = Instant::now();
            let (ranges, reads) = map.diff(base, newroot).unwrap();
            let dt = ms(t.elapsed());
            let nleaves_changed: u64 = ranges.iter().map(|r| r.1 - r.0).sum();
            assert_eq!(
                nleaves_changed, k,
                "diff must find exactly the dirtied leaves"
            );
            assert!(
                reads <= 2 * k * depth as u64,
                "nodes read {reads} > 2*k*depth"
            );
            let t = Instant::now();
            let n1 = map.diff_entries(base, newroot, &ranges).unwrap();
            let e1 = ms(t.elapsed());
            let t = Instant::now();
            let n2 = map.diff_entries(base, newroot, &ranges).unwrap();
            let e2 = ms(t.elapsed());
            assert_eq!(n1, n2);
            assert_eq!(n1, k, "one chunk edited per leaf");
            map.root = newroot;
            let mut flat_cols = String::new();
            for (f, m0) in flats.iter_mut() {
                f.manifest = m0.clone();
                let (_, changed) = f.delta_seal(e.pop_chunks(), &mut Edits::new(&pos, 77 + rep));
                let ff: &Flat = f;
                let edits: Vec<(u64, Ent)> = Edits::new(&pos, 77 + rep).collect();
                let patched = |blk: u64, buf: &mut Vec<u8>| {
                    let (mut sc, mut last) = (Vec::new(), u64::MAX);
                    ff.gen_block(blk, e.pop_chunks(), buf, &mut sc, &mut last);
                    let lo = edits.partition_point(|x| x.0 < blk * ff.rpb);
                    for (ci, ent) in edits[lo..].iter().take_while(|x| x.0 < (blk + 1) * ff.rpb) {
                        let o = (ci - blk * ff.rpb) as usize * flat::ROW;
                        buf[o..o + 32].copy_from_slice(&ent.id);
                        buf[o + 32..o + 40].copy_from_slice(&ent.seg.to_le_bytes());
                        buf[o + 40..o + 44].copy_from_slice(&ent.off.to_le_bytes());
                        buf[o + 44..o + 48].copy_from_slice(&ent.clen.to_le_bytes());
                    }
                };
                let (tm, tb, nb, rows) = ff.diff(m0, &changed, e.pop_chunks(), &patched);
                flat_cols.push_str(&format!(
                    "flat{}B,{:.3},{:.1},{},{};",
                    f.rpb * 48,
                    ms(tm),
                    ms(tb),
                    nb,
                    rows
                ));
            }
            println!(
                "radix,{},{k},{rep},{depth},{dt:.3},{reads},{},{},{k},{e1:.1},{e2:.1},{n1},{}",
                e.tag(),
                2 * k * depth as u64,
                ranges.len(),
                flat_cols
            );
            w.remove();
        }
    }
}

fn cmd_cold(a: &Args) {
    let e = env(a);
    let base = load_root(&e.root_path(""));
    let lat = a.u("--latency-ms", 25);
    println!(
        "tag,latency_ms,probe,depth,gets,first_get_ms,second_get_same_leaf_gets,found_hole,rss_mib"
    );
    for probe in 0..a.u("--probes", 3) {
        let store = e.store();
        store
            .latency_us
            .store(lat * 1000, std::sync::atomic::Ordering::Relaxed);
        let mut map = Map::open(e.cfg, store.clone(), base, 64 << 20);
        let ci = Rng(99 + probe).below(e.pop_chunks());
        let t = Instant::now();
        let r = map.get(ci).unwrap();
        let dt = ms(t.elapsed());
        let g1 = gets(&store);
        map.get(ci ^ 1).unwrap();
        let g2 = gets(&store) - g1;
        println!(
            "{},{lat},{probe},{},{g1},{dt:.1},{g2},{},{:.0}",
            e.tag(),
            e.cfg.depth(),
            r.is_none(),
            rss_mib()
        );
    }
}

fn cmd_sweep(a: &Args) {
    let e = env(a);
    let base = load_root(&e.root_path(""));
    let cache_mib = a.u("--cache-mib", 256);
    let n = a.u("--lookups", 1_000_000);
    let store = e.store();
    let cap = if cache_mib == 0 {
        usize::MAX
    } else {
        (cache_mib as usize) << 20
    };
    let rss0 = rss_mib();
    reset_hwm();
    let mut map = Map::open(e.cfg, store.clone(), base, cap);
    let mut r = Rng(0x5eed);
    let t = Instant::now();
    let mut found = 0u64;
    let mut checked = 0u64;
    let mut ents = Vec::new();
    for i in 0..n {
        let ci = r.below(e.pop_chunks());
        let l = map.get(ci).unwrap();
        found += l.is_some() as u64;
        if i % 1000 == 0 && e.virt {
            // spot-check against the model
            e.model.leaf(ci / e.cfg.fan as u64, &mut ents);
            let m = &ents[(ci % e.cfg.fan as u64) as usize];
            let l = l.expect("populated");
            assert_eq!((l.seg, l.off, l.clen), (m.seg, m.off, m.clen));
            checked += 1;
        }
    }
    let dt = t.elapsed().as_secs_f64();
    println!(
        "tag,cache_mib,lookups,found,spot_checked,secs,lookups_per_s,leaf_hits,leaf_miss,interior_reads,evictions,cached_leaves,cache_mib_used,interior_mib,rss_before_mib,rss_end_mib,peak_rss_mib\n{},{},{n},{found},{checked},{dt:.1},{:.0},{},{},{},{},{},{:.0},{:.1},{:.0},{:.0},{:.0}",
        e.tag(),
        if cache_mib == 0 { "unbounded".to_string() } else { cache_mib.to_string() },
        n as f64 / dt,
        map.ctr.leaf_hits,
        map.ctr.leaf_miss,
        map.ctr.int_reads,
        map.leaves.evictions,
        map.leaves.len(),
        map.leaves.bytes as f64 / 1048576.0,
        map.interior_bytes() as f64 / 1048576.0,
        rss0,
        rss_mib(),
        hwm_mib()
    );
}

/// Flat table held fully in memory (what a flat map would cost resident).
fn cmd_flatmem(a: &Args) {
    let e = env(a);
    let f = Flat::new(e.model, e.cfg.nchunks, 1 << 20);
    let (mut buf, mut sc, mut last) = (Vec::new(), Vec::new(), u64::MAX);
    let mut table: Vec<u8> = Vec::with_capacity(e.cfg.nchunks as usize * 48);
    reset_hwm();
    for b in 0..f.nblocks {
        f.gen_block(b, e.cfg.nchunks, &mut buf, &mut sc, &mut last);
        table.extend_from_slice(&buf);
    }
    let rss = rss_mib();
    let t = Instant::now();
    let mut stored = 0u64;
    let blk = f.rpb as usize * 48;
    let mut hs = Vec::new();
    for c in table.chunks(blk) {
        stored += lz4_flex::block::compress_prepend_size(c).len() as u64;
        hs.push(*blake3::hash(c).as_bytes());
    }
    println!(
        "tag,table_mib,rss_mib,full_seal_from_memory_s,stored_mib\n{},{:.0},{rss:.0},{:.2},{:.0}",
        e.tag(),
        table.len() as f64 / 1048576.0,
        t.elapsed().as_secs_f64(),
        stored as f64 / 1048576.0
    );
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().cloned().unwrap_or_default();
    let a = Args(args);
    match cmd.as_str() {
        "table" => cmd_table(),
        "build" => cmd_build(&a),
        "seal" => cmd_seal(&a),
        "diff" => cmd_diff(&a),
        "cold" => cmd_cold(&a),
        "sweep" => cmd_sweep(&a),
        "flatmem" => cmd_flatmem(&a),
        _ => eprintln!("usage: radixmap table|build|seal|diff|cold|sweep|flatmem [--tib N --fan F --leaf rb16|rb32|row48|row48lz4|row48z1 --pct P --dir D --virtual ...]"),
    }
}
