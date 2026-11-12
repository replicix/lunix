//! Recovery scenarios (M3 gate): ring wrap with a lost checkpoint, a zeroed
//! file, corruption above and at or below the floor, the checkpoint hint and
//! its guard, stale records beyond a gap, header forgeries, bad file headers.

mod common;

use common::*;
use lunix_format::journal::{Record, RecordHeader, RecordKind};
use lunix_format::local::CheckpointSlot;
use lunix_format::{CommitSeq, WriteSeq};
use lunix_iface::journal::{Journal, JournalLane};
use lunix_journal::{usable_hint, FileJournal, JournalConfig};

const BIG: u32 = 16; // 64 KiB writes

struct Rig {
    _dir: tempfile::TempDir,
    cfg: JournalConfig,
}

impl Rig {
    fn new(lanes: u16, files: u64) -> Self {
        let dir = scratch();
        let cfg = small_cfg(dir.path().join("journal"), lanes, files);
        Self { _dir: dir, cfg }
    }

    fn open(&self) -> FileJournal {
        open_journal(&self.cfg)
    }
}

/// Writes `n` big records round-robin over `lanes`, completing each, then
/// flushes. `tag` is added to the lba so incarnations can be told apart.
fn fill(j: &FileJournal, lanes: &mut [Box<dyn JournalLane>], n: usize, tag: u64) -> Vec<Written> {
    let mut v = Vec::new();
    for i in 0..n {
        let l = i % lanes.len();
        let w = write_rec(&mut *lanes[l], tag + v.len() as u64, BIG);
        complete_appends(&mut *lanes[l], 1);
        v.push(w);
    }
    group_flush(j);
    v
}

fn lanes_of(j: &FileJournal, n: u16) -> Vec<Box<dyn JournalLane>> {
    (0..n).map(|l| open_lane(j, l)).collect()
}

fn check_data(j: &dyn Journal, r: &lunix_iface::journal::Recovered) {
    for rec in &r.records {
        let mut buf = vec![0u8; rec.record.blocks as usize * 4096];
        j.read_at(rec.loc, &mut buf).expect("read_at");
        assert_eq!(buf, pattern(rec.record.write_seq.0, rec.record.blocks));
    }
}

#[test]
fn a_zeroed_file_ends_recovery_at_the_first_gap_and_reports_the_rest() {
    let rig = Rig::new(1, 4);
    let w = {
        let j = rig.open();
        recover_all(&j);
        let mut lanes = lanes_of(&j, 1);
        fill(&j, &mut lanes, 100, 0)
    };
    let ords: Vec<u32> = {
        let mut o: Vec<u32> = w.iter().map(|x| x.ordinal).collect();
        o.dedup();
        o
    };
    assert!(ords.len() >= 3, "workload spans {ords:?}");
    zero_bytes(&path_of(&rig.cfg, 0, ords[1]), 4096, SMALL_FILE - 4096);
    let first_of = |o: u32| w.iter().find(|x| x.ordinal == o).expect("file used").seq;
    let last_of_first = w.iter().filter(|x| x.ordinal == ords[0]).last().expect("rec").seq;
    {
        let j = rig.open();
        let r = recover_all(&j);
        assert_eq!(seqs(&r), (1..=last_of_first).collect::<Vec<_>>());
        assert_eq!(r.gap_at, Some(WriteSeq(first_of(ords[1]))));
        assert_eq!(r.next_seq, WriteSeq(last_of_first + 1));
        let ig = r.ignored_after_gap.expect("later valid records are reported");
        assert_eq!(ig.first, WriteSeq(first_of(ords[2])));
        assert_eq!(ig.last, WriteSeq(100));
        check_data(&j, &r);
        // The new incarnation reuses the sequence numbers; the stale records
        // beyond the gap must not come back.
        let mut lanes = lanes_of(&j, 1);
        fill(&j, &mut lanes, 5, 1000);
    }
    let j = rig.open();
    let r = recover_all(&j);
    assert_eq!(seqs(&r), (1..=last_of_first + 5).collect::<Vec<_>>());
    assert_eq!(r.ignored_after_gap, None);
    assert_eq!(r.records.last().expect("rec").record.lba, 1004);
    check_data(&j, &r);
}

