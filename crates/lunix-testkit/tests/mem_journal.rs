//! `MemJournal` beyond the contract suite: in-flight rules, limits, failures,
//! recovery after random crashes.

use std::sync::mpsc;
use std::time::Duration;

use lunix_format::journal::RecordKind;
use lunix_format::WriteSeq;
use lunix_iface::journal::{
    record_bytes, Appended, Journal, JournalCompletion, JournalLane, JournalLoc, LaneId, Recovered,
    Slot, SlotOutcome, Token,
};
use lunix_iface::wake::WakeFd;
use lunix_iface::ErrorKind;
use lunix_testkit::journal::MIN_FILE_BYTES;
use lunix_testkit::{CrashModel, JournalMode, MemJournal, MemJournalConfig};

const T: Duration = Duration::from_secs(10);
const SHORT: Duration = Duration::from_millis(100);
const BLOCK: usize = 4096;

fn journal(lanes: u16) -> MemJournal {
    let j = MemJournal::new(MemJournalConfig::with_lanes(lanes));
    j.recover(None, WriteSeq(0)).expect("recover");
    j
}

fn lane(j: &MemJournal, n: u16) -> Box<dyn JournalLane> {
    j.open_lane(LaneId(n), WakeFd::new().expect("eventfd"))
        .expect("open lane")
}

fn pattern(seq: u64, blocks: u32) -> Vec<u8> {
    (0..blocks as usize * BLOCK)
        .map(|i| (seq as usize).wrapping_mul(31).wrapping_add(i) as u8)
        .collect()
}

fn reserve(l: &mut dyn JournalLane, blocks: u32) -> Slot {
    match l.try_reserve(RecordKind::Write, blocks).expect("reserve") {
        SlotOutcome::Reserved(s) => s,
        other => panic!("refused: {other:?}"),
    }
}

fn write(l: &mut dyn JournalLane, blocks: u32) -> Appended {
    let slot = reserve(l, blocks);
    let data = pattern(slot.write_seq.0, blocks);
    l.append(slot, 8, &data).expect("append")
}

fn flush(j: &MemJournal, upto: WriteSeq) -> mpsc::Receiver<bool> {
    let (tx, rx) = mpsc::channel();
    j.submit_group_flush(
        upto,
        Box::new(move |r| {
            let _ = tx.send(r.is_ok());
        }),
    )
    .expect("submit");
    rx
}

fn drain(l: &mut dyn JournalLane) -> Vec<JournalCompletion> {
    let mut out = Vec::new();
    l.poll(&mut out);
    out
}

fn seqs(r: &Recovered) -> Vec<u64> {
    r.records.iter().map(|x| x.record.write_seq.0).collect()
}

#[test]
fn held_completions_hold_the_flush_and_the_watermark() {
    let j = journal(1);
    j.set_completion_mode(JournalMode::Manual);
    let mut l = lane(&j, 0);
    write(&mut *l, 1);
    write(&mut *l, 1);
    assert_eq!(j.held_appends(LaneId(0)), 2);
    assert_eq!(l.completed_watermark(), WriteSeq(0), "nothing completed");
    assert!(drain(&mut *l).is_empty());
    let rx = flush(&j, WriteSeq(2));
    assert!(rx.recv_timeout(SHORT).is_err());
    assert_eq!(j.complete_appends(LaneId(0), 1), 1);
    assert_eq!(l.completed_watermark(), WriteSeq(1));
    assert!(rx.recv_timeout(SHORT).is_err(), "seq 2 is still in flight");
    assert_eq!(j.complete_appends(LaneId(0), 5), 1);
    assert!(rx.recv_timeout(T).expect("flush"));
    assert_eq!(l.completed_watermark(), WriteSeq(2));
    assert_eq!(drain(&mut *l).len(), 2);
}

#[test]
fn a_flush_waits_only_for_sequence_numbers_up_to_its_upto() {
    let j = journal(2);
    let mut l0 = lane(&j, 0);
    let mut l1 = lane(&j, 1);
    write(&mut *l0, 1); // 1
    let pending = reserve(&mut *l1, 1); // 2, outstanding
                                        // A FUA write of seq 1 does not wait for the higher slot.
    assert!(flush(&j, WriteSeq(1)).recv_timeout(T).expect("fua flush"));
    let rx = flush(&j, WriteSeq(2));
    assert!(rx.recv_timeout(SHORT).is_err());
    l1.append(pending, 4, &pattern(2, 1)).expect("append");
    assert!(rx.recv_timeout(T).expect("flush"));
}

