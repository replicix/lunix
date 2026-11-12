//! Store contract tests. See the [`crate::store`] docs.

use std::collections::HashSet;

use lunix_format::gcfile::Condemned;
use lunix_format::local::CheckpointSlot;
use lunix_format::{ChunkId, CommitSeq, VolumeUuid};

use crate::error::ErrorKind;
use crate::store::{
    CheckpointStore, CommitStore, GcStateStore, PackStore, SegmentStore, SweepRequest, VolumeDir,
};

/// The stores of one volume directory, plus restart.
pub trait StoreFixture {
    /// Segments.
    fn segments(&self) -> &dyn SegmentStore;
    /// Map packs.
    fn packs(&self) -> &dyn PackStore;
    /// Commits.
    fn commits(&self) -> &dyn CommitStore;
    /// The checkpoint.
    fn checkpoint(&self) -> &dyn CheckpointStore;
    /// GC state.
    fn gc_state(&self) -> &dyn GcStateStore;
    /// The volume directory.
    fn volume(&self) -> &dyn VolumeDir;

    /// A complete, valid unencrypted-layout data extent for `id` with a
    /// payload of `payload_len` bytes (`ulen` equal to the volume chunk size
    /// is the fixture's business), in the layout the stores were built for.
    fn data_extent(&self, id: &ChunkId, payload_len: usize) -> Vec<u8>;

    /// A complete node extent for the pack tests.
    fn node_extent(&self, hash: &[u8; 32], payload_len: usize) -> Vec<u8>;

    /// Closes everything cleanly and reopens the same directory.
    fn reopen(&mut self);
}

fn cid(n: u8) -> ChunkId {
    ChunkId([n; 32])
}

fn slot(seq: u64, last_commit: u64) -> CheckpointSlot {
    CheckpointSlot {
        slot_seq: seq,
        volume_uuid: VolumeUuid([7; 16]),
        last_commit_seq: last_commit,
        id_high: 10 * seq,
        lane_tail_ordinals: vec![1, 2],
        stats: vec![seq, 2, 3],
    }
}

/// Appended extents read back byte for byte, the footer lists them, and the
/// segment survives a reopen.
pub fn segment_round_trip<F: StoreFixture>(f: &mut F) {
    let exts: Vec<(ChunkId, Vec<u8>)> = (1..=3u8)
        .map(|n| (cid(n), f.data_extent(&cid(n), 300 + usize::from(n))))
        .collect();
    let mut w = f.segments().open_segment().expect("open");
    assert!(w.is_empty());
    let hints: Vec<_> = exts
        .iter()
        .map(|(id, e)| w.append_extent(id, e).expect("append"))
        .collect();
    assert!(!w.is_full());
    let seg = w.id();
    let sealed = w.seal(None).expect("seal");
    assert_eq!((sealed.seg, sealed.extents), (seg, 3));
    for round in 0..2 {
        let s = f.segments();
        let listed = s.list().expect("list");
        assert!(listed
            .iter()
            .any(|i| i.seg == seg && i.size_bytes == sealed.size_bytes));
        let footer = s.footer(seg).expect("footer");
        assert_eq!(footer.entries.len(), 3);
        for ((id, bytes), hint) in exts.iter().zip(&hints) {
            assert_eq!(hint.seg, seg);
            assert!(footer
                .entries
                .iter()
                .any(|e| e.chunk_id == *id && e.off == hint.off));
            let mut out = Vec::new();
            s.read_extent(hint, &mut out).expect("read");
            assert_eq!(&out, bytes);
            let loc = s.locate(hint).expect("locate");
            assert_eq!(
                (loc.offset, loc.len as usize),
                (u64::from(hint.off), bytes.len())
            );
        }
        if round == 0 {
            f.reopen();
        }
    }
}

/// A writer dropped without sealing leaves nothing listed or readable.
pub fn abandoned_segment_is_not_listed<F: StoreFixture>(f: &mut F) {
    let before = f.segments().list().expect("list").len();
    let mut w = f.segments().open_segment().expect("open");
    let e = f.data_extent(&cid(1), 100);
    let hint = w.append_extent(&cid(1), &e).expect("append");
    drop(w);
    assert_eq!(f.segments().list().expect("list").len(), before);
    let mut out = Vec::new();
    assert!(f.segments().read_extent(&hint, &mut out).is_err());
}