#[test]
fn a_corrupted_record_above_the_floor_ends_recovery_there() {
    let rig = Rig::new(1, 4);
    let w = {
        let j = rig.open();
        recover_all(&j);
        let mut lanes = lanes_of(&j, 1);
        fill(&j, &mut lanes, 60, 0)
    };
    let bad = &w[39]; // seq 40
    flip_byte(&path_of(&rig.cfg, 0, bad.ordinal), bad.start + 64 + 100);
    let j = rig.open();
    let r = recover_all(&j);
    assert_eq!(seqs(&r), (1..=39).collect::<Vec<_>>());
    assert_eq!(r.gap_at, Some(WriteSeq(40)));
    assert_eq!(r.next_seq, WriteSeq(40));
    let ig = r.ignored_after_gap.expect("ignored");
    assert_eq!((ig.first, ig.last), (WriteSeq(41), WriteSeq(60)));
    let d = r
        .damaged
        .iter()
        .find(|d| d.start == bad.start)
        .expect("damaged range reported");
    assert_eq!((d.prev_seq, d.next_seq), (WriteSeq(39), Some(WriteSeq(41))));
    assert_eq!(d.end, w[40].start, "the range runs to the next valid record");
    check_data(&j, &r);
}

#[test]
fn a_corrupted_record_at_or_below_the_floor_changes_nothing() {
    let rig = Rig::new(1, 4);
    let w = {
        let j = rig.open();
        recover_all(&j);
        let mut lanes = lanes_of(&j, 1);
        fill(&j, &mut lanes, 60, 0)
    };
    let bad = &w[39]; // seq 40
    flip_byte(&path_of(&rig.cfg, 0, bad.ordinal), bad.start + 64 + 100);
    let j = rig.open();
    for floor in [40u64, 45, 59] {
        let r = j.recover(None, WriteSeq(floor)).expect("recover");
        assert_eq!(seqs(&r), (floor + 1..=60).collect::<Vec<_>>());
        assert_eq!((r.ignored_after_gap, r.gap_at), (None, None));
        assert_eq!(r.next_seq, WriteSeq(61));
        assert!(r.damaged.iter().any(|d| d.start == bad.start));
        check_data(&j, &r);
    }
    // One below the corrupted record, the same damage is a gap.
    let r = j.recover(None, WriteSeq(39)).expect("recover");
    assert!(r.records.is_empty());
    assert_eq!(r.gap_at, Some(WriteSeq(40)));
}

#[test]
fn the_floor_ignores_everything_at_or_below_it() {
    let rig = Rig::new(2, 3);
    {
        let j = rig.open();
        recover_all(&j);
        let mut lanes = lanes_of(&j, 2);
        fill(&j, &mut lanes, 40, 0);
    }
    let j = rig.open();
    for floor in [0u64, 1, 17, 39, 40] {
        let r = j.recover(None, WriteSeq(floor)).expect("recover");
        assert_eq!(seqs(&r), (floor + 1..=40).collect::<Vec<_>>());
        assert_eq!(r.next_seq, WriteSeq(41));
        check_data(&j, &r);
    }
    // A floor above everything on disk: nothing, and numbering follows the floor.
    let r = j.recover(None, WriteSeq(70)).expect("recover");
    assert!(r.records.is_empty());
    assert_eq!(r.next_seq, WriteSeq(71));
    assert_eq!(j.last_issued(), WriteSeq(70));
}

/// Writes `rounds` rounds of records and reclaims what the floor covers, so the
/// ring wraps several times. Returns every record written and the last floor.
fn wrap_ring(rig: &Rig, lanes: u16, rounds: usize, per_round: usize) -> (Vec<Written>, u64) {
    let j = rig.open();
    recover_all(&j);
    let mut ls = lanes_of(&j, lanes);
    let mut all: Vec<Written> = Vec::new();
    let mut floor = 0u64;
    for round in 0..rounds {
        all.extend(fill(&j, &mut ls, per_round, (round * 1000) as u64 + all.len() as u64));
        // Everything but the last 5 records counts as destaged and committed.
        floor = all.last().expect("rec").seq - 5;
        let tail = j.tail_after(WriteSeq(floor)).expect("tail");
        j.reclaim(&tail).expect("reclaim");
    }
    (all, floor)
}

