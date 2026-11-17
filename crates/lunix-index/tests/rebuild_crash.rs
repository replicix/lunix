//! `rebuild` is atomic (M14 re-review R-1): a kill before its one manifest
//! swap leaves the old index, a kill after it the new one; no reopen ever
//! sees an empty index. A child process rebuilds and is killed (SIGKILL) at
//! the point; the parent reopens the directory. Needs `--features
//! crashpoints`.
#![cfg(feature = "crashpoints")]

mod common;

use std::collections::BTreeMap;
use std::os::unix::process::ExitStatusExt;
use std::process::Command;

use common::{cid, hint, open, seg, Footers, Ids};
use lunix_format::{ChunkId, Hint};
use lunix_iface::index::ChunkIndex;
use lunix_iface::store::IdAllocator;
use lunix_index::LsmIndex;

fn footers() -> Footers {
    let f = Footers::default();
    f.add(seg(1), &[(cid(1), hint(1, 0)), (cid(2), hint(1, 10))]);
    f.add(seg(2), &[(cid(2), hint(2, 0)), (cid(3), hint(2, 10))]);
    f
}

fn entries(ix: &LsmIndex) -> BTreeMap<ChunkId, Hint> {
    let mut m = BTreeMap::new();
    ix.iter_entries(&mut |e| {
        m.insert(e.id, e.hint);
    })
    .expect("iter");
    m
}

/// The child: rebuilds from both segments.
#[test]
fn child_body() {
    let Some(dir) = std::env::var_os("LUNIX_REBUILD_CHILD_DIR") else {
        return;
    };
    let ids = Ids::new();
    ids.observe(2_000_000);
    let ix = open(std::path::Path::new(&dir), &ids);
    println!("rebuild start");
    ix.rebuild(&[seg(1), seg(2)], &footers()).expect("rebuild");
    println!("survived");
}

/// The old index holds segment 1 only, persisted.
fn old_index(dir: &std::path::Path) {
    let ids = Ids::new();
    let ix = open(dir, &ids);
    ix.insert(cid(1), hint(1, 0)).expect("insert");
    ix.insert(cid(2), hint(1, 10)).expect("insert");
    ix.persist(seg(1)).expect("persist");
}

fn kill_child_at(dir: &std::path::Path, point: &str) {
    let exe = std::env::current_exe().expect("exe");
    let out = Command::new(exe)
        .args(["--exact", "child_body", "--nocapture", "--test-threads=1"])
        .env("LUNIX_REBUILD_CHILD_DIR", dir)
        .env("LUNIX_CRASH_AT", point)
        .output()
        .expect("spawn child");
    let text = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.signal(), Some(9), "killed at {point}: {text}");
    assert!(
        text.contains("rebuild start"),
        "killed inside the rebuild: {text}"
    );
}

#[test]
fn a_kill_before_the_swap_leaves_the_old_index() {
    let dir = tempfile::tempdir().expect("tmp");
    old_index(dir.path());
    kill_child_at(dir.path(), "index.before_manifest_swap");
    let ids = Ids::new();
    ids.observe(3_000_000);
    let m = entries(&open(dir.path(), &ids));
    assert_eq!(m.len(), 2, "the old entries: {m:?}");
    assert_eq!(m[&cid(2)], hint(1, 10));
}

#[test]
fn a_kill_after_the_swap_leaves_the_rebuilt_index() {
    let dir = tempfile::tempdir().expect("tmp");
    old_index(dir.path());
    kill_child_at(dir.path(), "index.after_manifest_swap");
    let ids = Ids::new();
    ids.observe(3_000_000);
    let m = entries(&open(dir.path(), &ids));
    assert_eq!(m.len(), 3, "the rebuilt entries: {m:?}");
    assert_eq!(m[&cid(2)], hint(2, 0), "highest segment wins");
}