/// Deleting frees the file's bytes once and then nothing.
pub fn segment_delete_is_idempotent<F: StoreFixture>(f: &mut F) {
    let mut w = f.segments().open_segment().expect("open");
    let e = f.data_extent(&cid(1), 100);
    w.append_extent(&cid(1), &e).expect("append");
    let sealed = w.seal(None).expect("seal");
    assert_eq!(
        f.segments().delete(sealed.seg).expect("delete"),
        sealed.size_bytes
    );
    assert_eq!(f.segments().delete(sealed.seg).expect("again"), 0);
    assert_eq!(
        f.segments().footer(sealed.seg).expect_err("gone").kind(),
        ErrorKind::NotFound
    );
}

/// Ids rise across segments, packs and reopen, and are never reused.
pub fn ids_are_never_reused<F: StoreFixture>(f: &mut F) {
    let mut seen = Vec::new();
    for _ in 0..3 {
        let mut w = f.segments().open_segment().expect("open");
        let e = f.data_extent(&cid(1), 100);
        w.append_extent(&cid(1), &e).expect("append");
        seen.push(w.id().raw());
        w.seal(None).expect("seal");
    }
    f.reopen();
    let w = f.segments().open_segment().expect("open");
    seen.push(w.id().raw());
    let p = f.packs().open_pack().expect("pack");
    seen.push(p.id().raw());
    let mut sorted = seen.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted, seen, "ids strictly rise: {seen:?}");
    assert!(f.volume().ids().high_water() >= *seen.last().expect("id"));
}

/// Nodes appended to a pack read back through `PackRead` after sealing.
pub fn pack_round_trip<F: StoreFixture>(f: &mut F) {
    let mut p = f.packs().open_pack().expect("pack");
    let e1 = f.node_extent(&[1; 32], 200);
    let e2 = f.node_extent(&[2; 32], 300);
    let l1 = p.append_node(&[1; 32], &e1).expect("append");
    let l2 = p.append_node(&[2; 32], &e2).expect("append");
    assert_eq!((l1.pack, l2.pack), (p.id(), p.id()));
    let sealed = p.seal().expect("seal");
    assert_eq!(sealed.nodes, 2);
    let mut out = Vec::new();
    f.packs().read_node(l1, &mut out).expect("read");
    assert_eq!(out, e1);
    out.clear();
    f.packs().read_node(l2, &mut out).expect("read");
    assert_eq!(out, e2);
    assert!(f
        .packs()
        .list_packs()
        .expect("list")
        .iter()
        .any(|i| i.pack == sealed.pack));
    assert_eq!(
        f.packs().delete_pack(sealed.pack).expect("delete"),
        sealed.size_bytes
    );
    assert_eq!(f.packs().delete_pack(sealed.pack).expect("again"), 0);
}

/// Commits are create-only, readable, ordered and survive a reopen.
pub fn commits_are_create_only<F: StoreFixture>(f: &mut F) {
    let c = f.commits();
    assert_eq!(c.tip().expect("tip"), None);
    c.put(CommitSeq(0), b"zero").expect("put");
    c.put(CommitSeq(1), b"one").expect("put");
    assert_eq!(
        c.put(CommitSeq(1), b"other").expect_err("exists").kind(),
        ErrorKind::AlreadyExists
    );
    assert_eq!(c.get(CommitSeq(1)).expect("get"), b"one");
    assert_eq!(
        c.get(CommitSeq(9)).expect_err("missing").kind(),
        ErrorKind::NotFound
    );
    assert_eq!(c.tip().expect("tip"), Some(CommitSeq(1)));
    f.reopen();
    let list = f.commits().list().expect("list");
    assert_eq!(list.iter().map(|i| i.seq.0).collect::<Vec<_>>(), vec![0, 1]);
    assert_eq!(f.commits().get(CommitSeq(0)).expect("get"), b"zero");
}

/// Delete refuses the tip and anything at or above the floor, and frees once.
pub fn commit_delete_respects_floor_and_tip<F: StoreFixture>(f: &mut F) {
    let c = f.commits();
    for n in 0..4u64 {
        c.put(CommitSeq(n), format!("commit{n}").as_bytes())
            .expect("put");
    }
    for seq in [2u64, 3] {
        let e = c.delete(CommitSeq(seq), CommitSeq(2)).expect_err("refused");
        assert_eq!(e.kind(), ErrorKind::Refused, "seq {seq}");
    }
    assert_eq!(
        c.delete(CommitSeq(3), CommitSeq(9))
            .expect_err("tip")
            .kind(),
        ErrorKind::Refused,
        "the tip is never deleted"
    );
    assert_eq!(c.delete(CommitSeq(0), CommitSeq(2)).expect("delete"), 7);
    assert_eq!(c.delete(CommitSeq(0), CommitSeq(2)).expect("again"), 0);
    assert_eq!(c.list().expect("list").len(), 3);
}

