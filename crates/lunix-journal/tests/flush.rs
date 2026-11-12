//! Group flush (M3 gate): no `fdatasync` before every lane's watermark reaches
//! `upto`, at most one group in flight, the group completes when all lanes'
//! syncs have, poisoning and FUA-style partial flushes.

mod common;

use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::*;
use lunix_format::journal::RecordKind;
use lunix_format::WriteSeq;
use lunix_iface::journal::{Journal, LaneId, SlotOutcome};
use lunix_journal::{FileJournal, JournalConfig, TraceEvent};

type Events = Arc<Mutex<Vec<TraceEvent>>>;

fn traced(lanes: u16) -> (tempfile::TempDir, JournalConfig, Events) {
    let dir = scratch();
    let mut cfg = small_cfg(dir.path().join("journal"), lanes, 2);
    let ev: Events = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&ev);
    cfg.trace = Some(Arc::new(move |e: &TraceEvent| {
        sink.lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(e.clone());
    }));
    (dir, cfg, ev)
}

fn events(ev: &Events) -> Vec<TraceEvent> {
    ev.lock().unwrap_or_else(|p| p.into_inner()).clone()
}

fn flush(j: &dyn Journal, upto: u64) -> mpsc::Receiver<bool> {
    let (tx, rx) = mpsc::channel();
    j.submit_group_flush(
        WriteSeq(upto),
        Box::new(move |r| {
            let _ = tx.send(r.is_ok());
        }),
    )
    .expect("submit");
    rx
}

fn syncs(ev: &[TraceEvent]) -> usize {
    ev.iter()
        .filter(|e| matches!(e, TraceEvent::Fdatasync { .. }))
        .count()
}

#[test]
fn no_fdatasync_before_every_lane_has_reached_upto() {
    for lanes in [1u16, 2, 4, 8] {
        let (_dir, cfg, ev) = traced(lanes);
        let j = open_journal(&cfg);
        recover_all(&j);
        let mut ls: Vec<_> = (0..lanes).map(|l| open_lane(&j, l)).collect();
        // One write per lane. Nothing polls, so no lane has reaped its write:
        // every lane is below the group's `upto`.
        for (i, l) in ls.iter_mut().enumerate() {
            write(&mut **l, i as u64 * 8, 1);
        }
        let rx = flush(&j, j.last_issued().0);
        // The lane that is polled last gates the group.
        for (i, l) in ls.iter_mut().enumerate() {
            assert!(
                rx.recv_timeout(Duration::from_millis(60)).is_err(),
                "lanes={lanes}: the flush completed before lane {i} was polled"
            );
            assert_eq!(
                syncs(&events(&ev)),
                0,
                "lanes={lanes}: fdatasync issued early"
            );
            complete_appends(&mut **l, 1);
        }
        assert!(rx.recv_timeout(T).expect("flush"), "flush failed");
        let e = events(&ev);
        assert_eq!(syncs(&e), usize::from(lanes), "one fdatasync per lane");
        let start = e
            .iter()
            .position(|x| matches!(x, TraceEvent::GroupStart { .. }))
            .expect("group start");
        let done = e
            .iter()
            .position(|x| matches!(x, TraceEvent::GroupDone { ok: true, .. }))
            .expect("group done");
        for (i, x) in e.iter().enumerate() {
            if let TraceEvent::Fdatasync {
                upto,
                lane_pending_min,
                ..
            } = x
            {
                assert!(i > start && i < done);
                assert!(lane_pending_min.is_none_or(|m| m > *upto));
            }
        }
    }
}

#[test]
fn a_flush_does_not_wait_for_higher_sequence_numbers() {
    let (_dir, cfg, ev) = traced(2);
    let j = open_journal(&cfg);
    recover_all(&j);
    let mut l0 = open_lane(&j, 0);
    let mut l1 = open_lane(&j, 1);
    let a = write(&mut *l0, 0, 1); // seq 1
    complete_appends(&mut *l0, 1);
    let pending = match l1.try_reserve(RecordKind::Write, 1).expect("reserve") {
        SlotOutcome::Reserved(s) => s, // seq 2, outstanding
        other => panic!("{other:?}"),
    };
    assert_eq!(pending.write_seq, WriteSeq(2));
    // A FUA-style flush of seq 1 completes while seq 2 is still a slot.
    let rx = flush(&j, a.write_seq.0);
    assert!(rx.recv_timeout(T).expect("flush"), "flush failed");
    assert_eq!(syncs(&events(&ev)), 1, "only the lane with data was synced");
    // The covering flush waits.
    let rx = flush(&j, 2);
    assert!(rx.recv_timeout(Duration::from_millis(80)).is_err());
    l1.append(pending, 8, &pattern(2, 1)).expect("append");
    complete_appends(&mut *l1, 1);
    assert!(rx.recv_timeout(T).expect("flush"));
}

