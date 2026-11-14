//! The corruption matrix: a one-byte flip and a truncation in each file
//! class gives the documented exit code and code, in all three modes.

mod common;

use common::*;
use lunix_format::segment::{Trailer, TRAILER_LEN};
use lunix_format::CryptoMode;
use lunix_fsck::code;

fn trailer_of(b: &Built, rel: &str) -> Trailer {
    let bytes = b.read(rel);
    Trailer::decode(&bytes[bytes.len() - TRAILER_LEN..]).expect("trailer")
}

/// Runs fsck and requires exit 2 with at least one of `codes`.
fn expect_error(b: &Built, deep: bool, codes: &[&str], what: &str) {
    let r = b.fsck(&if deep { b.deep() } else { b.opts() });
    assert_eq!(
        r.exit_code(),
        2,
        "{what} [{}]: expected exit 2\n{}",
        mode_name(b.spec.mode),
        r.render_text()
    );
    assert!(
        codes.iter().any(|c| r.has(c)),
        "{what} [{}]: expected one of {codes:?}\n{}",
        mode_name(b.spec.mode),
        r.render_text()
    );
}

fn each_mode(mut f: impl FnMut(&Built)) {
    for mode in ALL_MODES {
        let mut spec = Spec::new(mode, 16 * 1024);
        spec.chunks = 4096;
        spec.blobs = 300;
        spec.per_commit = 300;
        let b = build(&spec);
        assert_eq!(b.fsck(&b.opts()).exit_code(), 0);
        f(&b);
    }
}

#[test]
fn volume_flip_and_truncation() {
    each_mode(|b| {
        let len = b.read("volume").len();
        let orig = b.read("volume");
        for at in [3usize, 20, 60, len / 2, len - 1] {
            b.flip("volume", at);
            expect_error(
                b,
                false,
                &[
                    code::VOLUME_PARSE,
                    code::VOLUME_MAC,
                    code::KEYRING_PARSE,
                    code::CREDENTIAL,
                ],
                &format!("volume flip at {at}"),
            );
            b.write("volume", &orig);
        }
        b.truncate("volume", len - 5);
        expect_error(b, false, &[code::VOLUME_PARSE], "volume truncated");
        b.write("volume", &orig);
        assert_eq!(b.fsck(&b.opts()).exit_code(), 0);
    });
}

#[test]
fn keyring_flip_and_truncation() {
    for mode in [CryptoMode::Encrypted, CryptoMode::Integrity] {
        let b = build(&Spec::new(mode, 16 * 1024));
        let kr = b.keyring_path.clone().expect("keyring");
        let orig = std::fs::read(&kr).expect("keyring");
        for at in [2usize, 30, orig.len() / 2, orig.len() - 1] {
            let mut x = orig.clone();
            x[at] ^= 0x5a;
            std::fs::write(&kr, x).expect("write");
            expect_error(
                &b,
                false,
                &[code::KEYRING_PARSE, code::CREDENTIAL],
                &format!("keyring flip at {at}"),
            );
        }
        std::fs::write(&kr, &orig[..orig.len() - 3]).expect("write");
        expect_error(&b, false, &[code::KEYRING_PARSE], "keyring truncated");
        std::fs::write(&kr, &orig).expect("write");
        assert_eq!(b.fsck(&b.opts()).exit_code(), 0);
        // The credential: a missing and a wrong key file.
        let r = b.fsck(&lunix_fsck::Options::default());
        if mode == CryptoMode::Encrypted {
            assert!(r.has(code::CREDENTIAL), "{}", r.render_text());
        }
        let wrong = b.tmp.path().join("wrong.key");
        std::fs::write(&wrong, [7u8; 32]).expect("write");
        let mut o = b.opts();
        o.credentials.key_file = Some(wrong);
        if mode == CryptoMode::Encrypted {
            assert!(b.fsck(&o).has(code::CREDENTIAL));
        }
    }
}

