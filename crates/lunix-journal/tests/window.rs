//! The resident journal window and the read-around (ADR-33, T1 and T2).

mod common;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Barrier;

use common::*;
use lunix_format::journal::RecordKind;
use lunix_format::WriteSeq;
use lunix_iface::journal::{
    record_bytes, Journal, JournalCompletion, JournalLane, JournalLoc, LaneId, Token,
};
use lunix_iface::ErrorKind;
use lunix_journal::{FileJournal, JournalConfig};
use proptest::prelude::*;

const MIB: u64 = 1 << 20;
/// One maximal batch (header block, 256 blocks of data, 4 KiB).
const BATCH_CAP: u64 = MIB + 4096;

fn loc_of(w: &Written) -> JournalLoc {
    JournalLoc {
        lane: LaneId(w.lane),
        ordinal: w.ordinal,
        offset: w.start + 64,
    }
}

fn enc(w: &Written) -> u64 {
    record_bytes(RecordKind::Write, w.blocks)
}

/// Writes `n` records of `blocks` blocks, waiting for each, and flushes.
fn fill(lane: &mut dyn JournalLane, j: &dyn Journal, n: usize, blocks: u32) -> Vec<Written> {
    let mut v = Vec::new();
    for i in 0..n {
        v.push(write_rec(lane, i as u64 * 256, blocks));
        complete_appends(lane, 1);
    }
    group_flush(j);
    v
}

fn read(j: &dyn Journal, w: &Written) -> Vec<u8> {
    let mut b = vec![0u8; w.blocks as usize * 4096];
    j.read_at(loc_of(w), &mut b).expect("read_at");
    b
}

/// Closes the journal and opens it again, recovered, with one lane open.
fn restart(
    cfg: &JournalConfig,
    old: (FileJournal, Box<dyn JournalLane>),
) -> (FileJournal, Box<dyn JournalLane>) {
    drop(old);
    let j = open_journal(cfg);
    recover_all(&j);
    let l = open_lane(&j, 0);
    (j, l)
}

fn fresh(cfg: &JournalConfig) -> (FileJournal, Box<dyn JournalLane>) {
    let j = open_journal(cfg);
    recover_all(&j);
    let l = open_lane(&j, 0);
    (j, l)
}

#[test]
fn what_was_written_is_served_from_memory_not_from_the_file() {
    let dir = scratch();
    let cfg = small_cfg(dir.path().join("journal"), 1, 3);
    let (j, mut l) = fresh(&cfg);
    let ws = fill(&mut *l, &j, 12, 4);
    let total: u64 = ws.iter().map(enc).sum();
    assert_eq!(j.window_stats().resident_bytes, total);
    // Damage the first record on disk: a hit does not read the file.
    flip_byte(&path_of(&cfg, 0, ws[0].ordinal), ws[0].start + 64 + 10);
    for w in &ws {
        assert_eq!(read(&j, w), pattern(w.seq, w.blocks));
    }
    let s = j.window_stats();
    assert_eq!((s.hits, s.misses, s.window_reads), (12, 0, 0), "{s:?}");

    // A lane read is a hit too, completed at once.
    l.submit_read(loc_of(&ws[0]), vec![0u8; 4 * 4096], Token(9))
        .expect("submit");
    let mut out = Vec::new();
    l.poll(&mut out);
    match out.pop() {
        Some(JournalCompletion::Read { token, status, buf }) => {
            assert_eq!(token, Token(9));
            assert!(status.is_ok());
            assert_eq!(buf, pattern(ws[0].seq, 4));
        }
        other => panic!("no read completion: {other:?}"),
    }
    assert_eq!(j.window_stats().hits, 13);
}

