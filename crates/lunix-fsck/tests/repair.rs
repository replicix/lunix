//! `--repair` and its explicit companions.

mod common;

use common::*;
use lunix_format::local::RollbackFloor;
use lunix_format::{CommitSeq, CryptoMode, ExtentKind, Level, ObjId};
use lunix_fsck::{code, Options};
use lunix_iface::crypto::{CommitCodec, EncodingDesc};
use lunix_iface::store::{CheckpointStore, CommitStore, SegmentStore, VolumeDir};

fn spec(mode: CryptoMode) -> Spec {
    let mut s = Spec::new(mode, 16 * 1024);
    s.chunks = 2048;
    s.blobs = 100;
    s.per_commit = 100;
    s
}

fn repair(b: &Built) -> Options {
    Options {
        repair: true,
        ..b.opts()
    }
}

fn has(rel: &str, b: &Built) -> bool {
    b.path(rel).exists()
}

/// A genuine, sealed segment that no commit names.
fn orphan_segment(b: &Built) -> ObjId {
    let store = b.open_store();
    let data = lcg(777, b.spec.chunk_size as usize);
    let id = b.crypto.extents().chunk_id(ExtentKind::Data, &data);
    let mut ext = Vec::new();
    b.crypto
        .extents()
        .seal(
            ExtentKind::Data,
            &id,
            &EncodingDesc {
                codec: lunix_format::Codec::Raw,
                level: Level(0),
                ulen: b.spec.chunk_size,
                dict_id: None,
            },
            &data,
            &mut ext,
        )
        .expect("seal");
    let mut w = store.open_segment().expect("open");
    w.append_extent(&id, None, &ext).expect("append");
    let seg = w.id();
    w.seal(None).expect("seal");
    seg
}

#[test]
fn repair_removes_leftovers_and_is_idempotent() {
    for mode in ALL_MODES {
        let b = build(&spec(mode));
        b.write("segments/0000000000000f01.open", b"half a segment");
        b.write("map/0000000000000f02.tmp", b"x");
        b.write("index/0000000000000f03.tmp", b"y");
        let r = b.fsck(&b.opts());
        assert_eq!(r.exit_code(), 1, "{}", r.render_text());
        assert_eq!(r.counts.get(code::LEFTOVER), Some(&3));
        let fixed = b.fsck(&repair(&b));
        assert_eq!(fixed.exit_code(), 0, "{}", fixed.render_text());
        assert!(fixed.repairs.iter().any(|a| a.action == "remove_leftovers"));
        assert!(!has("segments/0000000000000f01.open", &b));
        let again = b.fsck(&repair(&b));
        assert_eq!(again.exit_code(), 0);
        assert!(again.repairs.is_empty(), "nothing left to repair");
    }
}

#[test]
fn orphans_are_a_warning_and_go_only_with_delete_orphans() {
    for mode in ALL_MODES {
        let b = build(&spec(mode));
        let seg = orphan_segment(&b);
        let rel = b.seg_rel(seg);
        let r = b.fsck(&b.opts());
        assert_eq!(r.exit_code(), 1, "{}", r.render_text());
        assert!(
            r.has(code::ORPHAN),
            "closed cleanly: orphan\n{}",
            r.render_text()
        );
        // --repair alone keeps it.
        b.fsck(&repair(&b));
        assert!(has(&rel, &b));
        // After an unclean stop the same file is a crash leftover.
        b.write("segments/0000000000000f01.open", b"x");
        let r = b.fsck(&b.opts());
        assert!(r.has(code::CRASH_LEFTOVER), "{}", r.render_text());
        assert!(!r.has(code::ORPHAN));
        let fixed = b.fsck(&Options {
            delete_orphans: true,
            ..repair(&b)
        });
        assert!(
            fixed.repairs.iter().any(|a| a.action == "delete_orphans"),
            "{}",
            fixed.render_text()
        );
        assert!(!has(&rel, &b));
        assert_eq!(fixed.exit_code(), 0, "{}", fixed.render_text());
        // The highest id on disk was recorded before the delete: ids are
        // never reused.
        let store = b.open_store();
        let slot = store.load().expect("load").expect("checkpoint");
        assert!(slot.id_high >= seg.raw());
    }
}

