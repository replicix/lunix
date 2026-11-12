//! The performance gate against `bench/radixmap/RESULTS.md` (10 TiB, 64 KiB
//! chunks, fanout 512, one core). Ignored by default; run in release mode:
//!
//! ```text
//! cargo test --release -p lunix-map --test perf -- --ignored --nocapture --test-threads=1
//! ```
//!
//! The 10 TiB base volume is virtual, as in the spike: its 327 680 leaves are
//! regenerated from a deterministic model when a node source is asked for
//! them (and hash-verified like any other node); the 643 interior nodes are
//! real and live in memory packs. The model is `spikes/radixmap/src/model.rs`
//! (the leafenc `b-frag` worst case: 30 % of entries overwritten into
//! unrelated segments), with the extent overhead of 68 bytes so that
//! sequential entries form runs.

mod common;

use std::hint::black_box;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use common::{Blake3Codec, SplitMix};
use lunix_format::commit::MapRoot;
use lunix_format::node::{Child, DataEntry, Entry, Interior, Leaf, LeafParams, LeafView, NodeLoc};
use lunix_format::{ChunkId, Codec, ExtentKind, IdLen, IdPrefix, Level, LineageIdx, ObjId};
use lunix_iface::crypto::{EncodingDesc, ExtentCodec};
use lunix_iface::map::{BlockMap, ChunkNo, NodeSource};
use lunix_iface::store::{PackRead, PackStore};
use lunix_iface::Result;
use lunix_map::{MapConfig, RadixMap};
use lunix_testkit::MemPacks;

const FAN: u16 = 512;
const CHUNK: u64 = 65_536;
const NCHUNKS: u64 = 10 * (1 << 40) / CHUNK;
const NLEAVES: u64 = NCHUNKS / FAN as u64;
const OVH: u32 = 68;
const SEG_BYTES: u64 = 16 << 20;
const ID_LEN: IdLen = IdLen::Sixteen;

// ---- the synthetic volume ----------------------------------------------------

struct Rng(u64);

impl Rng {
    fn u(&mut self) -> u64 {
        let mut s = SplitMix(self.0);
        let v = s.next();
        self.0 = s.0;
        v
    }
    fn f(&mut self) -> f64 {
        (self.u() >> 11) as f64 / (1u64 << 53) as f64
    }
    fn below(&mut self, n: u64) -> u64 {
        ((u128::from(self.u()) * u128::from(n)) >> 64) as u64
    }
    fn nrm(&mut self) -> f64 {
        (0..12).map(|_| self.f()).sum::<f64>() - 6.0
    }
}

fn clen(r: &mut Rng) -> u32 {
    if r.f() < 0.08 {
        CHUNK as u32
    } else {
        let f = (r.nrm() * 0.15 + 0.45).clamp(0.06, 1.0);
        ((f * CHUNK as f64) as u32).clamp(256, CHUNK as u32)
    }
}

fn id_prefix(seed: u64) -> IdPrefix {
    let mut s = SplitMix(seed ^ 0xA5A5_5A5A_1234_5678);
    let mut id = [0u8; 32];
    for k in 0..4 {
        id[k * 8..k * 8 + 8].copy_from_slice(&s.next().to_le_bytes());
    }
    IdPrefix::of(&ChunkId(id), ID_LEN)
}

