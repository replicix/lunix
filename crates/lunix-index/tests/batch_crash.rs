//! A batched condemn or retire is one manifest swap: a kill (SIGKILL) at the
//! swap leaves all of the batch or none of it, and never a segment whose flag
//! is cleared while its entries are still reachable. A child process does the
//! batch; the parent counts the swaps and reopens the directory. Needs
//! `--features crashpoints`.
#![cfg(feature = "crashpoints")]

mod common;

use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::Command;

use common::{cid, hint, open, seg, Ids};
use lunix_iface::index::ChunkIndex;
use lunix_iface::store::IdAllocator;

const SEGS: u64 = 12;

fn all() -> Vec<lunix_format::SegId> {
    (1..=SEGS).map(seg).collect()
}

/// The child: one batch, then (if it survives) the crash point counts.
#[test]
fn child_body() {
    let (Some(dir), Some(op)) = (
        std::env::var_os("LUNIX_BATCH_CHILD_DIR"),
        std::env::var("LUNIX_BATCH_CHILD_OP").ok(),
    ) else {
        return;
    };
    let ids = Ids::new();
    ids.observe(2_000_000);
    let ix = open(Path::new(&dir), &ids);
    println!("batch start");
    match op.as_str() {
        "condemn" => ix.mark_condemned_all(&all()).expect("condemn"),
        "retire" => ix.retire_segments(&all()).expect("retire"),
        other => panic!("unknown op {other}"),
    }
    println!("survived");
    lunix_iface::crashpoint::dump_counts().expect("counts");
}

/// A persisted index over `SEGS` segments, three chunks each; for `retire`
/// the segments are flagged first (the state a delete phase starts from).
fn prepare(dir: &Path, flagged: bool) {
    let ids = Ids::new();
    let ix = open(dir, &ids);
    for s in 1..=SEGS {
        for k in 0..3u64 {
            ix.insert(cid(s * 10 + k), hint(s, k as u32 * 8))
                .expect("insert");
        }
    }
    ix.persist(seg(SEGS)).expect("persist");
    if flagged {
        ix.mark_condemned_all(&all()).expect("flag");
    }
}

fn spawn(dir: &Path, op: &str, envs: &[(&str, &str)]) -> std::process::Output {
    let exe = std::env::current_exe().expect("exe");
    let mut c = Command::new(exe);
    c.args(["--exact", "child_body", "--nocapture", "--test-threads=1"])
        .env("LUNIX_BATCH_CHILD_DIR", dir)
        .env("LUNIX_BATCH_CHILD_OP", op);
    for (k, v) in envs {
        c.env(k, v);
    }
    c.output().expect("spawn child")
}

fn kill_at(dir: &Path, op: &str, point: &str) {
    let out = spawn(dir, op, &[("LUNIX_CRASH_AT", point)]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.signal(), Some(9), "killed at {point}: {text}");
    assert!(
        text.contains("batch start"),
        "killed inside the batch: {text}"
    );
}

/// What a reopen shows: per segment, (flagged, entries reachable).
fn state(dir: &Path) -> Vec<(bool, bool)> {
    let ids = Ids::new();
    ids.observe(3_000_000);
    let ix = open(dir, &ids);
    let flags = ix.condemned().expect("condemned");
    (1..=SEGS)
        .map(|s| {
            (
                flags.contains(&seg(s)),
                ix.get(&cid(s * 10)).expect("get").is_some(),
            )
        })
        .collect()
}

#[test]
fn a_batch_is_one_manifest_swap_however_many_segments() {
    for op in ["condemn", "retire"] {
        let dir = tempfile::tempdir().expect("tmp");
        prepare(dir.path(), op == "retire");
        let counts = dir.path().join("counts");
        let out = spawn(
            dir.path(),
            op,
            &[
                ("LUNIX_CRASH_AT", "none.never:1"),
                ("LUNIX_CRASH_COUNT", counts.to_str().expect("utf8")),
            ],
        );
        assert!(out.status.success(), "{op}: {out:?}");
        let text = std::fs::read_to_string(&counts).expect("counts");
        let hits = |name: &str| -> u64 {
            text.lines()
                .find_map(|l| l.strip_prefix(name)?.trim().parse().ok())
                .unwrap_or_else(|| panic!("no count for {name} in {text}"))
        };
        assert_eq!(hits("index.before_manifest_swap"), 1, "{op}: {text}");
        assert_eq!(hits("index.after_manifest_swap"), 1, "{op}: {text}");
    }
}

#[test]
fn a_kill_around_the_condemn_swap_flags_all_or_none() {
    for (point, flagged) in [
        ("index.before_manifest_swap", false),
        ("index.after_manifest_swap", true),
        ("index.after_condemn_flag", true),
    ] {
        let dir = tempfile::tempdir().expect("tmp");
        prepare(dir.path(), false);
        kill_at(dir.path(), "condemn", point);
        let st = state(dir.path());
        assert!(
            st.iter().all(|&(f, e)| f == flagged && e),
            "{point}: {st:?}"
        );
    }
}

#[test]
fn a_kill_around_the_retire_swap_never_clears_a_flag_over_live_entries() {
    for (point, retired) in [
        ("index.before_manifest_swap", false),
        ("index.after_manifest_swap", true),
    ] {
        let dir = tempfile::tempdir().expect("tmp");
        prepare(dir.path(), true);
        kill_at(dir.path(), "retire", point);
        let st = state(dir.path());
        for (s, &(flag, entries)) in st.iter().enumerate() {
            assert!(
                flag || !entries,
                "{point}: segment {} lost its flag with entries reachable",
                s + 1
            );
        }
        assert!(
            st.iter().all(|&(f, e)| (f, e) == (!retired, !retired)),
            "{point}: all or none: {st:?}"
        );
    }
}
