//! Each named crash point (M3 gate): a child process runs a workload and is
//! killed (SIGKILL) at the point; the parent then recovers the directory and
//! checks the guarantees. Needs `--features crashpoints`.
#![cfg(feature = "crashpoints")]

mod common;

use std::os::unix::process::ExitStatusExt;
use std::process::Command;

use common::*;
use lunix_format::WriteSeq;
use lunix_iface::journal::Journal;

const BIG: u32 = 16;
const LANES: u16 = 2;
const FILES: u64 = 3;
const MARK_IDLE: std::time::Duration = std::time::Duration::from_millis(5);

/// The child: writes 8 rounds of 24 records over two lanes, flushing after each
/// round and then idling past the mark delay (the coordinator writes a mark
/// witnessing the flush, so recovery sees how far durability reached), and
/// from round 3 on treats everything but the last 10 records as committed and
/// reclaims it. Progress goes to stdout: `flushed <seq>` after each completed
/// flush, `floor <seq>` before each reclaim.
#[test]
fn child_body() {
    let Some(dir) = std::env::var_os("LUNIX_CRASH_CHILD_DIR") else {
        return;
    };
    let mut cfg = small_cfg(std::path::PathBuf::from(dir), LANES, FILES);
    cfg.mark_idle = Some(MARK_IDLE);
    let j = open_journal(&cfg);
    recover_all(&j);
    let mut ls: Vec<_> = (0..LANES).map(|l| open_lane(&j, l)).collect();
    let mut last = 0u64;
    for round in 0..8 {
        for i in 0..24usize {
            let l = i % usize::from(LANES);
            let w = write_rec(&mut *ls[l], 0, BIG);
            complete_appends(&mut *ls[l], 1);
            last = w.seq;
        }
        group_flush(&j);
        println!("flushed {last}");
        std::thread::sleep(MARK_IDLE * 8);
        last = j.last_issued().0;
        if round >= 2 {
            let floor = last - 10;
            println!("floor {floor}");
            let tail = j.tail_after(WriteSeq(floor)).expect("tail");
            j.reclaim(&tail).expect("reclaim");
        }
    }
    println!("survived");
}

struct Outcome {
    killed: bool,
    flushed: u64,
    floor: u64,
}

