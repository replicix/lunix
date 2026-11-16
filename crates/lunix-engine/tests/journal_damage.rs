//! `on_journal_damage = refuse` against the flushed watermark (ADR-4, maintainer
//! decision of 2026-11-14, rule change E1 signed off 2026-11-16): crash residue
//! above the watermark opens, loss at or below it (a missing or duplicate
//! number) is refused, a damaged range with no number missing opens, and
//! `wipe` opens either way.

mod common;

use common::*;
use lunix_engine::{EngineConfig, OnJournalDamage, OpenError};
use lunix_format::WriteSeq;
use lunix_iface::block::{BlockEngine, IoOp};
use lunix_testkit::{CrashModel, JournalMode};

const CS: u32 = 16 << 10;

fn refuse(cfg: &mut EngineConfig) {
    cfg.on_journal_damage = OnJournalDamage::Refuse;
}

fn wipe(cfg: &mut EngineConfig) {
    cfg.on_journal_damage = OnJournalDamage::Wipe;
}

/// Writes 4 KiB at block `blk` from queue `q` and returns its `write_seq`.
fn put(v: &Vol, r: &mut Running, q: usize, blk: u64) -> WriteSeq {
    v.hooks.take_seqs();
    assert!(r.write(q, blk * 4096, &pattern(blk, 4096), false).is_ok());
    let seqs = v.hooks.take_seqs();
    assert_eq!(seqs.len(), 1, "one record per write");
    seqs[0].2
}

/// Stops the engine and keeps every journal record (durable or not).
fn crash_keep_all(v: &mut Vol, r: Running) {
    let p = r.stop();
    v.store = p.store.crash(1);
    v.index = p.index.image();
    v.journal = p.journal.crash_with(1, CrashModel::keep_all());
}

#[test]
fn a_gap_across_lanes_above_the_watermark_opens_under_refuse() {
    let mut v = Vol::create(CS, 64, 2, JournalMode::Immediate);
    v.tweak = refuse;
    let mut r = v.open().map_err(|e| e.0).expect("open");
    put(&v, &mut r, 0, 0);
    let flushed = put(&v, &mut r, 1, 1);
    assert!(r.flush(0).is_ok());
    let lost = put(&v, &mut r, 0, 2);
    put(&v, &mut r, 1, 3);
    crash_keep_all(&mut v, r);
    // Lane 0's last record never reached the file; lane 1's later one did.
    assert!(v.journal.lose(lost));
    let r = v
        .open()
        .map_err(|e| e.0)
        .expect("residue opens under refuse");
    let rep = r.engine.recovery_report();
    assert!(rep.ignored_after_gap.is_some(), "{rep:?}");
    assert!(rep.flushed >= flushed && rep.flushed < lost, "{rep:?}");
}

#[test]
fn two_batches_in_flight_on_one_lane_open_under_refuse() {
    let mut v = Vol::create(CS, 64, 1, JournalMode::Immediate);
    v.tweak = refuse;
    let mut r = v.open().map_err(|e| e.0).expect("open");
    put(&v, &mut r, 0, 0);
    assert!(r.flush(0).is_ok());
    let torn = put(&v, &mut r, 0, 1);
    let landed = put(&v, &mut r, 0, 2);
    crash_keep_all(&mut v, r);
    // The first unflushed batch tore, the second landed.
    assert!(v.journal.tear(torn));
    let r = v
        .open()
        .map_err(|e| e.0)
        .expect("residue opens under refuse");
    let rep = r.engine.recovery_report();
    assert_eq!(rep.damaged.len(), 1, "{rep:?}");
    assert_eq!(rep.damaged[0].next_seq, Some(landed));
    assert!(rep.flushed < torn, "{rep:?}");
}

