//! The GC hook of fsck (M17.B, `src/gc.rs`) against volumes of the M14 test
//! builder: the condemned list, the liveness comparison with `lunix-gc`, the
//! dry run and `--repair --gc`.

mod common;

use std::collections::BTreeSet;

use common::*;
use lunix_format::{CryptoMode, IdLen, ObjId, TimeMs};
use lunix_fsck::{code, Options, Report};
use lunix_gc::{CondemnedEntry, CondemnedList, ObjectRef};
use lunix_iface::store::GcStateStore;

/// Four commits of 40 new chunks each, one extent per segment: the chunks a
/// later commit discards leave whole segments dead (gc.md: the dead objects
/// of a pass).
fn churn_spec(mode: CryptoMode) -> Spec {
    let mut s = Spec::new(mode, 16 * 1024);
    s.chunks = 1024;
    s.blobs = 700;
    s.per_commit = 40;
    s.commits = 4;
    s.segment_target = 16 * 1024;
    s
}

fn gc_repair(b: &Built) -> Options {
    Options {
        repair: true,
        gc: true,
        ..b.opts()
    }
}

fn segments(b: &Built) -> BTreeSet<String> {
    list(&b.dir, "segments").into_iter().collect()
}

fn seg_id(name: &str) -> ObjId {
    let hex = name.trim_end_matches(".seg");
    ObjId::from_raw(u64::from_str_radix(hex, 16).expect("hex name")).expect("id")
}

fn store_list(b: &Built, objs: &[ObjectRef]) {
    let mut l = CondemnedList::default();
    for &o in objs {
        l.entries.insert(
            o,
            CondemnedEntry {
                condemned_at: TimeMs(CREATED_AT),
                bytes: 1,
            },
        );
    }
    let vol = b.open_store();
    vol.store_condemned(&l.to_file().expect("list"))
        .expect("store the list");
}

fn listed(b: &Built) -> Vec<ObjectRef> {
    let vol = b.open_store();
    vol.load_condemned()
        .expect("load")
        .as_ref()
        .and_then(CondemnedList::from_file)
        .map(|l| l.entries.keys().copied().collect())
        .unwrap_or_default()
}

fn codes(r: &Report, c: &str) -> Vec<String> {
    r.findings
        .iter()
        .filter(|f| f.code == c)
        .map(|f| format!("{}: {}", f.object, f.message))
        .collect()
}

fn assert_no_gc_errors(r: &Report, what: &str) {
    assert!(
        !r.has(code::GC_LIVENESS),
        "{what}: liveness differs:\n{}",
        r.render_text()
    );
    assert_eq!(r.exit_code(), 0, "{what}:\n{}", r.render_text());
    let failed: Vec<String> = codes(r, code::GC_DRY_RUN)
        .into_iter()
        .filter(|m| m.contains(": no "))
        .collect();
    assert!(
        failed.is_empty(),
        "{what}: GC's figures are missing: {failed:?}"
    );
}

#[test]
fn clean_volumes_agree_with_lunix_gc_on_every_segment() {
    for mode in ALL_MODES {
        for chunk in [16 * 1024, 64 * 1024] {
            let mut spec = Spec::new(mode, chunk);
            if chunk == 64 * 1024 {
                spec.segment_target = 4 << 20;
                spec.blobs = 400;
                spec.per_commit = 400;
            }
            let b = build(&spec);
            let what = format!("{} {chunk}", mode_name(mode));
            assert_no_gc_errors(&b.fsck(&b.opts()), &what);
        }
    }
}

#[test]
fn a_lean_volume_uses_hint_only_liveness_and_agrees() {
    let mut spec = churn_spec(CryptoMode::Integrity);
    spec.id_len = IdLen::Zero;
    let b = build(&spec);
    assert_no_gc_errors(&b.fsck(&b.opts()), "lean");
    let r = b.fsck(&gc_repair(&b));
    assert!(!r.has(code::REPAIR_REFUSED), "{}", r.render_text());
    assert_no_gc_errors(&b.fsck(&b.deep()), "lean after --repair --gc");
}

