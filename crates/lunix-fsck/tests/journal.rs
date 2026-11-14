//! Journal resync and classification (D17, N4) and the `created_at` warning.

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

#[test]
fn clean_volumes_of_all_three_modes_exit_0_and_count_the_replayable_records() {
    for mode in ALL_MODES {
        let b = build(&spec(mode));
        let r = b.fsck(&b.opts());
        assert_eq!(r.exit_code(), 0, "{}", r.render_text());
        // Records 301 ..= 330 lie above the tip's cut.
        assert_eq!(r.stats.journal_records, 30);
    }
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
fn the_same_flip_above_the_floor_is_an_error_with_its_byte_range() {
    for mode in ALL_MODES {
        let b = build(&spec(mode));
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
        // Recovery stops at 310: the valid records beyond are a warning.
        assert!(r.has(code::IGNORED_AFTER_GAP), "{}", r.render_text());
    }
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
fn records_beyond_the_first_gap_are_a_warning_with_their_range() {
    let b = build(&spec(CryptoMode::Integrity));
    std::fs::remove_file(b.path(&lane_file(1))).expect("remove");
    let r = b.fsck(&b.opts());
    assert_eq!(r.exit_code(), 1, "{}", r.render_text());
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
