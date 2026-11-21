//! A lane read queued on a ring file races reclaim of that file: the guest
//! never gets zeros (or another incarnation's bytes) with a success status.
//! The read is refused (`ENOENT`, the engine reads the map) or it completes
//! before the file is zeroed.

mod common;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::*;
use lunix_format::WriteSeq;
use lunix_iface::journal::{Journal, JournalCompletion, JournalLane, JournalLoc, LaneId, Token};
use lunix_journal::FileJournal;

const BLOCKS: u32 = 16;

struct Rig {
    j: Arc<FileJournal>,
    l: Box<dyn JournalLane>,
    /// The records of the oldest ring file.
    first: Vec<Written>,
    tail: lunix_iface::journal::JournalTail,
}

fn loc_of(w: &Written) -> JournalLoc {
    JournalLoc {
        lane: LaneId(w.lane),
        ordinal: w.ordinal,
        offset: w.start + 64,
    }
}

/// Several ring files of records, a restart (the window is empty, as after an
/// attach that replayed records), and the tail that covers the first file.
fn rig() -> (tempfile::TempDir, Rig) {
    let dir = scratch();
    let cfg = small_cfg(dir.path().join("journal"), 1, 4);
    let j = open_journal(&cfg);
    recover_all(&j);
    let mut l = open_lane(&j, 0);
    let mut ws = Vec::new();
    for i in 0..100u64 {
        ws.push(write_rec(&mut *l, i * 16, BLOCKS));
        complete_appends(&mut *l, 1);
    }
    group_flush(&j);
    drop(l);
    drop(j);
    let j = open_journal(&cfg);
    recover_all(&j);
    let l = open_lane(&j, 0);
    let ord0 = ws[0].ordinal;
    let first: Vec<Written> = ws.iter().copied().filter(|w| w.ordinal == ord0).collect();
    let cut = first.last().expect("records").seq;
    let tail = j.tail_after(WriteSeq(cut)).expect("tail");
    (
        dir,
        Rig {
            j: Arc::new(j),
            l,
            first,
            tail,
        },
    )
}

fn poll_read(l: &mut dyn JournalLane) -> (lunix_iface::block::IoStatus, Vec<u8>) {
    let mut got = None;
    wait_until("read completion", || {
        let mut out = Vec::new();
        l.poll(&mut out);
        if let Some(JournalCompletion::Read { status, buf, .. }) = out.pop() {
            got = Some((status, buf));
        }
        got.is_some()
    });
    got.expect("completion")
}

/// Reclaim runs to completion between the lookup and the queued read.
#[test]
fn reclaim_in_the_gap_before_the_read_is_queued_never_yields_zeros() {
    let (_dir, mut r) = rig();
    let w = r.first[3];
    let fired = Arc::new(AtomicBool::new(false));
    {
        let j = Arc::clone(&r.j);
        let tail = r.tail.clone();
        let fired = Arc::clone(&fired);
        r.j.set_read_gap_hook(Some(Arc::new(move |_| {
            if !fired.swap(true, Ordering::SeqCst) {
                let rep = j.reclaim(&tail).expect("reclaim");
                assert!(rep.files >= 1, "the file was reclaimed in the gap");
            }
        })));
    }
    r.l.submit_read(loc_of(&w), vec![0xAA; BLOCKS as usize * 4096], Token(1))
        .expect("submit");
    assert!(fired.load(Ordering::SeqCst), "hook ran");
    let (status, buf) = poll_read(&mut *r.l);
    assert!(
        !status.is_ok(),
        "a reclaimed file is refused, got {:?} bytes ok={}",
        buf.first(),
        buf == pattern(w.seq, BLOCKS)
    );
    assert_eq!(status.errno(), Some(libc::ENOENT));
}