#[test]
fn damage_at_or_below_the_watermark_is_refused_and_wipe_opens() {
    let mut v = Vol::create(CS, 64, 2, JournalMode::Immediate);
    v.tweak = refuse;
    let mut r = v.open().map_err(|e| e.0).expect("open");
    put(&v, &mut r, 0, 0);
    let durable = put(&v, &mut r, 1, 1);
    assert!(r.flush(1).is_ok());
    // Appended after the flush: it witnesses `durable`.
    put(&v, &mut r, 0, 2);
    crash_keep_all(&mut v, r);
    let image = v.journal.clone();
    for damage in ["tear", "lose"] {
        v.journal = image.clone();
        if damage == "tear" {
            assert!(v.journal.tear(durable));
        } else {
            assert!(v.journal.lose(durable));
        }
        v.tweak = refuse;
        match v.open() {
            Err((OpenError::JournalDamage, _)) => {}
            Err((e, _)) => panic!("{damage}: {e}"),
            Ok(_) => panic!("{damage}: a durable record is gone and refuse opened"),
        }
        v.tweak = wipe;
        let r = v.open().map_err(|e| e.0).expect("wipe opens");
        let rep = r.engine.recovery_report();
        assert!(
            rep.ignored_after_gap.is_some() || !rep.damaged.is_empty(),
            "{damage}: {rep:?}"
        );
        assert!(rep.flushed >= durable, "{damage}: {rep:?}");
    }
}

/// Seeded crash images (review m4, 2026-11-14): a flushed history, then 8 to
/// 24 unflushed writes over both queues with out-of-order completions, then
/// 1 to 4 appends still in flight (held) at the crash. The crash model keeps,
/// drops or tears every record no completed flush covered. Every image must
/// open under `refuse`, and the run must actually produce residue.
#[test]
fn random_crash_residue_always_opens_under_refuse() {
    let mut residue = 0;
    let mut gaps = 0;
    for seed in 0..32u64 {
        let mut v = Vol::create(CS, 64, 2, JournalMode::Mixed(seed));
        v.tweak = refuse;
        v.marks = seed % 2 == 0;
        let mut r = v.open().map_err(|e| e.0).expect("open");
        let mut blk = 0u64;
        let mut next = || {
            let q = (blk % 2) as usize;
            let off = (blk % 200) * 4096;
            blk += 1;
            (q, off, pattern(seed * 1000 + blk, 4096))
        };
        for i in 0..12 {
            let (q, off, data) = next();
            assert!(r.write(q, off, &data, false).is_ok());
            if i % 5 == 4 {
                assert!(r.flush(q).is_ok());
            }
        }
        assert!(r.flush(0).is_ok());
        for _ in 0..8 + seed % 17 {
            let (q, off, data) = next();
            assert!(r.write(q, off, &data, false).is_ok());
        }
        // Appends in flight at the crash: submitted, never completed.
        r.journal.set_completion_mode(JournalMode::Manual);
        for _ in 0..1 + seed % 4 {
            let (q, off, data) = next();
            r.submit(q, IoOp::Write, false, off, 4096, &data);
        }
        let p = r.stop();
        v.store = p.store.crash(seed);
        v.index = p.index.image();
        v.journal = p.journal.crash_with(seed, CrashModel::default());
        let r = v
            .open()
            .map_err(|e| e.0)
            .unwrap_or_else(|e| panic!("seed {seed}: residue refused: {e}"));
        let rep = r.engine.recovery_report();
        if rep.ignored_after_gap.is_some() || !rep.damaged.is_empty() {
            residue += 1;
        }
        if rep.ignored_after_gap.is_some() {
            gaps += 1;
        }
        assert!(rep.flushed.0 >= 10, "seed {seed}: {rep:?}");
    }
    assert!(residue > 8, "too few crash images had residue: {residue}");
    assert!(gaps > 0, "no crash image had a gap");
}

/// Review M2 (2026-11-14), closed by the mark ring: ten writes, one FLUSH, no
/// later write, then medium damage to the third record. Without a mark no
/// surviving record witnesses the flush, so the damage reads as crash
/// residue and `refuse` opens (the window). With the mark the journal writes
/// after the flush, the watermark covers the ten records and `refuse` keeps
/// the volume offline.
#[test]
fn the_m2_probe_is_refused_once_the_mark_landed() {
    for marks in [false, true] {
        let mut v = Vol::create(CS, 64, 1, JournalMode::Immediate);
        v.marks = marks;
        v.tweak = refuse;
        let mut r = v.open().map_err(|e| e.0).expect("open");
        let seqs: Vec<WriteSeq> = (0..10).map(|b| put(&v, &mut r, 0, b)).collect();
        assert!(r.flush(0).is_ok());
        assert_eq!(r.journal.marks(), usize::from(marks), "one mark per flush");
        crash_keep_all(&mut v, r);
        assert!(v.journal.tear(seqs[2]));
        match (marks, v.open()) {
            (true, Err((OpenError::JournalDamage, _))) => {}
            (true, Err((e, _))) => panic!("with the mark: {e}"),
            (true, Ok(_)) => panic!("with the mark the damage is inside witnessed history"),
            (false, Ok(r)) => {
                let rep = r.engine.recovery_report();
                assert_eq!(rep.flushed, WriteSeq(0), "nothing witnessed the flush");
                assert_eq!(rep.replayed, 2, "the records after the damage are wiped");
            }
            (false, Err((e, _))) => panic!("without a mark the window reads as residue: {e}"),
        }
    }
}