#[test]
fn a_lane_read_that_misses_still_reads_the_file() {
    let dir = scratch();
    let cfg = small_cfg(dir.path().join("journal"), 1, 3);
    let (j, mut l) = fresh(&cfg);
    let ws = fill(&mut *l, &j, 3, 2);
    let (j, mut l) = restart(&cfg, (j, l));
    l.submit_read(loc_of(&ws[1]), vec![0u8; 2 * 4096], Token(4))
        .expect("submit");
    let mut got = None;
    wait_until("read completion", || {
        let mut out = Vec::new();
        l.poll(&mut out);
        if let Some(JournalCompletion::Read { status, buf, .. }) = out.pop() {
            assert!(status.is_ok());
            got = Some(buf);
        }
        got.is_some()
    });
    assert_eq!(got.expect("buf"), pattern(ws[1].seq, 2));
    let s = j.window_stats();
    assert_eq!((s.hits, s.misses, s.window_reads), (0, 1, 0), "{s:?}");
}

#[test]
fn reclaim_releases_exactly_the_entries_of_the_recycled_files() {
    let dir = scratch();
    let cfg = small_cfg(dir.path().join("journal"), 1, 4);
    let (j, mut l) = fresh(&cfg);
    let ws = fill(&mut *l, &j, 100, 16);
    let first = ws[0].ordinal;
    let in_first: Vec<&Written> = ws.iter().filter(|w| w.ordinal == first).collect();
    assert!(in_first.len() < ws.len());
    let all: u64 = ws.iter().map(enc).sum();
    assert_eq!(j.window_stats().resident_bytes, all);

    let cut = in_first.last().expect("rec").seq;
    let tail = j.tail_after(WriteSeq(cut)).expect("tail");
    let rep = j.reclaim(&tail).expect("reclaim");
    assert_eq!(rep.files, 1);
    let gone: u64 = in_first.iter().map(|w| enc(w)).sum();
    assert_eq!(j.window_stats().resident_bytes, all - gone);

    let e = j
        .read_at(loc_of(in_first[0]), &mut [0u8; 8])
        .expect_err("reclaimed");
    assert_eq!(e.kind(), ErrorKind::NotFound);
    for w in ws.iter().filter(|w| w.ordinal != first) {
        assert_eq!(read(&j, w), pattern(w.seq, w.blocks));
    }
}

#[test]
fn past_the_cap_a_batch_is_not_admitted_and_the_oldest_records_stay() {
    let dir = scratch();
    let mut cfg = small_cfg(dir.path().join("journal"), 1, 3);
    cfg.resident_max = Some(BATCH_CAP);
    let (j, mut l) = fresh(&cfg);
    // 16 records of 65600 bytes fit the cap (1049600 <= 1052672), the 17th
    // does not.
    let ws = fill(&mut *l, &j, 30, 16);
    let s = j.window_stats();
    assert_eq!(s.cap_bytes, BATCH_CAP);
    assert!(s.resident_bytes <= BATCH_CAP, "{s:?}");
    assert_eq!(s.resident_bytes, 16 * enc(&ws[0]));
    assert_eq!(s.batches_skipped, 14);

    // The oldest records are hits, the rest come from the file, byte-exact.
    for w in &ws {
        assert_eq!(read(&j, w), pattern(w.seq, w.blocks));
    }
    let s = j.window_stats();
    assert!(s.hits >= 16, "{s:?}");
    assert!(s.misses >= 1 && s.window_reads >= 1, "{s:?}");
    assert!(s.resident_bytes <= BATCH_CAP, "{s:?}");
}

#[test]
fn a_cap_above_the_ring_is_the_ring_and_a_cap_below_a_batch_is_refused() {
    let dir = scratch();
    let mut cfg = small_cfg(dir.path().join("journal"), 1, 3);
    cfg.resident_max = Some(u64::MAX);
    let j = open_journal(&cfg);
    assert_eq!(j.window_stats().cap_bytes, cfg.journal_max);
    drop(j);
    cfg.resident_max = Some(4096);
    let e = FileJournal::open(cfg).err().expect("refused");
    assert_eq!(e.kind(), ErrorKind::Invalid);
}

