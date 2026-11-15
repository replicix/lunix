//! The mark ring (amendment 2026-11-14, on-disk-formats §5.1 and §5.3): after
//! a group flush that raised the flushed watermark, the flush coordinator
//! writes a mark witnessing it unless a later group made a witness durable
//! within the idle delay. Recovery counts marks for contiguity and the
//! watermark and never lists them as records.

mod common;

use std::path::Path;
use std::time::Duration;

use common::*;
use lunix_format::journal::RecordKind;
use lunix_format::WriteSeq;
use lunix_iface::journal::{Journal, JournalLane, Recovered, MARK_RING_LANE};
use lunix_journal::JournalConfig;

/// The engine's rule for `on_journal_damage = refuse` (ADR-4).
fn refuse_worthy(r: &Recovered) -> bool {
    let w = r.flushed;
    r.duplicate.is_some()
        || r.gap_at.is_some_and(|g| g <= w)
        || r.damaged.iter().any(|d| d.next_seq.is_some_and(|n| n <= w))
}

fn cfg_with(dir: &Path, idle: Option<Duration>) -> JournalConfig {
    let mut c = small_cfg(dir.join("journal"), 1, 4);
    c.mark_idle = idle;
    c
}

/// Writes `n` one-block records on `lane`, each completed.
fn put_n(lane: &mut dyn JournalLane, n: u64) -> Vec<u64> {
    (0..n)
        .map(|i| {
            let w = write(lane, i, 1);
            complete_appends(lane, 1);
            w.write_seq.0
        })
        .collect()
}

/// The offset of the record numbered `seq` in the `q00` files.
fn find_q(dir: &Path, seq: u64) -> (std::path::PathBuf, usize) {
    find_in(dir, 'q', seq)
}

