//! Milestone gcbatch: batching the index updates changes the cost of a pass,
//! not what it does. Seeded random volumes (writes, overwrites, discards,
//! cuts, budgeted steps, urgent passes, retention changes, clock jumps) are
//! driven through the collector and, after every call, the objects that exist,
//! the index flags, the condemned list and the call's progress are folded
//! into one digest per seed. The expected digests were produced by the
//! per-object collector (commit 1738f2e, before the change) running this
//! same workload; the batched collector must reproduce them exactly: the
//! same objects condemned and deleted at the same points.
//!
//! `LUNIX_GC_DIGESTS=print` prints the digests instead of comparing.

use std::time::Duration;

use lunix_format::IdLen;
use lunix_iface::index::ChunkIndex;

use super::sim::{Sim, SimCfg};

fn next(z: &mut u64) -> u64 {
    *z = z.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut x = *z;
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}

/// FNV-1a over the bytes of what the volume shows.
struct Digest(u64);

impl Digest {
    fn put(&mut self, what: &str) {
        for b in what.bytes() {
            self.0 = (self.0 ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
        }
        self.0 = (self.0 ^ 0xff).wrapping_mul(0x0100_0000_01b3);
    }
}

/// The state a caller of the collector can see after a call.
fn observe(s: &Sim, d: &mut Digest) {
    d.put(&format!("{:?}", s.existing()));
    d.put(&format!("{:?}", s.index.condemned().expect("flags")));
    d.put(&format!("{:?}", s.gc.condemned()));
}

fn trace(seed: u64, pipeline: bool, chunk_size: u32) -> u64 {
    let mut cfg = SimCfg::new(chunk_size, IdLen::Sixteen);
    cfg.gc.pipeline = pipeline;
    cfg.chunk_count = 96;
    let mut s = Sim::new(cfg);
    let mut z = seed;
    let mut d = Digest(0xcbf2_9ce4_8422_2325);
    for _ in 0..400 {
        let r = next(&mut z) % 100;
        match r {
            0..=44 => s.write(next(&mut z) % 96, next(&mut z) % 24),
            45..=49 => s.discard(next(&mut z) % 96),
            50..=64 => s.commit().expect("commit"),
            65..=82 => {
                let p = s.step().expect("step");
                d.put(&format!(
                    "{:?} {} {} {}",
                    p.state, p.done, p.freed, p.cut_wanted
                ));
                s.clock.advance(Duration::from_secs(1 + next(&mut z) % 40));
            }
            83..=90 => s.clock.advance(Duration::from_secs(20 + next(&mut z) % 80)),
            91..=94 => {
                let p = s.urgent(next(&mut z) % (512 << 10)).expect("urgent");
                d.put(&format!("urgent {:?} {} {}", p.state, p.done, p.freed));
            }
            95..=96 => s.set_retain(1 + (next(&mut z) % 4) as u32),
            _ => s.pass().expect("pass"),
        }
        observe(&s, &mut d);
    }
    s.quiesce().expect("quiesce");
    observe(&s, &mut d);
    d.0
}

const SEEDS: u64 = 12;

fn digests(pipeline: bool, chunk_size: u32) -> Vec<u64> {
    (0..SEEDS)
        .map(|k| trace(0x6763_6261_7463_6800 + k, pipeline, chunk_size))
        .collect()
}

fn check(name: &str, got: &[u64], want: &[u64]) {
    if std::env::var("LUNIX_GC_DIGESTS").is_ok_and(|v| v == "print") {
        eprintln!("DIGESTS {name} {got:#x?}");
        return;
    }
    assert_eq!(
        got, want,
        "{name}: the collector's observable behaviour changed"
    );
}

#[test]
fn whole_passes_16k_match_the_per_object_collector() {
    check("whole_16k", &digests(false, 16 << 10), &WHOLE_16K);
}

#[test]
fn pipelined_passes_16k_match_the_per_object_collector() {
    check("pipelined_16k", &digests(true, 16 << 10), &PIPELINED_16K);
}

#[test]
fn pipelined_passes_64k_match_the_per_object_collector() {
    check("pipelined_64k", &digests(true, 64 << 10), &PIPELINED_64K);
}

const WHOLE_16K: [u64; 12] = [
    612844424699793436,
    18006824350806627123,
    4580476386957499193,
    172857680477355618,
    15213540151460675862,
    9380260420232306636,
    5487669908520053746,
    17389320951358881008,
    6855468807149403526,
    15739128925605048348,
    12011226105290940595,
    11003734656755560509,
];
const PIPELINED_16K: [u64; 12] = [
    4394852085295473258,
    11568133837111888827,
    17461653064766327737,
    9361518550086293736,
    16127445191366084523,
    13756943279411243736,
    15623065082414817654,
    808217575163533452,
    8605576278565566807,
    7806507486412530092,
    15782508021711220530,
    5750446730878635747,
];
const PIPELINED_64K: [u64; 12] = [
    897695586466580832,
    13165085180012601707,
    11108870199421189377,
    11640417125915983233,
    13871300170816336458,
    5530106467113365984,
    10851106168117719719,
    981160336815861794,
    4109212094454554432,
    2501401681119798169,
    15782508021711220530,
    10239751018482542141,
];