#[test]
fn concurrent_misses_in_one_window_read_it_once() {
    let dir = scratch();
    let cfg = small_cfg(dir.path().join("journal"), 1, 3);
    let (j, mut l) = fresh(&cfg);
    // 1-block records take 8 KiB of the file each: 100 records span the first
    // 800 KiB, one window.
    let ws = fill(&mut *l, &j, 100, 1);
    let (j, l) = restart(&cfg, (j, l));
    let bar = Barrier::new(8);
    std::thread::scope(|s| {
        for t in 0..8usize {
            let (j, ws, bar) = (&j, &ws, &bar);
            s.spawn(move || {
                bar.wait();
                for w in ws.iter().skip(t).step_by(8) {
                    assert_eq!(read(j, w), pattern(w.seq, w.blocks));
                }
            });
        }
    });
    let s = j.window_stats();
    assert_eq!(s.window_reads, 1, "{s:?}");
    assert_eq!(s.hits + s.misses, 100);
    // Everything is resident now.
    for w in &ws {
        assert_eq!(read(&j, w), pattern(w.seq, w.blocks));
    }
    assert_eq!(j.window_stats().window_reads, 1);
    drop(l);
}

#[test]
fn the_first_cut_after_a_restart_reads_a_window_per_megabyte() {
    let dir = scratch();
    let cfg = small_cfg(dir.path().join("journal"), 1, 3);
    let (j, mut l) = fresh(&cfg);
    let ws = fill(&mut *l, &j, 200, 1);
    let (j, l) = restart(&cfg, (j, l));
    assert_eq!(j.window_stats().resident_bytes, 0);
    // The recovered records, in write order, as a cut reads them.
    for w in &ws {
        assert_eq!(read(&j, w), pattern(w.seq, w.blocks));
    }
    let span = ws
        .iter()
        .map(|w| (w.ordinal, (w.start + 64) / MIB))
        .collect::<std::collections::BTreeSet<_>>();
    let s = j.window_stats();
    assert_eq!(s.window_reads, span.len() as u64, "{s:?}");
    assert!(s.completing_reads <= span.len() as u64, "{s:?}");
    drop(l);
}

#[test]
fn files_that_are_not_a_megabyte_multiple_end_in_a_short_window() {
    let dir = scratch();
    let mut cfg = small_cfg(dir.path().join("journal"), 1, 3);
    cfg.file_len = 2 * MIB + 512 * 1024;
    cfg.journal_max = 3 * cfg.file_len;
    let (j, mut l) = fresh(&cfg);
    let ws = fill(&mut *l, &j, 340, 1);
    let first = ws[0].ordinal;
    let last_in_first = ws.iter().rfind(|w| w.ordinal == first).expect("rec");
    assert!(
        last_in_first.start + 64 > 2 * MIB,
        "the first file reaches its short last window"
    );
    assert!(ws.last().expect("rec").ordinal != first, "rolled");
    let (j, l) = restart(&cfg, (j, l));
    for w in &ws {
        assert_eq!(read(&j, w), pattern(w.seq, w.blocks));
    }
    assert!(j.window_stats().window_reads >= 3);
    drop(l);
}

