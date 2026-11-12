//! Journal contract tests. See the [`Journal`] docs for what is promised.

use std::sync::mpsc;
use std::time::Duration;

use lunix_format::journal::RecordKind;
use lunix_format::WriteSeq;

use super::wait_until;
use crate::journal::{
    AppendOutcome, AppendRecord, Journal, JournalCompletion, JournalLane, JournalLoc, LaneId,
    Recovered, Token,
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
    /// journal replaces the old one. Lanes opened before are gone.
    fn crash_and_reopen(&mut self);
}

/// The data of record `seq`: every block starts with the sequence number.
fn pattern(seq: u64, blocks: u32) -> Vec<u8> {
    let mut v = vec![0u8; blocks as usize * BLOCK];
    for (i, b) in v.iter_mut().enumerate() {
        *b = (seq as usize).wrapping_mul(31).wrapping_add(i) as u8;
    }
    v
}

fn write_rec(seq: u64, lba: u64, blocks: u32) -> AppendRecord {
    AppendRecord {
        kind: RecordKind::Write,
        write_seq: WriteSeq(seq),
        lba,
        blocks,
    }
}

fn open_lane(j: &dyn Journal, lane: u16) -> Box<dyn JournalLane> {
    j.open_lane(LaneId(lane), WakeFd::new().expect("eventfd"))
        .expect("open lane")
}

