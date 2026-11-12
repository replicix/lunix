//! Journal contract tests. See the [`Journal`] docs for what is promised.

use std::sync::mpsc;
use std::time::Duration;

use lunix_format::journal::RecordKind;
use lunix_format::WriteSeq;

use super::wait_until;
use crate::journal::{
    record_bytes, Appended, Journal, JournalCompletion, JournalLane, LaneId, Recovered,
    SlotOutcome, Token,
};
use crate::wake::WakeFd;

const T: Duration = Duration::from_secs(10);
const BLOCK: usize = 4096;

/// A journal under test plus the one thing the trait cannot do generically.
pub trait JournalFixture {
    /// The journal. Valid until the next [`crash_and_reopen`](Self::crash_and_reopen).
    fn journal(&self) -> &dyn Journal;

    /// Simulates a crash and restart on the same storage: every lane and
    /// handle is dropped without flushing, the storage may lose or tear
    /// whatever was not made durable by a completed group flush (a fake does
    /// this at random; a real journal may simply reopen its files), and a new
    /// journal replaces the old one. Lanes opened before are gone. The caller
    /// runs `recover` afterwards, as the engine does.
    fn crash_and_reopen(&mut self);

    /// Makes the next `append` on `lane` fail with a fatal I/O error (after the
    /// slot was consumed). Returns false when the fixture cannot inject
    /// failures, in which case the test that needs it returns early.
    fn fail_next_append(&mut self, _lane: LaneId) -> bool {
        false
    }
}

/// The data of a record: every byte derives from the sequence number.
fn pattern(seq: u64, blocks: u32) -> Vec<u8> {
    let mut v = vec![0u8; blocks as usize * BLOCK];
    for (i, b) in v.iter_mut().enumerate() {
        *b = (seq as usize).wrapping_mul(31).wrapping_add(i) as u8;
    }
    v
}

fn open_lane(j: &dyn Journal, lane: u16) -> Box<dyn JournalLane> {
    j.open_lane(LaneId(lane), WakeFd::new().expect("eventfd"))
        .expect("open lane")
}

/// Reserves a one-block (or `blocks`) write slot; returns the slot's sequence
/// number without appending yet.
fn reserve(lane: &mut dyn JournalLane, blocks: u32) -> crate::journal::Slot {
    match lane
        .try_reserve(RecordKind::Write, blocks)
        .expect("reserve")
    {
        SlotOutcome::Reserved(slot) => {
            assert_eq!(slot.record_bytes(), record_bytes(RecordKind::Write, blocks));
            slot
        }
        other => panic!("reservation refused: {other:?}"),
    }
}

/// Reserves and appends a write whose data derives from its sequence number.
fn write(lane: &mut dyn JournalLane, lba: u64, blocks: u32) -> Appended {
    let slot = reserve(lane, blocks);
    let data = pattern(slot.write_seq.0, blocks);
    lane.append(slot, lba, &data).expect("append")
}

/// Polls until `n` append completions arrived; returns their `write_seq`s.
fn complete_appends(lane: &mut dyn JournalLane, n: usize) -> Vec<WriteSeq> {
    let mut done = Vec::new();
    let mut seen = 0;
    wait_until("append completions", T, || {
        let mut out = Vec::new();
        lane.poll(&mut out);
        for c in out {
            if let JournalCompletion::Append { write_seq, status } = c {
                assert!(status.is_ok(), "append failed: {status:?}");
                done.push(write_seq);
                seen += 1;
            }
        }
        seen >= n
    });
    done
}

fn flush_channel(j: &dyn Journal, upto: WriteSeq) -> mpsc::Receiver<bool> {
    let (tx, rx) = mpsc::channel();
    j.submit_group_flush(
        upto,
        Box::new(move |r| {
            let _ = tx.send(r.is_ok());
        }),
    )
    .expect("submit flush");
    rx
}

/// Flushes everything issued so far and waits for it.
fn group_flush(j: &dyn Journal) {
    let rx = flush_channel(j, j.last_issued());
    assert!(
        rx.recv_timeout(T).expect("flush completion"),
        "flush failed"
    );
}

fn seqs(r: &Recovered) -> Vec<u64> {
    r.records.iter().map(|x| x.record.write_seq.0).collect()
}

/// A fresh journal recovers nothing, the next sequence follows the floor and
/// `last_issued` agrees.
pub fn empty_journal_recovers_nothing<F: JournalFixture>(f: &mut F) {
    let r = f.journal().recover(None, WriteSeq(0)).expect("recover");
    assert!(r.records.is_empty());
    assert_eq!(r.next_seq, WriteSeq(1));
    assert_eq!((r.ignored_after_gap, r.gap_at), (None, None));
    assert_eq!(f.journal().last_issued(), WriteSeq(0));
    let r = f.journal().recover(None, WriteSeq(40)).expect("recover");
    assert_eq!(r.next_seq, WriteSeq(41));
    assert_eq!(f.journal().last_issued(), WriteSeq(40));
}

