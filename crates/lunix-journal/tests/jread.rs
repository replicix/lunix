//! A lane read queued on a ring file races reclaim of that file: the guest
//! never gets zeros (or another incarnation's bytes) with a success status.
//! The read is refused (`ENOENT`, the engine reads the map) or it completes
//! before the file is zeroed. Every test restarts the journal first, so the
//! resident window is empty (an attach that replayed records) and every read
//! is a ring file read.

mod common;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::channel;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::*;
use lunix_format::WriteSeq;
use lunix_iface::block::IoStatus;
use lunix_iface::journal::{
    Journal, JournalCompletion, JournalLane, JournalLoc, JournalTail, LaneId, Token,
};
use lunix_journal::FileJournal;

const BLOCKS: u32 = 16;
const SIZE: usize = BLOCKS as usize * 4096;

struct Rig {
    _dir: tempfile::TempDir,
    j: Arc<FileJournal>,
    lanes: Vec<Box<dyn JournalLane>>,
    /// The records of each lane, by file in ordinal order: `files[lane][n]`.
    files: Vec<Vec<Vec<Written>>>,
    /// Covers the first `cover` files of every lane.
    tail: JournalTail,
}

fn loc_of(w: &Written) -> JournalLoc {
    JournalLoc {
        lane: LaneId(w.lane),
        ordinal: w.ordinal,
        offset: w.start + 64,
    }
}

/// Records on every lane (about three ring files each), a restart, and the
/// tail of a cut that covers the first `cover` files of each lane.
fn rig(lanes: u16, cover: usize) -> Rig {
    let dir = scratch();
    let cfg = small_cfg(dir.path().join("journal"), lanes, 4);
    let j = open_journal(&cfg);
    recover_all(&j);
    let mut ls: Vec<_> = (0..lanes).map(|i| open_lane(&j, i)).collect();
    let mut ws: Vec<Vec<Written>> = vec![Vec::new(); usize::from(lanes)];
    for i in 0..90u64 {
        for (n, l) in ls.iter_mut().enumerate() {
            ws[n].push(write_rec(&mut **l, i * 16, BLOCKS));
            complete_appends(&mut **l, 1);
        }
    }
    group_flush(&j);
    drop(ls);
    drop(j);
    let j = open_journal(&cfg);
    recover_all(&j);
    let lanes_v: Vec<_> = (0..lanes).map(|i| open_lane(&j, i)).collect();
    let mut files = Vec::new();
    let mut cut = 0;
    for w in &ws {
        let mut by: Vec<Vec<Written>> = Vec::new();
        for r in w {
            match by.last_mut() {
                Some(f) if f[0].ordinal == r.ordinal => f.push(*r),
                _ => by.push(vec![*r]),
            }
        }
        for f in by.iter().take(cover) {
            cut = cut.max(f.last().expect("records").seq);
        }
        files.push(by);
    }
    let tail = j.tail_after(WriteSeq(cut)).expect("tail");
    Rig {
        _dir: dir,
        j: Arc::new(j),
        lanes: lanes_v,
        files,
        tail,
    }
}

fn poll_reads(l: &mut dyn JournalLane, n: usize) -> Vec<(Token, IoStatus, Vec<u8>)> {
    let mut got = Vec::new();
    wait_until("read completions", || {
        let mut out = Vec::new();
        l.poll(&mut out);
        for c in out {
            if let JournalCompletion::Read { token, status, buf } = c {
                got.push((token, status, buf));
            }
        }
        got.len() >= n
    });
    got
}

fn read_one(l: &mut dyn JournalLane, w: &Written, via: Token) -> (IoStatus, Vec<u8>) {
    l.submit_read(loc_of(w), vec![0xAA; SIZE], via)
        .expect("submit");
    let (_, s, b) = poll_reads(l, 1).pop().expect("completion");
    (s, b)
}

fn refused(status: IoStatus) {
    assert!(!status.is_ok(), "the read of a reclaimed file succeeded");
    assert_eq!(status.errno(), Some(libc::ENOENT));
}

/// Reclaim runs to completion between the lookup and the queued read.
#[test]
fn reclaim_in_the_gap_before_the_read_is_queued_never_yields_zeros() {
    let mut r = rig(1, 1);
    let w = r.files[0][0][3];
    let fired = Arc::new(AtomicBool::new(false));
    {
        let j = Arc::clone(&r.j);
        let tail = r.tail.clone();
        let fired = Arc::clone(&fired);
        r.j.set_read_gap_hook(Some(Arc::new(move |_| {
            if !fired.swap(true, Ordering::SeqCst) {
                assert!(j.reclaim(&tail).expect("reclaim").files >= 1);
            }
        })));
    }
    let (status, _) = read_one(&mut *r.lanes[0], &w, Token(1));
    assert!(fired.load(Ordering::SeqCst), "hook ran");
    refused(status);
}

