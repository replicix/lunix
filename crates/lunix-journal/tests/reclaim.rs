//! Reclaim, `Full` and space accounting (M3 gate).

mod common;

use common::*;
use lunix_format::journal::RecordKind;
use lunix_format::WriteSeq;
use lunix_iface::journal::{
    record_bytes, Journal, JournalCompletion, JournalLoc, LaneId, SlotOutcome, Token,
};

const BIG: u32 = 16;

#[test]
fn reclaim_never_deletes_a_file_holding_a_record_above_the_tail() {
    let dir = scratch();
    let cfg = small_cfg(dir.path().join("journal"), 1, 4);
    let j = open_journal(&cfg);
    recover_all(&j);
    let mut l = open_lane(&j, 0);
    let mut ws = Vec::new();
    for i in 0..100u64 {
        ws.push(write_rec(&mut *l, i, BIG));
        complete_appends(&mut *l, 1);
    }
    group_flush(&j);
    let ords: Vec<u32> = {
        let mut o: Vec<u32> = ws.iter().map(|w| w.ordinal).collect();
        o.dedup();
        o
    };
    assert!(ords.len() >= 3, "{ords:?}");
    let in_first: Vec<_> = ws.iter().filter(|w| w.ordinal == ords[0]).collect();
    let cut = in_first.last().expect("rec").seq + 5; // a few records into file 2
    let tail = j.tail_after(WriteSeq(cut)).expect("tail");
    assert_eq!(tail.lane_ordinals, vec![ords[1]]);

    // A tail that overshoots (a caller bug or a tail from a later cut) must not
    // take file 2 or the active file: files above every cut asked for stay.
    let wild = lunix_iface::journal::JournalTail {
        lane_ordinals: vec![9999],
    };
    let rep = j.reclaim(&wild).expect("reclaim");
    assert_eq!(
        rep.files, 1,
        "only the file wholly at or below the cut goes"
    );
    let first_bytes: u64 = in_first
        .iter()
        .map(|w| record_bytes(RecordKind::Write, w.blocks))
        .sum();
    assert_eq!(rep.bytes, first_bytes, "accounting: released = charged");

    // Records of file 2 and later still read back; file 1's are gone.
    for w in ws.iter().filter(|w| w.ordinal != ords[0]) {
        let loc = JournalLoc {
            lane: LaneId(0),
            ordinal: w.ordinal,
            offset: w.start + 64,
        };
        let mut buf = vec![0u8; w.blocks as usize * 4096];
        j.read_at(loc, &mut buf).expect("still readable");
        assert_eq!(buf, pattern(w.seq, w.blocks));
    }
    let gone = JournalLoc {
        lane: LaneId(0),
        ordinal: ords[0],
        offset: in_first[0].start + 64,
    };
    let e = j.read_at(gone, &mut [0u8; 4096]).expect_err("reclaimed");
    assert_eq!(e.kind(), lunix_iface::ErrorKind::NotFound);
    // Through a lane the same location completes with an error status.
    l.submit_read(gone, vec![0u8; 4096], Token(5))
        .expect("submit");
    let mut out = Vec::new();
    wait_until("read completion", || {
        l.poll(&mut out);
        !out.is_empty()
    });
    match &out[0] {
        JournalCompletion::Read { status, .. } => assert!(!status.is_ok()),
        other => panic!("{other:?}"),
    }
    // A second reclaim with the same tail does nothing more.
    assert_eq!(j.reclaim(&tail).expect("reclaim").files, 0);
    drop(l);
    drop(j);

    // Everything above the floor survives a restart.
    let j = open_journal(&cfg);
    let r = j.recover(None, WriteSeq(cut)).expect("recover");
    assert_eq!(seqs(&r), (cut + 1..=100).collect::<Vec<_>>());
}