#[test]
fn delete_orphans_refuses_when_the_map_cannot_be_walked() {
    let b = build(&spec(CryptoMode::Integrity));
    let seg = orphan_segment(&b);
    let pack = b.pack_rel(*b.known.packs.last().expect("pack"));
    b.flip(&pack, 100);
    let r = b.fsck(&Options {
        delete_orphans: true,
        ..repair(&b)
    });
    assert!(r.has(code::REPAIR_REFUSED), "{}", r.render_text());
    assert!(has(&b.seg_rel(seg), &b), "nothing was deleted");
}

#[test]
fn a_damaged_checkpoint_is_rewritten_with_no_tails() {
    for mode in ALL_MODES {
        let b = build(&spec(mode));
        b.flip("state/checkpoint", 40);
        let r = b.fsck(&b.opts());
        assert_eq!(r.exit_code(), 1, "{}", r.render_text());
        let fixed = b.fsck(&repair(&b));
        assert!(
            fixed
                .repairs
                .iter()
                .any(|a| a.action == "rewrite_checkpoint"),
            "{}",
            fixed.render_text()
        );
        assert_eq!(fixed.exit_code(), 0, "{}", fixed.render_text());
        let store = b.open_store();
        let slot = store.load().expect("load").expect("slot");
        assert_eq!(slot.last_commit_seq, b.known.tip);
        assert!(slot.lane_tail_ordinals.iter().all(|o| *o == 0));
        assert!(
            slot.id_high
                >= b.known
                    .packs
                    .iter()
                    .chain(&b.known.segments)
                    .map(|i| i.raw())
                    .max()
                    .unwrap_or(0)
        );
    }
}

// ---------------------------------------------------------------------------
// --truncate-tip

#[test]
fn truncate_tip_makes_the_previous_commit_the_tip() {
    for mode in ALL_MODES {
        let b = build(&spec(mode));
        let tip = b.commit_rel(b.known.tip);
        let good = b.read(&tip);
        b.flip(&tip, good.len() / 2);
        let damaged = b.read(&tip);
        assert!(b.fsck(&b.opts()).has(code::TIP_AUTH));
        // The checkpoint describes the old tip.
        let store = b.open_store();
        assert_eq!(
            store.load().expect("load").expect("slot").last_commit_seq,
            b.known.tip
        );
        drop(store);

        let r = b.fsck(&Options {
            truncate_tip: true,
            ..repair(&b)
        });
        assert!(
            r.repairs.iter().any(|a| a.action == "truncate_tip"),
            "{}",
            r.render_text()
        );
        assert!(r.repairs.iter().any(|a| a.action == "lower_floor"));
        assert_eq!(r.tip, Some(b.known.tip - 1));
        // The damaged object is kept aside, never deleted.
        let aside = format!("{tip}.damaged");
        assert!(has(&aside, &b));
        assert_eq!(b.read(&aside), damaged);
        assert!(!has(&tip, &b));
        // Both checkpoint slots are zero.
        assert!(b.read("state/checkpoint").iter().all(|x| *x == 0));
        // Store level: the volume authenticates at the new tip, and the
        // floor is that tip.
        let store = b.open_store();
        let seq = store.tip().expect("tip").expect("some");
        assert_eq!(seq, CommitSeq(b.known.tip - 1));
        let bytes = store.get(seq).expect("get");
        let c = b
            .crypto
            .meta()
            .open_commit(seq, &bytes)
            .expect("authenticates");
        assert_eq!(c.common().commit_seq, seq);
        let floor: RollbackFloor = store.rollback_floor().expect("floor").expect("some");
        assert_eq!(floor.max_commit_seq, seq);
        assert_eq!(floor.tip_hash, b.crypto.meta().object_hash(&bytes));
        drop(store);
        // Every journal record above the new cut is replayed: 201 ..= 330.
        let after = b.fsck(&b.opts());
        assert_eq!(after.stats.journal_records, 130, "{}", after.render_text());
        assert!(!after.has(code::ROLLBACK));
        assert!(!after.has(code::TIP_AUTH));
    }
}

#[test]
fn truncate_tip_reports_the_journal_records_that_are_gone() {
    let b = build(&spec(CryptoMode::Integrity));
    // C9 reclaimed 201 ..= 250: the ring holds 251 ..= 330 only.
    let seqs: Vec<u64> = (251..=330).collect();
    write_journal_seqs(&b.dir, b.desc.volume_uuid, b.spec.lanes, &seqs);
    let tip = b.commit_rel(b.known.tip);
    b.flip(&tip, 50);
    let r = b.fsck(&Options {
        truncate_tip: true,
        ..repair(&b)
    });
    let a = r
        .repairs
        .iter()
        .find(|a| a.action == "truncate_tip")
        .expect("repair");
    assert!(a.detail.contains("201..=250"), "{}", a.detail);
    assert!(a.detail.contains("lost"));
}