#[test]
fn a_wrapped_ring_with_a_lost_checkpoint_recovers_the_same_prefix() {
    let rig = Rig::new(2, 3);
    let (all, floor) = wrap_ring(&rig, 2, 6, 40);
    let last = all.last().expect("rec").seq;
    assert!(
        all.iter().map(|w| w.ordinal).max().expect("ord") > 3,
        "the ring wrapped (ordinals rose past the file count)"
    );
    let want: Vec<u64> = (floor + 1..=last).collect();
    let j = rig.open();
    // No checkpoint.
    let r = j.recover(None, WriteSeq(floor)).expect("recover");
    assert_eq!(seqs(&r), want);
    assert_eq!(r.next_seq, WriteSeq(last + 1));
    assert_eq!((r.ignored_after_gap, r.gap_at), (None, None));
    check_data(&j, &r);
    // A stale checkpoint (tails at the first file) and a correct one (the
    // tail after the floor) give the same answer.
    let stale = hint(&[1, 1], 0);
    let r2 = j.recover(Some(&stale), WriteSeq(floor)).expect("recover");
    assert_eq!(r2, r);
    let tail = j.tail_after(WriteSeq(floor)).expect("tail");
    let good = hint(&tail.lane_ordinals, 5);
    let r3 = j.recover(Some(&good), WriteSeq(floor)).expect("recover");
    assert_eq!(seqs(&r3), want);
    assert_eq!(r3.next_seq, r.next_seq);
}

fn hint(tails: &[u32], commit_seq: u64) -> CheckpointSlot {
    CheckpointSlot {
        slot_seq: 1,
        volume_uuid: UUID,
        last_commit_seq: commit_seq,
        id_high: 0,
        lane_tail_ordinals: tails.to_vec(),
        stats: Vec::new(),
    }
}

#[test]
fn a_hint_that_skips_the_resume_file_falls_back_to_a_full_scan() {
    let rig = Rig::new(1, 4);
    {
        let j = rig.open();
        recover_all(&j);
        let mut ls = lanes_of(&j, 1);
        fill(&j, &mut ls, 10, 0); // all in the first file; later files are empty
    }
    let j = rig.open();
    let plain = j.recover(None, WriteSeq(0)).expect("recover");
    // Tails past the first file would skip the file holding the records.
    let r = j
        .recover(Some(&hint(&[3], 1)), WriteSeq(0))
        .expect("recover");
    assert_eq!(r, plain);
    assert_eq!(seqs(&r), (1..=10).collect::<Vec<_>>());
    // Wrong lane count: ignored.
    let r = j
        .recover(Some(&hint(&[3, 3], 1)), WriteSeq(0))
        .expect("recover");
    assert_eq!(r, plain);
}

#[test]
fn a_checkpoint_above_the_tip_is_ignored() {
    let h = hint(&[9], 50);
    assert!(usable_hint(Some(&h), CommitSeq(49)).is_none());
    assert!(usable_hint(Some(&h), CommitSeq(50)).is_some());
    assert!(usable_hint(Some(&h), CommitSeq(51)).is_some());
    assert!(usable_hint(None, CommitSeq(51)).is_none());
}