fn find_in(dir: &Path, prefix: char, seq: u64) -> (std::path::PathBuf, usize) {
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        if !e.file_name().to_string_lossy().starts_with(prefix) {
            continue;
        }
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

fn wait_marks(j: &lunix_journal::FileJournal, n: u64) {
    wait_until("marks", || j.marks_written() >= n);
}

#[test]
fn an_idle_flush_gets_one_mark_and_recovery_counts_it() {
    let t = scratch();
    let cfg = cfg_with(t.path(), Some(Duration::from_millis(20)));
    let j = open_journal(&cfg);
    recover_all(&j);
    let mut l = open_lane(&j, 0);
    put_n(&mut *l, 10);
    group_flush(&j);
    wait_marks(&j, 1);
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(
        j.marks_written(),
        1,
        "one mark per flush that raised the watermark"
    );
    assert_eq!(j.last_issued(), WriteSeq(11), "the mark took number 11");
    drop(l);
    drop(j);
    let (mark_file, _) = find_in(&cfg.dir, 'm', 11);
    assert!(mark_file
        .file_name()
        .unwrap()
        .to_string_lossy()
        .starts_with("m00-"));
    let j = open_journal(&cfg);
    let r = recover_all(&j);
    assert_eq!(r.records.len(), 10, "the mark is no record");
    assert_eq!(r.next_seq, WriteSeq(12));
    assert_eq!(r.flushed, WriteSeq(10), "the mark witnesses the flush");
    assert!(r.gap_at.is_none() && r.damaged.is_empty());
    // The next record continues after the mark.
    let mut l = open_lane(&j, 0);
    assert_eq!(put_n(&mut *l, 1), vec![12]);
}

/// The review's M2 probe: ten records, one FLUSH, no later write, the data of
/// record 3 damaged. Once the mark landed, the loss is inside witnessed
/// history and `refuse` refuses it; without marks it reads as residue.
#[test]
fn the_m2_probe_is_refuse_worthy_once_the_mark_landed() {
    for idle in [Some(Duration::from_millis(10)), None] {
        let t = scratch();
        let cfg = cfg_with(t.path(), idle);
        let j = open_journal(&cfg);
        recover_all(&j);
        let mut l = open_lane(&j, 0);
        put_n(&mut *l, 10);
        group_flush(&j);
        if idle.is_some() {
            wait_marks(&j, 1);
        }
        drop(l);
        drop(j);
        let (f, p) = find_q(&cfg.dir, 3);
        let mut b = std::fs::read(&f).unwrap();
        b[p + 100] ^= 0xff;
        std::fs::write(&f, &b).unwrap();
        let j = open_journal(&cfg);
        let r = recover_all(&j);
        assert_eq!(r.gap_at, Some(WriteSeq(3)));
        if idle.is_some() {
            assert_eq!(r.flushed, WriteSeq(10));
            assert!(refuse_worthy(&r), "{r:?}");
        } else {
            assert_eq!(r.flushed, WriteSeq(0), "nothing witnessed the flush");
            assert!(!refuse_worthy(&r), "the window: reads as residue");
        }
    }
}

/// A crash inside the idle delay: no mark yet, so the flush is unwitnessed
/// and damage there is plain crash residue (the remaining window).
#[test]
fn no_mark_before_the_idle_delay_ends() {
    let t = scratch();
    let cfg = cfg_with(t.path(), Some(Duration::from_secs(30)));
    let j = open_journal(&cfg);
    recover_all(&j);
    let mut l = open_lane(&j, 0);
    put_n(&mut *l, 5);
    group_flush(&j);
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(j.marks_written(), 0);
    drop(l);
    drop(j);
    let j = open_journal(&cfg);
    let r = recover_all(&j);
    assert_eq!(r.flushed, WriteSeq(0));
    assert_eq!(r.next_seq, WriteSeq(6), "no number was taken");
}

/// A later group flush whose `upto` is above the counter at the raise makes a
/// record carrying the watermark durable: no mark for the earlier flush, one
/// for the last.
#[test]
fn a_later_flush_within_the_idle_delay_makes_the_mark_unneeded() {
    let t = scratch();
    let cfg = cfg_with(t.path(), Some(Duration::from_millis(300)));
    let j = open_journal(&cfg);
    recover_all(&j);
    let mut l = open_lane(&j, 0);
    put_n(&mut *l, 5);
    group_flush(&j);
    put_n(&mut *l, 1); // 6 carries 5
    group_flush(&j); // makes 6 durable: 5 is witnessed
    wait_marks(&j, 1);
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(j.marks_written(), 1, "only the last flush needed a mark");
    drop(l);
    drop(j);
    let j = open_journal(&cfg);
    let r = recover_all(&j);
    assert_eq!(r.flushed, WriteSeq(6));
    assert_eq!(r.next_seq, WriteSeq(8));
}

/// A mark that never reached the disk is a missing number above the
/// watermark: crash residue, not loss.
#[test]
fn a_lost_mark_is_crash_residue() {
    let t = scratch();
    let cfg = cfg_with(t.path(), Some(Duration::from_millis(10)));
    let j = open_journal(&cfg);
    recover_all(&j);
    let mut l = open_lane(&j, 0);
    put_n(&mut *l, 10);
    group_flush(&j);
    wait_marks(&j, 1);
    assert_eq!(put_n(&mut *l, 1), vec![12], "carries 10");
    drop(l);
    drop(j);
    let (mf, mp) = find_in(&cfg.dir, 'm', 11);
    let mut b = std::fs::read(&mf).unwrap();
    b[mp..mp + 4096].fill(0);
    std::fs::write(&mf, &b).unwrap();
    let j = open_journal(&cfg);
    let r = recover_all(&j);
    assert_eq!(r.gap_at, Some(WriteSeq(11)));
    assert_eq!(r.flushed, WriteSeq(10), "record 12 carries the watermark");
    assert_eq!(r.records.len(), 10);
    assert!(!refuse_worthy(&r), "the lost mark is residue: {r:?}");
    // A torn (damaged, not zeroed) mark is the same.
    drop(j);
    std::fs::write(&mf, {
        let mut c = std::fs::read(&mf).unwrap();
        c[mp..mp + 4].copy_from_slice(b"LXJR");
        c[mp + 8..mp + 16].copy_from_slice(&11u64.to_le_bytes());
        c
    })
    .unwrap();
    let j = open_journal(&cfg);
    let r = recover_all(&j);
    assert!(!refuse_worthy(&r), "{r:?}");
}

/// Damage inside the mark ring below the watermark is loss like damage in a
/// lane: the mark was synced, so only the medium can have destroyed it.
#[test]
fn a_damaged_durable_mark_followed_by_a_later_mark_is_reported() {
    let t = scratch();
    let cfg = cfg_with(t.path(), Some(Duration::from_millis(10)));
    let j = open_journal(&cfg);
    recover_all(&j);
    let mut l = open_lane(&j, 0);
    put_n(&mut *l, 3);
    group_flush(&j);
    wait_marks(&j, 1); // mark 4 carries 3
    put_n(&mut *l, 3); // 5..7
    group_flush(&j);
    wait_marks(&j, 2); // mark 8 carries 7
    drop(l);
    drop(j);
    let (mf, mp) = find_in(&cfg.dir, 'm', 4);
    let mut b = std::fs::read(&mf).unwrap();
    b[mp + 20] ^= 0xff;
    std::fs::write(&mf, &b).unwrap();
    let j = open_journal(&cfg);
    let r = recover_all(&j);
    assert_eq!(r.gap_at, Some(WriteSeq(4)));
    assert_eq!(r.flushed, WriteSeq(7));
    assert!(r
        .damaged
        .iter()
        .any(|d| d.lane == MARK_RING_LANE && d.next_seq == Some(WriteSeq(8))));
    assert!(refuse_worthy(&r));
}

#[test]
fn a_lane_cannot_reserve_a_mark() {
    let t = scratch();
    let cfg = cfg_with(t.path(), None);
    let j = open_journal(&cfg);
    recover_all(&j);
    let mut l = open_lane(&j, 0);
    assert!(l.try_reserve(RecordKind::Mark, 0).is_err());
    assert!(l.try_reserve(RecordKind::Mark, 1).is_err());
    assert_eq!(put_n(&mut *l, 1), vec![1], "nothing was consumed");
}

/// After a restart the recovered records are durable (prepare synced them):
/// the coordinator witnesses them with a mark once the lanes are idle.
#[test]
fn recovered_records_are_witnessed_after_open() {
    let t = scratch();
    let cfg = cfg_with(t.path(), Some(Duration::from_millis(10)));
    let j = open_journal(&cfg);
    recover_all(&j);
    let mut l = open_lane(&j, 0);
    put_n(&mut *l, 4); // unflushed, but they reach the file
    drop(l);
    drop(j);
    let j = open_journal(&cfg);
    let r = recover_all(&j);
    assert_eq!(r.records.len(), 4);
    let l = open_lane(&j, 0); // prepare
    wait_marks(&j, 1);
    drop(l);
    drop(j);
    let j = open_journal(&cfg);
    let r = recover_all(&j);
    assert_eq!(r.flushed, WriteSeq(4));
    assert_eq!(r.next_seq, WriteSeq(6));
}

fn cfg_lanes(dir: &Path, lanes: u16, idle: Option<Duration>) -> JournalConfig {
    let mut c = small_cfg(dir.join("journal"), lanes, 4);
    c.mark_idle = idle;
    c
}

/// Zeroes the one-block record numbered `seq` in a lane file.
fn erase_q(dir: &Path, seq: u64) {
    let (f, p) = find_q(dir, seq);
    let mut b = std::fs::read(&f).unwrap();
    b[p..p + 64 + 4096].fill(0);
    std::fs::write(&f, &b).unwrap();
}

/// Review K1: records 1 and 2 flushed, lane 0's record 3 lost, lane 1's record
/// 4 landed carrying watermark 2. The only witness lies beyond the gap and
/// the first lane open wipes it, so the journal must write a mark after open;
/// damage to record 1 is then refused.
#[test]
fn a_witness_beyond_the_gap_is_replaced_by_a_mark_after_open() {
    let t = scratch();
    let first = cfg_lanes(t.path(), 2, None);
    let j = open_journal(&first);
    recover_all(&j);
    let mut l0 = open_lane(&j, 0);
    let mut l1 = open_lane(&j, 1);
    assert_eq!(put_n(&mut *l0, 1), vec![1]);
    assert_eq!(put_n(&mut *l1, 1), vec![2]);
    group_flush(&j);
    assert_eq!(put_n(&mut *l0, 1), vec![3]);
    assert_eq!(put_n(&mut *l1, 1), vec![4]);
    drop((l0, l1));
    drop(j);
    erase_q(&first.dir, 3);
    let cfg = cfg_lanes(t.path(), 2, Some(Duration::from_millis(5)));
    let j = open_journal(&cfg);
    let r = recover_all(&j);
    assert_eq!((r.next_seq, r.flushed), (WriteSeq(3), WriteSeq(2)));
    let l = open_lane(&j, 0); // prepare wipes record 4
    wait_marks(&j, 1);
    drop(l);
    drop(j);
    let (f, p) = find_q(&cfg.dir, 1);
    let mut b = std::fs::read(&f).unwrap();
    b[p + 100] ^= 0xff;
    std::fs::write(&f, &b).unwrap();
    let j = open_journal(&cfg);
    let r = recover_all(&j);
    assert_eq!(r.gap_at, Some(WriteSeq(1)));
    assert_eq!(r.flushed, WriteSeq(2), "the mark written after open");
    assert!(refuse_worthy(&r), "{r:?}");
}

/// Review K2: a clean attach recovers nothing above the floor and takes no
/// number; a cut inside the idle delay cancels the owed mark.
#[test]
fn a_clean_attach_or_a_cut_inside_the_delay_writes_no_mark() {
    let t = scratch();
    let cfg = cfg_with(t.path(), Some(Duration::from_millis(5)));
    for _ in 0..2 {
        let j = open_journal(&cfg);
        j.recover(None, WriteSeq(7)).expect("recover");
        let l = open_lane(&j, 0);
        std::thread::sleep(Duration::from_millis(60));
        assert_eq!(j.marks_written(), 0);
        assert_eq!(j.last_issued(), WriteSeq(7), "no number taken");
        drop(l);
    }
    let cfg = cfg_with(t.path(), Some(Duration::from_millis(200)));
    let j = open_journal(&cfg);
    j.recover(None, WriteSeq(7)).expect("recover");
    let mut l = open_lane(&j, 0);
    assert_eq!(put_n(&mut *l, 3), vec![8, 9, 10]);
    group_flush(&j);
    j.tail_after(WriteSeq(10)).expect("cut at 10");
    std::thread::sleep(Duration::from_millis(350));
    assert_eq!(j.marks_written(), 0, "the cut covers the watermark");
    assert_eq!(j.last_issued(), WriteSeq(10));
}

/// Review k4: an empty FLUSH after a mark covers only the mark's number and
/// writes no mark of its own.
#[test]
fn an_empty_flush_after_a_mark_writes_no_mark() {
    let t = scratch();
    let cfg = cfg_with(t.path(), Some(Duration::from_millis(10)));
    let j = open_journal(&cfg);
    recover_all(&j);
    let mut l = open_lane(&j, 0);
    put_n(&mut *l, 1);
    group_flush(&j);
    wait_marks(&j, 1);
    group_flush(&j); // upto = the mark's number
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(j.marks_written(), 1);
}

/// Review k1: with the ring full the owed mark is skipped (counted once), and
/// written as soon as reclaim frees a mark file, without another flush.
#[test]
fn a_full_mark_ring_retries_after_reclaim() {
    let t = scratch();
    let mut cfg = cfg_with(t.path(), Some(Duration::from_millis(1)));
    // Room for the lane's ~1 000 one-block batches without a reclaim (a cut
    // would free the mark files too).
    cfg.file_len = 16 << 20;
    cfg.journal_max = 4 * cfg.file_len;
    let j = open_journal(&cfg);
    recover_all(&j);
    let mut l = open_lane(&j, 0);
    let capacity = 4 * 255;
    for i in 0..capacity {
        put_n(&mut *l, 1);
        group_flush(&j);
        wait_marks(&j, i + 1);
    }
    put_n(&mut *l, 1);
    group_flush(&j);
    wait_until("a skipped mark", || j.marks_skipped() >= 1);
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(j.marks_written(), capacity, "nothing written while full");
    assert_eq!(j.marks_skipped(), 1, "counted once per watermark");
    // The skipped mark took no number: the last flush covered everything.
    let w = j.last_issued();
    // A cut below the watermark frees the full mark files (a cut at or above
    // it would make the mark unneeded).
    let cut = WriteSeq(w.0 - 5);
    let tail = j.tail_after(cut).expect("tail");
    j.reclaim(&tail).expect("reclaim");
    wait_marks(&j, capacity + 1);
    drop(l);
    drop(j);
    let j = open_journal(&cfg);
    let r = j.recover(None, cut).expect("recover");
    assert!(r.gap_at.is_none() && r.damaged.is_empty(), "{r:?}");
    assert_eq!(r.flushed, w, "the retried mark witnesses the last flush");
}

/// Item 3 of amendment (b), enforced by recovery: a mark in a lane file and a
/// write in a mark file are damage, never records.
#[test]
fn a_record_of_the_wrong_kind_for_its_ring_is_damage() {
    use lunix_format::journal::{Record, RecordHeader};
    let t = scratch();
    let cfg = cfg_with(t.path(), Some(Duration::from_millis(5)));
    let j = open_journal(&cfg);
    recover_all(&j);
    let mut l = open_lane(&j, 0);
    put_n(&mut *l, 3);
    group_flush(&j);
    wait_marks(&j, 1); // mark 4 carries 3
    drop(l);
    drop(j);
    let gen_of = |b: &[u8]| u64::from_le_bytes(b[32..40].try_into().unwrap());
    let forge = |file: &Path, at: usize, kind: RecordKind, seq: u64, data: &[u8]| {
        let mut b = std::fs::read(file).unwrap();
        let mut enc = Vec::new();
        Record {
            header: RecordHeader {
                kind,
                write_seq: WriteSeq(seq),
                epoch: 0,
                lba: 0,
                len: (data.len() / 4096) as u32,
                gen: gen_of(&b),
                off: at as u64,
                flushed: WriteSeq(0),
            },
            data,
        }
        .encode(&mut enc)
        .unwrap();
        b[at..at + enc.len()].copy_from_slice(&enc);
        std::fs::write(file, &b).unwrap();
    };
    // A mark numbered 2 where lane record 2 was.
    let (qf, qp) = find_q(&cfg.dir, 2);
    forge(&qf, qp, RecordKind::Mark, 2, &[]);
    let j = open_journal(&cfg);
    let r = recover_all(&j);
    assert_eq!(r.gap_at, Some(WriteSeq(2)), "{r:?}");
    assert!(r
        .damaged
        .iter()
        .any(|d| d.lane.0 == 0 && d.start == qp as u64));
    drop(j);
    // A write numbered 4 where mark 4 was.
    let (mf, mp) = find_in(&cfg.dir, 'm', 4);
    forge(&mf, mp, RecordKind::Write, 4, &[1u8; 4096]);
    let j = open_journal(&cfg);
    let r = recover_all(&j);
    assert!(r
        .damaged
        .iter()
        .any(|d| d.lane == MARK_RING_LANE && d.start == mp as u64));
    assert!(r.records.iter().all(|x| x.loc.lane != MARK_RING_LANE));
}