/// The same, and the lane refills the freed file with other bytes before the
/// read goes on: a read of lane 0's file through lane 1.
#[test]
fn a_cross_lane_read_of_a_file_reclaimed_and_reused_in_the_gap_is_refused() {
    let mut r = rig(2, 1);
    let w = r.files[0][0][3];
    let old: Vec<u32> = r.files[0].iter().map(|f| f[0].ordinal).collect();
    let lane0 = Arc::new(Mutex::new(r.lanes.remove(0)));
    let reused = Arc::new(AtomicBool::new(false));
    {
        let j = Arc::clone(&r.j);
        let tail = r.tail.clone();
        let lane0 = Arc::clone(&lane0);
        let reused = Arc::clone(&reused);
        let fired = AtomicBool::new(false);
        r.j.set_read_gap_hook(Some(Arc::new(move |_| {
            if fired.swap(true, Ordering::SeqCst) {
                return;
            }
            assert!(j.reclaim(&tail).expect("reclaim").files >= 1);
            let mut l = lane0.lock().expect("lane 0");
            for i in 0..400u64 {
                let a = write_rec(&mut **l, 5_000 + i * 16, BLOCKS);
                complete_appends(&mut **l, 1);
                if !old.contains(&a.ordinal) {
                    reused.store(true, Ordering::SeqCst);
                    break;
                }
            }
        })));
    }
    let (status, _) = read_one(&mut *r.lanes[0], &w, Token(1));
    assert!(reused.load(Ordering::SeqCst), "lane 0 did not reuse a file");
    refused(status);
}

/// The file is in use when the read looks it up and `Recycling` when the read
/// pins it: reclaim has marked the second of two files and not touched it (the
/// first is free) in the gap between the two. The pin refuses it.
#[test]
fn a_file_that_turns_recycling_in_the_gap_is_refused_at_the_pin() {
    let mut r = rig(1, 2);
    let w = r.files[0][1][3];
    let (at_free_tx, at_free) = channel::<()>();
    let (hold_tx, hold_rx) = channel::<()>();
    let at_free_tx = Mutex::new(at_free_tx);
    let at_free = Mutex::new(at_free);
    let hold_rx = Mutex::new(hold_rx);
    r.j.set_free_hook(Some(Arc::new(move |_, freed| {
        if freed == 1 {
            at_free_tx.lock().expect("tx").send(()).expect("signal");
            hold_rx.lock().expect("rx").recv().expect("go");
        }
    })));
    let handle: Arc<Mutex<Option<std::thread::JoinHandle<usize>>>> = Arc::default();
    {
        let j = Arc::clone(&r.j);
        let tail = r.tail.clone();
        let handle = Arc::clone(&handle);
        r.j.set_read_gap_hook(Some(Arc::new(move |_| {
            let (j, tail) = (Arc::clone(&j), tail.clone());
            let h = std::thread::spawn(move || j.reclaim(&tail).expect("reclaim").files as usize);
            *handle.lock().expect("handle") = Some(h);
            at_free
                .lock()
                .expect("rx")
                .recv_timeout(T)
                .expect("first file freed");
        })));
    }
    let (status, _) = read_one(&mut *r.lanes[0], &w, Token(1));
    hold_tx.send(()).expect("release");
    let h = handle
        .lock()
        .expect("handle")
        .take()
        .expect("reclaim started");
    assert_eq!(h.join().expect("reclaim thread"), 2);
    refused(status);
}