#[test]
fn a_ring_without_a_free_file_is_full_until_reclaim() {
    let dir = scratch();
    let cfg = small_cfg(dir.path().join("journal"), 1, 2);
    let j = open_journal(&cfg);
    recover_all(&j);
    let mut l = open_lane(&j, 0);
    let mut charged = 0u64;
    let mut last = 0u64;
    loop {
        match l.try_reserve(RecordKind::Write, BIG).expect("reserve") {
            SlotOutcome::Reserved(s) => {
                last = s.write_seq.0;
                charged += s.record_bytes();
                l.append(s, 0, &pattern(last, BIG)).expect("append");
                complete_appends(&mut *l, 1);
            }
            SlotOutcome::Full => break,
            SlotOutcome::NoSpace => panic!("unexpected NoSpace"),
        }
        assert!(last < 200, "never full");
    }
    // Full consumed nothing.
    assert_eq!(j.last_issued(), WriteSeq(last));
    assert!(j.fill().ratio() > 0.9, "{:?}", j.fill());
    group_flush(&j);
    let tail = j.tail_after(WriteSeq(last)).expect("tail");
    let rep = j.reclaim(&tail).expect("reclaim");
    assert_eq!(rep.files, 1);
    // The released bytes are those of the records in the first file; the rest
    // (the records of the file still active) stay charged.
    assert!(rep.bytes > 0 && rep.bytes < charged);
    match l.try_reserve(RecordKind::Write, BIG).expect("reserve") {
        SlotOutcome::Reserved(s) => {
            l.append(s, 0, &pattern(last + 1, BIG)).expect("append");
            complete_appends(&mut *l, 1);
        }
        other => panic!("still not reservable after reclaim: {other:?}"),
    }
}

/// Sum of the encoded lengths of the records in `locs` whose data can still be
/// read: what the ledger should still be charged.
fn still_charged(j: &dyn Journal, locs: &[(JournalLoc, u64)]) -> u64 {
    locs.iter()
        .filter(|(loc, _)| j.read_at(*loc, &mut [0u8; 8]).is_ok())
        .map(|(_, bytes)| bytes)
        .sum()
}

#[test]
fn space_accounting_returns_to_zero_once_files_are_rolled_and_reclaimed() {
    let dir = scratch();
    let cfg = small_cfg(dir.path().join("journal"), 2, 3);
    let mut locs: Vec<(JournalLoc, u64)> = Vec::new();
    let mut charged = 0u64;
    let mut released = 0u64;
    let floor;
    {
        let j = open_journal(&cfg);
        recover_all(&j);
        let mut ls = [open_lane(&j, 0), open_lane(&j, 1)];
        let mut last = 0u64;
        for round in 0..8 {
            for i in 0..30usize {
                let l = i % 2;
                let w = write_rec(&mut *ls[l], i as u64, BIG);
                complete_appends(&mut *ls[l], 1);
                let b = record_bytes(RecordKind::Write, w.blocks);
                charged += b;
                last = w.seq;
                locs.push((
                    JournalLoc {
                        lane: LaneId(w.lane),
                        ordinal: w.ordinal,
                        offset: w.start + 64,
                    },
                    b,
                ));
            }
            group_flush(&j);
            let tail = j.tail_after(WriteSeq(last)).expect("tail");
            released += j.reclaim(&tail).expect("reclaim").bytes;
            assert_eq!(
                charged - released,
                still_charged(&j, &locs),
                "round {round}: the ledger and the files disagree"
            );
        }
        // The two active files still hold records: they stay charged until the
        // lanes roll past them.
        assert!(charged > released);
        floor = last - 7;
    }

    // After a restart the ledger is rebuilt from the recovered records (those
    // above the floor), and reclaim releases exactly those again.
    let j = open_journal(&cfg);
    let r = j.recover(None, WriteSeq(floor)).expect("recover");
    assert_eq!(seqs(&r), (floor + 1..floor + 8).collect::<Vec<_>>());
    let mut locs: Vec<(JournalLoc, u64)> = r
        .records
        .iter()
        .map(|x| (x.loc, record_bytes(x.record.kind, x.record.blocks)))
        .collect();
    let mut charged: u64 = locs.iter().map(|l| l.1).sum();
    let mut released = 0u64;
    let mut ls = [open_lane(&j, 0), open_lane(&j, 1)];
    let mut last = floor + 7;
    for _ in 0..8 {
        for i in 0..30usize {
            let l = i % 2;
            let w = write_rec(&mut *ls[l], i as u64, BIG);
            complete_appends(&mut *ls[l], 1);
            let b = record_bytes(RecordKind::Write, w.blocks);
            charged += b;
            last = w.seq;
            locs.push((
                JournalLoc {
                    lane: LaneId(w.lane),
                    ordinal: w.ordinal,
                    offset: w.start + 64,
                },
                b,
            ));
        }
        group_flush(&j);
        let tail = j.tail_after(WriteSeq(last)).expect("tail");
        released += j.reclaim(&tail).expect("reclaim").bytes;
        assert_eq!(charged - released, still_charged(&j, &locs));
    }
    // Everything recovered after the restart has been released.
    for (loc, _) in locs.iter().take(r.records.len()) {
        assert!(j.read_at(*loc, &mut [0u8; 8]).is_err());
    }
}