fn model_leaf(idx: u64) -> Leaf {
    let mut r = Rng(0x5eed_0007 ^ idx.wrapping_mul(0x9E37_79B9_7F4A_7C15));
    let seg_span = (NLEAVES * 4).max(1024);
    let mut seg = 1 + idx * 4;
    let mut off = 0u64;
    let first = idx * u64::from(FAN);
    let mut entries = Vec::with_capacity(usize::from(FAN));
    for j in 0..u64::from(FAN) {
        let c = clen(&mut r);
        let sz = u64::from(OVH) + u64::from(c);
        if off + sz > SEG_BYTES {
            seg += 1;
            off = 0;
        }
        let mut e = DataEntry {
            lineage: LineageIdx(0),
            seg: ObjId::new(0, seg).expect("seg"),
            off: off as u32,
            clen: c,
            prefix: id_prefix(first + j + 1),
        };
        off += sz;
        if r.f() < 0.30 {
            let c2 = clen(&mut r);
            e.prefix = id_prefix((1 << 62) | (first + j));
            e.seg = ObjId::new(0, 1 + r.below(seg_span)).expect("seg");
            e.off = r.below(SEG_BYTES - u64::from(OVH) - u64::from(c2)) as u32;
            e.clen = c2;
        }
        entries.push(Entry::Data(e));
    }
    Leaf { entries }
}

fn params() -> LeafParams {
    LeafParams {
        fanout: FAN,
        id_len: ID_LEN,
        ovh: OVH,
    }
}

fn seal_ext(codec: &dyn ExtentCodec, hash: [u8; 32], body: &[u8]) -> Vec<u8> {
    let desc = EncodingDesc {
        codec: Codec::Raw,
        level: Level(0),
        ulen: u32::try_from(body.len()).expect("small"),
        dict_id: None,
    };
    let mut out = Vec::new();
    codec
        .seal(ExtentKind::Node, &ChunkId(hash), &desc, body, &mut out)
        .expect("seal");
    out
}

fn virt_pack() -> ObjId {
    ObjId::new(0, 0xFF_FFFF).expect("id")
}

/// Interior nodes from memory packs, leaves from the model.
struct VirtSource {
    packs: MemPacks,
    codec: Arc<dyn ExtentCodec>,
    reads: AtomicU64,
    virt_reads: AtomicU64,
}

impl NodeSource for VirtSource {
    fn load_node(&self, loc: NodeLoc, out: &mut Vec<u8>) -> Result<()> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        if loc.pack != virt_pack() {
            return self.packs.read_node(loc, out);
        }
        self.virt_reads.fetch_add(1, Ordering::Relaxed);
        let mut body = Vec::with_capacity(16 << 10);
        model_leaf(u64::from(loc.off))
            .encode(&params(), &mut body)
            .expect("encode");
        let hash = self.codec.chunk_id(ExtentKind::Node, &body).0;
        out.extend_from_slice(&seal_ext(&*self.codec, hash, &body));
        Ok(())
    }
}

struct Base {
    src: Arc<VirtSource>,
    root: MapRoot,
    avg_leaf: f64,
}

/// Builds the interior nodes over the virtual leaves, bottom-up.
fn build_base(codec: &Arc<dyn ExtentCodec>) -> Base {
    let packs = MemPacks::new();
    let mut pack = packs.open_pack().expect("pack");
    let fan = usize::from(FAN);
    let mut level: Vec<Child> = Vec::new();
    let mut leaf_bytes = 0u64;
    let mut body = Vec::new();
    for li in 0..NLEAVES {
        body.clear();
        model_leaf(li).encode(&params(), &mut body).expect("encode");
        leaf_bytes += body.len() as u64;
        let hash = codec.chunk_id(ExtentKind::Node, &body).0;
        let len = u32::try_from(seal_ext(&**codec, hash, &body).len()).expect("len");
        level.push(Child {
            hash,
            loc: Some(NodeLoc {
                pack: virt_pack(),
                off: li as u32,
                len,
            }),
        });
    }
    let empty = lunix_format::node::empty_hashes(FAN, 6, |b| codec.chunk_id(ExtentKind::Node, b).0)
        .expect("empty");
    let mut lvl = 1u8;
    let top = loop {
        let mut next = Vec::new();
        for kids in level.chunks(fan) {
            let mut children = kids.to_vec();
            children.resize(
                fan,
                Child {
                    hash: empty[usize::from(lvl) - 1],
                    loc: None,
                },
            );
            let node = Interior {
                level: lvl,
                children,
            };
            let hash = codec
                .chunk_id(ExtentKind::Node, &node.hash_input().expect("input"))
                .0;
            let mut b = Vec::new();
            node.encode(&mut b).expect("encode");
            let loc = pack
                .append_node(&hash, &seal_ext(&**codec, hash, &b))
                .expect("append");
            next.push(Child {
                hash,
                loc: Some(loc),
            });
        }
        if next.len() == 1 && lvl >= 2 {
            break next[0];
        }
        level = next;
        lvl += 1;
    };
    pack.seal().expect("seal");
    let loc = top.loc.expect("loc");
    Base {
        src: Arc::new(VirtSource {
            packs,
            codec: codec.clone(),
            reads: AtomicU64::new(0),
            virt_reads: AtomicU64::new(0),
        }),
        root: MapRoot {
            hash: top.hash,
            pack: Some(loc.pack),
            off: loc.off,
            len: loc.len,
        },
        avg_leaf: leaf_bytes as f64 / NLEAVES as f64,
    }
}

