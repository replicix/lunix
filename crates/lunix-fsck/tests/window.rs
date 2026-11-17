//! GC's retained set below `chain_floor` (N24) is a root set for fsck too
//! (M17.B review B-3): what only the FULL below the tip's `chain_floor`
//! reaches is checked, and `--delete-orphans` keeps it even when the commit
//! that named it is gone.

mod common;

use std::sync::Arc;

use common::*;
use lunix_format::commit::Commit;
use lunix_format::{CommitSeq, CryptoMode};
use lunix_fsck::{code, Options};
use lunix_iface::crypto::CommitCodec;
use lunix_iface::map::{entry_hint, BlockMap, NodeSource};
use lunix_iface::store::CommitStore;
use lunix_map::{MapConfig, RadixMap};

/// Four commits of 40 new chunks, small segments: commit 2 is GC's extra
/// root under tip 4 (`retain_last = 2`: chain_floor 3, commit 3 records 2).
fn spec() -> Spec {
    let mut s = Spec::new(CryptoMode::Integrity, 16 * 1024);
    s.chunks = 1024;
    s.per_commit = 40;
    s.commits = 4;
    s.segment_target = 16 * 1024;
    s
}

/// The segments commit `seq`'s map hints into.
fn hinted_segments(b: &Built, seq: u64) -> Vec<lunix_format::ObjId> {
    let vol = b.open_store();
    let bytes = vol.get(CommitSeq(seq)).expect("commit");
    let Commit::Full(f) = b
        .crypto
        .meta()
        .open_commit(CommitSeq(seq), &bytes)
        .expect("open")
    else {
        panic!("not a FULL");
    };
    let map = RadixMap::open(
        b.crypto.extents().clone(),
        Arc::new(vol.clone()) as Arc<dyn NodeSource>,
        &f.map_root,
        f.size / u64::from(b.spec.chunk_size),
        MapConfig::new(b.spec.fanout, b.spec.id_len),
    )
    .expect("map");
    let mut segs = Vec::new();
    map.walk_leaves(&f.map_root, &mut |_, e| segs.push(entry_hint(e).seg))
        .expect("walk");
    segs.sort_unstable();
    segs.dedup();
    segs
}

#[test]
fn a_missing_pack_only_the_extra_root_reaches_is_an_error() {
    let b = build(&spec());
    assert_eq!(b.fsck(&b.opts()).exit_code(), 0);
    // Commit 2's pack holds its root node; commits 3 and 4 have their own.
    let pack = b.known.packs[1];
    std::fs::remove_file(b.path(&b.pack_rel(pack))).expect("rm");
    let r = b.fsck(&b.opts());
    assert_eq!(r.exit_code(), 2, "{}", r.render_text());
    assert!(r.has(code::MAP_NODE_MISSING), "{}", r.render_text());
}

#[test]
fn delete_orphans_keeps_what_the_extra_root_reaches_after_its_naming_commit_is_gone() {
    let b = build(&spec());
    let needed = hinted_segments(&b, 2);
    // Commits 0 and 1 are below GC's retained set: gone, as GC leaves them.
    std::fs::remove_file(b.path(&b.commit_rel(0))).expect("rm");
    std::fs::remove_file(b.path(&b.commit_rel(1))).expect("rm");
    let r = b.fsck(&Options {
        repair: true,
        delete_orphans: true,
        ..b.opts()
    });
    assert!(!r.has(code::REPAIR_REFUSED), "{}", r.render_text());
    for s in &needed {
        assert!(
            b.path(&b.seg_rel(*s)).exists(),
            "segment {s:?} that commit 2 hints into is kept"
        );
    }
    let r = b.fsck(&b.deep());
    assert!(r.exit_code() <= 1, "{}", r.render_text());
    assert!(!r.has(code::MAP_ENTRY_UNRESOLVED), "{}", r.render_text());
}

/// B-2: after `--truncate-tip` GC's retained set would reach one FULL further
/// down, whose data GC may have collected already; the truncation deletes
/// that commit, so the new tip's set is what the old one kept.
#[test]
fn truncate_tip_drops_the_commit_that_would_newly_join_gcs_retained_set() {
    let b = build(&spec());
    assert!(b.path(&b.commit_rel(1)).exists());
    let r = b.fsck(&Options {
        repair: true,
        truncate_tip: true,
        ..b.opts()
    });
    assert!(!r.has(code::REPAIR_REFUSED), "{}", r.render_text());
    assert!(
        r.repairs.iter().any(|a| a.action == "drop_old_commits"),
        "{:?}",
        r.repairs
    );
    assert_eq!(r.tip, Some(3));
    assert!(!b.path(&b.commit_rel(1)).exists(), "commit 1 is gone");
    assert!(
        b.path(&b.commit_rel(2)).exists(),
        "commit 2 is the new chain_floor"
    );
    let r = b.fsck(&b.deep());
    assert!(r.exit_code() <= 1, "{}", r.render_text());
}
