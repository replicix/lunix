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

#[test]
fn two_batches_in_flight_on_one_lane_lose_the_first_above_the_watermark() {
    let t = scratch();
    let cfg = small_cfg(t.path().join("journal"), 1, 4);
    let j = open_journal(&cfg);
    recover_all(&j);
    let mut l = lanes_of(&j, 1);
    assert_eq!(put(&mut *l[0], 1), 1);
    group_flush(&j);
    // Two batches after the flush, neither flushed: at a crash the second can
    // land while the first is torn.
    assert_eq!(put(&mut *l[0], 2), 2);
    assert_eq!(put(&mut *l[0], 3), 3);
    drop(l);
    drop(j);
    assert_eq!(
        flushed_of(&cfg.dir, 3),
        1,
        "only the flush of 1 is witnessed"
    );
    damage(&cfg.dir, 2);
    let j = open_journal(&cfg);
    let r = recover_all(&j);
    assert_eq!(r.gap_at, Some(WriteSeq(2)));
    assert_eq!(
        r.flushed,
        WriteSeq(1),
        "record 2 was never durable: residue"
    );
    assert_eq!(r.damaged.len(), 1);
    assert_eq!(r.damaged[0].next_seq, Some(WriteSeq(3)));
}

/// Review M1 (2026-11-14): `reinit_slot` (reclaim, or a bad file at open)
/// zeroes the body, syncs, writes a header with the new ordinal, syncs and
/// renames. A crash between the header and the rename leaves a valid header
/// that does not match the file name, over a zero body. No record is lost:
/// it must not be reported as damage, which `refuse` would refuse because the
/// lane's next record is far below the watermark.
#[test]
fn an_interrupted_reinitialisation_is_not_damage() {
    use lunix_format::journal::FileHeader;
    for body_dirty in [false, true] {
        let t = scratch();
        let cfg = small_cfg(t.path().join("journal"), 1, 4);
        let j = open_journal(&cfg);
        recover_all(&j);
        let mut l = lanes_of(&j, 1);
        for i in 0..40u64 {
            write(&mut *l[0], i * 16, 16);
            complete_appends(&mut *l[0], 1);
        }
        group_flush(&j);
        assert_eq!(put(&mut *l[0], 1000), 41, "witnesses the flush of 40");
        drop(l);
        drop(j);
        // The first file: the cut covers everything in it, so reclaim takes it.
        let (first, _) = find(&cfg.dir, 1);
        let cut = (1..=41u64)
            .take_while(|&s| find(&cfg.dir, s).0 == first)
            .last()
            .expect("records in the first file");
        assert!(cut < 40, "the first file holds part of the journal only");
        let mut b = std::fs::read(&first).unwrap();
        let mut h = FileHeader::decode(&b[..4096]).expect("header");
        h.ordinal = 99;
        h.generation = h.generation.rotate_left(7) | 1;
        let mut block = Vec::new();
        h.encode(&mut block).expect("encode");
        b.fill(0);
        b[..4096].copy_from_slice(&block);
        if body_dirty {
            b[3 * 4096 + 17] = 0xa5;
        }
        std::fs::write(&first, &b).unwrap();

        let j = open_journal(&cfg);
        let r = j.recover(None, WriteSeq(cut)).expect("recover");
        assert_eq!(r.flushed, WriteSeq(40));
        assert_eq!(r.gap_at, None);
        assert_eq!(r.next_seq, WriteSeq(42));
        assert_eq!(r.records.len() as u64, 41 - cut);
        if body_dirty {
            // Not what reinit_slot leaves: real damage, reported as before.
            assert_eq!(r.damaged.len(), 1, "{:?}", r.damaged);
            assert_eq!(r.damaged[0].next_seq, Some(WriteSeq(cut + 1)));
            continue;
        }
        assert!(r.damaged.is_empty(), "{:?}", r.damaged);
        // Opening a lane finishes the re-initialisation; the journal goes on.
        let mut l = lanes_of(&j, 1);
        assert_eq!(put(&mut *l[0], 2000), 42);
        group_flush(&j);
        drop(l);
        drop(j);
        let j = open_journal(&cfg);
        let r = j.recover(None, WriteSeq(cut)).expect("recover again");
        assert!(r.damaged.is_empty() && r.gap_at.is_none(), "{r:?}");
        assert_eq!(r.next_seq, WriteSeq(43));
    }
}