fn open(base: &Base, codec: &Arc<dyn ExtentCodec>, cfg: MapConfig) -> RadixMap {
    RadixMap::open(codec.clone(), base.src.clone(), &base.root, NCHUNKS, cfg).expect("open")
}

fn status_kib(key: &str) -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .expect("status")
        .lines()
        .find_map(|l| l.strip_prefix(key))
        .and_then(|v| v.split_whitespace().next())
        .and_then(|v| v.parse().ok())
        .expect("field")
}

fn reset_hwm() {
    std::fs::write("/proc/self/clear_refs", "5").expect("clear_refs");
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

#[test]
#[ignore = "performance gate: run in release mode, see the module docs"]
fn the_phase_0_performance_gate() {
    let codec = Blake3Codec::arc();
    let t = Instant::now();
    let base = build_base(&codec);
    let base_rss = status_kib("VmRSS:") / 1024;
    println!(
        "base: {NLEAVES} virtual leaves, avg {:.1} KiB, built in {:.1} s, RSS {base_rss} MiB",
        base.avg_leaf / 1024.0,
        t.elapsed().as_secs_f64()
    );
    let cfg = MapConfig::new(FAN, ID_LEN);

    // First, in a clean process: a 256 MiB leaf cache over 1 M random lookups.
    {
        let cfg = MapConfig::new(FAN, ID_LEN).with_cache_max(256 << 20);
        let map = open(&base, &codec, cfg);
        map.warm_interior().expect("warm");
        reset_hwm();
        let start_rss = status_kib("VmRSS:") / 1024;
        let mut rng = SplitMix(77);
        let t = Instant::now();
        for _ in 0..1_000_000 {
            black_box(map.get(ChunkNo(rng.below(NCHUNKS))).expect("get"));
        }
        let hwm = status_kib("VmHWM:") / 1024;
        let s = map.cache_stats();
        println!(
            "1M random lookups, 256 MiB cache: peak RSS {hwm} MiB (start {start_rss} MiB), cache {:.0} MiB in {} leaves, {} interior, {:.1} s",
            s.cached_bytes as f64 / 1048576.0,
            s.cached_leaves,
            s.resident_interior,
            t.elapsed().as_secs_f64()
        );
        assert!(hwm <= 320, "peak RSS {hwm} MiB");
    }

    // Cold open and the first lookup read exactly `depth` nodes.
    {
        let map = open(&base, &codec, cfg);
        let depth = u64::from(map.levels());
        assert_eq!(depth, 4);
        let before = base.src.reads.load(Ordering::Relaxed);
        let ci = 98_765_432;
        assert!(map.get(ChunkNo(ci)).expect("get").is_some());
        let n = base.src.reads.load(Ordering::Relaxed) - before;
        println!("cold open + first lookup: {n} node reads (depth {depth})");
        assert_eq!(n, depth);
    }

    // Point lookup on an encoded leaf (decoded tier off).
    {
        let cfg = cfg.with_decoded_tier(0);
        let map = open(&base, &codec, cfg);
        map.warm_interior().expect("warm");
        let mut rng = SplitMix(5);
        // One hot leaf, then 256 leaves round-robin.
        for (label, nleaves) in [("one leaf", 1u64), ("256 leaves", 256)] {
            let lis: Vec<u64> = (0..nleaves).map(|_| rng.below(NLEAVES)).collect();
            for &li in &lis {
                map.get(ChunkNo(li * u64::from(FAN))).expect("load");
            }
            let probes: Vec<u64> = (0..1000)
                .map(|_| lis[rng.below(nleaves) as usize] * u64::from(FAN) + rng.below(512))
                .collect();
            let mut per = Vec::new();
            for _ in 0..2000 {
                let t = Instant::now();
                for &ci in &probes {
                    black_box(map.get(ChunkNo(ci)).expect("get"));
                }
                per.push(t.elapsed().as_nanos() as f64 / probes.len() as f64);
            }
            println!(
                "get on an encoded leaf, {label}: median {:.0} ns (min {:.0}, p90 {:.0})",
                median(per.clone()),
                per.iter().copied().fold(f64::MAX, f64::min),
                {
                    let mut s = per.clone();
                    s.sort_by(f64::total_cmp);
                    s[s.len() * 9 / 10]
                }
            );
        }
        // The leaf alone, for scale.
        let mut body = Vec::new();
        model_leaf(12345).encode(&params(), &mut body).expect("enc");
        let probes: Vec<usize> = (0..1000).map(|_| rng.below(512) as usize).collect();
        let mut per = Vec::new();
        for _ in 0..2000 {
            let t = Instant::now();
            for &s in &probes {
                let v = LeafView::parse(black_box(&body), &params()).expect("parse");
                black_box(v.get(s).expect("get"));
            }
            per.push(t.elapsed().as_nanos() as f64 / probes.len() as f64);
        }
        println!("LeafView::parse + get alone: median {:.0} ns", median(per));
    }

    // Seal of 1 000 scattered dirty leaves.
    {
        let mut times = Vec::new();
        let mut stored = 0u64;
        let mut written = 0u64;
        for rep in 0..5u64 {
            let map = open(&base, &codec, cfg);
            map.warm_interior().expect("warm");
            let mut rng = SplitMix(0xD1_0000 + rep);
            let mut alloc = 0u64;
            let mut txn = map.begin_update().expect("begin");
            let mut done = std::collections::HashSet::new();
            while done.len() < 1000 {
                let li = rng.below(NLEAVES);
                if !done.insert(li) {
                    continue;
                }
                let ci = li * u64::from(FAN) + rng.below(512);
                alloc += 1;
                let e = DataEntry {
                    lineage: LineageIdx(0),
                    seg: ObjId::new(0, (1 << 30) + alloc / 200).expect("seg"),
                    off: ((alloc % 200) * 70_000) as u32,
                    clen: 30_000,
                    prefix: id_prefix((3 << 62) | alloc),
                };
                txn.set(ChunkNo(ci), e).expect("set");
            }
            txn.commit().expect("commit");
            let packs = MemPacks::new();
            let mut pack = packs.open_pack().expect("pack");
            let t = Instant::now();
            let res = map.seal(&mut *pack).expect("seal");
            let ms = t.elapsed().as_secs_f64() * 1e3;
            times.push(ms);
            stored = res.bytes;
            written = res.dirty_leaves;
            println!(
                "  seal rep {rep}: {ms:.1} ms, {} dirty leaves written",
                res.dirty_leaves
            );
        }
        println!(
            "seal of 1000 scattered dirty leaves: median {:.1} ms (spike 59.7 ms, limit 119.4 ms), {:.1} MiB stored, {written} leaves",
            median(times.clone()),
            stored as f64 / 1048576.0
        );
        assert!(median(times) <= 119.4);
    }
}
