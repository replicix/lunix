//! Memory with many distinct extents: the figure the doc states, measured.

mod common;

use std::process::Command;

use common::*;
use lunix_format::CryptoMode;

#[test]
fn bytes_per_distinct_chunk() {
    let mut spec = Spec::new(CryptoMode::Integrity, 16 * 1024);
    spec.chunks = 1 << 19;
    spec.per_commit = 400_000;
    spec.blobs = 400_000;
    spec.commits = 1;
    spec.journal_records = 10;
    spec.compressible_only = true;
    spec.segment_target = 16 << 20;
    let b = build(&spec);
    let bin = env!("CARGO_BIN_EXE_lunix-fsck");
    let out = Command::new(bin)
        .arg("--json")
        .arg(&b.dir)
        .output()
        .expect("run");
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    let rss = v["peak_rss_kib"].as_u64().expect("rss") * 1024;
    let unique = v["ratios"]["unique_chunks"].as_u64().expect("unique");
    let mapped = v["ratios"]["mapped_chunks"].as_u64().expect("mapped");
    eprintln!(
        "memory (distinct extents): peak RSS {} MiB for {unique} distinct and {mapped} mapped chunks = {} bytes per distinct chunk",
        rss >> 20,
        rss / unique
    );
    assert_eq!(out.status.code(), Some(0), "{v}");
    assert_eq!(unique, b.known.unique);
    // A generous ceiling: the point is the printed figure.
    assert!(rss / unique < 2048, "{} bytes per chunk", rss / unique);
}
