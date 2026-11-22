//! Milestone gcbatch: batching the index updates changes the cost of a pass,
//! not what it does. Seeded random volumes (writes, overwrites, discards,
//! cuts, budgeted steps, urgent passes, retention changes, clock jumps) are
//! driven through the collector and, after every call, the objects that exist,
//! the index flags, the segments the index has entries into that no longer
//! exist (the follow-up of a delete), the condemned list and the call's
//! progress are folded into one digest per seed. Each seed must delete segments,
//! or it pins nothing about deletes. The expected digests were produced by the
//! per-object collector (commit 1738f2e, before the change) running this
//! same workload; the batched collector must reproduce them exactly: the
//! same objects condemned and deleted at the same points.
//!
//! `LUNIX_GC_DIGESTS=print` prints the digests instead of comparing.

use std::collections::BTreeSet;
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
    // Entries into segments with no file: none for the per-object collector
    // at any call, and for the batched one only between a delete and the end
    // of its step.
    let here: BTreeSet<_> = s.existing().into_iter().filter(|(k, _)| *k == 1).collect();
    let mut gone = BTreeSet::new();
    s.index
        .iter_entries(&mut |e| {
            if !here.contains(&(1, e.hint.seg.raw())) {
                gone.insert(e.hint.seg.raw());
            }
        })
        .expect("entries");
    d.put(&format!("{gone:?}"));
}

fn trace(seed: u64, pipeline: bool, chunk_size: u32) -> (u64, u64) {
    let mut cfg = SimCfg::new(chunk_size, IdLen::Sixteen);
    cfg.gc.pipeline = pipeline;
    cfg.chunk_count = 96;
    let mut s = Sim::new(cfg);
    let mut z = seed;
    let mut d = Digest(0xcbf2_9ce4_8422_2325);
    let mut deleted = 0;
    let mut before: BTreeSet<(u8, u64)> = BTreeSet::new();
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
        let now: BTreeSet<_> = s.existing().into_iter().filter(|(k, _)| *k == 1).collect();
        deleted += before.difference(&now).count() as u64;
        before = now;
    }
    s.quiesce().expect("quiesce");
    observe(&s, &mut d);
    let now: BTreeSet<_> = s.existing().into_iter().filter(|(k, _)| *k == 1).collect();
    deleted += before.difference(&now).count() as u64;
    (d.0, deleted)
}

const SEEDS: u64 = 12;

fn digests(pipeline: bool, chunk_size: u32) -> Vec<(u64, u64)> {
    (0..SEEDS)
        .map(|k| trace(0x6763_6261_7463_6800 + k, pipeline, chunk_size))
        .collect()
}

fn check(name: &str, got: &[(u64, u64)], want: &[u64]) {
    let digests: Vec<u64> = got.iter().map(|g| g.0).collect();
    if std::env::var("LUNIX_GC_DIGESTS").is_ok_and(|v| v == "print") {
        let idle: Vec<usize> = got
            .iter()
            .enumerate()
            .filter(|(_, g)| g.1 == 0)
            .map(|(i, _)| i)
            .collect();
        eprintln!("DIGESTS {name} {digests:#x?} seeds that delete no segment: {idle:?}");
        return;
    }
    assert!(
        got.iter().all(|g| g.1 > 0),
        "{name}: a seed deletes no segment: {got:?}"
    );
    assert_eq!(
        digests, want,
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

const PIPELINED_16K: [u64; 12] = [
    11838385640782921851,
    12015783355913402454,
    18195595722017657706,
    12730649140715374883,
    18303281722859845342,
    11487029666268145741,
    2427264855359187231,
    17713181731897616089,
    5803104700612107110,
    17397157759735394813,
    12037204598047661019,
    7459656548615221218,
];
const PIPELINED_64K: [u64; 12] = [
    2417252883962830031,
    1915748981740358712,
    13647065932547179976,
    5910612645936589384,
    7779516139830359395,
    1039211265857033615,
    15726039369745758544,
    2058589337004930411,
    6841662282672422327,
    17964102502592398728,
    12037204598047661019,
    2785328998942951164,
];
const WHOLE_16K: [u64; 12] = [
    13681658128094051329,
    1241784618532613024,
    3907129506239904488,
    11289416236110711171,
    4539599748269809987,
    4417702069135488003,
    2137198271182498911,
    18113578616279462741,
    7783413334470055111,
    15913237111870611313,
    7742289507471666892,
    8143370713091996198,
];