#[test]
fn groups_complete_in_submission_order() {
    let j = journal(1);
    j.set_completion_mode(JournalMode::Manual);
    let mut l = lane(&j, 0);
    write(&mut *l, 1);
    let first = flush(&j, WriteSeq(1));
    let second = flush(&j, WriteSeq(0));
    assert!(
        second.recv_timeout(SHORT).is_err(),
        "queued behind the first"
    );
    j.complete_appends(LaneId(0), 1);
    assert!(first.recv_timeout(T).expect("first"));
    assert!(second.recv_timeout(T).expect("second"));
}

#[test]
fn a_full_ring_refuses_without_consuming_a_sequence_number_and_reclaim_frees_it() {
    let cfg = MemJournalConfig {
        lanes: 1,
        file_bytes: MIN_FILE_BYTES,
        max_files: 2,
        pool_depth: 8,
        mode: JournalMode::Immediate,
    };
    let j = MemJournal::new(cfg);
    j.recover(None, WriteSeq(0)).expect("recover");
    let mut l = lane(&j, 0);
    let a = write(&mut *l, 256);
    let b = write(&mut *l, 256);
    assert_eq!(
        (a.loc.ordinal, b.loc.ordinal),
        (0, 1),
        "one record per file"
    );
    assert_eq!(j.fill().used_bytes, 2 * MIN_FILE_BYTES);
    assert!((j.fill().ratio() - 1.0).abs() < f64::EPSILON);
    assert!(matches!(
        l.try_reserve(RecordKind::Write, 1).expect("reserve"),
        SlotOutcome::Full
    ));
    assert_eq!(j.last_issued(), WriteSeq(2), "Full consumed nothing");
    let tail = j.tail_after(WriteSeq(2)).expect("tail");
    assert_eq!(
        tail.lane_ordinals,
        vec![1],
        "nothing above the cut: current file"
    );
    let freed = j.reclaim(&tail).expect("reclaim");
    assert_eq!(freed.files, 1);
    assert_eq!(freed.bytes, record_bytes(RecordKind::Write, 256));
    let slot = reserve(&mut *l, 1);
    l.append(slot, 0, &pattern(3, 1)).expect("append");
}

#[test]
fn nospace_and_pool_exhaustion_consume_nothing() {
    let cfg = MemJournalConfig {
        pool_depth: 2,
        ..MemJournalConfig::with_lanes(1)
    };
    let j = MemJournal::new(cfg);
    j.recover(None, WriteSeq(0)).expect("recover");
    j.set_completion_mode(JournalMode::Manual);
    let mut l = lane(&j, 0);
    j.set_nospace(true);
    assert!(matches!(
        l.try_reserve(RecordKind::Write, 1).expect("reserve"),
        SlotOutcome::NoSpace
    ));
    j.set_nospace(false);
    write(&mut *l, 1);
    write(&mut *l, 1);
    assert!(matches!(
        l.try_reserve(RecordKind::Write, 1).expect("reserve"),
        SlotOutcome::Full
    ));
    assert_eq!(j.last_issued(), WriteSeq(2));
    j.complete_appends(LaneId(0), 1);
    let slot = reserve(&mut *l, 1);
    assert_eq!(slot.write_seq, WriteSeq(3));
    l.append(slot, 0, &pattern(3, 1)).expect("append");
}

#[test]
fn malformed_reservations_are_invalid_and_do_not_poison() {
    let j = journal(1);
    let mut l = lane(&j, 0);
    for (kind, blocks) in [
        (RecordKind::Write, 0),
        (RecordKind::Write, 257),
        (RecordKind::Hole, 0),
    ] {
        let e = l.try_reserve(kind, blocks).expect_err("malformed");
        assert_eq!(e.kind(), ErrorKind::Invalid);
    }
    assert!(!j.is_poisoned());
    let slot = match l.try_reserve(RecordKind::Hole, 1 << 20).expect("big hole") {
        SlotOutcome::Reserved(s) => s,
        other => panic!("{other:?}"),
    };
    l.append(slot, 0, &[]).expect("hole append");
}