#[test]
fn commit_flip_and_truncation() {
    each_mode(|b| {
        let tip = b.commit_rel(b.known.tip);
        let orig = b.read(&tip);
        for at in [1usize, 30, orig.len() / 2, orig.len() - 1] {
            b.flip(&tip, at);
            expect_error(b, false, &[code::TIP_AUTH], &format!("tip flip at {at}"));
            b.write(&tip, &orig);
        }
        b.truncate(&tip, orig.len() - 1);
        expect_error(b, false, &[code::TIP_AUTH], "tip truncated");
        b.write(&tip, &orig);
        // A retained commit below the tip (the floor commit).
        let floor = b.commit_rel(b.known.tip - 1);
        let orig2 = b.read(&floor);
        b.flip(&floor, orig2.len() / 2);
        expect_error(b, false, &[code::COMMIT_AUTH], "retained commit flip");
        b.truncate(&floor, orig2.len() - 2);
        expect_error(b, false, &[code::COMMIT_AUTH], "retained commit truncated");
        b.write(&floor, &orig2);
        // Below the floor: a warning only.
        let below = b.commit_rel(b.known.tip - 2);
        let orig3 = b.read(&below);
        b.flip(&below, orig3.len() / 2);
        let r = b.fsck(&b.opts());
        assert_eq!(r.exit_code(), 1, "{}", r.render_text());
        assert!(r.has(code::COMMIT_BELOW_FLOOR));
        b.write(&below, &orig3);
        // A retained commit missing.
        std::fs::remove_file(b.path(&floor)).expect("remove");
        expect_error(b, false, &[code::COMMIT_MISSING], "retained commit missing");
        b.write(&floor, &orig2);
        assert_eq!(b.fsck(&b.opts()).exit_code(), 0);
    });
}

#[test]
fn segment_extent_flip_and_truncation() {
    each_mode(|b| {
        let seg = b.seg_rel(b.known.segments[0]);
        let orig = b.read(&seg);
        for at in [10usize, 100, 5000] {
            b.flip(&seg, at);
            expect_error(
                b,
                false,
                &[code::SEG_HASH],
                &format!("segment flip at {at}"),
            );
            expect_error(
                b,
                true,
                &[code::SEG_HASH, code::EXTENT_AUTH],
                "segment flip, deep",
            );
            b.write(&seg, &orig);
        }
        b.truncate(&seg, orig.len() / 2);
        expect_error(b, false, &[code::CONTAINER_TRAILER], "segment truncated");
        b.truncate(&seg, 10);
        expect_error(
            b,
            false,
            &[code::CONTAINER_TRAILER],
            "segment truncated to nothing",
        );
        b.write(&seg, &orig);
        assert_eq!(b.fsck(&b.deep()).exit_code(), 0);
    });
}

#[test]
fn footer_and_trailer_flip() {
    each_mode(|b| {
        let seg = b.seg_rel(b.known.segments[0]);
        let orig = b.read(&seg);
        let t = trailer_of(b, &seg);
        let foot = t.footer_off as usize;
        for at in [foot + 2, foot + 40, orig.len() - TRAILER_LEN - 1] {
            b.flip(&seg, at);
            expect_error(
                b,
                false,
                &[code::FOOTER_AUTH, code::FOOTER_CORRUPT],
                &format!("footer flip at {at}"),
            );
            b.write(&seg, &orig);
        }
        for at in [orig.len() - TRAILER_LEN, orig.len() - 30, orig.len() - 1] {
            b.flip(&seg, at);
            expect_error(
                b,
                false,
                &[code::CONTAINER_TRAILER],
                &format!("trailer flip at {at}"),
            );
            b.write(&seg, &orig);
        }
        assert_eq!(b.fsck(&b.opts()).exit_code(), 0);
    });
}

#[test]
fn map_pack_node_flip_and_truncation() {
    each_mode(|b| {
        let pack = b.pack_rel(b.known.packs[b.known.packs.len() - 1]);
        let orig = b.read(&pack);
        b.flip(&pack, 100);
        expect_error(
            b,
            false,
            &[code::SEG_HASH, code::MAP_NODE_AUTH],
            "pack node flip",
        );
        let r = b.fsck(&b.opts());
        assert!(r.has(code::SEG_HASH), "{}", r.render_text());
        b.write(&pack, &orig);
        b.truncate(&pack, orig.len() - 70);
        expect_error(
            b,
            false,
            &[code::CONTAINER_TRAILER, code::MAP_NODE_MISSING],
            "pack truncated",
        );
        b.write(&pack, &orig);
        assert_eq!(b.fsck(&b.opts()).exit_code(), 0);
    });
}