/// Review m3 (2026-11-14): a number found twice means an earlier wipe did not
/// happen. Recovery stops there and reports it apart from a gap, so the
/// engine refuses it at any watermark.
#[test]
fn a_duplicate_write_seq_is_reported_apart_from_a_gap() {
    let t = scratch();
    let cfg = small_cfg(t.path().join("journal"), 2, 4);
    let j = open_journal(&cfg);
    recover_all(&j);
    let mut l = lanes_of(&j, 2);
    assert_eq!(put(&mut *l[0], 10), 1);
    assert_eq!(put(&mut *l[1], 20), 2);
    assert_eq!(put(&mut *l[0], 11), 3);
    group_flush(&j);
    drop(l);
    drop(j);
    // Keep lane 1's file as it is, then lose record 2: the next incarnation
    // wipes lane 0 after record 1 and reissues 2 and 3 there.
    let (lane1, _) = find(&cfg.dir, 2);
    let saved = std::fs::read(&lane1).unwrap();
    erase(&cfg.dir, 2);
    let j = open_journal(&cfg);
    let r = recover_all(&j);
    assert_eq!(r.gap_at, Some(WriteSeq(2)));
    assert_eq!(r.duplicate, None, "a gap is not a duplicate");
    let mut l = lanes_of(&j, 2);
    assert_eq!(put(&mut *l[0], 12), 2);
    assert_eq!(put(&mut *l[0], 13), 3);
    group_flush(&j);
    drop(l);
    drop(j);
    // The wipe of lane 1 "did not happen": the old record 2 is back.
    std::fs::write(&lane1, &saved).unwrap();
    let j = open_journal(&cfg);
    let r = recover_all(&j);
    assert_eq!(r.duplicate, Some(WriteSeq(2)), "{r:?}");
    assert!(r.gap_at.is_some());
}

/// Review m1 (2026-11-14): a flush whose `upto` is above `last_issued()` is
/// clamped at submission. Unclamped, it would raise the watermark to a number
/// handed out later and not yet written, and a later record would claim it
/// durable.
#[test]
fn a_flush_above_last_issued_does_not_raise_the_watermark_past_it() {
    use lunix_format::journal::RecordKind;
    use lunix_iface::journal::SlotOutcome;
    let t = scratch();
    let cfg = small_cfg(t.path().join("journal"), 2, 4);
    let j = open_journal(&cfg);
    recover_all(&j);
    let mut l = lanes_of(&j, 2);
    assert_eq!(put(&mut *l[0], 1), 1);
    let (tx, rx) = std::sync::mpsc::channel();
    j.submit_group_flush(
        WriteSeq(1000),
        Box::new(move |r| {
            let _ = tx.send(r.is_ok());
        }),
    )
    .expect("submit");
    assert!(rx.recv_timeout(T).expect("flush completion"));
    // Lane 0 holds a slot for 2 (not written); lane 1 writes 3.
    let slot = match l[0].try_reserve(RecordKind::Write, 1).expect("reserve") {
        SlotOutcome::Reserved(s) => s,
        other => panic!("{other:?}"),
    };
    assert_eq!(slot.write_seq, WriteSeq(2));
    assert_eq!(put(&mut *l[1], 3), 3);
    assert_eq!(flushed_of(&cfg.dir, 3), 1, "record 2 was never written");
    l[0].append(slot, 2, &pattern(2, 1)).expect("append");
    complete_appends(&mut *l[0], 1);
}