/// Reclaim starts while a queued read has not been reaped: it waits, the read
/// returns the record, the file is freed afterwards.
#[test]
fn reclaim_waits_for_a_queued_read_and_the_read_sees_the_record() {
    let (_dir, mut r) = rig();
    let w = r.first[5];
    r.l.submit_read(loc_of(&w), vec![0xAA; BLOCKS as usize * 4096], Token(2))
        .expect("submit");
    let done = Arc::new(Mutex::new(None));
    let h = {
        let j = Arc::clone(&r.j);
        let tail = r.tail.clone();
        let done = Arc::clone(&done);
        std::thread::spawn(move || {
            let rep = j.reclaim(&tail).expect("reclaim");
            *done.lock().expect("lock") = Some(rep.files);
        })
    };
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        done.lock().expect("lock").is_none(),
        "reclaim zeroed a file with a read in flight"
    );
    let (status, buf) = poll_read(&mut *r.l);
    assert!(status.is_ok(), "{status:?}");
    assert_eq!(buf, pattern(w.seq, BLOCKS));
    h.join().expect("reclaim thread");
    assert!(done.lock().expect("lock").expect("done") >= 1);
    // After the reclaim the location is refused.
    r.l.submit_read(loc_of(&w), vec![0u8; 4096], Token(3))
        .expect("submit");
    let (status, _) = poll_read(&mut *r.l);
    assert_eq!(status.errno(), Some(libc::ENOENT));
}

/// Reads queued while another thread reclaims the files below the cut, and the
/// lane refilling them with other bytes: every completion is the record or an
/// error, never zeros or a newer record. The window holds one batch, so the
/// older records are file reads.
#[test]
fn reads_racing_reclaim_and_reuse_return_the_record_or_an_error() {
    let dir = scratch();
    let mut cfg = small_cfg(dir.path().join("journal"), 1, 3);
    cfg.resident_max = Some((1 << 20) + 4096);
    let j = Arc::new(open_journal(&cfg));
    recover_all(&*j);
    let mut l = open_lane(&*j, 0);
    let mut live: Vec<Written> = Vec::new();
    let (mut oks, mut refused) = (0u32, 0u32);
    let mut lcg = 0x2545_f491_4f6c_dd1du64;
    for round in 0..60u64 {
        for i in 0..10 {
            live.push(write_rec(&mut *l, (round * 10 + i) * 16, BLOCKS));
            complete_appends(&mut *l, 1);
        }
        group_flush(&*j);
        let newest = live.last().expect("live").ordinal;
        // Reads of records outside the window, queued before the reclaim.
        let older: Vec<Written> = live[..live.len().saturating_sub(24)].to_vec();
        let mut want = std::collections::HashMap::new();
        for k in 0..8u64 {
            if older.is_empty() {
                break;
            }
            lcg = lcg.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            let w = older[(lcg >> 33) as usize % older.len()];
            l.submit_read(loc_of(&w), vec![0xAA; BLOCKS as usize * 4096], Token(k))
                .expect("submit");
            want.insert(k, w);
        }
        let cut = live
            .iter()
            .filter(|w| w.ordinal < newest)
            .map(|w| w.seq)
            .max();
        let h = cut.map(|cut| {
            let j = Arc::clone(&j);
            std::thread::spawn(move || {
                let tail = j.tail_after(WriteSeq(cut)).expect("tail");
                j.reclaim(&tail).expect("reclaim");
                tail.lane_ordinals[0]
            })
        });
        let mut seen = 0;
        wait_until("read completions", || {
            let mut out = Vec::new();
            l.poll(&mut out);
            for c in out {
                if let JournalCompletion::Read { token, status, buf } = c {
                    let w = want[&token.0];
                    if status.is_ok() {
                        assert_eq!(buf, pattern(w.seq, BLOCKS), "round {round}: wrong bytes");
                        oks += 1;
                    } else {
                        assert_eq!(status.errno(), Some(libc::ENOENT));
                        refused += 1;
                    }
                    seen += 1;
                }
            }
            seen >= want.len()
        });
        if let Some(h) = h {
            let tail0 = h.join().expect("reclaim thread");
            live.retain(|w| w.ordinal >= tail0);
        }
    }
    assert!(oks + refused > 100, "{oks} {refused}");
    eprintln!("jread stress: {oks} reads served, {refused} refused");
}
