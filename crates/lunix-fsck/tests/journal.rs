//! Journal resync and classification (D17, N4, ADR-4 with the mark ring) and
//! the `created_at` warning.

mod common;

use common::*;
use lunix_format::CryptoMode;
use lunix_fsck::code;

fn spec(mode: CryptoMode) -> Spec {
    let mut s = Spec::new(mode, 16 * 1024);
    s.chunks = 2048;
    s.blobs = 100;
    s.per_commit = 100;
    s
}

fn lane_file(lane: u16) -> String {
    format!("journal/q{lane:02x}-00000001.j")
}

/// Rewrites lane 0, lane 1 and the first mark ring file: `lane0`/`lane1`/
/// `marks` hold those records, every record witnessing `min(wm, seq - 1)`.
/// The tip's cut is 300.
fn rewrite(b: &Built, lane0: &[JRec], lane1: &[JRec], marks: &[JRec], wm: u64) {
    let u = b.desc.volume_uuid;
    write_ring_file(&b.dir, u, "q00-00000001.j", 0, lane0, wm);
    write_ring_file(&b.dir, u, "q01-00000001.j", 1, lane1, wm);
    write_ring_file(&b.dir, u, "m00-00000001.j", 0, marks, wm);
}

/// 1 ..= 330 split by parity, except the numbers in `marks`, which are marks.
fn lanes_and_marks(marks: &[u64]) -> (Vec<JRec>, Vec<JRec>, Vec<JRec>) {
    let (mut l0, mut l1) = (Vec::new(), Vec::new());
    for s in (1..=330u64).filter(|s| !marks.contains(s)) {
        if s % 2 == 0 {
            l0.push(JRec::Write(s));
        } else {
            l1.push(JRec::Write(s));
        }
    }
    (l0, l1, marks.iter().map(|m| JRec::Mark(*m)).collect())
}

#[test]
fn clean_volumes_of_all_three_modes_exit_0_and_count_the_replayable_records() {
    for mode in ALL_MODES {
        let b = build(&spec(mode));
        let r = b.fsck(&b.opts());
        assert_eq!(r.exit_code(), 0, "{}", r.render_text());
        // Records 301 ..= 330 lie above the tip's cut; the mark ring files
        // (empty) are ring files, not unknown names.
        assert_eq!(r.stats.journal_records, 30);
        assert_eq!(r.stats.journal_marks, 0);
        assert!(!r.has(code::JOURNAL_FILE), "{}", r.render_text());
    }
}

#[test]
fn marks_fill_their_numbers_and_a_healthy_volume_with_marks_exits_0() {
    for mode in ALL_MODES {
        let b = build(&spec(mode));
        let (l0, l1, m) = lanes_and_marks(&[150, 305, 317]);
        rewrite(&b, &l0, &l1, &m, 320);
        let r = b.fsck(&b.opts());
        assert_eq!(r.exit_code(), 0, "{}", r.render_text());
        assert!(!r.has(code::IGNORED_AFTER_GAP), "{}", r.render_text());
        assert!(!r.has(code::JOURNAL_FILE), "{}", r.render_text());
        // 301 ..= 330 above the cut: 28 records and 2 marks (150 is below).
        assert_eq!(r.stats.journal_records, 28);
        assert_eq!(r.stats.journal_marks, 2);
        assert_eq!(r.stats.journal_flushed, 320);
        assert!(r.render_text().contains("2 marks"), "{}", r.render_text());
    }
}

#[test]
fn without_the_mark_ring_a_marked_number_is_a_gap() {
    let b = build(&spec(CryptoMode::Integrity));
    let (l0, l1, m) = lanes_and_marks(&[305]);
    // Unwitnessed: the gap is crash residue (a note).
    rewrite(&b, &l0, &l1, &m, 0);
    std::fs::remove_file(b.path("journal/m00-00000001.j")).expect("remove");
    let r = b.fsck(&b.opts());
    assert_eq!(r.exit_code(), 0, "{}", r.render_text());
    let f = r
        .with_code(code::IGNORED_AFTER_GAP)
        .next()
        .expect("finding");
    assert!(f.message.contains("305"), "{}", f.message);
    assert_eq!(r.stats.journal_records, 4);
    // Witnessed (a record above it carries 320): durable loss, an error.
    rewrite(&b, &l0, &l1, &m, 320);
    std::fs::remove_file(b.path("journal/m00-00000001.j")).expect("remove");
    let r = b.fsck(&b.opts());
    assert_eq!(r.exit_code(), 2, "{}", r.render_text());
    assert!(r.has(code::JOURNAL_LOSS), "{}", r.render_text());
    assert!(!r.has(code::IGNORED_AFTER_GAP));
}

