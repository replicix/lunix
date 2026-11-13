//! Throughput gates (hard floors from the plan). Run in release:
//! `cargo test --release -p lunix-destage --test perf -- --ignored --nocapture`.

mod common;

use std::time::Instant;

use common::*;
use lunix_destage::scan::is_all_zero;
use lunix_destage::DestageConfig;
use lunix_iface::crypto::ExtentCodec;
use lunix_testkit::PlainCodec;

const CS: u32 = 64 << 10;

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

#[test]
#[ignore = "performance gate: release build, quiet host"]
fn zero_scan_is_at_least_20_gb_per_second_per_core() {
    let buf = vec![0u8; CS as usize];
    let mut rates = Vec::new();
    for _ in 0..7 {
        let iters = 40_000u64;
        let t = Instant::now();
        let mut zeros = 0u64;
        for _ in 0..iters {
            zeros += u64::from(is_all_zero(std::hint::black_box(&buf)));
        }
        std::hint::black_box(zeros);
        rates.push(iters as f64 * f64::from(CS) / t.elapsed().as_secs_f64() / 1e9);
    }
    let r = median(rates);
    println!("zero scan, 64 KiB, all zero (worst case): {r:.1} GB/s per core");
    assert!(r >= 20.0, "{r:.1} GB/s");
}

#[test]
#[ignore = "performance gate: release build, quiet host"]
fn blake3_is_at_least_4_gb_per_second_per_core_at_64_kib() {
    let buf = random_bytes(1, CS as usize);
    let codec = PlainCodec::unkeyed();
    let mut rates = Vec::new();
    let mut rates_codec = Vec::new();
    for _ in 0..7 {
        let iters = 10_000u64;
        let t = Instant::now();
        for _ in 0..iters {
            std::hint::black_box(blake3::hash(std::hint::black_box(&buf)));
        }
        rates.push(iters as f64 * f64::from(CS) / t.elapsed().as_secs_f64() / 1e9);
        let t = Instant::now();
        for _ in 0..iters {
            std::hint::black_box(
                codec.chunk_id(lunix_format::ExtentKind::Data, std::hint::black_box(&buf)),
            );
        }
        rates_codec.push(iters as f64 * f64::from(CS) / t.elapsed().as_secs_f64() / 1e9);
    }
    let (r, rc) = (median(rates), median(rates_codec));
    println!("BLAKE3 64 KiB: {r:.2} GB/s per core (through ExtentCodec::chunk_id: {rc:.2})");
    assert!(r >= 4.0, "{r:.2} GB/s");
}

#[test]
#[ignore = "performance gate: release build, quiet host"]
fn destage_throughput_with_8_workers_on_text() {
    const CHUNKS: u64 = 2048; // 128 MiB
    let mut runs = Vec::new();
    let mut last = None;
    for _ in 0..5 {
        let mut c = RigConfig::new(CS, CHUNKS);
        c.workers = 8;
        let mut r = Rig::new(c);
        r.config = DestageConfig {
            workers: 8,
            lz4_cut_bytes: None,
            raw_cut_bytes: None,
            ..r.config
        };
        for ci in 0..CHUNKS {
            r.write_chunk(ci, &text_bytes(ci, CS as usize));
        }
        let cut = r.cut(r.last_seq());
        let t = Instant::now();
        let out = r.destage(&cut).expect("destage");
        let wall = t.elapsed();
        runs.push(out.stats.bytes_in as f64 / wall.as_secs_f64() / 1e9);
        last = Some(out.stats);
        r.finish(&cut, out);
        r.assert_equals(&r.model_at(r.last_seq()));
    }
    let s = last.expect("a run");
    let gbps = median(runs.clone());
    let bytes = s.bytes_in as f64;
    println!(
        "destage 8 workers, 128 MiB text, 64 KiB chunks: runs {runs:.2?} GB/s, median {gbps:.2}"
    );
    println!(
        "ratio stored/in: {:.3}; per-stage CPU (all workers), GB/s of input per core-second:",
        s.stored_clen() as f64 / bytes
    );
    for (name, ns) in [
        ("assemble", s.assemble_ns),
        ("zero scan", s.scan_ns),
        ("hash", s.hash_ns),
        ("dedup", s.dedup_ns),
        ("compress", s.compress_ns),
        ("seal", s.seal_ns),
        ("append (in order, 1 thread)", s.append_ns),
        ("map", s.map_ns),
    ] {
        println!(
            "  {name:30} {:>10.1} ms  {:>8.2} GB/s",
            ns as f64 / 1e6,
            bytes / ns.max(1) as f64
        );
    }
    println!(
        "DESIGN section 20 target 3 GB/s: {}",
        if gbps >= 3.0 { "met" } else { "not met" }
    );
    // The floor is per run, not per median.
    let worst = runs.iter().copied().fold(f64::INFINITY, f64::min);
    assert!(
        worst >= 1.0,
        "hard floor 1.0 GB/s per run, worst run {worst:.2}"
    );
}