#[test]
fn a_location_reclaimed_while_it_is_read_never_returns_stale_bytes() {
    let dir = scratch();
    let cfg = small_cfg(dir.path().join("journal"), 1, 4);
    let (j, mut l) = fresh(&cfg);
    let ws = fill(&mut *l, &j, 100, 16);
    let first = ws[0].ordinal;
    let in_first: Vec<Written> = ws.iter().filter(|w| w.ordinal == first).copied().collect();
    let cut = in_first.last().expect("rec").seq;
    let (j, l) = restart(&cfg, (j, l));
    let stop = AtomicBool::new(false);
    let reads = AtomicU64::new(0);
    let gone = AtomicBool::new(false);
    std::thread::scope(|s| {
        for t in 0..4usize {
            let (j, ws, stop, reads, gone) = (&j, &in_first, &stop, &reads, &gone);
            s.spawn(move || {
                let mut i = t;
                while !stop.load(Ordering::Relaxed) {
                    let w = &ws[i % ws.len()];
                    let mut b = vec![0u8; w.blocks as usize * 4096];
                    // Read the flag first: a read that began after the
                    // reclaim returned must fail.
                    let after = gone.load(Ordering::SeqCst);
                    match j.read_at(loc_of(w), &mut b) {
                        Ok(()) => {
                            assert!(!after, "a reclaimed location was served");
                            assert_eq!(b, pattern(w.seq, w.blocks));
                        }
                        Err(e) => assert_eq!(e.kind(), ErrorKind::NotFound, "{e}"),
                    }
                    reads.fetch_add(1, Ordering::SeqCst);
                    i += 4;
                }
            });
        }
        // Readers are running; reclaim; readers keep reading after it.
        wait_until("reads before the reclaim", || {
            reads.load(Ordering::SeqCst) >= 20
        });
        let tail = j.tail_after(WriteSeq(cut)).expect("tail");
        assert_eq!(j.reclaim(&tail).expect("reclaim").files, 1);
        gone.store(true, Ordering::SeqCst);
        let mark = reads.load(Ordering::SeqCst);
        wait_until("reads after the reclaim", || {
            reads.load(Ordering::SeqCst) >= mark + 40
        });
        stop.store(true, Ordering::Relaxed);
    });
    // Nothing of the recycled file stays resident or readable.
    assert_eq!(j.window_stats().resident_bytes, 0);
    assert_eq!(j.window_stats().check_failures, 0);
    for w in &in_first {
        let e = j.read_at(loc_of(w), &mut [0u8; 8]).expect_err("gone");
        assert_eq!(e.kind(), ErrorKind::NotFound);
    }
    drop(l);
}

#[test]
fn readers_see_exact_bytes_during_appends_and_reclaim() {
    let dir = scratch();
    let cfg = small_cfg(dir.path().join("journal"), 1, 4);
    let (j, mut l) = fresh(&cfg);
    let (tx, rx) = std::sync::mpsc::channel::<Written>();
    let rx = std::sync::Mutex::new(rx);
    let stop = AtomicBool::new(false);
    let done = AtomicU64::new(0);
    std::thread::scope(|s| {
        for _ in 0..3 {
            let (j, rx, stop, done) = (&j, &rx, &stop, &done);
            s.spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    let got = rx
                        .lock()
                        .expect("rx")
                        .recv_timeout(std::time::Duration::from_millis(5));
                    let Ok(w) = got else { continue };
                    let mut b = vec![0u8; w.blocks as usize * 4096];
                    match j.read_at(loc_of(&w), &mut b) {
                        Ok(()) => assert_eq!(b, pattern(w.seq, w.blocks)),
                        // Only a reclaimed location may fail.
                        Err(e) => assert_eq!(e.kind(), ErrorKind::NotFound, "{e}"),
                    }
                    done.fetch_add(1, Ordering::SeqCst);
                }
            });
        }
        for i in 0..150u64 {
            let w = write_rec(&mut *l, i, 8);
            complete_appends(&mut *l, 1);
            tx.send(w).expect("send");
            if i % 40 == 39 {
                group_flush(&j);
                let tail = j
                    .tail_after(WriteSeq(w.seq.saturating_sub(10)))
                    .expect("tail");
                j.reclaim(&tail).expect("reclaim");
            }
        }
        wait_until("every sent record was read", || {
            done.load(Ordering::SeqCst) == 150
        });
        stop.store(true, Ordering::Relaxed);
    });
    let s = j.window_stats();
    assert!(s.resident_bytes <= s.cap_bytes);
    assert_eq!(s.check_failures, 0);
}

