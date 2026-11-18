//! The GC hook of fsck (M17.B, `src/gc.rs`) against volumes of the M14 test
//! builder: the condemned list, the liveness comparison with `lunix-gc`, the
//! dry run and `--repair --gc`.

mod common;

use std::collections::BTreeSet;
use std::sync::Arc;

use common::*;
use lunix_format::commit::{Commit, CommitKind, CommonHeader, Full, MapRoot};
use lunix_format::local::RollbackFloor;
use lunix_format::node::{Child, DataEntry, Entry, Interior, Leaf, LeafParams, NodeLoc};
use lunix_format::{
    ChunkId, Codec, CommitSeq, CryptoMode, Epoch, ExtentKind, Hint, IdLen, Level, ObjId, TimeMs,
};
use lunix_fsck::{code, Options, Report};
use lunix_gc::{CondemnedEntry, CondemnedList, ObjectRef};
use lunix_iface::crypto::{CommitCodec, EncodingDesc, OpenExpect, OpenOutput};
use lunix_iface::index::ChunkIndex;
use lunix_iface::map::{BlockMap, NodeSource};
use lunix_iface::store::{
    CommitStore, GcStateStore, PackStore, SegmentFooters, SegmentStore, VolumeDir,
};
use lunix_index::LsmIndex;
use lunix_map::{MapConfig, RadixMap};

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
    assert!(
        !r.has(code::GC_UNCHECKED),
        "{what}: GC's figures are missing:\n{}",
        r.render_text()
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
            codes(&r2, code::GC_REPAIRED)
                .iter()
                .any(|m| m.contains("freed 0 bytes")),
            "{what}: {:?}",
            codes(&r2, code::GC_REPAIRED)
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
    // Live through the tip's map only: no commit of GC's retained set names
    // it, so only liveness (not "named by a retained commit") keeps it.
    let live = live_but_unnamed(&b);
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
    for old in [false, true] {
        let b = build(&churn_spec(CryptoMode::Integrity));
        let needed = if old {
            live_but_unnamed(&b)
        } else {
            *b.known.segments.last().expect("a segment of the tip")
        };
        std::fs::remove_file(b.path(&b.seg_rel(needed))).expect("rm");
        let before = segments(&b);
        let r = b.fsck(&b.opts());
        assert_eq!(r.exit_code(), 2, "{}", r.render_text());
        assert!(r.has(code::MAP_ENTRY_UNRESOLVED), "{}", r.render_text());
        let r = b.fsck(&gc_repair(&b));
        assert!(r.has(code::REPAIR_REFUSED), "{}", r.render_text());
        assert!(!r.repairs.iter().any(|a| a.action == "gc"));
        assert_eq!(segments(&b), before, "nothing deleted");
    }
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

// ---------------------------------------------------------------------------
// Volumes beyond the builder's: commits read back, stale hints, a copy.

/// FULL `seq` of the volume.
fn full(b: &Built, seq: u64) -> Full {
    let vol = b.open_store();
    let bytes = vol.get(CommitSeq(seq)).expect("commit");
    match b
        .crypto
        .meta()
        .open_commit(CommitSeq(seq), &bytes)
        .expect("open")
    {
        Commit::Full(f) => f,
        _ => panic!("not a FULL"),
    }
}

/// Every entry of FULL `seq`'s map.
fn entries(b: &Built, seq: u64) -> Vec<(u64, DataEntry)> {
    let f = full(b, seq);
    let vol = b.open_store();
    let map = RadixMap::open(
        b.crypto.extents().clone(),
        Arc::new(vol.clone()) as Arc<dyn NodeSource>,
        &f.map_root,
        f.size / u64::from(b.spec.chunk_size),
        MapConfig::new(b.spec.fanout, b.spec.id_len),
    )
    .expect("map");
    let mut out = Vec::new();
    map.walk_leaves(&f.map_root, &mut |ci, e| out.push((ci.0, *e)))
        .expect("walk");
    out
}

/// A segment the tip's map hints into that no FULL of GC's retained set
/// names (commits 2 to 4 of a churn volume): one of commit 1's.
fn live_but_unnamed(b: &Built) -> ObjId {
    let named: BTreeSet<ObjId> = (2..=b.known.tip)
        .flat_map(|q| full(b, q).new_segments)
        .collect();
    let first: BTreeSet<ObjId> = full(b, 1).new_segments.into_iter().collect();
    entries(b, b.known.tip)
        .iter()
        .map(|(_, e)| e.seg)
        .find(|s| first.contains(s) && !named.contains(s))
        .expect("a segment of commit 1 the tip still maps")
}

fn open_node(b: &Built, loc: NodeLoc) -> Vec<u8> {
    let bytes = b.read(&b.pack_rel(loc.pack));
    let ext = &bytes[loc.off as usize..loc.off as usize + loc.len as usize];
    let mut out = Vec::new();
    b.crypto
        .extents()
        .open(
            ExtentKind::Node,
            ext,
            &OpenExpect {
                chunk_size: 65536,
                chunk_id: None,
                id_prefix: None,
                verify_id: false,
                output: OpenOutput::Plaintext,
            },
            &mut out,
        )
        .expect("open node");
    out
}

fn seal_node(b: &Built, hash: &[u8; 32], body: &[u8]) -> Vec<u8> {
    let mut ext = Vec::new();
    b.crypto
        .extents()
        .seal(
            ExtentKind::Node,
            &ChunkId(*hash),
            &EncodingDesc {
                codec: Codec::Raw,
                level: Level(0),
                ulen: body.len() as u32,
                dict_id: None,
            },
            body,
            &mut ext,
        )
        .expect("seal");
    ext
}

/// A new tip (commit `known.tip + 1`) whose map is the tip's with every
/// entry passed through `f`, naming `new_segments` (as M14's stale tests do;
/// the map is two levels deep in these volumes).
fn restamp(b: &Built, new_segments: Vec<ObjId>, f: impl Fn(u64, &mut DataEntry)) {
    let tip = full(b, b.known.tip);
    let store = b.open_store();
    let root_loc = NodeLoc {
        pack: tip.map_root.pack.expect("pack"),
        off: tip.map_root.off,
        len: tip.map_root.len,
    };
    let fanout = b.spec.fanout;
    let root = Interior::decode(&open_node(b, root_loc), fanout).expect("root");
    let params = LeafParams {
        fanout,
        id_len: b.spec.id_len,
        ovh: b.ovh() as u32,
    };
    let mut pw = store.open_pack().expect("pack");
    let mut kids: Vec<Child> = root.children.clone();
    for (i, kid) in kids.iter_mut().enumerate() {
        let Some(loc) = kid.loc else { continue };
        let mut leaf = Leaf::decode(&open_node(b, loc), &params).expect("leaf");
        for (slot, e) in leaf.entries.iter_mut().enumerate() {
            if let Entry::Data(d) = e {
                f(i as u64 * u64::from(fanout) + slot as u64, d);
            }
        }
        let mut body = Vec::new();
        leaf.encode(&params, &mut body).expect("encode leaf");
        let hash = b.crypto.extents().chunk_id(ExtentKind::Node, &body).0;
        kid.hash = hash;
        kid.loc = Some(
            pw.append_node(&hash, &seal_node(b, &hash, &body))
                .expect("append"),
        );
    }
    let new_root = Interior {
        level: root.level,
        children: kids,
    };
    let rhash = b
        .crypto
        .extents()
        .chunk_id(ExtentKind::Node, &new_root.hash_input().expect("input"))
        .0;
    let mut body = Vec::new();
    new_root.encode(&mut body).expect("encode");
    let rloc = pw
        .append_node(&rhash, &seal_node(b, &rhash, &body))
        .expect("root");
    let pack = pw.seal().expect("seal").pack;
    let seq = b.known.tip + 1;
    let next = Full {
        common: CommonHeader {
            kind: CommitKind::Full,
            commit_seq: CommitSeq(seq),
            epoch: Epoch(0),
            parent_hash: b.known.tip_hash,
            created_at: TimeMs(CREATED_AT + seq * 1000),
        },
        cut_seq: tip.cut_seq,
        size: tip.size,
        chain_floor: CommitSeq(
            tip.chain_floor
                .0
                .max(seq - u64::from(b.spec.retain_last - 1)),
        ),
        map_root: MapRoot {
            hash: rhash,
            pack: Some(pack),
            off: rloc.off,
            len: rloc.len,
        },
        heat: None,
        map_packs: vec![pack],
        new_segments,
        index_packs: Vec::new(),
    };
    let sc = b
        .crypto
        .meta()
        .seal_commit(&Commit::Full(next))
        .expect("seal commit");
    store.put(CommitSeq(seq), &sc.bytes).expect("put");
    store
        .raise_rollback_floor(&RollbackFloor {
            max_commit_seq: CommitSeq(seq),
            tip_hash: sc.hash,
        })
        .expect("floor");
}

/// Copies every extent of `seg` into a new sealed segment and points the
/// index's entries at the copies (what compaction does); returns the new
/// segment and the old-to-new hints.
fn copy_segment(b: &Built, seg: ObjId) -> (ObjId, Vec<(Hint, Hint)>) {
    let store = b.open_store();
    let footer = store.segment_footer(seg).expect("footer");
    let mut w = store.open_segment().expect("open segment");
    let mut moved = Vec::new();
    for e in &footer.entries {
        let old = Hint {
            seg,
            off: e.off,
            clen: e.clen,
        };
        let mut ext = Vec::new();
        store.read_extent(&old, &mut ext).expect("read");
        let new = w.append_extent(&e.chunk_id, None, &ext).expect("append");
        moved.push((old, new, e.chunk_id));
    }
    let out = w.id();
    w.seal(None).expect("seal");
    let idx = LsmIndex::open(&b.dir, store.ids(), b.spec.id_len).expect("index");
    let mut pairs = Vec::new();
    for (old, new, id) in moved {
        let new = Hint { seg: out, ..new };
        idx.relocate(&id, &old, &new).expect("relocate");
        pairs.push((old, new));
    }
    idx.persist(out).expect("persist");
    (out, pairs)
}

fn stale(_chunk: u64, d: &mut DataEntry) {
    d.off = d.off.wrapping_add(4);
}

#[test]
fn stale_hints_resolve_alike_in_fsck_and_gc() {
    for mode in ALL_MODES {
        let b = build(&churn_spec(mode));
        restamp(&b, Vec::new(), stale);
        let r = b.fsck(&b.deep());
        assert_no_gc_errors(&r, mode_name(mode));
        let r = b.fsck(&gc_repair(&b));
        assert!(!r.has(code::REPAIR_REFUSED), "{}", r.render_text());
        assert!(r.repairs.iter().any(|a| a.action == "gc"));
        assert_no_gc_errors(&b.fsck(&b.deep()), mode_name(mode));
    }
}

/// The hint lands on a copy the index does not name: the live copy is the
/// index's (in another segment), so the hinted segment holds dead bytes.
#[test]
fn a_hint_to_an_older_copy_counts_the_index_copy_in_fsck_and_gc() {
    let b = build(&churn_spec(CryptoMode::Integrity));
    let seg = *b.known.segments.last().expect("a segment of the tip");
    let (copy, pairs) = copy_segment(&b, seg);
    assert!(!pairs.is_empty());
    let r = b.fsck(&b.opts());
    assert!(!r.has(code::GC_LIVENESS), "{}", r.render_text());
    assert!(!r.has(code::GC_UNCHECKED), "{}", r.render_text());
    assert!(r.exit_code() <= 1, "{}", r.render_text());
    // The copy carries the live bytes in both counts: the dry run would not
    // delete it.
    let r = b.fsck(&gc_repair(&b));
    assert!(!r.has(code::REPAIR_REFUSED), "{}", r.render_text());
    assert!(b.path(&b.seg_rel(copy)).exists(), "the index's copy stays");
    let r = b.fsck(&b.deep());
    assert!(!r.has(code::MAP_ENTRY_UNRESOLVED), "{}", r.render_text());
}

/// M17.B review B-1 through fsck: compaction moved a segment's extents, only
/// the tip names the output, and the older roots reach the moved chunks only
/// through the index. `--truncate-tip` would leave the output above the new
/// chain's mark (the mount sweep deletes it): refused.
#[test]
fn truncate_tip_refuses_when_older_roots_resolve_into_an_output_only_the_tip_names() {
    let b = build(&churn_spec(CryptoMode::Integrity));
    let seg = *b.known.segments.last().expect("a segment of the tip");
    let (out, pairs) = copy_segment(&b, seg);
    let moves: std::collections::HashMap<(u32, u32), Hint> =
        pairs.iter().map(|(o, n)| ((o.off, o.clen), *n)).collect();
    restamp(&b, vec![out], |_, d| {
        if d.seg == seg {
            if let Some(n) = moves.get(&(d.off, d.clen)) {
                d.seg = n.seg;
                d.off = n.off;
            }
        }
    });
    // The source goes, as a GC without the B-1 rule would delete it.
    {
        let store = b.open_store();
        let idx = LsmIndex::open(&b.dir, store.ids(), b.spec.id_len).expect("index");
        idx.drop_segment(seg).expect("drop");
        drop(idx);
        SegmentStore::delete(&store, seg).expect("delete");
    }
    let r = b.fsck(&b.opts());
    assert!(r.exit_code() <= 1, "{}", r.render_text());
    let tip_before = r.tip;
    let r = b.fsck(&Options {
        repair: true,
        truncate_tip: true,
        ..b.opts()
    });
    assert!(r.has(code::REPAIR_REFUSED), "{}", r.render_text());
    assert!(
        codes(&r, code::REPAIR_REFUSED)
            .iter()
            .any(|m| m.contains("only the dropped tip names")),
        "{:?}",
        codes(&r, code::REPAIR_REFUSED)
    );
    assert_eq!(r.tip, tip_before, "nothing truncated");
    assert!(b.path(&b.seg_rel(out)).exists());
}

/// A-1: a volume that fails R4 (rollback, fork) or any other check is never
/// collected.
#[test]
fn repair_gc_refuses_a_rolled_back_or_forked_volume() {
    for fork in [false, true] {
        let b = build(&churn_spec(CryptoMode::Integrity));
        let floor = if fork {
            RollbackFloor {
                max_commit_seq: CommitSeq(b.known.tip),
                tip_hash: [0x77; 32],
            }
        } else {
            RollbackFloor {
                max_commit_seq: CommitSeq(b.known.tip + 3),
                tip_hash: [0x77; 32],
            }
        };
        set_floor(&b, Some(floor));
        let before = segments(&b);
        let r = b.fsck(&gc_repair(&b));
        assert!(
            r.has(if fork { code::FORK } else { code::ROLLBACK }),
            "{}",
            r.render_text()
        );
        assert!(
            codes(&r, code::REPAIR_REFUSED)
                .iter()
                .any(|m| m.contains("the check found errors")),
            "{}",
            r.render_text()
        );
        assert!(!r.repairs.iter().any(|a| a.action == "gc"));
        assert_eq!(segments(&b), before, "nothing deleted");
    }
}

#[test]
fn repair_gc_refuses_a_rejected_pins_file() {
    let b = build(&churn_spec(CryptoMode::Integrity));
    b.flip("state/pins", 8);
    let before = segments(&b);
    let r = b.fsck(&gc_repair(&b));
    assert!(r.has(code::STATE_PINS), "{}", r.render_text());
    assert!(r.has(code::REPAIR_REFUSED), "{}", r.render_text());
    assert_eq!(segments(&b), before, "nothing deleted");
}

#[test]
fn repair_gc_after_the_index_is_emptied_rebuilds_it_first() {
    let b = build(&churn_spec(CryptoMode::Integrity));
    restamp(&b, Vec::new(), stale);
    for name in list(&b.dir, "index") {
        std::fs::remove_file(b.path(&format!("index/{name}"))).expect("rm");
    }
    let r = b.fsck(&gc_repair(&b));
    assert!(
        r.repairs.iter().any(|a| a.action == "rebuild_index"),
        "{}",
        r.render_text()
    );
    assert!(
        r.repairs.iter().any(|a| a.action == "gc"),
        "{}",
        r.render_text()
    );
    assert_no_gc_errors(&b.fsck(&b.deep()), "after the repair");
}

#[test]
fn truncate_tip_and_gc_in_one_run_leave_a_clean_volume() {
    let b = build(&churn_spec(CryptoMode::Integrity));
    let r = b.fsck(&Options {
        truncate_tip: true,
        ..gc_repair(&b)
    });
    assert!(!r.has(code::REPAIR_REFUSED), "{}", r.render_text());
    assert_eq!(r.tip, Some(b.known.tip - 1));
    assert!(
        r.repairs.iter().any(|a| a.action == "gc"),
        "{}",
        r.render_text()
    );
    let r = b.fsck(&b.deep());
    assert!(!r.has(code::GC_LIVENESS) && !r.has(code::GC_UNCHECKED));
    assert!(r.exit_code() <= 1, "{}", r.render_text());
}

// ---------------------------------------------------------------------------
// The orphan and index_lost rules of the M17.B stack round (re-review M-3).

/// An unreferenced segment below the high-water mark whose naming commit is
/// gone (as GC leaves it) is GC's garbage, not an `orphan`: the dry run
/// counts it. A sealed segment above the mark that nothing names is one.
#[test]
fn below_the_mark_unreferenced_is_gc_garbage_above_it_an_orphan() {
    let dead = dead_segments();
    let b = build(&churn_spec(CryptoMode::Integrity));
    // GC's retained set under tip 4 reaches down to commit 2: commits 0 and
    // 1 go first, the dead segments they named stay until a pass.
    std::fs::remove_file(b.path(&b.commit_rel(0))).expect("rm");
    std::fs::remove_file(b.path(&b.commit_rel(1))).expect("rm");
    let r = b.fsck(&b.opts());
    assert!(!r.has(code::ORPHAN), "{}", r.render_text());
    assert_eq!(r.exit_code(), 0, "{}", r.render_text());
    let dry = codes(&r, code::GC_DRY_RUN);
    assert!(
        dry.iter().any(|m| !m.contains("condemn 0 segments")),
        "the dry run counts the dead segments: {dry:?}"
    );
    assert!(!dead.is_empty());

    // A sealed segment above the mark that nothing names: an orphan.
    let store = b.open_store();
    let id = b.crypto.extents().chunk_id(ExtentKind::Data, b"orphan");
    let mut ext = Vec::new();
    b.crypto
        .extents()
        .seal(
            ExtentKind::Data,
            &id,
            &EncodingDesc {
                codec: Codec::Raw,
                level: Level(0),
                ulen: 6,
                dict_id: None,
            },
            b"orphan",
            &mut ext,
        )
        .expect("seal");
    let mut w = store.open_segment().expect("segment");
    w.append_extent(&id, None, &ext).expect("append");
    w.seal(None).expect("seal");
    drop(store);
    let r = b.fsck(&b.opts());
    assert!(r.has(code::ORPHAN), "{}", r.render_text());
}

/// Footers with one chunk id left out: an index rebuilt through it lacks
/// that id's entry.
struct Without {
    store: lunix_store::LocalVolume,
    id: ChunkId,
}

impl SegmentFooters for Without {
    fn segment_footer(
        &self,
        seg: lunix_format::SegId,
    ) -> lunix_iface::Result<lunix_format::segment::FooterBody> {
        let mut f = self.store.segment_footer(seg)?;
        f.entries.retain(|e| e.chunk_id != self.id);
        Ok(f)
    }
}

/// A segment the tip's chain does not name that holds a live and a dead
/// extent: `(segment, a live id, a dead id)`.
fn mixed_segment(b: &Built) -> (ObjId, ChunkId, ChunkId) {
    let tip = b.known.tip;
    let live: BTreeSet<(ObjId, u32)> = (2..=tip)
        .flat_map(|q| entries(b, q))
        .map(|(_, e)| (e.seg, e.off))
        .collect();
    let chain: BTreeSet<ObjId> = (tip - 1..=tip)
        .flat_map(|q| full(b, q).new_segments)
        .collect();
    let store = b.open_store();
    for &seg in &b.known.segments {
        if chain.contains(&seg) {
            continue;
        }
        let Ok(f) = store.segment_footer(seg) else {
            continue;
        };
        let l = f.entries.iter().find(|e| live.contains(&(seg, e.off)));
        let d = f.entries.iter().find(|e| !live.contains(&(seg, e.off)));
        if let (Some(l), Some(d)) = (l, d) {
            return (seg, l.chunk_id, d.chunk_id);
        }
    }
    panic!("no segment with a live and a dead extent");
}

/// Rebuilds the index without `id`.
fn rebuild_without(b: &Built, id: ChunkId) {
    let store = b.open_store();
    let segs: Vec<ObjId> = SegmentStore::list(&store)
        .expect("list")
        .iter()
        .map(|s| s.seg)
        .collect();
    let idx = LsmIndex::open(&b.dir, store.ids(), b.spec.id_len).expect("index");
    idx.rebuild(
        &segs,
        &Without {
            store: store.clone(),
            id,
        },
    )
    .expect("rebuild");
}

/// `index_lost` counts only live extents: a dead id of a segment in use
/// without an entry is not lost (nothing needs it), a live one is.
#[test]
fn index_lost_counts_only_live_extents() {
    let mut spec = churn_spec(CryptoMode::Integrity);
    spec.segment_target = 64 * 1024;
    let b = build(&spec);
    let (_seg, live, dead) = mixed_segment(&b);
    rebuild_without(&b, dead);
    let r = b.fsck(&b.opts());
    assert!(!r.has(code::INDEX_LOST), "{}", r.render_text());
    rebuild_without(&b, live);
    let r = b.fsck(&b.opts());
    assert!(r.has(code::INDEX_LOST), "{}", r.render_text());
}