/// Sequence numbers come from the journal, strictly rising across lanes; an
/// accepted append completes, its data reads back through `read_at`, through
/// the lane's own `submit_read` and through another lane's, and a sub-range is
/// addressed with `advance`.
pub fn appended_records_complete_and_read_back<F: JournalFixture>(f: &mut F) {
    f.journal().recover(None, WriteSeq(0)).expect("recover");
    let j = f.journal();
    let mut l0 = open_lane(j, 0);
    let mut l1 = open_lane(j, 1);
    let a = write(&mut *l0, 8, 2);
    let b = write(&mut *l1, 20, 1);
    assert_eq!((a.write_seq, b.write_seq), (WriteSeq(1), WriteSeq(2)));
    assert_eq!(a.loc.lane, LaneId(0));
    assert_eq!(j.last_issued(), WriteSeq(2));
    assert_eq!(complete_appends(&mut *l0, 1), vec![WriteSeq(1)]);
    assert_eq!(complete_appends(&mut *l1, 1), vec![WriteSeq(2)]);

    let data = pattern(1, 2);
    let mut buf = vec![0u8; data.len()];
    j.read_at(a.loc, &mut buf).expect("read_at");
    assert_eq!(buf, data);
    let second = a.loc.advance(BLOCK as u64).expect("advance");
    let mut one = vec![0u8; BLOCK];
    j.read_at(second, &mut one).expect("read_at sub-range");
    assert_eq!(one, data[BLOCK..]);

    // Lane 1 reads what lane 0 wrote.
    l1.submit_read(a.loc, vec![0u8; data.len()], Token(77))
        .expect("submit_read");
    let mut got = None;
    wait_until("read completion", T, || {
        let mut out = Vec::new();
        l1.poll(&mut out);
        for c in out {
            if let JournalCompletion::Read { token, status, buf } = c {
                assert_eq!(token, Token(77));
                assert!(status.is_ok());
                got = Some(buf);
            }
        }
        got.is_some()
    });
    assert_eq!(got.expect("buf"), data);
}

/// A hole record needs no data and completes like a write.
pub fn holes_carry_no_data<F: JournalFixture>(f: &mut F) {
    f.journal().recover(None, WriteSeq(0)).expect("recover");
    let mut lane = open_lane(f.journal(), 0);
    let slot = match lane.try_reserve(RecordKind::Hole, 64).expect("reserve") {
        SlotOutcome::Reserved(s) => s,
        other => panic!("refused: {other:?}"),
    };
    assert_eq!(slot.record_bytes(), 64);
    lane.append(slot, 0, &[]).expect("append");
    assert_eq!(complete_appends(&mut *lane, 1), vec![WriteSeq(1)]);
}

/// The watermark is zero when idle and fresh, and covers completed appends.
pub fn watermark_follows_completions<F: JournalFixture>(f: &mut F) {
    f.journal().recover(None, WriteSeq(0)).expect("recover");
    let mut lane = open_lane(f.journal(), 0);
    assert_eq!(lane.completed_watermark(), WriteSeq(0));
    for i in 1..=3 {
        write(&mut *lane, i * 4, 1);
    }
    complete_appends(&mut *lane, 3);
    assert!(lane.completed_watermark() >= WriteSeq(3));
}

/// Records appended before a completed group flush survive a crash, in
/// `write_seq` order, across lanes.
pub fn group_flush_makes_completed_records_durable<F: JournalFixture>(f: &mut F) {
    f.journal().recover(None, WriteSeq(0)).expect("recover");
    {
        let j = f.journal();
        let mut l0 = open_lane(j, 0);
        let mut l1 = open_lane(j, 1);
        for i in 1..=6u64 {
            let lane: &mut dyn JournalLane = if i % 2 == 1 { &mut *l0 } else { &mut *l1 };
            write(lane, i * 4, 1);
        }
        complete_appends(&mut *l0, 3);
        complete_appends(&mut *l1, 3);
        group_flush(j);
    }
    f.crash_and_reopen();
    let r = f.journal().recover(None, WriteSeq(0)).expect("recover");
    assert_eq!(seqs(&r), vec![1, 2, 3, 4, 5, 6]);
    assert_eq!(r.next_seq, WriteSeq(7));
    assert_eq!((r.ignored_after_gap, r.gap_at), (None, None));
    assert_eq!(f.journal().last_issued(), WriteSeq(6));
    for rec in &r.records {
        let mut buf = vec![0u8; BLOCK];
        f.journal().read_at(rec.loc, &mut buf).expect("read_at");
        assert_eq!(buf, pattern(rec.record.write_seq.0, 1));
    }
}