#[test]
fn repair_gc_deletes_the_dead_segments_and_leaves_a_clean_volume() {
    for mode in ALL_MODES {
        let b = build(&churn_spec(mode));
        let what = mode_name(mode);
        let before = b.fsck(&b.opts());
        assert_no_gc_errors(&before, what);
        let dry = codes(&before, code::GC_DRY_RUN);
        assert!(
            dry.iter().any(|m| m.contains("delete")),
            "{what}: the dry run names work: {dry:?}"
        );
        let ratios = before.ratios.expect("ratios");
        let segs = segments(&b);

        let r = b.fsck(&gc_repair(&b));
        assert!(
            r.repairs.iter().any(|a| a.action == "gc"),
            "{what}: the repair ran:\n{}",
            r.render_text()
        );
        assert!(!r.has(code::REPAIR_REFUSED), "{what}:\n{}", r.render_text());
        let after = segments(&b);
        assert!(
            after.len() < segs.len() && after.is_subset(&segs),
            "{what}: dead segments are gone ({} -> {})",
            segs.len(),
            after.len()
        );
        assert!(
            !b.open_store().evidence_passes().expect("gc/").is_empty(),
            "{what}: the pass wrote its evidence object"
        );
        assert!(listed(&b).is_empty(), "{what}: nothing is left condemned");

        // The volume is complete and nothing is left to do.
        let deep = b.fsck(&b.deep());
        assert_no_gc_errors(&deep, what);
        // Compaction is not part of the offline pass; nothing is left dead.
        assert!(
            codes(&deep, code::GC_DRY_RUN).iter().all(|m| m.contains(
                "condemn 0 segments and delete 0 segments, 0 packs, 0 index runs and 0 commits (0 bytes)"
            )),
            "{what}: a second pass would delete nothing: {:?}",
            codes(&deep, code::GC_DRY_RUN)
        );
        let again = deep.ratios.expect("ratios");
        assert_eq!(again.mapped_chunks, ratios.mapped_chunks, "{what}");
        assert_eq!(again.unique_chunks, ratios.unique_chunks, "{what}");
        assert_eq!(again.unique_clen, ratios.unique_clen, "{what}");
        assert_eq!(b.known.tip, deep.tip.expect("tip"), "{what}");

        // A second repair frees nothing.
        let r2 = b.fsck(&gc_repair(&b));
        assert!(
            codes(&r2, code::GC_DRY_RUN)
                .iter()
                .any(|m| m.contains("freed 0 bytes")),
            "{what}: {:?}",
            codes(&r2, code::GC_DRY_RUN)
        );
        assert_eq!(segments(&b), after, "{what}");
    }
}

/// The ids of the segments `--repair --gc` deletes from a churn volume (the
/// builder is deterministic for an integrity volume).
fn dead_segments() -> Vec<ObjId> {
    let b = build(&churn_spec(CryptoMode::Integrity));
    let segs = segments(&b);
    let r = b.fsck(&gc_repair(&b));
    assert!(!r.has(code::REPAIR_REFUSED), "{}", r.render_text());
    segs.difference(&segments(&b)).map(|n| seg_id(n)).collect()
}

#[test]
fn a_listed_dead_segment_is_gc_pending_not_an_orphan_and_the_repair_deletes_it() {
    let dead = dead_segments();
    assert!(!dead.is_empty());
    let b = build(&churn_spec(CryptoMode::Integrity));
    let objs: Vec<ObjectRef> = dead.iter().map(|&s| ObjectRef::Segment(s)).collect();
    store_list(&b, &objs);
    let r = b.fsck(&b.opts());
    assert_eq!(r.exit_code(), 0, "{}", r.render_text());
    let pending = codes(&r, code::GC_PENDING);
    assert_eq!(
        pending.len(),
        dead.len().min(lunix_fsck::PER_CODE_CAP),
        "{pending:?}"
    );
    assert!(
        pending
            .iter()
            .all(|m| m.contains("deletes it after its grace")),
        "{pending:?}"
    );
    assert!(!r.has(code::ORPHAN) && !r.has(code::CRASH_LEFTOVER));

    let r = b.fsck(&gc_repair(&b));
    assert!(!r.has(code::REPAIR_REFUSED), "{}", r.render_text());
    for s in &dead {
        assert!(!b.path(&b.seg_rel(*s)).exists(), "{s:?} deleted");
    }
    assert!(listed(&b).is_empty());
    assert_no_gc_errors(&b.fsck(&b.deep()), "after the repair");
}