#[test]
fn an_exhausted_buffer_pool_is_full_and_consumes_no_sequence_number() {
    let dir = scratch();
    let mut cfg = small_cfg(dir.path().join("journal"), 1, 4);
    cfg.pool_buffers = 2;
    cfg.max_inflight = 2;
    let j = open_journal(&cfg);
    recover_all(&j);
    let mut l = open_lane(&j, 0);
    // Maximal records each need a batch of their own; nothing is polled, so no
    // buffer comes back.
    let mut n = 0u64;
    loop {
        match l.try_reserve(RecordKind::Write, 256).expect("reserve") {
            SlotOutcome::Reserved(s) => {
                n += 1;
                l.append(s, 0, &pattern(n, 256)).expect("append");
            }
            SlotOutcome::Full => break,
            SlotOutcome::NoSpace => panic!("no space"),
        }
        assert!(n < 10);
    }
    assert_eq!(n, 2, "two buffers, two maximal records");
    assert_eq!(j.last_issued(), WriteSeq(n));
    complete_appends(&mut *l, 2);
    match l.try_reserve(RecordKind::Write, 256).expect("reserve") {
        SlotOutcome::Reserved(s) => {
            assert_eq!(s.write_seq, WriteSeq(3));
            l.append(s, 0, &pattern(3, 256)).expect("append");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_lane_can_be_reopened_and_continues_after_its_last_record() {
    let dir = scratch();
    let cfg = small_cfg(dir.path().join("journal"), 1, 3);
    let j = open_journal(&cfg);
    recover_all(&j);
    {
        let mut l = open_lane(&j, 0);
        for i in 0..5u64 {
            write(&mut *l, i, 1);
        }
        complete_appends(&mut *l, 5);
    }
    {
        let mut l = open_lane(&j, 0);
        let a = write(&mut *l, 9, 1);
        assert_eq!(a.write_seq, WriteSeq(6));
        complete_appends(&mut *l, 1);
        let again = j.open_lane(LaneId(0), lunix_iface::wake::WakeFd::new().expect("fd"));
        assert!(again.is_err(), "a lane opens once at a time");
    }
    group_flush(&j);
    drop(j);
    let j = open_journal(&cfg);
    let r = recover_all(&j);
    assert_eq!(seqs(&r), (1..=6).collect::<Vec<_>>());
}

/// Writes to `lane` until the ring refuses (`Full`); returns the last `write_seq`.
fn fill_lane(lane: &mut dyn lunix_iface::journal::JournalLane) -> u64 {
    let mut last = 0;
    loop {
        match lane.try_reserve(RecordKind::Write, BIG).expect("reserve") {
            SlotOutcome::Reserved(s) => {
                last = s.write_seq.0;
                lane.append(s, 0, &pattern(last, BIG)).expect("append");
                complete_appends(lane, 1);
            }
            SlotOutcome::Full => return last,
            SlotOutcome::NoSpace => panic!("unexpected NoSpace"),
        }
        assert!(last < 400, "never full");
    }
}

#[test]
fn a_lane_is_woken_as_soon_as_one_of_its_files_is_free() {
    // ADR-33 T4: two lanes with a full ring each; one reclaim frees a file of
    // each. Parked inside the reclaim, after the first file was freed and
    // before the second lane's, the first lane has been signalled and takes a
    // reservation, and the other lane has not been signalled and is still full.
    use lunix_iface::wake::WakeFd;
    let dir = scratch();
    let cfg = small_cfg(dir.path().join("journal"), 2, 2);
    let j = open_journal(&cfg);
    recover_all(&j);
    let wakes = [
        WakeFd::new().expect("eventfd"),
        WakeFd::new().expect("eventfd"),
    ];
    let mut ls: Vec<_> = (0..2u16)
        .map(|i| {
            j.open_lane(LaneId(i), wakes[usize::from(i)].clone())
                .expect("open lane")
        })
        .collect();
    let a = fill_lane(&mut *ls[0]);
    let b = fill_lane(&mut *ls[1]);
    let last = a.max(b);
    group_flush(&j);
    for w in &wakes {
        w.drain();
    }
    let tail = j.tail_after(WriteSeq(last)).expect("tail");

    let (reached_tx, reached_rx) = std::sync::mpsc::channel();
    let (go_tx, go_rx) = std::sync::mpsc::channel::<()>();
    let go_rx = std::sync::Mutex::new(go_rx);
    j.set_reclaim_hook(Some(std::sync::Arc::new(move |lane, freed| {
        let _ = reached_tx.send((lane, freed));
        if freed == 1 {
            let _ = go_rx.lock().expect("gate").recv_timeout(T);
        }
    })));
    std::thread::scope(|s| {
        let h = s.spawn(|| j.reclaim(&tail).expect("reclaim"));
        let (first, freed) = reached_rx.recv_timeout(T).expect("a file was freed");
        assert_eq!(freed, 1, "parked after the first file");
        let (this, other) = (usize::from(first), usize::from(1 - first));
        assert!(
            wakes[this].drain() > 0,
            "the lane whose file is free is signalled while the reclaim goes on"
        );
        assert_eq!(
            wakes[other].drain(),
            0,
            "the other lane's file is not free yet"
        );
        match ls[other]
            .try_reserve(RecordKind::Write, BIG)
            .expect("reserve")
        {
            SlotOutcome::Full => {}
            o => panic!("the other lane is still full, got {o:?}"),
        }
        match ls[this]
            .try_reserve(RecordKind::Write, BIG)
            .expect("reserve")
        {
            SlotOutcome::Reserved(slot) => {
                let seq = slot.write_seq.0;
                ls[this]
                    .append(slot, 0, &pattern(seq, BIG))
                    .expect("append");
                complete_appends(&mut *ls[this], 1);
            }
            o => panic!("the woken lane takes a reservation, got {o:?}"),
        }
        go_tx.send(()).expect("release");
        let rep = h.join().expect("reclaim thread");
        assert_eq!(rep.files, 2);
        assert!(
            wakes[other].drain() > 0,
            "the other lane is signalled when its file is free"
        );
    });
}

#[test]
fn the_cut_that_frees_a_file_is_the_last_record_of_the_oldest_sealed_file() {
    // ADR-33 `cut_freeing_file`: none before any file is sealed; then the
    // highest `write_seq` of the fullest lane's oldest sealed file; one below
    // it frees nothing, it frees that file.
    let dir = scratch();
    let cfg = small_cfg(dir.path().join("journal"), 2, 4);
    let j = open_journal(&cfg);
    recover_all(&j);
    let mut l0 = open_lane(&j, 0);
    let mut l1 = open_lane(&j, 1);
    assert_eq!(j.cut_freeing_file(), None, "nothing sealed yet");
    let mut w0 = Vec::new();
    let mut w1 = Vec::new();
    // Lane 0 seals two files, lane 1 one.
    for i in 0..70u64 {
        w0.push(write_rec(&mut *l0, i, BIG));
        complete_appends(&mut *l0, 1);
        if i < 40 {
            w1.push(write_rec(&mut *l1, i, BIG));
            complete_appends(&mut *l1, 1);
        }
    }
    group_flush(&j);
    let ord0: Vec<u32> = {
        let mut o: Vec<u32> = w0.iter().map(|w| w.ordinal).collect();
        o.dedup();
        o
    };
    assert!(ord0.len() >= 3, "{ord0:?}");
    let first0 = w0
        .iter()
        .filter(|w| w.ordinal == ord0[0])
        .map(|w| w.seq)
        .max()
        .expect("records");
    // Lane 0 holds more files than lane 1: it is the fullest lane.
    let c = j.cut_freeing_file().expect("a sealed file");
    assert_eq!(c, WriteSeq(first0));
    let below = j.tail_after(WriteSeq(c.0 - 1)).expect("tail");
    assert_eq!(j.reclaim(&below).expect("reclaim").files, 0);
    let at = j.tail_after(c).expect("tail");
    assert!(j.reclaim(&at).expect("reclaim").files >= 1);
    // Reclaiming did not touch what the next answer needs.
    assert!(j.cut_freeing_file().is_some_and(|n| n > c));
}

#[test]
fn a_lane_that_fills_a_just_freed_file_keeps_its_counters() {
    // The review of ADR-33's early wake: reclaim put a freed slot on the free
    // list and reset its counters after, so a woken lane could fill the file
    // before the reset; the file then went `Used` with `max_seq` 0 and the
    // next reclaim freed it with records above the cut. One lane, three
    // files: all full, one cut, two files to free. Parked in the hook after
    // the first, the lane fills that file; the reclaim goes on; the lane moves
    // on to the second file, so the first is sealed; a reclaim asked for the
    // old cut must keep it.
    use lunix_iface::wake::WakeFd;
    let dir = scratch();
    let cfg = small_cfg(dir.path().join("journal"), 1, 3);
    let j = open_journal(&cfg);
    recover_all(&j);
    let mut l = j
        .open_lane(LaneId(0), WakeFd::new().expect("eventfd"))
        .expect("open lane");
    let old = fill_lane(&mut *l);
    group_flush(&j);
    let tail = j.tail_after(WriteSeq(old)).expect("tail");

    let (reached_tx, reached_rx) = std::sync::mpsc::channel();
    let (go_tx, go_rx) = std::sync::mpsc::channel::<()>();
    let go_rx = std::sync::Mutex::new(go_rx);
    // Parked right after the slot is on the free list, before its lane is
    // signalled: the lane can take the file from here.
    let once = std::sync::atomic::AtomicBool::new(true);
    j.set_free_hook(Some(std::sync::Arc::new(move |_lane, freed| {
        let _ = reached_tx.send(freed);
        if freed == 1 && once.swap(false, std::sync::atomic::Ordering::SeqCst) {
            let _ = go_rx.lock().expect("gate").recv_timeout(T);
        }
    })));
    let mut newest = Vec::new();
    std::thread::scope(|s| {
        let h = s.spawn(|| j.reclaim(&tail).expect("reclaim"));
        assert_eq!(reached_rx.recv_timeout(T).expect("freed"), 1);
        // The lane fills the file just freed, while the reclaim is parked.
        loop {
            match l.try_reserve(RecordKind::Write, BIG).expect("reserve") {
                SlotOutcome::Reserved(s) => {
                    let seq = s.write_seq.0;
                    l.append(s, 0, &pattern(seq, BIG)).expect("append");
                    complete_appends(&mut *l, 1);
                    newest.push(seq);
                }
                SlotOutcome::Full => break,
                SlotOutcome::NoSpace => panic!("unexpected NoSpace"),
            }
            assert!(newest.len() < 600, "never full");
        }
        go_tx.send(()).expect("release");
        assert_eq!(h.join().expect("reclaim thread").files, 2);
    });
    // The second file is free now: the lane moves on, the first is sealed.
    write_wait(&mut *l, 0, BIG);
    complete_appends(&mut *l, 1);
    assert!(!newest.is_empty());
    group_flush(&j);
    // Records above the old cut are in the sealed file: a cut at the old one
    // frees only the old active file (its records are at or below it), a cut
    // past the last record of the new file frees that too.
    let tail = j.tail_after(WriteSeq(old)).expect("tail");
    assert_eq!(
        j.reclaim(&tail).expect("reclaim").files,
        1,
        "the file the lane filled holds records above the cut"
    );
    let tail = j
        .tail_after(WriteSeq(*newest.last().expect("one")))
        .expect("tail");
    assert_eq!(j.reclaim(&tail).expect("reclaim").files, 1);
}

#[test]
fn a_reopened_lane_is_woken_per_file_after_its_predecessor_closed() {
    // The closing lane clears its wake fd before it counts as closed, so a
    // successor's wake is never erased by it; the predecessor's fd is not
    // signalled any more.
    use lunix_iface::wake::WakeFd;
    let dir = scratch();
    let cfg = small_cfg(dir.path().join("journal"), 1, 2);
    let j = open_journal(&cfg);
    recover_all(&j);
    let w1 = WakeFd::new().expect("eventfd");
    let mut l = j.open_lane(LaneId(0), w1.clone()).expect("open lane");
    let last = fill_lane(&mut *l);
    group_flush(&j);
    drop(l);
    let w2 = WakeFd::new().expect("eventfd");
    let _l = j.open_lane(LaneId(0), w2.clone()).expect("reopen");
    w1.drain();
    w2.drain();
    let tail = j.tail_after(WriteSeq(last)).expect("tail");
    assert_eq!(j.reclaim(&tail).expect("reclaim").files, 1);
    assert!(w2.drain() > 0, "the new lane is signalled");
    assert_eq!(w1.drain(), 0, "the closed lane's wake is not");
}