#[test]
fn a_record_of_the_wrong_kind_for_its_ring_is_damage() {
    let b = build(&spec(CryptoMode::Integrity));
    // A write in the mark ring, then a mark: the write is damage, and its
    // number 305 is missing.
    let (l0, l1, _) = lanes_and_marks(&[305, 317]);
    let marks = [JRec::Write(305), JRec::Mark(317)];
    rewrite(&b, &l0, &l1, &marks, 320);
    let r = b.fsck(&b.opts());
    assert_eq!(r.exit_code(), 2, "{}", r.render_text());
    let f = r.with_code(code::JOURNAL_DAMAGE).next().expect("finding");
    assert!(f.object.ends_with("m00-00000001.j"), "{}", f.object);
    assert!(r.has(code::JOURNAL_LOSS), "{}", r.render_text());
    // A mark in a lane file, followed by a write of the lane.
    let (mut l0, l1, m) = lanes_and_marks(&[]);
    let at = l0
        .iter()
        .position(|r| matches!(r, JRec::Write(310)))
        .expect("310");
    l0[at] = JRec::Mark(310);
    rewrite(&b, &l0, &l1, &m, 320);
    let r = b.fsck(&b.opts());
    assert_eq!(r.exit_code(), 2, "{}", r.render_text());
    let f = r.with_code(code::JOURNAL_DAMAGE).next().expect("finding");
    assert!(f.object.ends_with("q00-00000001.j"), "{}", f.object);
    assert!(f.message.contains("312"), "{}", f.message);
}

#[test]
fn a_number_held_twice_is_an_error_at_any_watermark() {
    let b = build(&spec(CryptoMode::Integrity));
    let (l0, l1, _) = lanes_and_marks(&[]);
    rewrite(&b, &l0, &l1, &[JRec::Mark(310)], 0);
    let r = b.fsck(&b.opts());
    assert_eq!(r.exit_code(), 2, "{}", r.render_text());
    let f = r.with_code(code::JOURNAL_LOSS).next().expect("finding");
    assert!(
        f.message.contains("310") && f.message.contains("two"),
        "{}",
        f.message
    );
}

#[test]
fn a_flip_in_a_record_at_or_below_the_floor_is_a_warning() {
    for mode in ALL_MODES {
        let b = build(&spec(mode));
        let (seq, off) = journal_records(&b.dir, 0)
            .into_iter()
            .find(|(s, _)| *s == 100)
            .expect("record 100");
        assert_eq!(seq, 100);
        b.flip(&lane_file(0), off + 80);
        let r = b.fsck(&b.opts());
        assert_eq!(r.exit_code(), 1, "{}", r.render_text());
        assert!(
            r.has(code::JOURNAL_DAMAGE_BELOW_FLOOR),
            "{}",
            r.render_text()
        );
        assert!(!r.has(code::JOURNAL_DAMAGE));
    }
}

#[test]
fn the_same_flip_above_the_floor_and_at_or_below_the_watermark_is_an_error_with_its_byte_range() {
    for mode in ALL_MODES {
        let b = build(&spec(mode));
        let (l0, l1, m) = lanes_and_marks(&[]);
        rewrite(&b, &l0, &l1, &m, 320);
        let (_, off) = journal_records(&b.dir, 0)
            .into_iter()
            .find(|(s, _)| *s == 310)
            .expect("record 310");
        b.flip(&lane_file(0), off + 80);
        let r = b.fsck(&b.opts());
        assert_eq!(r.exit_code(), 2, "{}", r.render_text());
        let f = r.with_code(code::JOURNAL_DAMAGE).next().expect("finding");
        let (start, end) = f.range.expect("byte range");
        assert_eq!(start, off as u64, "the range starts at the record");
        assert!(end > start);
        assert!(f.object.ends_with("q00-00000001.j"));
        // Recovery stops at 310, below the watermark: durable loss.
        assert!(r.has(code::JOURNAL_LOSS), "{}", r.render_text());
    }
}

#[test]
fn the_same_flip_above_the_watermark_is_crash_residue() {
    let b = build(&spec(CryptoMode::Integrity));
    // The fixture's records carry no watermark.
    let (_, off) = journal_records(&b.dir, 0)
        .into_iter()
        .find(|(s, _)| *s == 310)
        .expect("record 310");
    b.flip(&lane_file(0), off + 80);
    let r = b.fsck(&b.opts());
    assert_eq!(r.exit_code(), 0, "{}", r.render_text());
    assert!(r.has(code::JOURNAL_RESIDUE), "{}", r.render_text());
    assert!(r.has(code::IGNORED_AFTER_GAP), "{}", r.render_text());
    assert!(!r.has(code::JOURNAL_DAMAGE));
}