/// `set_aside` moves the tip away so the previous commit becomes the tip, and
/// refuses anything else.
pub fn set_aside_moves_only_the_tip<F: StoreFixture>(f: &mut F) {
    let c = f.commits();
    for n in 0..3u64 {
        c.put(CommitSeq(n), b"x").expect("put");
    }
    assert_eq!(
        c.set_aside(CommitSeq(1)).expect_err("not tip").kind(),
        ErrorKind::Refused
    );
    c.set_aside(CommitSeq(2)).expect("aside");
    assert_eq!(c.tip().expect("tip"), Some(CommitSeq(1)));
    assert_eq!(
        c.get(CommitSeq(2)).expect_err("gone").kind(),
        ErrorKind::NotFound
    );
}

/// The checkpoint returns the newest slot, tolerates absence, and `zero` clears it.
pub fn checkpoint_is_a_double_slot_hint<F: StoreFixture>(f: &mut F) {
    assert_eq!(f.checkpoint().load().expect("load"), None);
    f.checkpoint().store(&slot(1, 4)).expect("store");
    assert_eq!(f.checkpoint().load().expect("load"), Some(slot(1, 4)));
    f.checkpoint().store(&slot(2, 5)).expect("store");
    f.reopen();
    assert_eq!(f.checkpoint().load().expect("load"), Some(slot(2, 5)));
    f.checkpoint().zero().expect("zero");
    assert_eq!(f.checkpoint().load().expect("load"), None);
}

/// Evidence objects are create-only and listed.
pub fn evidence_is_create_only<F: StoreFixture>(f: &mut F) {
    let g = f.gc_state();
    assert!(g.evidence_passes().expect("list").is_empty());
    g.put_evidence(1, b"{\"a\":1}\n").expect("put");
    assert_eq!(
        g.put_evidence(1, b"other").expect_err("exists").kind(),
        ErrorKind::AlreadyExists
    );
    g.put_evidence(3, b"{}\n").expect("put");
    assert_eq!(g.evidence_passes().expect("list"), vec![1, 3]);
}

/// The condemned list is absent until stored, and replaced atomically.
pub fn condemned_list_round_trips<F: StoreFixture>(f: &mut F) {
    assert_eq!(f.gc_state().load_condemned().expect("load"), None);
    let list = Condemned {
        body_ver: 1,
        body: b"list".to_vec(),
    };
    f.gc_state().store_condemned(&list).expect("store");
    f.reopen();
    assert_eq!(f.gc_state().load_condemned().expect("load"), Some(list));
    let newer = Condemned {
        body_ver: 1,
        body: b"newer".to_vec(),
    };
    f.gc_state().store_condemned(&newer).expect("store");
    assert_eq!(f.gc_state().load_condemned().expect("load"), Some(newer));
}

/// The sweep deletes unnamed sealed files above the mark only, reports the
/// highest id present before deleting, and leaves the rest for GC.
pub fn sweep_deletes_only_unnamed_orphans_above_the_mark<F: StoreFixture>(f: &mut F) {
    let mut ids = Vec::new();
    for n in 1..=3u8 {
        let mut w = f.segments().open_segment().expect("open");
        let e = f.data_extent(&cid(n), 100);
        w.append_extent(&cid(n), &e).expect("append");
        ids.push(w.id());
        w.seal(None).expect("seal");
    }
    let (low, named, orphan) = (ids[0], ids[1], ids[2]);
    let named_set: HashSet<_> = [named].into_iter().collect();
    let packs = HashSet::new();
    let report = f
        .volume()
        .sweep_orphans(&SweepRequest {
            high_water_mark: named.raw(),
            named_segments: &named_set,
            named_packs: &packs,
        })
        .expect("sweep");
    assert_eq!(report.highest_id_before, orphan.raw());
    assert_eq!(report.deleted_segments, vec![orphan]);
    assert!(
        report.kept_unreferenced.contains(&low),
        "below the mark: left for GC"
    );
    let left: Vec<_> = f
        .segments()
        .list()
        .expect("list")
        .iter()
        .map(|i| i.seg)
        .collect();
    assert_eq!(left, vec![low, named]);
    let again = f
        .volume()
        .sweep_orphans(&SweepRequest {
            high_water_mark: named.raw(),
            named_segments: &named_set,
            named_packs: &packs,
        })
        .expect("sweep");
    assert!(again.deleted_segments.is_empty(), "idempotent");
}