#[test]
fn a_missing_needed_segment_or_pack_is_an_error() {
    each_mode(|b| {
        let seg = b.seg_rel(b.known.segments[0]);
        let orig = b.read(&seg);
        std::fs::remove_file(b.path(&seg)).expect("remove");
        let r = b.fsck(&b.opts());
        assert_eq!(r.exit_code(), 2, "{}", r.render_text());
        assert!(r.has(code::MAP_ENTRY_UNRESOLVED), "{}", r.render_text());
        b.write(&seg, &orig);
        let pack = b.pack_rel(b.known.packs[b.known.packs.len() - 1]);
        let orig = b.read(&pack);
        std::fs::remove_file(b.path(&pack)).expect("remove");
        expect_error(b, false, &[code::MAP_NODE_MISSING], "needed pack removed");
        b.write(&pack, &orig);
    });
}

#[test]
fn index_run_flip_and_truncation() {
    each_mode(|b| {
        let run = format!("index/{:016x}", b.known.runs[0].raw());
        let orig = b.read(&run);
        for at in [30usize, orig.len() / 2, orig.len() - 1] {
            b.flip(&run, at);
            expect_error(b, false, &[code::INDEX_RUN], &format!("run flip at {at}"));
            b.write(&run, &orig);
        }
        b.truncate(&run, orig.len() - 10);
        expect_error(b, false, &[code::INDEX_RUN], "run truncated");
        b.write(&run, &orig);
        // The manifest.
        let man = b.read("index/manifest");
        b.flip("index/manifest", man.len() / 2);
        let r = b.fsck(&b.opts());
        assert!(r.exit_code() >= 1, "{}", r.render_text());
        b.write("index/manifest", &man);
        assert_eq!(b.fsck(&b.opts()).exit_code(), 0);
    });
}

#[test]
fn checkpoint_flip_gives_a_warning_at_most() {
    each_mode(|b| {
        let orig = b.read("state/checkpoint");
        for at in [4usize, 40, 100, 4094, 4096 + 10] {
            b.flip("state/checkpoint", at);
            let r = b.fsck(&b.opts());
            assert!(r.exit_code() <= 1, "flip at {at}\n{}", r.render_text());
            b.write("state/checkpoint", &orig);
        }
        b.truncate("state/checkpoint", 100);
        assert!(b.fsck(&b.opts()).exit_code() <= 1);
        b.write("state/checkpoint", &orig);
        // A zeroed checkpoint is silent.
        b.write("state/checkpoint", &vec![0u8; orig.len()]);
        assert_eq!(b.fsck(&b.opts()).exit_code(), 0);
    });
}

#[test]
fn pins_flip_in_every_byte_is_detected() {
    each_mode(|b| {
        let orig = b.read("state/pins");
        for at in 0..orig.len() {
            b.flip("state/pins", at);
            let r = b.fsck(&b.opts());
            assert!(
                r.has(code::STATE_PINS) || r.has(code::ATTACH) || r.exit_code() == 2,
                "flip at {at}\n{}",
                r.render_text()
            );
            b.write("state/pins", &orig);
        }
        b.truncate("state/pins", orig.len() - 1);
        assert!(b.fsck(&b.opts()).has(code::STATE_PINS));
        b.write("state/pins", &orig);
    });
}

#[test]
fn volume_json_and_device_record() {
    each_mode(|b| {
        let orig = b.read("volume.json");
        b.write("volume.json", b"{ not json");
        expect_error(b, false, &[code::VOLUME_JSON], "volume.json invalid");
        std::fs::remove_file(b.path("volume.json")).expect("remove");
        expect_error(b, false, &[code::VOLUME_JSON], "volume.json missing");
        b.write("volume.json", &orig);
        b.write("state/device", &[1u8; 96]);
        let r = b.fsck(&b.opts());
        assert!(
            r.has(code::STATE_DEVICE) && r.exit_code() == 1,
            "{}",
            r.render_text()
        );
        std::fs::remove_file(b.path("state/device")).expect("remove");
        assert_eq!(b.fsck(&b.opts()).exit_code(), 0);
    });
}