#[test]
fn truncate_tip_refuses_without_a_previous_commit_that_authenticates() {
    let b = build(&spec(CryptoMode::Integrity));
    let tip = b.commit_rel(b.known.tip);
    let prev = b.commit_rel(b.known.tip - 1);
    b.flip(&tip, 50);
    b.flip(&prev, 50);
    let before = (
        b.read(&tip),
        b.read("state/checkpoint"),
        b.read("state/pins"),
    );
    let r = b.fsck(&Options {
        truncate_tip: true,
        ..repair(&b)
    });
    assert!(r.has(code::REPAIR_REFUSED), "{}", r.render_text());
    assert_eq!(
        (
            b.read(&tip),
            b.read("state/checkpoint"),
            b.read("state/pins")
        ),
        before
    );
    assert!(!has(&format!("{tip}.damaged"), &b));
    // An absent previous commit.
    std::fs::remove_file(b.path(&prev)).expect("remove");
    let r = b.fsck(&Options {
        truncate_tip: true,
        ..repair(&b)
    });
    assert!(r.has(code::REPAIR_REFUSED));
    assert!(has(&tip, &b));
}

#[test]
fn truncate_tip_needs_the_repair_flag() {
    let b = build(&spec(CryptoMode::Plain));
    b.flip(&b.commit_rel(b.known.tip), 50);
    let r = b.fsck(&Options {
        truncate_tip: true,
        ..b.opts()
    });
    assert!(r.has(code::REPAIR_REFUSED));
    assert!(has(&b.commit_rel(b.known.tip), &b));
}

// ---------------------------------------------------------------------------
// --reset-floor

#[test]
fn reset_floor_sets_the_floor_to_the_verified_tip_and_is_never_implied() {
    for mode in ALL_MODES {
        let b = build(&spec(mode));
        // A floor above the tip: a lagging copy (a DR steal, phase 5).
        let pins_before = b.read("state/pins");
        let f = lunix_format::local::LocalPinsFile::decode(&pins_before).expect("pins");
        let mut hi = f.clone();
        hi.floor = Some(RollbackFloor {
            max_commit_seq: CommitSeq(b.known.tip + 4),
            tip_hash: [3; 32],
        });
        let mut bytes = Vec::new();
        hi.encode(&mut bytes).expect("encode");
        b.write("state/pins", &bytes);
        assert!(b.fsck(&b.opts()).has(code::ROLLBACK));
        // --repair alone changes nothing about the floor.
        let r = b.fsck(&repair(&b));
        assert!(r.has(code::ROLLBACK), "{}", r.render_text());
        assert_eq!(b.read("state/pins"), bytes);
        // With it the floor is the verified tip, and old and new are printed.
        let r = b.fsck(&Options {
            reset_floor: true,
            ..repair(&b)
        });
        let a = r
            .repairs
            .iter()
            .find(|a| a.action == "reset_floor")
            .expect("repair");
        assert!(
            a.detail.contains(&format!("commit {}", b.known.tip + 4)),
            "{}",
            a.detail
        );
        assert!(
            a.detail.contains(&format!("now commit {}", b.known.tip)),
            "{}",
            a.detail
        );
        assert_eq!(r.exit_code(), 0, "{}", r.render_text());
        let store = b.open_store();
        let floor = store.rollback_floor().expect("floor").expect("some");
        assert_eq!(floor.max_commit_seq, CommitSeq(b.known.tip));
        assert_eq!(floor.tip_hash, b.known.tip_hash);
    }
}

#[test]
fn reset_floor_raises_a_lower_floor() {
    let b = build(&spec(CryptoMode::Integrity));
    let mut f = lunix_format::local::LocalPinsFile::decode(&b.read("state/pins")).expect("pins");
    f.floor = None;
    let mut bytes = Vec::new();
    f.encode(&mut bytes).expect("encode");
    b.write("state/pins", &bytes);
    let r = b.fsck(&Options {
        repair: true,
        reset_floor: true,
        ..b.opts()
    });
    assert!(
        r.repairs.iter().any(|a| a.action == "reset_floor"),
        "{}",
        r.render_text()
    );
    assert!(!r.has(code::REPAIR_REFUSED));
    let store = b.open_store();
    assert_eq!(
        store
            .rollback_floor()
            .expect("f")
            .expect("some")
            .max_commit_seq,
        CommitSeq(b.known.tip)
    );
}