fn append_ok(lane: &mut dyn JournalLane, rec: &AppendRecord, data: &[u8]) -> JournalLoc {
    match lane.try_append(rec, data).expect("append") {
        AppendOutcome::Appended { loc } => loc,
        other => panic!("append was not accepted: {other:?}"),
    }
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

fn group_flush(j: &dyn Journal, upto: u64) {
    let (tx, rx) = mpsc::channel();
    j.submit_group_flush(
        WriteSeq(upto),
        Box::new(move |r| {
            let _ = tx.send(r.is_ok());
        }),
    )
    .expect("submit flush");
    assert!(
        rx.recv_timeout(T).expect("flush completion"),
        "flush failed"
    );
}

fn seqs(r: &Recovered) -> Vec<u64> {
    r.records.iter().map(|x| x.record.write_seq.0).collect()
}

/// A fresh journal recovers nothing and the next sequence follows the floor.
pub fn empty_journal_recovers_nothing<F: JournalFixture>(f: &mut F) {
    let r = f.journal().recover(None, WriteSeq(0)).expect("recover");
    assert!(r.records.is_empty());
    assert_eq!(r.next_seq, WriteSeq(1));
    assert_eq!(r.ignored_after_gap, None);
    let r = f.journal().recover(None, WriteSeq(40)).expect("recover");
    assert_eq!(r.next_seq, WriteSeq(41));
}

/// An accepted append completes, its data reads back through the lane and
/// through the blocking `read_at`, and a sub-range is addressed with `advance`.
pub fn appended_records_complete_and_read_back<F: JournalFixture>(f: &mut F) {
    let j = f.journal();
    let mut lane = open_lane(j, 0);
    let data = pattern(1, 2);
    let loc = append_ok(&mut *lane, &write_rec(1, 8, 2), &data);
    assert_eq!(loc.lane, LaneId(0));
    assert_eq!(complete_appends(&mut *lane, 1), vec![WriteSeq(1)]);

    let mut buf = vec![0u8; data.len()];
    j.read_at(loc, &mut buf).expect("read_at");
    assert_eq!(buf, data);

    let second = loc.advance(BLOCK as u64).expect("advance");
    let mut one = vec![0u8; BLOCK];
    j.read_at(second, &mut one).expect("read_at sub-range");
    assert_eq!(one, data[BLOCK..]);

    lane.submit_read(loc, vec![0u8; data.len()], Token(77))
        .expect("submit_read");
    let mut got = None;
    wait_until("read completion", T, || {
        let mut out = Vec::new();
        lane.poll(&mut out);
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
    let mut lane = open_lane(f.journal(), 0);
    let rec = AppendRecord {
        kind: RecordKind::Hole,
        write_seq: WriteSeq(1),
        lba: 0,
        blocks: 64,
    };
    append_ok(&mut *lane, &rec, &[]);
    assert_eq!(complete_appends(&mut *lane, 1), vec![WriteSeq(1)]);
}

/// The watermark is zero when idle and fresh, and covers completed appends.
pub fn watermark_follows_completions<F: JournalFixture>(f: &mut F) {
    let mut lane = open_lane(f.journal(), 0);
    assert_eq!(lane.completed_watermark(), WriteSeq(0));
    for seq in 1..=3 {
        append_ok(&mut *lane, &write_rec(seq, seq * 4, 1), &pattern(seq, 1));
    }
    complete_appends(&mut *lane, 3);
    assert!(lane.completed_watermark() >= WriteSeq(3));
}

/// Records whose appends completed before a group flush completed survive a
/// crash, in `write_seq` order, across lanes.
pub fn group_flush_makes_completed_records_durable<F: JournalFixture>(f: &mut F) {
    {
        let j = f.journal();
        let mut l0 = open_lane(j, 0);
        let mut l1 = open_lane(j, 1);
        for seq in 1..=6u64 {
            let lane: &mut dyn JournalLane = if seq % 2 == 1 { &mut *l0 } else { &mut *l1 };
            append_ok(lane, &write_rec(seq, seq * 4, 1), &pattern(seq, 1));
        }
        complete_appends(&mut *l0, 3);
        complete_appends(&mut *l1, 3);
        group_flush(j, 6);
    }
    f.crash_and_reopen();
    let r = f.journal().recover(None, WriteSeq(0)).expect("recover");
    assert_eq!(seqs(&r), vec![1, 2, 3, 4, 5, 6]);
    assert_eq!(r.next_seq, WriteSeq(7));
    assert_eq!(r.ignored_after_gap, None);
    for rec in &r.records {
        let mut buf = vec![0u8; BLOCK];
        f.journal().read_at(rec.loc, &mut buf).expect("read_at");
        assert_eq!(buf, pattern(rec.record.write_seq.0, 1));
    }
}

/// `recover` returns only records above the floor and never moves `next_seq`
/// below `floor + 1`.
pub fn recover_filters_by_floor<F: JournalFixture>(f: &mut F) {
    {
        let j = f.journal();
        let mut lane = open_lane(j, 0);
        for seq in 1..=6u64 {
            append_ok(&mut *lane, &write_rec(seq, seq * 4, 1), &pattern(seq, 1));
        }
        complete_appends(&mut *lane, 6);
        group_flush(j, 6);
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
    {
        let j = f.journal();
        let mut lane = open_lane(j, 0);
        for seq in 1..=4u64 {
            append_ok(&mut *lane, &write_rec(seq, seq * 4, 1), &pattern(seq, 1));
        }
        complete_appends(&mut *lane, 4);
        group_flush(j, 4);
    }
    f.crash_and_reopen();
    let a = f.journal().recover(None, WriteSeq(0)).expect("recover");
    let b = f.journal().recover(None, WriteSeq(0)).expect("recover");
    assert_eq!(a, b);
}

/// After a crash with an unflushed tail, recovery returns a contiguous prefix
/// that contains every flushed record, and every returned record reads back.
pub fn recovery_after_crash_is_a_contiguous_prefix<F: JournalFixture>(f: &mut F) {
    {
        let j = f.journal();
        let mut l0 = open_lane(j, 0);
        let mut l1 = open_lane(j, 1);
        for seq in 1..=4u64 {
            let lane: &mut dyn JournalLane = if seq % 2 == 1 { &mut *l0 } else { &mut *l1 };
            append_ok(lane, &write_rec(seq, seq * 4, 1), &pattern(seq, 1));
        }
        complete_appends(&mut *l0, 2);
        complete_appends(&mut *l1, 2);
        group_flush(j, 4);
        for seq in 5..=8u64 {
            let lane: &mut dyn JournalLane = if seq % 2 == 1 { &mut *l0 } else { &mut *l1 };
            append_ok(lane, &write_rec(seq, seq * 4, 1), &pattern(seq, 1));
        }
        // Whether these reach the file before the crash is the fixture's choice.
    }
    f.crash_and_reopen();
    let r = f.journal().recover(None, WriteSeq(0)).expect("recover");
    let got = seqs(&r);
    assert!(got.len() >= 4 && got.len() <= 8, "got {got:?}");
    assert_eq!(got, (1..=got.len() as u64).collect::<Vec<_>>());
    assert_eq!(r.next_seq, WriteSeq(got.len() as u64 + 1));
    for rec in &r.records {
        let mut buf = vec![0u8; BLOCK];
        f.journal().read_at(rec.loc, &mut buf).expect("read_at");
        assert_eq!(buf, pattern(rec.record.write_seq.0, 1));
    }
}

/// Reclaiming up to the tail after a cut never removes a record above the cut.
pub fn reclaim_keeps_records_above_the_tail<F: JournalFixture>(f: &mut F) {
    {
        let j = f.journal();
        let mut lane = open_lane(j, 0);
        for seq in 1..=6u64 {
            append_ok(&mut *lane, &write_rec(seq, seq * 4, 1), &pattern(seq, 1));
        }
        complete_appends(&mut *lane, 6);
        group_flush(j, 6);
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