#[test]
fn mismatched_data_is_a_caller_bug_that_poisons() {
    for (kind, blocks, data_len) in [(RecordKind::Write, 2, BLOCK), (RecordKind::Hole, 4, BLOCK)] {
        let j = journal(1);
        let mut l = lane(&j, 0);
        let slot = match l.try_reserve(kind, blocks).expect("reserve") {
            SlotOutcome::Reserved(s) => s,
            other => panic!("{other:?}"),
        };
        let e = l
            .append(slot, 0, &vec![0u8; data_len])
            .expect_err("mismatch");
        assert_eq!(e.kind(), ErrorKind::Invalid);
        assert!(j.is_poisoned());
        assert!(!flush(&j, j.last_issued()).recv_timeout(T).expect("flush"));
    }
}

#[test]
fn a_failed_completion_poisons_like_a_failed_append() {
    let j = journal(2);
    let mut l0 = lane(&j, 0);
    let mut l1 = lane(&j, 1);
    write(&mut *l1, 1); // seq 1, fine
    j.fail_next_completion(LaneId(0));
    let a = write(&mut *l0, 1); // seq 2: accepted, completion fails
    assert_eq!(a.write_seq, WriteSeq(2));
    let done = drain(&mut *l0);
    assert!(matches!(
        done.as_slice(),
        [JournalCompletion::Append { write_seq: WriteSeq(2), status }] if !status.is_ok()
    ));
    assert!(j.is_poisoned());
    assert!(flush(&j, WriteSeq(1)).recv_timeout(T).expect("below"));
    assert!(!flush(&j, WriteSeq(2)).recv_timeout(T).expect("at"));
    assert_eq!(
        l1.try_reserve(RecordKind::Write, 1)
            .expect_err("poisoned")
            .kind(),
        ErrorKind::Io
    );
}

#[test]
fn a_flush_below_the_failed_number_is_unaffected() {
    let j = journal(2);
    let mut l0 = lane(&j, 0);
    let mut l1 = lane(&j, 1);
    write(&mut *l0, 1); // 1
    j.fail_next_append(LaneId(1));
    let slot = reserve(&mut *l1, 1); // 2
    let e = l1.append(slot, 0, &pattern(2, 1)).expect_err("injected");
    assert_eq!(e.kind(), ErrorKind::Io);
    assert!(flush(&j, WriteSeq(1)).recv_timeout(T).expect("below"));
    assert!(!flush(&j, WriteSeq(2)).recv_timeout(T).expect("at"));
    assert!(!flush(&j, WriteSeq(9)).recv_timeout(T).expect("above"));
}

#[test]
fn a_failed_fdatasync_poisons_and_nothing_after_it_is_promised() {
    let j = journal(1);
    let mut l = lane(&j, 0);
    write(&mut *l, 1);
    write(&mut *l, 1);
    j.fail_next_flush();
    assert!(!flush(&j, WriteSeq(2)).recv_timeout(T).expect("failed sync"));
    assert!(j.is_poisoned());
    assert!(!flush(&j, WriteSeq(2)).recv_timeout(T).expect("later"));
    assert!(l.try_reserve(RecordKind::Write, 1).is_err());
}

#[test]
fn lanes_and_recovery_are_checked() {
    let j = journal(2);
    let _l0 = lane(&j, 0);
    assert_eq!(
        j.open_lane(LaneId(0), WakeFd::new().expect("fd"))
            .err()
            .map(|e| e.kind()),
        Some(ErrorKind::Invalid),
        "already open"
    );
    assert_eq!(
        j.open_lane(LaneId(5), WakeFd::new().expect("fd"))
            .err()
            .map(|e| e.kind()),
        Some(ErrorKind::Invalid),
        "no such lane"
    );
    assert_eq!(
        j.recover(None, WriteSeq(0)).expect_err("late").kind(),
        ErrorKind::Invalid
    );
}