/// The regression for the original gap: a slot reserved on one lane but not yet
/// appended, with a higher sequence number completed and acknowledged on
/// another lane. A group flush formed at that moment (`upto = last_issued()`)
/// must not complete until the outstanding slot is appended and written, so a
/// crash after the flush returns can never leave a gap below an acknowledged
/// record.
pub fn flush_waits_for_an_outstanding_slot<F: JournalFixture>(f: &mut F) {
    f.journal().recover(None, WriteSeq(0)).expect("recover");
    {
        let j = f.journal();
        let mut l0 = open_lane(j, 0);
        let mut l1 = open_lane(j, 1);
        let pending = reserve(&mut *l0, 1); // seq 1, in flight, not appended
        let b = write(&mut *l1, 40, 1); // seq 2
        assert_eq!((pending.write_seq, b.write_seq), (WriteSeq(1), WriteSeq(2)));
        complete_appends(&mut *l1, 1);

        let rx = flush_channel(j, j.last_issued());
        assert!(
            rx.recv_timeout(Duration::from_millis(100)).is_err(),
            "the flush must wait for the outstanding slot"
        );
        let data = pattern(1, 1);
        l0.append(pending, 4, &data).expect("append");
        complete_appends(&mut *l0, 1);
        assert!(
            rx.recv_timeout(T).expect("flush completion"),
            "flush failed"
        );
    }
    f.crash_and_reopen();
    let r = f.journal().recover(None, WriteSeq(0)).expect("recover");
    assert_eq!(seqs(&r), vec![1, 2], "no gap below the acknowledged record");
    assert_eq!(r.ignored_after_gap, None);
}

/// `recover` returns only records above the floor and never moves `next_seq`
/// below `floor + 1`.
pub fn recover_filters_by_floor<F: JournalFixture>(f: &mut F) {
    f.journal().recover(None, WriteSeq(0)).expect("recover");
    {
        let j = f.journal();
        let mut lane = open_lane(j, 0);
        for i in 1..=6u64 {
            write(&mut *lane, i * 4, 1);
        }
        complete_appends(&mut *lane, 6);
        group_flush(j);
    }
    f.crash_and_reopen();
    let r = f.journal().recover(None, WriteSeq(3)).expect("recover");
    assert_eq!(seqs(&r), vec![4, 5, 6]);
    assert_eq!(r.next_seq, WriteSeq(7));
    let r = f.journal().recover(None, WriteSeq(6)).expect("recover");
    assert!(r.records.is_empty());
    assert_eq!(r.next_seq, WriteSeq(7));
}

/// Recovery does not change what it reads: running it twice agrees.
pub fn recover_is_idempotent<F: JournalFixture>(f: &mut F) {
    f.journal().recover(None, WriteSeq(0)).expect("recover");
    {
        let j = f.journal();
        let mut lane = open_lane(j, 0);
        for i in 1..=4u64 {
            write(&mut *lane, i * 4, 1);
        }
        complete_appends(&mut *lane, 4);
        group_flush(j);
    }
    f.crash_and_reopen();
    let a = f.journal().recover(None, WriteSeq(0)).expect("recover");
    let b = f.journal().recover(None, WriteSeq(0)).expect("recover");
    assert_eq!(a, b);
}

/// After a crash with an unflushed tail, recovery returns a contiguous prefix
/// that contains every flushed record, reports what lies beyond a gap, and
/// every returned record reads back.
pub fn recovery_after_crash_is_a_contiguous_prefix<F: JournalFixture>(f: &mut F) {
    f.journal().recover(None, WriteSeq(0)).expect("recover");
    {
        let j = f.journal();
        let mut l0 = open_lane(j, 0);
        let mut l1 = open_lane(j, 1);
        for i in 1..=4u64 {
            let lane: &mut dyn JournalLane = if i % 2 == 1 { &mut *l0 } else { &mut *l1 };
            write(lane, i * 4, 1);
        }
        complete_appends(&mut *l0, 2);
        complete_appends(&mut *l1, 2);
        group_flush(j);
        for i in 5..=8u64 {
            let lane: &mut dyn JournalLane = if i % 2 == 1 { &mut *l0 } else { &mut *l1 };
            write(lane, i * 4, 1);
        }
        // Whether these reach the file before the crash is the fixture's choice.
    }
    f.crash_and_reopen();
    let r = f.journal().recover(None, WriteSeq(0)).expect("recover");
    let got = seqs(&r);
    assert!(got.len() >= 4 && got.len() <= 8, "got {got:?}");
    assert_eq!(got, (1..=got.len() as u64).collect::<Vec<_>>());
    assert_eq!(r.next_seq, WriteSeq(got.len() as u64 + 1));
    assert_eq!(r.ignored_after_gap.is_some(), r.gap_at.is_some());
    if let Some(gap) = r.gap_at {
        assert_eq!(
            gap, r.next_seq,
            "the first missing seq is the next free one"
        );
    }
    for rec in &r.records {
        let mut buf = vec![0u8; BLOCK];
        f.journal().read_at(rec.loc, &mut buf).expect("read_at");
        assert_eq!(buf, pattern(rec.record.write_seq.0, 1));
    }
}