fn run_child(dir: &std::path::Path, crash_at: &str) -> Outcome {
    let exe = std::env::current_exe().expect("exe");
    let out = Command::new(exe)
        .args(["--exact", "child_body", "--nocapture", "--test-threads=1"])
        .env("LUNIX_CRASH_CHILD_DIR", dir)
        .env("LUNIX_CRASH_AT", crash_at)
        .output()
        .expect("spawn child");
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let mut o = Outcome {
        killed: out.status.signal() == Some(9),
        flushed: 0,
        floor: 0,
    };
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("flushed ") {
            o.flushed = v.trim().parse().expect("seq");
        } else if let Some(v) = line.strip_prefix("floor ") {
            o.floor = v.trim().parse().expect("seq");
        }
    }
    if !o.killed {
        assert!(
            text.contains("survived"),
            "child neither killed nor done: {text}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    o
}

/// The `write_seq` of every mark in the mark ring files (one per block).
fn mark_seqs(dir: &std::path::Path) -> std::collections::BTreeSet<u64> {
    let mut out = std::collections::BTreeSet::new();
    for e in std::fs::read_dir(dir).expect("readdir").flatten() {
        if !e.file_name().to_string_lossy().starts_with("m00-") {
            continue;
        }
        let b = std::fs::read(e.path()).expect("read mark file");
        for blk in b.as_chunks::<4096>().0.iter().skip(1) {
            if &blk[..4] == b"LXJR" {
                out.insert(u64::from_le_bytes(blk[8..16].try_into().expect("seq")));
            }
        }
    }
    out
}

fn check_after_kill(point: &str, o: &Outcome, dir: &std::path::Path, witnessed: bool) {
    let cfg = small_cfg(dir.to_path_buf(), LANES, FILES);
    let j = open_journal(&cfg);
    // The floor the engine would have: a reclaim that started is covered by a
    // committed floor. If it was announced, records at or below it may be gone.
    let r = j.recover(None, WriteSeq(o.floor)).expect("recover");
    let got: Vec<u64> = r.records.iter().map(|x| x.record.write_seq.0).collect();
    let k = r.next_seq.0 - 1;
    // Every number in (floor, k] is a record or a mark in the mark ring
    // (marks are not listed as records).
    let marks = mark_seqs(dir);
    let want: Vec<u64> = (o.floor + 1..=k).filter(|s| !marks.contains(s)).collect();
    assert_eq!(got, want, "{point}: not a contiguous prefix");
    assert!(
        k >= o.flushed,
        "{point}: a flushed record was lost (flushed {}, recovered up to {k})",
        o.flushed
    );
    if witnessed {
        // Killed after the mark for the last reported flush was synced.
        assert!(
            r.flushed.0 >= o.flushed,
            "{point}: the mark did not witness flush {} (watermark {:?})",
            o.flushed,
            r.flushed
        );
    }
    // A kill loses no durable record, so `on_journal_damage = refuse` must
    // open: no gap and no damage at or below the flushed watermark (the
    // engine's rule, ADR-4).
    let w = r.flushed;
    assert!(
        !(r.gap_at.is_some_and(|g| g <= w)
            || r.damaged.iter().any(|d| d.next_seq.is_some_and(|n| n <= w))),
        "{point}: refuse would refuse a kill: gap {:?}, damaged {:?}, flushed {w:?}",
        r.gap_at,
        r.damaged
    );
    for rec in &r.records {
        let mut buf = vec![0u8; rec.record.blocks as usize * 4096];
        j.read_at(rec.loc, &mut buf).expect("read_at");
        assert_eq!(
            buf,
            pattern(rec.record.write_seq.0, rec.record.blocks),
            "{point}"
        );
    }
    // The journal is usable again and the new records survive a second crash.
    {
        let mut ls: Vec<_> = (0..LANES).map(|l| open_lane(&j, l)).collect();
        for i in 0..6usize {
            let l = i % usize::from(LANES);
            write(&mut *ls[l], 0, 1);
            complete_appends(&mut *ls[l], 1);
        }
        group_flush(&j);
    }
    drop(j);
    let j = open_journal(&cfg);
    let r2 = j.recover(None, WriteSeq(o.floor)).expect("recover");
    assert_eq!(r2.records.len(), got.len() + 6, "{point}");
    assert_eq!(r2.ignored_after_gap, None, "{point}");
}

fn kill_at(point: &str, hits: &[u32]) {
    kill_at_checked(point, hits, false);
}

fn kill_at_checked(point: &str, hits: &[u32], witnessed: bool) {
    for &hit in hits {
        let dir = scratch();
        let jdir = dir.path().join("journal");
        let spec = format!("{point}:{hit}");
        let o = run_child(&jdir, &spec);
        assert!(o.killed, "{spec}: the crash point was not reached");
        check_after_kill(&spec, &o, &jdir, witnessed);
    }
}

#[test]
fn kill_after_append_submit() {
    kill_at("journal.after_append_submit", &[1, 7, 40, 150]);
}

#[test]
fn kill_before_fdatasync() {
    kill_at("journal.before_fdatasync", &[1, 3, 8]);
}

#[test]
fn kill_after_fdatasync() {
    kill_at("journal.after_fdatasync", &[1, 3, 8]);
}

#[test]
fn kill_after_the_group_flush_watermark_wait() {
    kill_at("journal.group_flush.after_watermark_wait", &[1, 4, 8]);
}

#[test]
fn kill_before_reclaim_delete() {
    kill_at("journal.before_reclaim_delete", &[1, 2]);
}

#[test]
fn kill_after_reclaim_delete() {
    kill_at("journal.after_reclaim_delete", &[1, 2]);
}

/// Review M1 (2026-11-14): a reclaim killed between the new header and the
/// rename leaves a file whose header does not match its name. It is an
/// interrupted re-initialisation, not damage.
#[test]
fn kill_between_the_reinit_header_and_the_rename() {
    kill_at("journal.reinit.before_rename", &[1, 2, 5]);
}

/// Inside the idle delay's end: the mark is due but not written. The flush is
/// unwitnessed (the remaining window); everything still holds.
#[test]
fn kill_before_the_mark_write() {
    kill_at("journal.mark.before_write", &[1, 3, 7]);
}

/// After the idle delay: the mark is synced, so recovery's watermark covers
/// the last flush the child saw complete.
#[test]
fn kill_after_the_mark_sync() {
    kill_at_checked("journal.mark.after_sync", &[1, 3, 7], true);
}

#[test]
fn the_crash_points_are_registered_and_unique() {
    // Touch every point once so the registry sees them (they register on first
    // execution through inventory, at link time).
    let names = lunix_iface::crashpoint::registered();
    for n in [
        "journal.after_append_submit",
        "journal.before_fdatasync",
        "journal.after_fdatasync",
        "journal.group_flush.after_watermark_wait",
        "journal.before_reclaim_delete",
        "journal.after_reclaim_delete",
        "journal.reinit.before_rename",
        "journal.mark.before_write",
        "journal.mark.after_sync",
    ] {
        assert!(names.contains(&n), "{n} missing from {names:?}");
    }
    lunix_iface::crashpoint::check_registry().expect("registry");
}
