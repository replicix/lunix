//! The map walk keeps its pack descriptors bounded: a volume with far more
//! live packs than the descriptor limit still checks clean.

mod common;

use std::process::Command;

use common::*;
use lunix_format::CryptoMode;

#[test]
fn more_packs_than_the_descriptor_limit() {
    let mut s = Spec::new(CryptoMode::Plain, 16 * 1024);
    s.chunks = 4096;
    s.blobs = 40;
    s.per_commit = 24;
    s.commits = 100;
    s.retain_last = 100;
    s.journal_records = 10;
    let b = build(&s);
    assert!(b.known.packs.len() >= 90, "{} packs", b.known.packs.len());
    let bin = env!("CARGO_BIN_EXE_lunix-fsck");
    let out = Command::new("sh")
        .arg("-c")
        .arg("ulimit -n 40; exec \"$0\" --json \"$1\"")
        .arg(bin)
        .arg(&b.dir)
        .output()
        .expect("run");
    let text = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(0), "{text}");
}