#[test]
fn reset_floor_refuses_on_an_unverified_tip() {
    let b = build(&spec(CryptoMode::Integrity));
    b.flip(&b.commit_rel(b.known.tip), 50);
    let r = b.fsck(&Options {
        reset_floor: true,
        ..repair(&b)
    });
    assert!(r.has(code::REPAIR_REFUSED), "{}", r.render_text());
}

#[test]
fn review_truncate_tip_with_reset_floor_leaves_a_mountable_floor() {
    let b = build(&spec(CryptoMode::Integrity));
    // The floor lags one commit behind the tip.
    let prev = b.known.tip - 1;
    let prev_hash = b.crypto.meta().object_hash(&b.read(&b.commit_rel(prev)));
    set_floor(
        &b,
        Some(RollbackFloor {
            max_commit_seq: CommitSeq(prev),
            tip_hash: prev_hash,
        }),
    );
    b.fsck(&Options {
        repair: true,
        truncate_tip: true,
        reset_floor: true,
        ..b.opts()
    });
    let again = b.fsck(&b.opts());
    assert!(!again.has(code::ROLLBACK), "{}", again.render_text());
    assert_eq!(again.tip, Some(prev));
}

#[test]
fn delete_orphans_is_refused_while_pins_exist_or_cannot_be_read() {
    use lunix_format::pins::{Consistency, SnapshotPin};
    use lunix_format::{paths, ObjectKind, TimeMs};
    use lunix_iface::crypto::MetaCodec;
    let b = build(&spec(CryptoMode::Integrity));
    let seg = orphan_segment(&b);
    // Garbage in snapshots/: roots() fails.
    b.write("snapshots/junk", b"not a pin");
    let r = b.fsck(&Options {
        delete_orphans: true,
        ..repair(&b)
    });
    assert!(
        r.has(code::REPAIR_REFUSED) && r.has(code::PIN_OBJECT),
        "{}",
        r.render_text()
    );
    assert!(has(&b.seg_rel(seg), &b));
    std::fs::remove_file(b.path("snapshots/junk")).expect("rm");
    // A valid pin on an old commit: roots() is not empty.
    let mut body = Vec::new();
    SnapshotPin {
        consistency: Consistency::Crash,
        commit_seq: CommitSeq(1),
        created: TimeMs(1),
        label: "keep".into(),
    }
    .encode(&mut body)
    .expect("pin");
    let name = paths::snapshot("keep").expect("name");
    let sealed = b
        .crypto
        .meta()
        .seal(ObjectKind::Snapshots, &name, &body)
        .expect("seal");
    b.write("snapshots/keep", &sealed);
    let r = b.fsck(&Options {
        delete_orphans: true,
        ..repair(&b)
    });
    assert!(r.has(code::REPAIR_REFUSED), "{}", r.render_text());
    assert!(has(&b.seg_rel(seg), &b));
}

#[test]
fn truncate_tip_refuses_when_the_previous_commit_is_no_longer_a_root() {
    // retain_last = 1: the previous commit is below the tip's chain_floor.
    let mut s = spec(CryptoMode::Integrity);
    s.retain_last = 1;
    let b = build(&s);
    let tip = b.commit_rel(b.known.tip);
    let r = b.fsck(&Options {
        truncate_tip: true,
        ..repair(&b)
    });
    let f = r.with_code(code::REPAIR_REFUSED).next().expect("refused");
    assert!(f.message.contains("chain_floor"), "{}", f.message);
    assert!(has(&tip, &b));
}

#[test]
fn truncate_tip_with_retain_last_1_and_a_damaged_tip_goes_ahead_when_the_previous_root_resolves() {
    // M14 re-review R-2: the walk of the previous root decides, not retain_last.
    let mut s = spec(CryptoMode::Integrity);
    s.retain_last = 1;
    let b = build(&s);
    let tip = b.commit_rel(b.known.tip);
    b.flip(&tip, 50);
    let r = b.fsck(&Options {
        truncate_tip: true,
        ..repair(&b)
    });
    let a = r
        .repairs
        .iter()
        .find(|a| a.action == "truncate_tip")
        .unwrap_or_else(|| panic!("{}", r.render_text()));
    assert!(a.detail.contains("retain_last is below 2"), "{}", a.detail);
    assert!(has(&format!("{tip}.damaged"), &b));
    assert_eq!(r.tip, Some(b.known.tip - 1));
}