#[test]
fn a_record_damaged_on_disk_after_the_write_fails_the_read_and_is_never_served() {
    let dir = scratch();
    let cfg = small_cfg(dir.path().join("journal"), 1, 3);
    let (j, mut l) = fresh(&cfg);
    let ws = fill(&mut *l, &j, 6, 2);
    let (j, l) = restart(&cfg, (j, l));
    // Damaged after the recovery scan accepted it, before any read of it.
    let bad = &ws[3];
    flip_byte(&path_of(&cfg, 0, bad.ordinal), bad.start + 64 + 4096 + 7);

    let mut b = vec![0xEEu8; 2 * 4096];
    let e = j.read_at(loc_of(bad), &mut b).expect_err("damaged");
    assert_eq!(e.kind(), ErrorKind::Corrupt, "{e}");
    assert!(b.iter().all(|&x| x == 0xEE), "nothing was copied");
    // A sub-range of the record fails too: the checksum covers all of it.
    let mut one = vec![0xEEu8; 4096];
    let e = j
        .read_at(loc_of(bad).advance(4096).expect("adv"), &mut one)
        .expect_err("damaged sub-range");
    assert_eq!(e.kind(), ErrorKind::Corrupt);
    assert!(one.iter().all(|&x| x == 0xEE));
    let s = j.window_stats();
    assert!(s.check_failures >= 2, "{s:?}");

    // The neighbours in the same window are intact and resident.
    for w in ws.iter().filter(|w| w.seq != bad.seq) {
        assert_eq!(read(&j, w), pattern(w.seq, w.blocks));
    }
    let resident: u64 = ws.iter().filter(|w| w.seq != bad.seq).map(enc).sum();
    assert_eq!(j.window_stats().resident_bytes, resident);
    // The damaged record is still not served.
    let e = j.read_at(loc_of(bad), &mut b).expect_err("still damaged");
    assert_eq!(e.kind(), ErrorKind::Corrupt);
    drop(l);
}

#[test]
fn a_damaged_header_fails_the_read_too() {
    let dir = scratch();
    let cfg = small_cfg(dir.path().join("journal"), 1, 3);
    let (j, mut l) = fresh(&cfg);
    let ws = fill(&mut *l, &j, 4, 1);
    let (j, l) = restart(&cfg, (j, l));
    // The record's own offset field.
    flip_byte(&path_of(&cfg, 0, ws[1].ordinal), ws[1].start + 48);
    let mut b = vec![0u8; 4096];
    let e = j.read_at(loc_of(&ws[1]), &mut b).expect_err("damaged");
    assert_eq!(e.kind(), ErrorKind::Corrupt);
    assert_eq!(read(&j, &ws[0]), pattern(ws[0].seq, 1));
    drop(l);
}

/// Writes `prefix` one-block records, then `blocks` blocks, and returns the
/// last record; the journal is restarted so that nothing is resident.
fn cut_rig(
    cfg: &JournalConfig,
    prefix: usize,
    blocks: u32,
) -> (FileJournal, Box<dyn JournalLane>, Written) {
    let (j, mut l) = fresh(cfg);
    for i in 0..prefix {
        write_rec(&mut *l, i as u64, 1);
        complete_appends(&mut *l, 1);
    }
    let w = write_rec(&mut *l, 9999, blocks);
    complete_appends(&mut *l, 1);
    group_flush(&j);
    let (j, l) = restart(cfg, (j, l));
    (j, l, w)
}

#[test]
fn a_record_cut_by_the_window_edge_is_completed_with_one_read() {
    let dir = scratch();
    let cfg = small_cfg(dir.path().join("journal"), 1, 3);
    // 115 one-block batches of 8 KiB, so the record starts at 946176 and runs
    // 262208 bytes: the 1 MiB edge cuts it.
    let (j, l, w) = cut_rig(&cfg, 115, 64);
    assert!(w.start < MIB && w.start + w.len > MIB, "{w:?}");

    // The whole record, asked from the first window: its tail is read in.
    assert_eq!(read(&j, &w), pattern(w.seq, w.blocks));
    let s = j.window_stats();
    assert_eq!((s.window_reads, s.completing_reads), (1, 1), "{s:?}");
    drop(l);

    // Only the last block, asked from the second window: the read looks back.
    let dir2 = scratch();
    let cfg2 = small_cfg(dir2.path().join("journal"), 1, 3);
    let (j, l, w) = cut_rig(&cfg2, 115, 64);
    let last = loc_of(&w).advance(63 * 4096).expect("advance");
    assert!(last.offset >= MIB);
    let mut b = vec![0u8; 4096];
    j.read_at(last, &mut b).expect("read");
    assert_eq!(b, pattern(w.seq, 64)[63 * 4096..]);
    let s = j.window_stats();
    assert_eq!((s.window_reads, s.completing_reads), (1, 1), "{s:?}");
    // The completed record is resident: the rest of it is a hit.
    let mut c = vec![0u8; 4096];
    j.read_at(loc_of(&w), &mut c).expect("read");
    assert_eq!(c, pattern(w.seq, 64)[..4096]);
    assert_eq!(j.window_stats().window_reads, 1);
    drop(l);
}