#[test]
fn a_condemned_list_naming_a_live_segment_is_uncondemned_never_deleted() {
    let b = build(&churn_spec(CryptoMode::Integrity));
    let live = *b.known.segments.last().expect("a segment of the tip");
    store_list(&b, &[ObjectRef::Segment(live)]);
    let r = b.fsck(&b.opts());
    assert_eq!(r.exit_code(), 0, "{}", r.render_text());
    let pending = codes(&r, code::GC_PENDING);
    assert!(
        pending.len() == 1 && pending[0].contains("un-condemns"),
        "{pending:?}"
    );
    let r = b.fsck(&gc_repair(&b));
    assert!(!r.has(code::REPAIR_REFUSED), "{}", r.render_text());
    assert!(b.path(&b.seg_rel(live)).exists(), "the live segment stays");
    assert!(
        !listed(&b).contains(&ObjectRef::Segment(live)),
        "the recheck un-condemned it"
    );
    assert_no_gc_errors(&b.fsck(&b.deep()), "after the repair");
}

#[test]
fn a_listed_object_that_is_gone_is_fine() {
    let b = build(&churn_spec(CryptoMode::Plain));
    let gone = ObjId::from_raw(0xffff_fff0).expect("id");
    store_list(
        &b,
        &[
            ObjectRef::Segment(gone),
            ObjectRef::Pack(gone),
            ObjectRef::Run(gone),
        ],
    );
    let r = b.fsck(&b.opts());
    assert_eq!(r.exit_code(), 0, "{}", r.render_text());
    assert!(!r.has(code::GC_PENDING), "{}", r.render_text());
}

#[test]
fn a_corrupted_condemned_list_is_a_warning_and_ignored() {
    let b = build(&churn_spec(CryptoMode::Integrity));
    let vol = b.open_store();
    vol.store_condemned(&lunix_format::gcfile::Condemned {
        body_ver: 1,
        body: vec![9, 0, 0, 0, 1],
    })
    .expect("store");
    drop(vol);
    let r = b.fsck(&b.opts());
    assert_eq!(r.exit_code(), 1, "{}", r.render_text());
    assert!(r.has(code::CONDEMNED_INVALID));
    assert!(!r.has(code::GC_LIVENESS));
    // A flipped envelope is the same warning.
    b.flip("state/gc/condemned", 6);
    let r = b.fsck(&b.opts());
    assert!(r.has(code::CONDEMNED_INVALID), "{}", r.render_text());
    assert!(r.exit_code() <= 1, "{}", r.render_text());
    // The repair still runs.
    let r = b.fsck(&gc_repair(&b));
    assert!(!r.has(code::REPAIR_REFUSED), "{}", r.render_text());
    assert!(r.repairs.iter().any(|a| a.action == "gc"));
}

#[test]
fn a_deleted_needed_segment_is_an_error_and_the_repair_is_refused() {
    let b = build(&churn_spec(CryptoMode::Integrity));
    let needed = *b.known.segments.last().expect("a segment of the tip");
    std::fs::remove_file(b.path(&b.seg_rel(needed))).expect("rm");
    let before = segments(&b);
    let r = b.fsck(&b.opts());
    assert_eq!(r.exit_code(), 2, "{}", r.render_text());
    assert!(r.has(code::MAP_ENTRY_UNRESOLVED), "{}", r.render_text());
    let r = b.fsck(&gc_repair(&b));
    assert!(r.has(code::REPAIR_REFUSED), "{}", r.render_text());
    assert_eq!(segments(&b), before, "nothing deleted");
}

#[test]
fn a_deleted_needed_map_pack_is_an_error_and_the_repair_is_refused() {
    let b = build(&churn_spec(CryptoMode::Integrity));
    let pack = *b.known.packs.last().expect("the tip's pack");
    std::fs::remove_file(b.path(&b.pack_rel(pack))).expect("rm");
    let before = segments(&b);
    let r = b.fsck(&b.opts());
    assert_eq!(r.exit_code(), 2, "{}", r.render_text());
    assert!(r.has(code::MAP_NODE_MISSING), "{}", r.render_text());
    let r = b.fsck(&gc_repair(&b));
    assert!(r.has(code::REPAIR_REFUSED), "{}", r.render_text());
    assert_eq!(segments(&b), before, "nothing deleted");
}

#[test]
fn gc_without_repair_is_refused() {
    let b = build(&churn_spec(CryptoMode::Plain));
    let before = segments(&b);
    let r = b.fsck(&Options {
        gc: true,
        ..b.opts()
    });
    assert!(r.has(code::REPAIR_REFUSED), "{}", r.render_text());
    assert_eq!(segments(&b), before);
}