#[test]
fn reads_need_a_completed_data_record() {
    let j = journal(1);
    j.set_completion_mode(JournalMode::Manual);
    let mut l = lane(&j, 0);
    let a = write(&mut *l, 1);
    let mut buf = vec![0u8; BLOCK];
    assert_eq!(
        j.read_at(a.loc, &mut buf)
            .expect_err("not completed")
            .kind(),
        ErrorKind::Invalid
    );
    j.complete_appends(LaneId(0), 1);
    j.read_at(a.loc, &mut buf).expect("completed");
    assert_eq!(buf, pattern(1, 1));
    // Past the record, and a hole.
    let mut two = vec![0u8; 2 * BLOCK];
    assert!(j.read_at(a.loc, &mut two).is_err());
    let slot = match l.try_reserve(RecordKind::Hole, 4).expect("reserve") {
        SlotOutcome::Reserved(s) => s,
        other => panic!("{other:?}"),
    };
    let h = l.append(slot, 0, &[]).expect("hole");
    j.complete_appends(LaneId(0), 1);
    assert!(j.read_at(h.loc, &mut buf).is_err());
    let bad = JournalLoc {
        lane: LaneId(9),
        ..a.loc
    };
    assert_eq!(
        l.submit_read(bad, vec![0; 8], Token(1))
            .expect_err("lane")
            .kind(),
        ErrorKind::Invalid
    );
}

#[test]
fn reclaimed_files_are_gone_and_report_what_they_held() {
    let cfg = MemJournalConfig {
        file_bytes: MIN_FILE_BYTES,
        ..MemJournalConfig::with_lanes(1)
    };
    let j = MemJournal::new(cfg);
    j.recover(None, WriteSeq(0)).expect("recover");
    let mut l = lane(&j, 0);
    let recs: Vec<_> = (0..3).map(|_| write(&mut *l, 256)).collect();
    assert_eq!(drain(&mut *l).len(), 3, "three append completions");
    assert!(flush(&j, j.last_issued()).recv_timeout(T).expect("flush"));
    let tail = j.tail_after(WriteSeq(2)).expect("tail");
    assert_eq!(tail.lane_ordinals, vec![2]);
    let freed = j.reclaim(&tail).expect("reclaim");
    assert_eq!(freed.files, 2);
    assert_eq!(freed.bytes, 2 * record_bytes(RecordKind::Write, 256));
    let mut buf = vec![0u8; BLOCK];
    assert_eq!(
        j.read_at(recs[0].loc, &mut buf).expect_err("gone").kind(),
        ErrorKind::NotFound
    );
    l.submit_read(recs[1].loc, vec![0; BLOCK], Token(3))
        .expect("queued");
    let done = drain(&mut *l);
    assert!(matches!(
        done.as_slice(),
        [JournalCompletion::Read { token: Token(3), status, .. }] if !status.is_ok()
    ));
    j.read_at(recs[2].loc, &mut buf).expect("kept");
    assert_eq!(
        j.reclaim(&lunix_iface::journal::JournalTail {
            lane_ordinals: vec![0, 0]
        })
        .expect_err("lane count")
        .kind(),
        ErrorKind::Invalid
    );
}

#[test]
fn a_crash_freezes_the_journal_and_fails_pending_flushes() {
    let j = journal(1);
    j.set_completion_mode(JournalMode::Manual);
    let mut l = lane(&j, 0);
    write(&mut *l, 1);
    let rx = {
        let (tx, rx) = mpsc::channel();
        j.submit_group_flush(
            WriteSeq(1),
            Box::new(move |r| {
                let _ = tx.send(r.err().map(|e| e.kind()));
            }),
        )
        .expect("submit");
        rx
    };
    let _image = j.crash(7);
    assert_eq!(
        rx.recv_timeout(T).expect("callback"),
        Some(ErrorKind::Closed)
    );
    assert_eq!(
        l.try_reserve(RecordKind::Write, 1)
            .expect_err("frozen")
            .kind(),
        ErrorKind::Closed
    );
    assert_eq!(
        j.submit_group_flush(WriteSeq(1), Box::new(|_| {}))
            .expect_err("closed")
            .kind(),
        ErrorKind::Closed
    );
}