#[test]
fn valid_records_beyond_a_gap_do_not_come_back_in_the_next_incarnation() {
    let rig = Rig::new(2, 3);
    let w = {
        let j = rig.open();
        recover_all(&j);
        let mut ls = lanes_of(&j, 2);
        // seq 1-4 flushed, 5-8 not; lba says "old incarnation".
        let mut v = Vec::new();
        for i in 0..8usize {
            let l = i % 2;
            v.push(write_rec(&mut *ls[l], 100 + i as u64, 1));
            complete_appends(&mut *ls[l], 1);
            if i == 3 {
                group_flush(&j);
            }
        }
        v
    };
    // The write of seq 5 never reached the disk; 6, 7, 8 did.
    let lost = &w[4];
    zero_bytes(&path_of(&rig.cfg, lost.lane, lost.ordinal), lost.start, lost.len);
    {
        let j = rig.open();
        let r = recover_all(&j);
        assert_eq!(seqs(&r), vec![1, 2, 3, 4]);
        let ig = r.ignored_after_gap.expect("6-8 are valid beyond the gap");
        assert_eq!((ig.first.0, ig.last.0), (6, 8));
        let mut ls = lanes_of(&j, 2);
        // Seq 5, 6 again, on other lanes, with new lbas; flushed.
        let a = write_rec(&mut *ls[1], 900, 1);
        let b = write_rec(&mut *ls[0], 901, 1);
        assert_eq!((a.seq, b.seq), (5, 6));
        complete_appends(&mut *ls[1], 1);
        complete_appends(&mut *ls[0], 1);
        group_flush(&j);
    }
    let j = rig.open();
    let r = recover_all(&j);
    assert_eq!(seqs(&r), vec![1, 2, 3, 4, 5, 6]);
    assert_eq!(r.ignored_after_gap, None);
    let lbas: Vec<u64> = r.records.iter().map(|x| x.record.lba).collect();
    assert_eq!(lbas, vec![100, 101, 102, 103, 900, 901]);
}

#[test]
fn guest_data_that_imitates_a_record_is_not_accepted_by_resync() {
    for variant in 0..3 {
        let rig = Rig::new(1, 3);
        let (w, gen) = {
            let j = rig.open();
            recover_all(&j);
            let mut ls = lanes_of(&j, 1);
            let w = write_rec(&mut *ls[0], 0, 1);
            complete_appends(&mut *ls[0], 1);
            let f = write_rec(&mut *ls[0], 1, 1);
            complete_appends(&mut *ls[0], 1);
            group_flush(&j);
            let head = read_at_path(&path_of(&rig.cfg, 0, w.ordinal), 0, 4096);
            let gen = lunix_format::journal::FileHeader::decode(&head)
                .expect("header")
                .generation;
            assert_eq!(f.seq, 2);
            (w, gen)
        };
        // The payload of record 1 (as the guest could write it) is a perfect
        // record with a huge write_seq, built with the real codec.
        let at = w.start + 64 + 512;
        let (g, off) = match variant {
            0 => (0x1234_5678_9abc_def0, at), // wrong gen, right off
            1 => (gen, at + 8),               // right gen, wrong off
            _ => (gen, at),                   // the file's own gen and off: forged
        };
        let mut forged = Vec::new();
        Record {
            header: RecordHeader {
                kind: RecordKind::Write,
                write_seq: WriteSeq(2),
                epoch: 0,
                lba: 77,
                len: 1,
                gen: g,
                off,
            },
            data: &[0xAB; 4096],
        }
        .encode(&mut forged)
        .expect("encode");
        let path = path_of(&rig.cfg, 0, w.ordinal);
        write_at(&path, at, &forged);
        // Record 1's own header is damaged, so the reader resyncs through the
        // forged bytes: the payload now lies inside record 1's data.
        flip_byte(&path, w.start + 1);
        let j = rig.open();
        let r = recover_all(&j);
        let got = seqs(&r);
        if variant < 2 {
            assert!(got.is_empty(), "variant {variant}: {got:?}");
            assert_eq!(r.gap_at, Some(WriteSeq(1)));
            // Only the genuine second record is reported, not the forgery.
            let ig = r.ignored_after_gap.expect("record 2 is valid");
            assert_eq!((ig.first.0, ig.last.0), (2, 2));
        } else {
            // Documented limit: whoever knows the 64-bit generation can forge a
            // record; the guest never sees it. The forged record has seq 2 and
            // is rejected because the real record 2 follows it, so it is only a
            // gap at 1 in any case.
            assert!(got.is_empty(), "variant {variant}: {got:?}");
        }
    }
}