/// Reclaiming up to the tail after a cut never removes a record above the cut.
pub fn reclaim_keeps_records_above_the_tail<F: JournalFixture>(f: &mut F) {
    f.journal().recover(None, WriteSeq(0)).expect("recover");
    {
        let j = f.journal();
        let mut lane = open_lane(j, 0);
        for i in 1..=6u64 {
            write(&mut *lane, i * 4, 1);
        }
        complete_appends(&mut *lane, 6);
        group_flush(j);
        let tail = j.tail_after(WriteSeq(3)).expect("tail");
        assert_eq!(tail.lane_ordinals.len(), usize::from(j.lanes()));
        j.reclaim(&tail).expect("reclaim");
        assert!(j.fill().ratio() <= 1.0);
    }
    f.crash_and_reopen();
    let r = f.journal().recover(None, WriteSeq(3)).expect("recover");
    assert_eq!(seqs(&r), vec![4, 5, 6]);
    for rec in &r.records {
        let mut buf = vec![0u8; BLOCK];
        f.journal().read_at(rec.loc, &mut buf).expect("read_at");
        assert_eq!(buf, pattern(rec.record.write_seq.0, 1));
    }
}

/// A failed append poisons the journal: a flush covering the failed sequence
/// number, pending or later, never reports success, and no lane accepts further
/// reservations or appends. (Skipped when the fixture cannot inject failures.)
pub fn failed_append_poisons_the_journal<F: JournalFixture>(f: &mut F) {
    f.journal().recover(None, WriteSeq(0)).expect("recover");
    if !f.fail_next_append(LaneId(0)) {
        eprintln!("skipped: fixture cannot inject append failures");
        return;
    }
    let j = f.journal();
    let mut l0 = open_lane(j, 0);
    let mut l1 = open_lane(j, 1);
    let pending = reserve(&mut *l0, 1); // seq 1
    write(&mut *l1, 40, 1); // seq 2, acknowledged
    complete_appends(&mut *l1, 1);
    let rx = flush_channel(j, j.last_issued());
    let e = l0
        .append(pending, 4, &pattern(1, 1))
        .expect_err("injected failure");
    assert_eq!(e.kind(), crate::ErrorKind::Io);
    assert!(
        !rx.recv_timeout(T).expect("the pending flush must complete"),
        "a flush over a missing sequence number must not succeed"
    );
    assert!(!flush_channel(j, j.last_issued())
        .recv_timeout(T)
        .expect("later flush"));
    for lane in [&mut l0, &mut l1] {
        let e = lane
            .try_reserve(RecordKind::Write, 1)
            .expect_err("poisoned");
        assert_eq!(e.kind(), crate::ErrorKind::Io);
    }
}

/// A slot that is never appended is detected at the next call on its lane:
/// `Invalid`, and the journal is poisoned so a covering flush fails instead of
/// hanging. (The slot is leaked on purpose; `Drop` would assert in debug builds.)
pub fn a_dropped_slot_is_detected_and_poisons<F: JournalFixture>(f: &mut F) {
    f.journal().recover(None, WriteSeq(0)).expect("recover");
    let j = f.journal();
    let mut lane = open_lane(j, 0);
    std::mem::forget(reserve(&mut *lane, 1));
    let e = lane
        .try_reserve(RecordKind::Write, 1)
        .expect_err("outstanding slot");
    assert_eq!(e.kind(), crate::ErrorKind::Invalid);
    assert!(!flush_channel(j, j.last_issued())
        .recv_timeout(T)
        .expect("flush completes"));
    assert!(lane.try_reserve(RecordKind::Write, 1).is_err());
}

/// A slot from another lane is refused with `Invalid` (and poisons).
pub fn a_foreign_slot_is_refused<F: JournalFixture>(f: &mut F) {
    f.journal().recover(None, WriteSeq(0)).expect("recover");
    let j = f.journal();
    let mut l0 = open_lane(j, 0);
    let mut l1 = open_lane(j, 1);
    let slot = reserve(&mut *l0, 1);
    let e = l1
        .append(slot, 4, &pattern(1, 1))
        .expect_err("foreign slot");
    assert_eq!(e.kind(), crate::ErrorKind::Invalid);
    assert!(l1.try_reserve(RecordKind::Write, 1).is_err());
}