/// Writes `n` records alternating over two lanes; returns the journal and the
/// lanes' handles dropped (records are complete).
fn fill(j: &MemJournal, from: usize, n: usize) {
    let mut l0 = lane(j, 0);
    let mut l1 = lane(j, 1);
    for i in from..from + n {
        let l: &mut dyn JournalLane = if i % 2 == 0 { &mut *l0 } else { &mut *l1 };
        write(l, 1);
    }
}

#[test]
fn the_crash_model_produces_gaps_and_damage_and_recovery_copes() {
    let (mut gaps, mut damage, mut clean) = (0, 0, 0);
    for seed in 0..300u64 {
        let j = journal(2);
        fill(&j, 0, 2);
        assert!(flush(&j, j.last_issued()).recv_timeout(T).expect("flush"));
        // Reopen the lanes: the helper above dropped them; write 6 more.
        let image0 = j.crash_with(0, CrashModel::keep_all());
        let j = MemJournal::from_image(&image0);
        j.recover(None, WriteSeq(0)).expect("recover");
        fill(&j, 2, 6);
        let image = j.crash(seed);
        let j2 = MemJournal::from_image(&image);
        let r = j2.recover(None, WriteSeq(0)).expect("recover");
        let got = seqs(&r);
        assert!(got.len() >= 2, "flushed records survive (seed {seed})");
        // Records 3-8 witness the recovered 1-2; nothing below that is lost.
        assert!(
            r.flushed == WriteSeq(2) || r.flushed == WriteSeq(0),
            "seed {seed}: {:?}",
            r.flushed
        );
        assert!(r.gap_at.is_none_or(|g| g > r.flushed), "seed {seed}");
        assert_eq!(got, (1..=got.len() as u64).collect::<Vec<_>>());
        assert_eq!(r.next_seq, WriteSeq(got.len() as u64 + 1));
        match (r.ignored_after_gap, r.gap_at) {
            (Some(range), Some(gap)) => {
                gaps += 1;
                assert_eq!(gap, r.next_seq);
                assert!(range.first > gap && range.first <= range.last);
                assert!(range.last <= WriteSeq(8));
            }
            (None, None) => clean += 1,
            other => panic!("ignored/gap disagree: {other:?}"),
        }
        if !r.damaged.is_empty() {
            damage += 1;
            for d in &r.damaged {
                assert!(d.start < d.end);
                assert!(d.next_seq.is_none_or(|n| n > d.prev_seq));
            }
        }
        for rec in &r.records {
            let mut buf = vec![0u8; BLOCK];
            j2.read_at(rec.loc, &mut buf).expect("read");
            assert_eq!(buf, pattern(rec.record.write_seq.0, 1));
        }
    }
    assert!(
        gaps > 20 && damage > 20 && clean > 5,
        "{gaps} {damage} {clean}"
    );
}

#[test]
fn records_beyond_a_gap_are_retired_when_the_gap_is_refilled() {
    // Find a crash with a gap, then continue writing and crash again: the stale
    // records must not come back as duplicates or fill the new numbering.
    for seed in 0..300u64 {
        let j = journal(2);
        fill(&j, 0, 8);
        let image = j.crash(seed);
        let j2 = MemJournal::from_image(&image);
        let r = j2.recover(None, WriteSeq(0)).expect("recover");
        let (Some(ignored), Some(gap)) = (r.ignored_after_gap, r.gap_at) else {
            continue;
        };
        assert!(ignored.last.0 > gap.0);
        let kept = r.records.len();
        {
            let mut l0 = lane(&j2, 0);
            for _ in 0..8 {
                write(&mut *l0, 1);
            }
        }
        assert!(flush(&j2, j2.last_issued()).recv_timeout(T).expect("flush"));
        let image2 = j2.crash_with(1, CrashModel::lose_all());
        let j3 = MemJournal::from_image(&image2);
        let r2 = j3.recover(None, WriteSeq(0)).expect("recover");
        assert_eq!(seqs(&r2), (1..=(kept + 8) as u64).collect::<Vec<_>>());
        assert_eq!(r2.ignored_after_gap, None, "stale records are gone");
        return;
    }
    panic!("no seed produced a gap");
}