#[test]
fn at_most_one_group_is_in_flight_and_every_flush_is_answered() {
    let (_dir, cfg, ev) = traced(4);
    let j = open_journal(&cfg);
    recover_all(&j);
    let mut ls: Vec<_> = (0..4u16).map(|l| open_lane(&j, l)).collect();
    let mut rxs = Vec::new();
    for round in 0..40u64 {
        let l = (round % 4) as usize;
        let w = write(&mut *ls[l], round, 1);
        complete_appends(&mut *ls[l], 1);
        rxs.push(flush(&j, w.write_seq.0));
        if round % 3 == 0 {
            rxs.push(flush(&j, j.last_issued().0));
        }
    }
    for rx in rxs {
        assert!(rx.recv_timeout(T).expect("flush"), "flush failed");
    }
    let mut open = false;
    let mut groups = 0;
    for x in events(&ev) {
        match x {
            TraceEvent::GroupStart { .. } => {
                assert!(!open, "a second group started before the first finished");
                open = true;
                groups += 1;
            }
            TraceEvent::GroupDone { .. } => {
                assert!(open);
                open = false;
            }
            TraceEvent::Fdatasync { .. } => assert!(open, "fdatasync outside a group"),
            _ => {}
        }
    }
    assert!(!open);
    assert!((1..=60).contains(&groups), "groups: {groups}");
}

#[test]
fn an_idle_flush_completes_without_syncing() {
    let (_dir, cfg, ev) = traced(2);
    let j = open_journal(&cfg);
    recover_all(&j);
    let rx = flush(&j, 0);
    assert!(rx.recv_timeout(T).expect("flush"));
    assert_eq!(syncs(&events(&ev)), 0);
}

#[test]
fn a_failed_append_fails_only_flushes_that_cover_it() {
    let (_dir, cfg, _ev) = traced(2);
    let j = open_journal(&cfg);
    recover_all(&j);
    let mut l0 = open_lane(&j, 0);
    let mut l1 = open_lane(&j, 1);
    write(&mut *l0, 0, 1); // seq 1
    complete_appends(&mut *l0, 1);
    j.fail_next_append(LaneId(1));
    let slot = match l1.try_reserve(RecordKind::Write, 1).expect("reserve") {
        SlotOutcome::Reserved(s) => s, // seq 2
        other => panic!("{other:?}"),
    };
    assert_eq!(slot.write_seq, WriteSeq(2));
    let e = l1.append(slot, 8, &pattern(2, 1)).expect_err("injected");
    assert_eq!(e.kind(), lunix_iface::ErrorKind::Io);
    // Below the failed number: unaffected. At or above: Io, now and later.
    assert!(flush(&j, 1).recv_timeout(T).expect("flush 1"));
    assert!(!flush(&j, 2).recv_timeout(T).expect("flush 2"));
    assert!(!flush(&j, 9).recv_timeout(T).expect("flush 9"));
    // Appends of sequence numbers handed out before the poisoning are also
    // refused afterwards and lower the poison mark.
    assert!(l0.try_reserve(RecordKind::Write, 1).is_err());
}

#[test]
fn a_slot_appended_after_the_journal_was_poisoned_lowers_the_mark() {
    // Seq 5 reserved on lane 0, seq 6 fails on lane 1: the flush for 5 must
    // not succeed once lane 0 is refused its append (a gap would sit below 6).
    let (_dir, cfg, _ev) = traced(2);
    let j = open_journal(&cfg);
    recover_all(&j);
    let mut l0 = open_lane(&j, 0);
    let mut l1 = open_lane(&j, 1);
    let early = match l0.try_reserve(RecordKind::Write, 1).expect("reserve") {
        SlotOutcome::Reserved(s) => s,
        other => panic!("{other:?}"),
    };
    j.fail_next_append(LaneId(1));
    let late = match l1.try_reserve(RecordKind::Write, 1).expect("reserve") {
        SlotOutcome::Reserved(s) => s,
        other => panic!("{other:?}"),
    };
    assert_eq!((early.write_seq.0, late.write_seq.0), (1, 2));
    l1.append(late, 0, &pattern(2, 1)).expect_err("injected");
    // A flush for seq 1 is pending behind the outstanding slot.
    let rx = flush(&j, 1);
    assert!(rx.recv_timeout(Duration::from_millis(80)).is_err());
    let e = l0.append(early, 4, &pattern(1, 1)).expect_err("poisoned");
    assert_eq!(e.kind(), lunix_iface::ErrorKind::Io);
    assert!(!rx.recv_timeout(T).expect("flush"), "must fail");
}

#[test]
fn dropping_the_journal_answers_queued_flushes() {
    let (_dir, cfg, _ev) = traced(1);
    let j = open_journal(&cfg);
    recover_all(&j);
    let mut l = open_lane(&j, 0);
    let slot = match l.try_reserve(RecordKind::Write, 1).expect("reserve") {
        SlotOutcome::Reserved(s) => s,
        other => panic!("{other:?}"),
    };
    let (tx, rx) = mpsc::channel();
    j.submit_group_flush(
        WriteSeq(1),
        Box::new(move |r| {
            let _ = tx.send(r.err().map(|e| e.kind()));
        }),
    )
    .expect("submit");
    // Stranded behind the slot; shutdown must still answer it.
    std::thread::sleep(Duration::from_millis(50));
    let worker: FileJournal = j;
    std::thread::spawn(move || drop(worker));
    assert_eq!(
        rx.recv_timeout(T).expect("answered"),
        Some(lunix_iface::ErrorKind::Closed)
    );
    l.append(slot, 0, &pattern(1, 1))
        .expect("append still works");
}