#[test]
fn truncate_tip_walks_the_previous_root_before_changing_anything() {
    let b = build(&spec(CryptoMode::Integrity));
    let tip = b.commit_rel(b.known.tip);
    b.flip(&tip, 50);
    // A segment the previous commit's map needs is gone.
    std::fs::remove_file(b.path(&b.seg_rel(b.known.segments[0]))).expect("rm");
    let before = b.read("state/checkpoint");
    let r = b.fsck(&Options {
        truncate_tip: true,
        ..repair(&b)
    });
    let f = r.with_code(code::REPAIR_REFUSED).next().expect("refused");
    assert!(f.message.contains("does not resolve"), "{}", f.message);
    assert!(has(&tip, &b));
    assert!(!has(&format!("{tip}.damaged"), &b));
    assert_eq!(b.read("state/checkpoint"), before);
}

#[test]
fn repair_pins_replaces_a_rejected_pins_file_from_the_verified_tip() {
    for mode in ALL_MODES {
        let b = build(&spec(mode));
        b.flip("state/pins", 20);
        let r = b.fsck(&b.opts());
        assert!(
            r.has(code::STATE_PINS) && r.exit_code() == 2,
            "{}",
            r.render_text()
        );
        // Without the option nothing replaces it; with it, but without
        // --repair, it is refused.
        let r = b.fsck(&Options {
            repair_pins: true,
            ..b.opts()
        });
        assert!(r.has(code::REPAIR_REFUSED));
        let r = b.fsck(&Options {
            repair_pins: true,
            ..repair(&b)
        });
        assert!(
            r.repairs.iter().any(|a| a.action == "repair_pins"),
            "{}",
            r.render_text()
        );
        assert_eq!(r.exit_code(), 0, "{}", r.render_text());
        let store = b.open_store();
        let floor = store.rollback_floor().expect("floor").expect("some");
        assert_eq!(floor.max_commit_seq, CommitSeq(b.known.tip));
        let log = std::fs::read_to_string(b.path("state/floor-resets.log")).expect("audit");
        assert!(log.contains("repair_pins"), "{log}");
        // The keyring that unlocked the volume stays the rollback mark (P-4).
        let pins = lunix_format::local::LocalPinsFile::decode(&b.read("state/pins")).expect("pins");
        let want = match &b.keyring_path {
            Some(p) => {
                lunix_format::keyring::Keyring::decode(&std::fs::read(p).expect("keyring"))
                    .expect("keyring")
                    .keyring_seq
            }
            None => 0,
        };
        assert_eq!(pins.pins.max_keyring_seq, want, "mode {}", mode_name(mode));
        // A readable file is not repaired.
        drop(store);
        let r = b.fsck(&Options {
            repair_pins: true,
            ..repair(&b)
        });
        assert!(r.has(code::REPAIR_REFUSED));
    }
}

#[test]
fn the_floor_repairs_leave_an_audit_line_each() {
    let b = build(&spec(CryptoMode::Integrity));
    b.flip(&b.commit_rel(b.known.tip), 50);
    b.fsck(&Options {
        truncate_tip: true,
        ..repair(&b)
    });
    let log = std::fs::read_to_string(b.path("state/floor-resets.log")).expect("audit");
    assert_eq!(log.lines().count(), 1, "{log}");
    assert!(log.contains("truncate_tip"));
}

#[test]
fn commits_kept_only_for_the_rollback_floor_are_a_note() {
    // M14 re-review R-3: the window between the rollback floor and GC's
    // retained set (here commit 0; the set reaches down to commit 1) is
    // reported as information, and the volume stays clean.
    let b = build(&spec(CryptoMode::Integrity));
    assert!(b.known.tip >= 3);
    let hash = b.crypto.meta().object_hash(&b.read(&b.commit_rel(0)));
    set_floor(
        &b,
        Some(RollbackFloor {
            max_commit_seq: CommitSeq(0),
            tip_hash: hash,
        }),
    );
    let r = b.fsck(&b.opts());
    let f = r
        .with_code(code::FLOOR_WINDOW)
        .next()
        .unwrap_or_else(|| panic!("{}", r.render_text()));
    assert!(f.message.contains("0..=0"), "{}", f.message);
    assert_eq!(r.exit_code(), 0, "{}", r.render_text());
}