#[test]
fn lanes_on_threads_get_unique_rising_numbers_and_everything_flushed_survives() {
    let j = std::sync::Arc::new(journal(2));
    let mut handles = Vec::new();
    for n in 0..2u16 {
        let j = j.clone();
        handles.push(std::thread::spawn(move || {
            let mut l = lane(&j, n);
            let mut mine = Vec::new();
            for _ in 0..300 {
                mine.push(write(&mut *l, 1).write_seq.0);
            }
            mine
        }));
    }
    let flusher = {
        let j = j.clone();
        std::thread::spawn(move || {
            for _ in 0..20 {
                let rx = flush(&j, j.last_issued());
                assert!(rx.recv_timeout(T).expect("flush"));
            }
        })
    };
    let mut all = Vec::new();
    for h in handles {
        let mine = h.join().expect("thread");
        assert!(mine.windows(2).all(|w| w[0] < w[1]), "rising per lane");
        all.extend(mine);
    }
    flusher.join().expect("flusher");
    all.sort_unstable();
    assert_eq!(all, (1..=600).collect::<Vec<u64>>(), "dense and unique");
    assert!(flush(&j, j.last_issued()).recv_timeout(T).expect("final"));
    let image = j.crash_with(3, CrashModel::lose_all());
    let r = MemJournal::from_image(&image)
        .recover(None, WriteSeq(0))
        .expect("recover");
    assert_eq!(seqs(&r), (1..=600).collect::<Vec<u64>>());
}

#[test]
fn random_workloads_keep_everything_a_completed_flush_covered() {
    for seed in 0..200u64 {
        let mut rng = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
        let mut next = move || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng
        };
        let j = journal(3);
        let mut lanes: Vec<_> = (0..3).map(|n| lane(&j, n)).collect();
        let mut flushed = 0u64;
        for _ in 0..40 {
            let n = (next() % 3) as usize;
            let blocks = 1 + (next() % 3) as u32;
            write(&mut *lanes[n], blocks);
            if next() % 6 == 0 {
                let upto = j.last_issued();
                assert!(flush(&j, upto).recv_timeout(T).expect("flush"));
                flushed = upto.0;
            }
        }
        let image = j.crash(next());
        let r = MemJournal::from_image(&image)
            .recover(None, WriteSeq(0))
            .expect("recover");
        let got = seqs(&r);
        assert!(
            got.len() as u64 >= flushed,
            "seed {seed}: {} < {flushed}",
            got.len()
        );
        // The watermark never claims more than a completed flush, and
        // nothing at or below it is ever lost.
        assert!(r.flushed.0 <= flushed, "seed {seed}");
        assert!(r.gap_at.is_none_or(|g| g > r.flushed), "seed {seed}");
        assert!(
            r.damaged
                .iter()
                .all(|d| d.next_seq.is_none_or(|n| n > r.flushed)),
            "seed {seed}"
        );
        assert_eq!(
            got,
            (1..=got.len() as u64).collect::<Vec<_>>(),
            "seed {seed}"
        );
    }
}

#[test]
fn mixed_mode_completes_out_of_order_from_poll_and_the_watermark_is_the_minimum() {
    for seed in 0..40u64 {
        let cfg = MemJournalConfig {
            mode: JournalMode::Mixed(seed),
            pool_depth: 6,
            ..MemJournalConfig::with_lanes(1)
        };
        let j = MemJournal::new(cfg);
        j.recover(None, WriteSeq(0)).unwrap();
        let mut l = lane(&j, 0);
        let (mut out_of_order, mut full) = (false, false);
        let mut seen: Vec<u64> = Vec::new();
        let mut written = 0u64;
        while seen.len() < 40 {
            if written < 40 {
                match l.try_reserve(RecordKind::Write, 1).unwrap() {
                    SlotOutcome::Reserved(slot) => {
                        let data = pattern(slot.write_seq.0, 1);
                        l.append(slot, 0, &data).unwrap();
                        written += 1;
                    }
                    SlotOutcome::Full => full = true,
                    SlotOutcome::NoSpace => panic!("no space"),
                }
            }
            let mut got = Vec::new();
            l.poll(&mut got);
            for c in got {
                if let JournalCompletion::Append { write_seq, status } = c {
                    assert!(status.is_ok());
                    if seen.last().is_some_and(|&p| p > write_seq.0) {
                        out_of_order = true;
                    }
                    seen.push(write_seq.0);
                }
            }
            // The watermark never passes a held append.
            let w = l.completed_watermark().0;
            assert!(
                seen.iter().all(|&s| s != w + 1),
                "seed {seed}: {w} {seen:?}"
            );
            assert!(w <= written);
        }
        let mut sorted = seen.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, (1..=40).collect::<Vec<_>>(), "each completes once");
        if seed == 39 {
            assert!(out_of_order || full);
        }
        assert!(out_of_order || full || seed > 0);
    }
}