#[test]
fn a_record_that_runs_a_few_bytes_past_the_edge_needs_the_completing_read() {
    // 15 blocks: the record at 987136 ends 64 bytes after the 1 MiB edge.
    let dir = scratch();
    let cfg = small_cfg(dir.path().join("journal"), 1, 3);
    let (j, mut l) = fresh(&cfg);
    let mut ws = Vec::new();
    for i in 0..18 {
        ws.push(write_rec(&mut *l, i, 15));
        complete_appends(&mut *l, 1);
    }
    group_flush(&j);
    let cut = ws
        .iter()
        .find(|w| w.start < MIB && w.start + w.len > MIB)
        .copied()
        .expect("a cut record");
    let (j, l) = restart(&cfg, (j, l));
    for w in &ws {
        assert_eq!(read(&j, w), pattern(w.seq, w.blocks));
    }
    let s = j.window_stats();
    assert_eq!(s.window_reads, 2, "{s:?}");
    assert!(s.completing_reads >= 1, "{s:?}");
    assert_eq!(cut.blocks, 15);
    drop(l);
}

#[test]
fn a_failed_batch_write_gives_its_reservation_back() {
    let dir = scratch();
    let cfg = small_cfg(dir.path().join("journal"), 1, 3);
    let (j, mut l) = fresh(&cfg);
    let ok = write_rec(&mut *l, 0, 4);
    complete_appends(&mut *l, 1);
    let resident = j.window_stats().resident_bytes;
    assert_eq!(resident, enc(&ok));
    j.fail_next_batch_write();
    write_rec(&mut *l, 1, 4);
    let mut out = Vec::new();
    wait_until("the failed completion", || {
        l.poll(&mut out);
        !out.is_empty()
    });
    match &out[0] {
        JournalCompletion::Append { status, .. } => assert!(!status.is_ok()),
        other => panic!("{other:?}"),
    }
    // Only the first record's bytes stay reserved.
    assert_eq!(j.window_stats().resident_bytes, resident);
}

#[test]
fn a_lane_dropped_with_writes_in_flight_indexes_them_all() {
    let dir = scratch();
    let cfg = small_cfg(dir.path().join("journal"), 1, 3);
    let (j, mut l) = fresh(&cfg);
    for i in 0..4 {
        write_rec(&mut *l, i, 4);
    }
    // Dropping the lane waits for its writes and completes them.
    drop(l);
    let expect: u64 = 4 * record_bytes(RecordKind::Write, 4);
    assert_eq!(j.window_stats().resident_bytes, expect);
}

#[test]
fn records_beyond_the_stop_point_read_before_the_wipe_never_come_back() {
    let dir = scratch();
    let mut cfg = small_cfg(dir.path().join("journal"), 1, 3);
    cfg.resident_max = Some(BATCH_CAP);
    let (j, mut l) = fresh(&cfg);
    let ws = fill(&mut *l, &j, 6, 16);
    drop((j, l));
    // Damage the third record: recovery stops after the second, the later
    // records are valid but beyond the stop point and will be zeroed.
    flip_byte(&path_of(&cfg, 0, ws[2].ordinal), ws[2].start + 64 + 5);
    let j = open_journal(&cfg);
    let r = recover_all(&j);
    assert_eq!(seqs(&r), vec![1, 2]);
    // A read-around before the first lane open sees the stale records.
    assert_eq!(read(&j, &ws[0]), pattern(ws[0].seq, 16));
    let mut b = vec![0u8; 16 * 4096];
    assert!(j.read_at(loc_of(&ws[4]), &mut b).is_ok());
    let mut l = open_lane(&j, 0);
    assert_eq!(j.window_stats().resident_bytes, 0, "the wipe cleared it");
    // New writes land where the stale records were, under a cap that will
    // not admit most of them; every one reads back as written.
    let mut new = Vec::new();
    for i in 0..8u64 {
        new.push(write_rec(&mut *l, 100 + i, 16));
        complete_appends(&mut *l, 1);
    }
    group_flush(&j);
    for w in &new {
        assert_eq!(read(&j, w), pattern(w.seq, w.blocks));
    }
}