#[test]
fn leftovers_and_orphans_are_warnings() {
    each_mode(|b| {
        b.write("segments/00000000000fff01.open", b"x");
        b.write("map/00000000000fff02.tmp", b"y");
        let r = b.fsck(&b.opts());
        assert_eq!(r.exit_code(), 1, "{}", r.render_text());
        assert_eq!(r.counts.get(code::LEFTOVER), Some(&2));
        std::fs::remove_file(b.path("segments/00000000000fff01.open")).expect("rm");
        std::fs::remove_file(b.path("map/00000000000fff02.tmp")).expect("rm");
        // A copy of a segment under another id nothing names: an orphan.
        let seg = b.seg_rel(b.known.segments[0]);
        let orig = b.read(&seg);
        // The trailer names the id, so a copy under another name is an
        // error of its own; an orphan has to be a genuine container.
        b.write("segments/0000000000000fff.seg", &orig);
        let r = b.fsck(&b.opts());
        assert!(r.has(code::CONTAINER_TRAILER), "{}", r.render_text());
        std::fs::remove_file(b.path("segments/0000000000000fff.seg")).expect("rm");
        assert_eq!(b.fsck(&b.opts()).exit_code(), 0);
    });
}

#[test]
fn one_byte_truncations_of_containers_and_a_journal_file() {
    each_mode(|b| {
        for rel in [b.seg_rel(b.known.segments[0]), b.pack_rel(b.known.packs[0])] {
            let orig = b.read(&rel);
            b.truncate(&rel, orig.len() - 1);
            expect_error(
                b,
                false,
                &[
                    code::CONTAINER_TRAILER,
                    code::MAP_NODE_MISSING,
                    code::MAP_ENTRY_UNRESOLVED,
                ],
                &format!("{rel} short by one byte"),
            );
            b.write(&rel, &orig);
        }
        // Half of a ring file: the records past it are lost, a warning at
        // most (records beyond the first gap are ignored, N4).
        let j = "journal/q00-00000001.j";
        let orig = b.read(j);
        let recs = journal_records(&b.dir, 0);
        let mid = recs[recs.len() / 2].1;
        let mut cut = orig.clone();
        for x in &mut cut[mid..] {
            *x = 0;
        }
        b.write(j, &cut);
        let r = b.fsck(&b.opts());
        assert!(r.exit_code() <= 1, "{}", r.render_text());
        assert!(r.has(code::IGNORED_AFTER_GAP), "{}", r.render_text());
        b.write(j, &orig);
        assert_eq!(b.fsck(&b.opts()).exit_code(), 0);
    });
}

#[test]
fn a_volume_held_by_another_process_is_busy() {
    each_mode(|b| {
        let held = b.open_store();
        let r = b.fsck(&b.opts());
        assert_eq!(r.exit_code(), 2, "{}", r.render_text());
        assert!(r.has(code::BUSY), "{}", r.render_text());
        drop(held);
        assert_eq!(b.fsck(&b.opts()).exit_code(), 0);
    });
}

#[test]
fn the_report_lists_what_it_does_not_cover() {
    let b = build(&Spec::new(CryptoMode::Plain, 16 * 1024));
    let shallow = b.fsck(&b.opts());
    assert!(shallow.uncovered.iter().any(|u| u.contains("--deep")));
    assert!(shallow.uncovered.iter().any(|u| u.contains("journal")));
    let deep = b.fsck(&b.deep());
    assert!(!deep.uncovered.iter().any(|u| u.contains("--deep")));
    assert!(deep.render_text().contains("not covered by this run"));
}

#[test]
fn a_corrupt_condemned_list_is_a_warning_and_ignored() {
    each_mode(|b| {
        let orig = b.read("state/gc/condemned");
        b.flip("state/gc/condemned", 3);
        let r = b.fsck(&b.opts());
        assert_eq!(r.exit_code(), 1, "{}", r.render_text());
        assert!(r.has(code::CONDEMNED_INVALID));
        b.truncate("state/gc/condemned", 5);
        assert!(b.fsck(&b.opts()).has(code::CONDEMNED_INVALID));
        b.write("state/gc/condemned", &orig);
        assert_eq!(b.fsck(&b.opts()).exit_code(), 0);
    });
}
