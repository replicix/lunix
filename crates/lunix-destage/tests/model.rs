//! The map after a cut equals a flat model of the overlay: random writes,
//! discards, cuts (some below the newest write, so later writes stay out of the
//! cut), hinted reopens of the index, all policies, worker counts and segment
//! sizes, at 16 KiB and 64 KiB chunks.

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};

use common::*;
use lunix_format::volume::CodecPolicy;
use lunix_format::WriteSeq;
use proptest::prelude::*;
use proptest::test_runner::{Config, TestRunner};

/// Device sizes in chunks: small (dense overwrites) and larger (many jobs,
/// the result window, more segments per cut).
const SMALL: u64 = 6;
const BIG: u64 = 40;

#[derive(Clone, Debug)]
enum Op {
    Write {
        chunk: u8,
        blk: u8,
        nblk: u8,
        kind: u8,
        seed: u16,
    },
    Discard {
        chunk: u8,
        blk: u8,
        nblk: u8,
    },
    Cut {
        back: u8,
    },
    ReopenIndex,
}

#[derive(Clone, Debug)]
struct Case {
    ops: Vec<Op>,
    policy: u8,
    workers: u8,
    batch: u8,
    small_segments: bool,
    codec: u8,
    big: bool,
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        5 => (0..BIG as u8, 0u8..16, 1u8..40, 0u8..5, any::<u16>())
            .prop_map(|(chunk, blk, nblk, kind, seed)| Op::Write { chunk, blk, nblk, kind, seed }),
        2 => (0..BIG as u8, 0u8..16, 1u8..40)
            .prop_map(|(chunk, blk, nblk)| Op::Discard { chunk, blk, nblk }),
        3 => (0u8..4).prop_map(|back| Op::Cut { back }),
        1 => Just(Op::ReopenIndex),
    ]
}

fn case() -> impl Strategy<Value = Case> {
    (
        prop::collection::vec(op(), 1..32),
        0u8..6,
        1u8..5,
        prop::sample::select(vec![1u8, 2, 16]),
        any::<bool>(),
        0u8..5,
        any::<bool>(),
    )
        .prop_map(
            |(ops, policy, workers, batch, small_segments, codec, big)| Case {
                ops,
                policy,
                workers,
                batch,
                small_segments,
                codec,
                big,
            },
        )
}

fn data(kind: u8, seed: u16, len: usize) -> Vec<u8> {
    match kind {
        0 => random_bytes(u64::from(seed), len),
        1 => text_bytes(u64::from(seed % 6), len),
        // Constants and a few repeating blocks make whole-chunk duplicates likely.
        2 => vec![(seed % 4) as u8 + 1; len],
        3 => vec![0; len],
        _ => random_bytes(u64::from(seed % 3), BLOCK)
            .iter()
            .copied()
            .cycle()
            .take(len)
            .collect(),
    }
}

fn run_case(cs: u32, c: &Case, cuts: &AtomicUsize) {
    let policies = [
        (CodecPolicy::None, 0),
        (CodecPolicy::Lz4, 0),
        (CodecPolicy::Zstd, 1),
        (CodecPolicy::Zstd, 3),
        (CodecPolicy::Auto, 1),
        (CodecPolicy::Auto, 5),
    ];
    let (policy, level) = policies[usize::from(c.policy)];
    let chunks = if c.big { BIG } else { SMALL };
    let mut rc = RigConfig::new(cs, chunks);
    rc.policy = policy;
    rc.level = level;
    rc.workers = usize::from(c.workers);
    rc.codec = codecs(cs).swap_remove(usize::from(c.codec)).1;
    if c.small_segments {
        rc.segment_target = 2 * u64::from(cs);
    }
    let mut r = Rig::new(rc);
    r.config.batch_chunks = usize::from(c.batch);
    let chunk_blocks = u64::from(cs) / BLOCK as u64;
    let total_blocks = chunks * chunk_blocks;
    let mut prev_cut = WriteSeq(0);

    let do_cut = |r: &Rig, back: u8, prev: &mut WriteSeq| {
        let last = r.last_seq();
        let cut_at = WriteSeq(last.0.saturating_sub(u64::from(back)).max(prev.0));
        *prev = cut_at;
        let cut = r.cut(cut_at);
        let out = r.destage(&cut).expect("destage");
        r.finish(&cut, out);
        cuts.fetch_add(1, Ordering::Relaxed);
        r.assert_equals(&r.model_at(cut_at));
    };

    for o in &c.ops {
        match *o {
            Op::Write {
                chunk,
                blk,
                nblk,
                kind,
                seed,
            } => {
                let start =
                    (u64::from(chunk) % chunks) * chunk_blocks + u64::from(blk) % chunk_blocks;
                let n = u64::from(nblk).min(total_blocks - start);
                let bytes = data(kind, seed, n as usize * BLOCK);
                r.write(start as usize * BLOCK, &bytes);
            }
            Op::Discard { chunk, blk, nblk } => {
                let start =
                    (u64::from(chunk) % chunks) * chunk_blocks + u64::from(blk) % chunk_blocks;
                let n = u64::from(nblk).min(total_blocks - start);
                r.discard(start as usize * BLOCK, n as usize * BLOCK);
            }
            Op::Cut { back } => do_cut(&r, back, &mut prev_cut),
            Op::ReopenIndex => r.reopen_index_hinted(),
        }
    }
    do_cut(&r, 0, &mut prev_cut);
}

fn model(cs: u32) {
    let cuts = AtomicUsize::new(0);
    let mut runner = TestRunner::new(Config {
        cases: 500,
        failure_persistence: None,
        ..Config::default()
    });
    runner
        .run(&case(), |c| {
            run_case(cs, &c, &cuts);
            Ok(())
        })
        .unwrap_or_else(|e| panic!("{e}"));
    let n = cuts.load(Ordering::Relaxed);
    assert!(n >= 2000, "only {n} random cuts ran");
    println!("random cuts at {cs}: {n}");
}

#[test]
fn the_map_equals_the_flat_model_at_16_kib() {
    model(16 << 10);
}

#[test]
fn the_map_equals_the_flat_model_at_64_kib() {
    model(64 << 10);
}