#[test]
fn a_torn_last_record_is_legal() {
    let b = build(&spec(CryptoMode::Integrity));
    let last = journal_records(&b.dir, 0).last().copied().expect("last");
    assert_eq!(last.0, 330);
    b.flip(&lane_file(0), last.1 + 80);
    let r = b.fsck(&b.opts());
    assert_eq!(r.exit_code(), 0, "{}", r.render_text());
    assert!(r.has(code::JOURNAL_TAIL));
}

#[test]
fn records_beyond_an_unwitnessed_gap_are_a_note_with_their_range() {
    let b = build(&spec(CryptoMode::Integrity));
    std::fs::remove_file(b.path(&lane_file(1))).expect("remove");
    let r = b.fsck(&b.opts());
    assert_eq!(r.exit_code(), 0, "{}", r.render_text());
    let f = r
        .with_code(code::IGNORED_AFTER_GAP)
        .next()
        .expect("finding");
    assert!(f.message.contains("301"), "{}", f.message);
}

#[test]
fn a_ring_file_of_another_volume_is_an_error_and_an_uninitialised_one_is_silent() {
    let b = build(&spec(CryptoMode::Plain));
    b.write("journal/q00-00000002.j", &vec![0u8; 1 << 20]);
    assert_eq!(b.fsck(&b.opts()).exit_code(), 0);
    let mut other = vec![];
    lunix_format::journal::FileHeader {
        lane: 0,
        ordinal: 3,
        volume_uuid: lunix_format::VolumeUuid([9; 16]),
        generation: 7,
    }
    .encode(&mut other)
    .expect("header");
    other.resize(1 << 20, 0);
    b.write("journal/q00-00000003.j", &other);
    let r = b.fsck(&b.opts());
    assert!(r.has(code::JOURNAL_UUID), "{}", r.render_text());
    assert_eq!(r.exit_code(), 2);
}

#[test]
fn a_decreasing_created_at_is_a_warning_and_never_an_error() {
    for mode in ALL_MODES {
        let mut s = spec(mode);
        s.clock_back = true;
        let b = build(&s);
        let r = b.fsck(&b.opts());
        assert_eq!(r.exit_code(), 1, "{}", r.render_text());
        assert!(r.has(code::CREATED_AT));
    }
}

#[test]
fn a_damaged_ring_file_header_is_at_least_a_warning() {
    for mode in ALL_MODES {
        let b = build(&spec(mode));
        // Lane 0's only file: its header goes, and every record in it.
        b.flip(&lane_file(0), 10);
        let r = b.fsck(&b.opts());
        assert!(r.exit_code() >= 1, "{}", r.render_text());
        assert!(r.has(code::JOURNAL_FILE), "{}", r.render_text());
    }
}

/// Rewrites the header of `rel` with `ordinal`, keeping lane and generation.
fn renumber(b: &Built, rel: &str, ordinal: u32) {
    let mut f = b.read(rel);
    let old = lunix_format::journal::FileHeader::decode(&f[..4096]).expect("header");
    let mut h = Vec::new();
    lunix_format::journal::FileHeader { ordinal, ..old }
        .encode(&mut h)
        .expect("encode");
    f[..h.len()].copy_from_slice(&h);
    b.write(rel, &f);
}

#[test]
fn an_interrupted_reinitialisation_is_silent_and_a_mismatched_header_is_not() {
    let b = build(&spec(CryptoMode::Integrity));
    let u = b.desc.volume_uuid;
    // Mark ring file 2 was being reinitialised as ordinal 5: header written,
    // body zero, rename not done.
    write_ring_file(&b.dir, u, "m00-00000002.j", 0, &[], 0);
    renumber(&b, "journal/m00-00000002.j", 5);
    let r = b.fsck(&b.opts());
    assert_eq!(r.exit_code(), 0, "{}", r.render_text());
    // The same header over a file that still holds a record is reported.
    let (l0, _, _) = lanes_and_marks(&[]);
    write_ring_file(&b.dir, u, "q00-00000001.j", 0, &l0, 0);
    renumber(&b, "journal/q00-00000001.j", 5);
    let r = b.fsck(&b.opts());
    assert!(r.has(code::JOURNAL_FILE), "{}", r.render_text());
}