#[test]
fn manual_mode_completes_a_chosen_append_out_of_order_and_fills_the_pool() {
    let cfg = MemJournalConfig {
        mode: JournalMode::Manual,
        pool_depth: 3,
        ..MemJournalConfig::with_lanes(1)
    };
    let j = MemJournal::new(cfg);
    j.recover(None, WriteSeq(0)).unwrap();
    let mut l = lane(&j, 0);
    for _ in 0..3 {
        write(&mut *l, 1);
    }
    assert!(matches!(
        l.try_reserve(RecordKind::Write, 1).unwrap(),
        SlotOutcome::Full
    ));
    assert!(j.complete_append_seq(LaneId(0), WriteSeq(2)));
    assert!(!j.complete_append_seq(LaneId(0), WriteSeq(2)));
    assert_eq!(l.completed_watermark(), WriteSeq(0), "seq 1 is still held");
    let done = drain(&mut *l);
    assert!(matches!(
        done.as_slice(),
        [JournalCompletion::Append {
            write_seq: WriteSeq(2),
            ..
        }]
    ));
    j.complete_appends(LaneId(0), 5);
    assert_eq!(l.completed_watermark(), WriteSeq(3));
}

#[test]
fn lanes_cannot_open_before_recover() {
    let j = MemJournal::new(MemJournalConfig::with_lanes(1));
    assert_eq!(
        j.open_lane(LaneId(0), WakeFd::new().unwrap())
            .err()
            .map(|e| e.kind()),
        Some(ErrorKind::Invalid)
    );
    j.recover(None, WriteSeq(0)).unwrap();
    assert!(j.open_lane(LaneId(0), WakeFd::new().unwrap()).is_ok());
}

#[test]
fn a_flush_below_the_poison_point_promises_nothing_above_it() {
    let j = journal(1);
    let mut l = lane(&j, 0);
    write(&mut *l, 1); // 1
    assert!(flush(&j, WriteSeq(1)).recv_timeout(T).unwrap());
    write(&mut *l, 1); // 2
    write(&mut *l, 1); // 3
    j.fail_next_flush();
    assert!(!flush(&j, WriteSeq(3)).recv_timeout(T).unwrap());
    assert!(
        flush(&j, WriteSeq(1)).recv_timeout(T).unwrap(),
        "below the failure"
    );
    let image = j.crash_with(1, CrashModel::lose_all());
    let r = MemJournal::from_image(&image)
        .recover(None, WriteSeq(0))
        .unwrap();
    assert_eq!(seqs(&r), vec![1], "records 2 and 3 were never made durable");
}

#[test]
fn reclaim_refuses_a_tail_that_would_drop_a_record_above_the_last_cut() {
    let cfg = MemJournalConfig {
        file_bytes: MIN_FILE_BYTES,
        ..MemJournalConfig::with_lanes(1)
    };
    let j = MemJournal::new(cfg);
    j.recover(None, WriteSeq(0)).unwrap();
    let mut l = lane(&j, 0);
    for _ in 0..3 {
        write(&mut *l, 256);
    }
    let tail = lunix_iface::journal::JournalTail {
        lane_ordinals: vec![2],
    };
    assert_eq!(
        j.reclaim(&tail).unwrap_err().kind(),
        ErrorKind::Invalid,
        "no cut was ever asked about: everything is above it"
    );
    assert_eq!(j.reclaim_calls(), 0);
    j.tail_after(WriteSeq(1)).unwrap();
    assert_eq!(
        j.reclaim(&tail).unwrap_err().kind(),
        ErrorKind::Invalid,
        "file 1 holds seq 2"
    );
    j.tail_after(WriteSeq(2)).unwrap();
    assert_eq!(j.reclaim(&tail).unwrap().files, 2);
    assert_eq!(j.reclaim_calls(), 1);
}