/// A mark takes a `write_seq` of its own: the engine sees numbers it never
/// issued between its writes, and nothing of the mark reaches the overlay.
#[test]
fn marks_take_numbers_and_change_no_block() {
    let mut v = Vol::create(CS, 64, 2, JournalMode::Immediate);
    v.marks = true;
    v.tweak = refuse;
    let mut r = v.open().map_err(|e| e.0).expect("open");
    let a = put(&v, &mut r, 0, 0);
    assert!(r.flush(0).is_ok());
    let b = put(&v, &mut r, 1, 1);
    assert_eq!(b.0, a.0 + 2, "the mark took the number between");
    assert!(r.flush(1).is_ok());
    let before = r.read_all();
    crash_keep_all(&mut v, r);
    let mut r = v.open().map_err(|e| e.0).expect("reopen");
    let rep = r.engine.recovery_report().clone();
    assert_eq!(rep.replayed, 2, "two writes; the marks are no records");
    assert_eq!(rep.next_seq, WriteSeq(b.0 + 2), "the last mark is counted");
    assert!(rep.ignored_after_gap.is_none() && rep.damaged.is_empty());
    assert_eq!(r.read_all(), before);
}

/// The M12b review's E1 probe: one crash tears a record at or below the cut
/// (the commit does not sync the journal) while a later record of its lane
/// survives. Recovery keeps that later record, so the torn range stays in the
/// ring file; once the watermark passes it the range lies "at or below the
/// watermark" with no number missing. Every later `refuse` open must succeed,
/// after a clean close as after another crash.
#[test]
fn a_torn_record_at_or_below_the_cut_never_keeps_refuse_offline() {
    let mut v = Vol::create(CS, 64, 2, JournalMode::Immediate);
    v.marks = true;
    v.tweak = refuse;
    let mut r = v.open().map_err(|e| e.0).expect("open");
    let torn = put(&v, &mut r, 0, 0);
    let kept = put(&v, &mut r, 0, 1);
    r.engine.commit_now(false).expect("commit");
    assert!(r.engine.tip().1 >= kept);
    // A FLUSH after the commit and a record witnessing it: the watermark
    // passes `kept` (the real journal's first lane open after the crash does
    // the same with its mark, the review's one-crash form).
    put(&v, &mut r, 1, 5);
    assert!(r.flush(1).is_ok());
    put(&v, &mut r, 1, 6);
    crash_keep_all(&mut v, r);
    assert!(v.journal.tear(torn));
    let mut saw_the_e1_shape = false;
    for round in 0..4u64 {
        let mut r = v
            .open()
            .map_err(|e| e.0)
            .unwrap_or_else(|e| panic!("round {round}: refuse refused: {e}"));
        let rep = r.engine.recovery_report().clone();
        assert!(rep.gap_at.is_none() && rep.duplicate.is_none(), "{rep:?}");
        // The shape the old range rule refused: damage whose next record of
        // its lane is at or below the watermark, nothing missing.
        if rep
            .damaged
            .iter()
            .any(|d| d.next_seq.is_some_and(|n| n <= rep.flushed))
        {
            saw_the_e1_shape = true;
        }
        assert_eq!(r.read(0, 0, 4096).1, pattern(0, 4096), "round {round}");
        assert_eq!(r.read(0, 4096, 4096).1, pattern(1, 4096), "round {round}");
        // Writes on both lanes, a FLUSH (the watermark passes the torn
        // range), one more write to witness it.
        put(&v, &mut r, 1, 10 + round);
        put(&v, &mut r, 0, 20 + round);
        assert!(r.flush(0).is_ok());
        put(&v, &mut r, 1, 30 + round);
        if round % 2 == 0 {
            // A clean close.
            r.engine.quiesce().expect("quiesce");
        }
        crash_keep_all(&mut v, r);
    }
    assert!(saw_the_e1_shape, "the probe never produced the E1 shape");
}
