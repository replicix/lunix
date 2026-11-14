//! Memory: fsck of a 1 TiB sparse volume with 1 M mapped chunks stays under
//! 500 MiB. The binary runs in its own process and reports its peak resident
//! set (`VmHWM`) in the JSON, so the builder's memory is not counted.

mod common;

use std::process::Command;

use common::*;
use lunix_format::CryptoMode;

#[test]
fn a_one_tib_volume_with_a_million_mapped_chunks_stays_under_500_mib() {
    let mut spec = Spec::new(CryptoMode::Integrity, 1 << 20);
    spec.chunks = 1 << 20; // 1 TiB of 1 MiB chunks
    spec.per_commit = 1 << 20; // every chunk mapped
    spec.blobs = 64;
    spec.commits = 1;
    spec.journal_records = 20;
    spec.segment_target = 8 << 20;
    let t0 = std::time::Instant::now();
    let b = build(&spec);
    let built = t0.elapsed();
    assert_eq!(b.known.mapped, 1 << 20);
    assert!(b.known.dedup_ratio() > 10_000.0);

    let t1 = std::time::Instant::now();
    let out = Command::new(env!("CARGO_BIN_EXE_lunix-fsck"))
        .arg("--json")
        .arg(&b.dir)
        .output()
        .expect("run lunix-fsck");
    let took = t1.elapsed();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json output");
    let rss = v["peak_rss_kib"].as_u64().expect("peak_rss_kib");
    eprintln!(
        "memory gate: peak RSS {} MiB, check {:.1} s, build {:.1} s, exit {}, mapped {}, unique {}",
        rss / 1024,
        took.as_secs_f64(),
        built.as_secs_f64(),
        out.status.code().unwrap_or(-1),
        v["ratios"]["mapped_chunks"],
        v["ratios"]["unique_chunks"],
    );
    assert_eq!(out.status.code(), Some(0), "{v}");
    assert_eq!(v["ratios"]["mapped_chunks"], 1 << 20);
    assert_eq!(v["ratios"]["chunk_count"], 1 << 20);
    assert!(rss < 500 * 1024, "peak RSS {} MiB", rss / 1024);
}

#[test]
fn the_binary_exit_codes_and_usage() {
    let b = build(&Spec::new(CryptoMode::Plain, 16 * 1024));
    let bin = env!("CARGO_BIN_EXE_lunix-fsck");
    let ok = Command::new(bin).arg(&b.dir).output().expect("run");
    assert_eq!(ok.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&ok.stdout).contains("result: clean"));
    b.write("segments/0000000000000f01.open", b"x");
    let warn = Command::new(bin).arg(&b.dir).output().expect("run");
    assert_eq!(warn.status.code(), Some(1));
    b.flip(&b.seg_rel(b.known.segments[0]), 100);
    let err = Command::new(bin)
        .arg("--deep")
        .arg(&b.dir)
        .output()
        .expect("run");
    assert_eq!(err.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&err.stdout).contains("seg_hash"));
    let usage = Command::new(bin).output().expect("run");
    assert_eq!(usage.status.code(), Some(64));
    let usage = Command::new(bin)
        .arg("--nonsense")
        .arg(&b.dir)
        .output()
        .expect("run");
    assert_eq!(usage.status.code(), Some(64));
}