#[test]
fn many_threads_past_the_cap_read_exact_bytes() {
    let dir = scratch();
    let mut cfg = small_cfg(dir.path().join("journal"), 1, 3);
    cfg.resident_max = Some(BATCH_CAP);
    let (j, mut l) = fresh(&cfg);
    let ws = fill(&mut *l, &j, 24, 16);
    let (j, l) = restart(&cfg, (j, l));
    let bar = Barrier::new(6);
    std::thread::scope(|s| {
        for t in 0..6usize {
            let (j, ws, bar) = (&j, &ws, &bar);
            s.spawn(move || {
                bar.wait();
                for w in ws.iter().skip(t).step_by(6) {
                    assert_eq!(read(j, w), pattern(w.seq, w.blocks));
                }
            });
        }
    });
    let s = j.window_stats();
    assert!(s.resident_bytes <= s.cap_bytes, "{s:?}");
    assert_eq!(s.check_failures, 0);
    drop(l);
}

fn arb_plan() -> impl Strategy<Value = (Vec<u32>, Vec<(usize, u16, u16)>, bool, bool)> {
    (
        proptest::collection::vec(1u32..=24, 4..28),
        proptest::collection::vec((any::<usize>(), any::<u16>(), any::<u16>()), 1..40),
        any::<bool>(),
        any::<bool>(),
    )
}

struct Rig {
    j: FileJournal,
    lanes: Vec<Box<dyn JournalLane>>,
}

fn open_rig(cfg: &JournalConfig) -> Rig {
    let j = open_journal(cfg);
    recover_all(&j);
    let lanes = (0..cfg.lanes).map(|i| open_lane(&j, i)).collect();
    Rig { j, lanes }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 64, .. ProptestConfig::default() })]

    /// The window and the file always return the same bytes: live (resident
    /// or past a small cap), and after a restart (the read-around), over one
    /// or two lanes, for random records and sub-ranges, and they are the
    /// bytes that were written.
    #[test]
    fn the_window_and_the_file_return_the_same_bytes(plan in arb_plan()) {
        let (sizes, reads, capped, two) = plan;
        let dir = scratch();
        let lanes = if two { 2 } else { 1 };
        let mut cfg = small_cfg(dir.path().join("journal"), lanes, 4);
        if capped {
            cfg.resident_max = Some(BATCH_CAP);
        }
        let mut rig = open_rig(&cfg);
        let mut ws = Vec::new();
        for (i, &b) in sizes.iter().enumerate() {
            let l = i % usize::from(lanes);
            ws.push(write_rec(&mut *rig.lanes[l], i as u64, b));
            complete_appends(&mut *rig.lanes[l], 1);
        }
        group_flush(&rig.j);
        for phase in 0..2 {
            for &(wi, a, z) in &reads {
                let w = &ws[wi % ws.len()];
                let blocks = w.blocks as usize;
                let (x, y) = (usize::from(a) % blocks, usize::from(z) % blocks);
                let (from, to) = (x.min(y), x.max(y) + 1);
                let loc = loc_of(w).advance((from * 4096) as u64).expect("advance");
                let mut got = vec![0u8; (to - from) * 4096];
                rig.j.read_at(loc, &mut got).expect("read_at");
                let on_disk = read_at_path(&path_of(&cfg, w.lane, w.ordinal), loc.offset, got.len());
                prop_assert_eq!(&got, &on_disk);
                prop_assert_eq!(&got[..], &pattern(w.seq, w.blocks)[from * 4096..to * 4096]);
            }
            if phase == 0 {
                let s = rig.j.window_stats();
                prop_assert!(s.resident_bytes <= s.cap_bytes);
                drop(rig);
                rig = open_rig(&cfg);
            }
        }
    }
}
