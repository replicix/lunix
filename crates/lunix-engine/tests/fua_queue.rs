//! The FUA write on its own queue (ADR-32, review M2 and re-review m2-b): a
//! small FUA write overtakes the writes of its queue that wait in the gate
//! for their pace, but never one that waits for the journal ring, and two
//! overlapping writes in flight together end in the order of their
//! `write_seq`.
//!
//! The gate is the testkit's `MemSpace` with scripted pacing; the real gate's
//! band is tested in `lunix-space`, the whole path in
//! `lunixd/tests/stack_throttle.rs`.

mod common;

use common::*;
use lunix_iface::block::IoOp;
use lunix_testkit::JournalMode;

const CS: u32 = 16 << 10;

#[test]
fn a_fua_write_overtakes_a_paced_write_and_the_later_write_seq_wins_an_overlap() {
    let v = Vol::create(CS, 16, 1, JournalMode::Immediate);
    let mut r = v.open().map_err(|e| e.0).expect("open");
    r.space.set_pacing(true);
    // A bulk write waits for its pace; B, a FUA write over the same block,
    // is admitted at once and completes first.
    let a = r.submit(0, IoOp::Write, false, 0, 4096, &pattern(1, 4096));
    for _ in 0..20 {
        r.pump();
    }
    assert!(!r.is_done(a), "the bulk write is paced");
    let b = r.submit(0, IoOp::Write, true, 0, 4096, &pattern(2, 4096));
    let sb = r.wait(&[b])[0].0;
    assert!(sb.is_ok(), "{sb:?}");
    assert!(!r.is_done(a), "the FUA write overtook the paced write");
    assert_eq!(r.read(0, 0, 4096).1, pattern(2, 4096));
    // The pace ends: A is admitted later, gets the later `write_seq`, and
    // its data is what the block holds (two requests in flight together
    // have no defined order; this one is the order of admission).
    r.space.set_pacing(false);
    let sa = r.wait(&[a])[0].0;
    assert!(sa.is_ok(), "{sa:?}");
    assert_eq!(r.read(0, 0, 4096).1, pattern(1, 4096));
    // The same after a cut and a replay of nothing: the map agrees.
    r.engine.commit_now(false).expect("commit");
    assert_eq!(r.read(0, 0, 4096).1, pattern(1, 4096));
}

#[test]
fn a_fua_write_never_overtakes_a_write_that_waits_for_the_journal_ring() {
    // The front write waits for the ring; the ring has room again by the time
    // the FUA write arrives. Without the guard the FUA write is journaled
    // first and the front write, admitted after it, wins the overlap;
    // behind the guard the order is the queue's.
    let v = Vol::create(CS, 16, 1, JournalMode::Immediate);
    let mut r = v.open().map_err(|e| e.0).expect("open");
    assert!(r.write(0, 0, &pattern(1, 4096), false).is_ok());
    r.journal.set_nospace(true);
    let a = r.submit(0, IoOp::Write, false, 4096, 4096, &pattern(2, 4096));
    for _ in 0..50 {
        r.pump();
    }
    assert!(!r.is_done(a), "it waits for the ring");
    // Room again; nobody has retried A yet.
    r.journal.set_nospace(false);
    let b = r.submit(0, IoOp::Write, true, 4096, 4096, &pattern(3, 4096));
    let st = r.wait(&[a, b]);
    assert!(st[0].0.is_ok() && st[1].0.is_ok(), "{st:?}");
    assert_eq!(
        r.read(0, 4096, 4096).1,
        pattern(3, 4096),
        "the FUA write was journaled first and lost the overlap"
    );
}