/// Reclaim starts while a queued read has not been reaped: it waits (the hook
/// says so), the read returns the record, the file is freed afterwards.
#[test]
fn reclaim_waits_for_a_queued_read_and_the_read_sees_the_record() {
    let mut r = rig(1, 1);
    let w = r.files[0][0][5];
    let (tx, rx) = channel::<()>();
    let tx = Mutex::new(tx);
    r.j.set_reclaim_drain_hook(Some(Arc::new(move || {
        let _ = tx.lock().expect("tx").send(());
    })));
    r.lanes[0]
        .submit_read(loc_of(&w), vec![0xAA; SIZE], Token(2))
        .expect("submit");
    let done = Arc::new(AtomicBool::new(false));
    let h = {
        let j = Arc::clone(&r.j);
        let tail = r.tail.clone();
        let done = Arc::clone(&done);
        std::thread::spawn(move || {
            let rep = j.reclaim(&tail).expect("reclaim");
            done.store(true, Ordering::SeqCst);
            rep.files
        })
    };
    rx.recv_timeout(T)
        .expect("reclaim never waited for the read");
    assert!(!done.load(Ordering::SeqCst), "reclaim zeroed under a read");
    let (_, status, buf) = poll_reads(&mut *r.lanes[0], 1).pop().expect("read");
    assert!(status.is_ok(), "{status:?}");
    assert_eq!(buf, pattern(w.seq, BLOCKS));
    assert!(h.join().expect("reclaim thread") >= 1);
    let (status, _) = read_one(&mut *r.lanes[0], &w, Token(3));
    refused(status);
}

/// A read that is never reaped: reclaim times out on its file only. The other
/// lane's files are freed in the same call, the pinned file stays `Used` and
/// readable, and the next call frees it once the read is reaped.
#[test]
fn a_drain_timeout_leaves_the_other_lanes_reclaimed_and_the_file_readable() {
    let mut r = rig(2, 1);
    r.j.set_reclaim_drain_limit(Duration::from_millis(300));
    let w = r.files[0][0][2];
    r.lanes[0]
        .submit_read(loc_of(&w), vec![0xAA; SIZE], Token(7))
        .expect("submit");
    let t0 = std::time::Instant::now();
    let err = r.j.reclaim(&r.tail).expect_err("the wait times out");
    assert!(
        t0.elapsed() < Duration::from_secs(5),
        "one deadline per call"
    );
    assert_eq!(err.kind(), lunix_iface::ErrorKind::Io, "{err}");
    let freed = r.j.freed_files_by_lane();
    assert_eq!(freed[0], 0, "the pinned lane's file stays: {freed:?}");
    assert!(
        freed[1] >= 1,
        "lane 1 was reclaimed in the same call: {freed:?}"
    );
    // Lane 1 can use its freed files; the pinned file still reads right.
    let a = write_rec(&mut *r.lanes[1], 900_000, BLOCKS);
    complete_appends(&mut *r.lanes[1], 1);
    assert_eq!(a.lane, 1);
    let mut b = vec![0u8; SIZE];
    r.j.read_at(loc_of(&w), &mut b).expect("still readable");
    assert_eq!(b, pattern(w.seq, BLOCKS));
    let (_, status, buf) = poll_reads(&mut *r.lanes[0], 1).pop().expect("read");
    assert!(status.is_ok(), "{status:?}");
    assert_eq!(buf, pattern(w.seq, BLOCKS));
    let rep = r.j.reclaim(&r.tail).expect("second call");
    assert!(rep.files >= 1);
    assert!(r.j.freed_files_by_lane()[0] >= 1);
}

/// A reclaim that failed on a file leaves it `Used` but refused to reads (its
/// zeroing may have begun) until a reclaim redid it.
#[test]
fn a_file_whose_reinit_failed_is_refused_to_reads_until_it_is_redone() {
    let mut r = rig(1, 1);
    let w = r.files[0][0][4];
    r.j.fail_nth_reclaim_file(1);
    assert!(r.j.reclaim(&r.tail).is_err());
    let (status, _) = read_one(&mut *r.lanes[0], &w, Token(1));
    refused(status);
    let rep = r.j.reclaim(&r.tail).expect("retry");
    assert!(rep.files >= 1);
}

/// A read whose `submit` failed after its entry was queued completes
/// normally; its id is not reused while the entry may still complete.
#[test]
fn a_failed_submit_of_a_queued_read_does_not_recycle_its_id() {
    let mut r = rig(1, 1);
    let (a, b) = (r.files[0][0][1], r.files[0][0][6]);
    r.j.fail_next_read_submit();
    let l = &mut *r.lanes[0];
    l.submit_read(loc_of(&a), vec![0xAA; SIZE], Token(1))
        .expect("submit a");
    l.submit_read(loc_of(&b), vec![0xBB; SIZE], Token(2))
        .expect("submit b");
    let mut got = poll_reads(l, 2);
    got.sort_by_key(|g| g.0 .0);
    for ((tok, status, buf), w) in got.into_iter().zip([a, b]) {
        assert!(status.is_ok(), "read {tok:?}: {status:?}");
        assert_eq!(buf, pattern(w.seq, BLOCKS), "read {tok:?}");
    }
}
