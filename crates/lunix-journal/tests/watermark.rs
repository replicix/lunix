//! The flushed watermark (maintainer decision of 2026-11-14, on-disk-formats
//! §5.2): every record carries the highest `write_seq` a completed group flush
//! had made durable when it was appended; recovery reports the highest one
//! found, so the engine can tell real loss (a gap or damage at or below it)
//! from crash residue (above it).

mod common;

use std::path::Path;

use common::*;
use lunix_format::WriteSeq;
use lunix_iface::journal::{Journal, JournalLane};

fn lanes_of(j: &dyn Journal, n: u16) -> Vec<Box<dyn JournalLane>> {
    (0..n).map(|l| open_lane(j, l)).collect()
}

/// Writes one 4 KiB record on `lane` and waits for its completion.
fn put(lane: &mut dyn JournalLane, lba: u64) -> u64 {
    let a = write(lane, lba, 1);
    complete_appends(lane, 1);
    a.write_seq.0
}

/// The file offset of the record with `write_seq` `seq`, in any ring file.
fn find(dir: &Path, seq: u64) -> (std::path::PathBuf, usize) {
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let b = std::fs::read(e.path()).unwrap();
        let mut p = 4096;
        while p + 64 <= b.len() {
            if &b[p..p + 4] == b"LXJR"
                && u64::from_le_bytes(b[p + 8..p + 16].try_into().unwrap()) == seq
            {
                return (e.path(), p);
            }
            p += 8;
        }
    }
    panic!("record {seq} not found");
}

fn flushed_of(dir: &Path, seq: u64) -> u64 {
    let (f, p) = find(dir, seq);
    let b = std::fs::read(f).unwrap();
    u64::from_le_bytes(b[p + 56..p + 64].try_into().unwrap())
}

/// Damages the data of record `seq` (its CRC fails).
fn damage(dir: &Path, seq: u64) {
    let (f, p) = find(dir, seq);
    let mut b = std::fs::read(&f).unwrap();
    b[p + 100] ^= 0xff;
    std::fs::write(&f, &b).unwrap();
}

/// Removes record `seq` as if its batch had never been written (zeros).
fn erase(dir: &Path, seq: u64) {
    let (f, p) = find(dir, seq);
    let mut b = std::fs::read(&f).unwrap();
    b[p..p + 64 + 4096].fill(0);
    std::fs::write(&f, &b).unwrap();
}

/// Two lanes: seq 1 (lane 0) and 2 (lane 1), a group flush, then 3 (lane 0)
/// and 4 (lane 1) unflushed. Returns the journal directory.
fn two_lanes_two_rounds() -> (tempfile::TempDir, lunix_journal::JournalConfig) {
    let t = scratch();
    let cfg = small_cfg(t.path().join("journal"), 2, 4);
    let j = open_journal(&cfg);
    recover_all(&j);
    let mut l = lanes_of(&j, 2);
    assert_eq!(put(&mut *l[0], 10), 1);
    assert_eq!(put(&mut *l[1], 20), 2);
    group_flush(&j);
    assert_eq!(put(&mut *l[0], 11), 3);
    assert_eq!(put(&mut *l[1], 21), 4);
    drop(l);
    drop(j);
    (t, cfg)
}

#[test]
fn records_carry_the_watermark_and_recovery_reports_it() {
    let (_t, cfg) = two_lanes_two_rounds();
    let dir = cfg.dir.clone();
    assert_eq!(flushed_of(&dir, 1), 0, "nothing was flushed yet");
    assert_eq!(flushed_of(&dir, 2), 0);
    assert_eq!(
        flushed_of(&dir, 3),
        2,
        "appended after the flush of 1 and 2"
    );
    assert_eq!(flushed_of(&dir, 4), 2);
    let j = open_journal(&cfg);
    let r = recover_all(&j);
    assert_eq!(r.records.len(), 4);
    assert_eq!(r.flushed, WriteSeq(2));
    assert!(r.gap_at.is_none());
    // After prepare (the first lane open) the recovered records are durable:
    // the next record carries them as its watermark.
    let mut l = lanes_of(&j, 2);
    let s = put(&mut *l[0], 12);
    assert_eq!(s, 5);
    drop(l);
    drop(j);
    assert_eq!(flushed_of(&dir, 5), 4);
}

#[test]
fn a_gap_across_lanes_above_the_watermark_is_reported_with_it() {
    let (_t, cfg) = two_lanes_two_rounds();
    // Lane 0's record 3 never reached the disk (still in its open batch at a
    // kill); lane 1's record 4 did.
    erase(&cfg.dir, 3);
    let j = open_journal(&cfg);
    let r = recover_all(&j);
    assert_eq!(r.gap_at, Some(WriteSeq(3)));
    assert_eq!(
        r.flushed,
        WriteSeq(2),
        "the gap is above the watermark: residue"
    );
}

#[test]
fn damage_at_or_below_the_watermark_is_visible() {
    let t = scratch();
    let cfg = small_cfg(t.path().join("journal"), 1, 4);
    let j = open_journal(&cfg);
    recover_all(&j);
    let mut l = lanes_of(&j, 1);
    assert_eq!(put(&mut *l[0], 1), 1);
    group_flush(&j);
    assert_eq!(put(&mut *l[0], 2), 2);
    group_flush(&j);
    assert_eq!(put(&mut *l[0], 3), 3); // witnesses the flush of 2
    drop(l);
    drop(j);
    damage(&cfg.dir, 2);
    let j = open_journal(&cfg);
    let r = recover_all(&j);
    assert_eq!(r.gap_at, Some(WriteSeq(2)));
    assert_eq!(r.flushed, WriteSeq(2), "record 2 was durable: real loss");
    assert_eq!(r.damaged.len(), 1);
    assert_eq!(r.damaged[0].next_seq, Some(WriteSeq(3)));
}