#[test]
fn a_relocated_record_is_rejected() {
    let rig = Rig::new(1, 3);
    let w = {
        let j = rig.open();
        recover_all(&j);
        let mut ls = lanes_of(&j, 1);
        let v: Vec<Written> = (0..4)
            .map(|i| {
                let w = write_rec(&mut *ls[0], i, 1);
                complete_appends(&mut *ls[0], 1);
                w
            })
            .collect();
        group_flush(&j);
        v
    };
    // Copy record 4 over record 2 (valid CRC, valid gen, wrong off).
    let path = path_of(&rig.cfg, 0, w[0].ordinal);
    let rec4 = read_at_path(&path, w[3].start, w[3].len as usize);
    write_at(&path, w[1].start, &rec4);
    let j = rig.open();
    let r = recover_all(&j);
    assert_eq!(seqs(&r), vec![1]);
    assert_eq!(r.gap_at, Some(WriteSeq(2)));
}

#[test]
fn a_file_with_a_bad_header_holds_no_records_and_is_reused() {
    let rig = Rig::new(1, 3);
    {
        let j = rig.open();
        recover_all(&j);
        let mut ls = lanes_of(&j, 1);
        fill(&j, &mut ls, 50, 0); // spans files 1 and 2 (31 records of 64 KiB each)
    }
    // Ruin the header CRC of file 3, which is still free.
    flip_byte(&path_of(&rig.cfg, 0, 3), 60);
    let before = ring_files(&rig.cfg);
    let j = rig.open();
    let r = recover_all(&j);
    assert_eq!(seqs(&r), (1..=50).collect::<Vec<_>>());
    assert!(
        r.damaged.iter().any(|d| d.ordinal == 3 && d.start == 0),
        "the bad header is reported"
    );
    // Opening a lane re-initialises it under a new ordinal.
    let mut ls = lanes_of(&j, 1);
    let more = fill(&j, &mut ls, 40, 5000);
    assert_eq!(more.first().expect("rec").seq, 51);
    drop(ls);
    drop(j);
    let after = ring_files(&rig.cfg);
    assert_eq!(before.len(), after.len());
    let j = rig.open();
    let r = recover_all(&j);
    assert_eq!(seqs(&r), (1..=90).collect::<Vec<_>>());
    assert_eq!(r.damaged.len(), 0, "{:?}", r.damaged);
}

#[test]
fn a_header_that_does_not_match_its_file_name_is_not_trusted() {
    let rig = Rig::new(1, 3);
    {
        let j = rig.open();
        recover_all(&j);
        let mut ls = lanes_of(&j, 1);
        fill(&j, &mut ls, 10, 0);
    }
    // File 2 renamed to a name that claims ordinal 9: the crash window of a
    // re-initialisation after the header but before the rename is the mirror
    // image of this.
    std::fs::rename(path_of(&rig.cfg, 0, 2), path_of(&rig.cfg, 0, 9)).expect("rename");
    let j = rig.open();
    let r = recover_all(&j);
    assert_eq!(seqs(&r), (1..=10).collect::<Vec<_>>());
    assert!(r.damaged.iter().any(|d| d.ordinal == 9));
}

#[test]
fn files_of_a_lane_beyond_the_configured_count_are_refused() {
    let rig = Rig::new(2, 2);
    drop(rig.open());
    let mut cfg = rig.cfg.clone();
    cfg.lanes = 1;
    cfg.journal_max = 2 * SMALL_FILE;
    let e = FileJournal::open(cfg).err().expect("unsupported");
    assert_eq!(e.kind(), lunix_iface::ErrorKind::Unsupported);
}

#[test]
fn a_torn_final_batch_loses_only_its_unacknowledged_tail() {
    let rig = Rig::new(1, 3);
    let w = {
        let j = rig.open();
        recover_all(&j);
        let mut ls = lanes_of(&j, 1);
        // Several records, no completion waits in between, so they share batches.
        let mut v = Vec::new();
        for i in 0..12u64 {
            v.push(write_rec(&mut *ls[0], i, 1));
        }
        complete_appends(&mut *ls[0], 12);
        group_flush(&j);
        v
    };
    // Tear record 10 in half and zero everything after it in the file, as a
    // device losing the end of the last write would.
    let r10 = &w[9];
    let path = path_of(&rig.cfg, 0, r10.ordinal);
    zero_bytes(&path, r10.start + 2000, 8192);
    let j = rig.open();
    let r = recover_all(&j);
    assert_eq!(seqs(&r), (1..=9).collect::<Vec<_>>());
    assert_eq!(r.next_seq, WriteSeq(10));
    check_data(&j, &r);
}
