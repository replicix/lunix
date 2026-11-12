//! `MemStore` beyond the contract suite: validation, footers, fault points,
//! the crash model, capacity.

use std::collections::HashSet;
use std::os::fd::AsFd;
use std::os::unix::fs::FileExt;

use lunix_format::local::VolumeJson;
use lunix_format::segment::ContainerKind;
use lunix_format::{ChunkId, Codec, CommitSeq, ExtentKind, Hint, Level, ObjId, WriteSeq};
use lunix_iface::crypto::{EncodingDesc, ExtentCodec};
use lunix_iface::journal::WriteSeqRange;
use lunix_iface::store::{
    CheckpointStore, CommitStore, GcStateStore, PackStore, PinKind, PinRoot, PinSource,
    SegmentStore, SweepRequest, VolumeDir,
};
use lunix_iface::ErrorKind;
use lunix_testkit::{Fault, MemStore, MemStoreConfig, PlainCodec, StoreImage};

mod common;
use common::data_extent;

fn segs(s: &MemStore) -> &dyn SegmentStore {
    s
}

fn commits(s: &MemStore) -> &dyn CommitStore {
    s
}

fn cid(n: u8) -> ChunkId {
    ChunkId([n; 32])
}

fn sealed_segment(store: &MemStore, n: u8) -> (ObjId, Hint) {
    let mut w = store.open_segment().unwrap();
    let id = cid(n);
    let h = w.append_extent(&id, None, &data_extent(&id, 300)).unwrap();
    (w.seal(None).unwrap().seg, h)
}

#[test]
fn append_validates_the_extent_it_is_given() {
    let store = MemStore::new();
    let mut w = store.open_segment().unwrap();
    let id = cid(1);
    let good = data_extent(&id, 300);
    let kind = |r: lunix_iface::Result<Hint>| r.unwrap_err().kind();
    assert_eq!(
        kind(w.append_extent(&cid(2), None, &good)),
        ErrorKind::Invalid,
        "id"
    );
    assert_eq!(
        kind(w.append_extent(&id, None, &good[..good.len() - 1])),
        ErrorKind::Invalid
    );
    assert_eq!(
        kind(w.append_extent(&id, None, b"nonsense")),
        ErrorKind::Invalid
    );
    assert_eq!(
        kind(w.append_extent(&id, Some(7), &good)),
        ErrorKind::Invalid,
        "dict id on raw"
    );
    assert!(w.is_empty(), "refusals append nothing");
    let h = w.append_extent(&id, None, &good).unwrap();
    assert_eq!((h.off, h.clen), (0, 300));
    let h2 = w.append_extent(&id, None, &good).unwrap();
    assert_eq!(h2.off as usize, good.len());
    assert_eq!(w.len() as usize, 2 * good.len());
}

#[test]
fn the_footer_describes_the_extents_and_the_write_range() {
    let store = MemStore::new();
    let mut w = store.open_segment().unwrap();
    let mut hints = Vec::new();
    for n in [9u8, 3, 7] {
        let id = cid(n);
        hints.push((
            id,
            w.append_extent(&id, None, &data_extent(&id, 100 + usize::from(n)))
                .unwrap(),
        ));
    }
    let seg = w.id();
    let range = WriteSeqRange {
        first: WriteSeq(10),
        last: WriteSeq(20),
    };
    let sealed = w.seal(Some(range)).unwrap();
    assert_eq!(sealed.extents, 3);
    let f = store.footer(seg).unwrap();
    assert_eq!(f.container_kind, ContainerKind::Segment);
    assert_eq!(
        (f.write_seq_lo, f.write_seq_hi),
        (WriteSeq(10), WriteSeq(20))
    );
    let ids: Vec<_> = f.entries.iter().map(|e| e.chunk_id).collect();
    assert_eq!(ids, vec![cid(3), cid(7), cid(9)], "sorted by chunk id");
    assert!(f
        .entries
        .iter()
        .all(|e| e.codec == Codec::Raw && e.clen == e.ulen));
    let mut region = Vec::new();
    for (_, h) in &hints {
        store.read_extent(h, &mut region).unwrap();
    }
    assert_eq!(f.extents_len, region.len() as u64);
    assert!(
        sealed.size_bytes > f.extents_len + 64,
        "footer and trailer count"
    );
    // Compaction output has no write range.
    let (s2, _) = sealed_segment(&store, 4);
    assert_eq!(store.footer(s2).unwrap().write_seq_hi, WriteSeq(0));
}

#[test]
fn an_empty_segment_cannot_be_sealed_and_a_reversed_range_is_refused() {
    let store = MemStore::new();
    let w = store.open_segment().unwrap();
    assert_eq!(w.seal(None).unwrap_err().kind(), ErrorKind::Invalid);
    let mut w = store.open_segment().unwrap();
    w.append_extent(&cid(1), None, &data_extent(&cid(1), 50))
        .unwrap();
    let bad = WriteSeqRange {
        first: WriteSeq(5),
        last: WriteSeq(4),
    };
    assert_eq!(w.seal(Some(bad)).unwrap_err().kind(), ErrorKind::Invalid);
    assert!(segs(&store).list().unwrap().is_empty());
}

#[test]
fn reads_are_bounds_checked_and_locate_gives_a_working_fd() {
    let store = MemStore::new();
    let (seg, h) = sealed_segment(&store, 1);
    let mut bytes = Vec::new();
    store.read_extent(&h, &mut bytes).unwrap();
    let past = Hint {
        off: h.off + 10_000,
        ..h
    };
    assert_eq!(
        store
            .read_extent(&past, &mut Vec::new())
            .unwrap_err()
            .kind(),
        ErrorKind::Corrupt
    );
    let missing = Hint {
        seg: ObjId::new(0, 999).unwrap(),
        ..h
    };
    assert_eq!(
        store
            .read_extent(&missing, &mut Vec::new())
            .unwrap_err()
            .kind(),
        ErrorKind::NotFound
    );
    let loc = store.locate(&h).unwrap();
    let file = std::fs::File::from(loc.fd.as_fd().try_clone_to_owned().unwrap());
    let mut got = vec![0u8; loc.len as usize];
    file.read_exact_at(&mut got, loc.offset).unwrap();
    assert_eq!(got, bytes);
    segs(&store).delete(seg).unwrap();
    file.read_exact_at(&mut got, loc.offset).unwrap();
    assert_eq!(got, bytes, "the descriptor outlives the delete");
}

#[test]
fn a_clean_close_abandons_open_writers_and_their_ids_come_back_unless_observed() {
    let store = MemStore::new();
    let (kept, _) = sealed_segment(&store, 1);
    let mut w = store.open_segment().unwrap();
    w.append_extent(&cid(2), None, &data_extent(&cid(2), 50))
        .unwrap();
    let abandoned = w.id();
    let image = store.close();
    assert_eq!(
        segs(&store).list().unwrap_err().kind(),
        ErrorKind::Io,
        "old handle is dead"
    );
    drop(w);
    let again = MemStore::from_image(&image);
    assert_eq!(segs(&again).list().unwrap().len(), 1);
    assert!(again.leftovers().unwrap().is_empty());
    // The allocator restarts from the disk: the abandoned id comes back until
    // the caller feeds `observe` (the engine does, from the checkpoint).
    assert_eq!(again.open_segment().unwrap().id(), abandoned);
    again.ids().observe(abandoned.raw());
    assert!(again.ids().allocate().unwrap().raw() > abandoned.raw());
    assert!(abandoned.raw() > kept.raw());
}

#[test]
fn a_crash_leaves_open_files_at_random_and_the_sweep_removes_them() {
    let (mut with, mut without) = (0, 0);
    for seed in 0..40u64 {
        let store = MemStore::new();
        let (named, _) = sealed_segment(&store, 1);
        let mut w = store.open_segment().unwrap();
        w.append_extent(&cid(2), None, &data_extent(&cid(2), 50))
            .unwrap();
        let open_id = w.id();
        let image = store.crash(seed);
        drop(w);
        let after = MemStore::from_image(&image);
        let left = after.leftovers().unwrap();
        if left.is_empty() {
            without += 1;
        } else {
            with += 1;
            assert_eq!(left.len(), 1);
            assert!(left[0].name.ends_with(".open") && left[0].size_bytes > 0);
            assert_eq!(after.ids().high_water(), open_id.raw(), "ids on disk count");
        }
        let set: HashSet<_> = [named].into_iter().collect();
        let report = after
            .sweep_orphans(&SweepRequest {
                high_water_mark: named.raw(),
                named_segments: &set,
                named_packs: &HashSet::new(),
            })
            .unwrap();
        assert_eq!(report.deleted_temp as usize, left.len());
        assert!(after.leftovers().unwrap().is_empty());
    }
    assert!(with > 5 && without > 5, "{with} {without}");
}

#[test]
fn a_crash_fault_at_seal_decides_whether_the_segment_exists() {
    for (point, exists) in [("segment.seal.before", false), ("segment.seal.after", true)] {
        let store = MemStore::new();
        store.faults().arm(point, 1, Fault::Crash);
        let mut w = store.open_segment().unwrap();
        w.append_extent(&cid(1), None, &data_extent(&cid(1), 50))
            .unwrap();
        let id = w.id();
        assert_eq!(w.seal(None).unwrap_err().kind(), ErrorKind::Io);
        assert_eq!(
            segs(&store).list().unwrap_err().kind(),
            ErrorKind::Io,
            "dead"
        );
        let after = MemStore::from_image(&store.crash(0));
        assert_eq!(
            segs(&after).list().unwrap().len(),
            usize::from(exists),
            "{point}"
        );
        if !exists {
            let left = after.leftovers().unwrap();
            assert_eq!(left.len(), 1, "the half-written file stays");
            assert_eq!(after.ids().high_water(), id.raw());
        }
        assert!(store.faults().hits().get(point).copied().unwrap_or(0) >= 1);
    }
}

#[test]
fn a_one_shot_fault_fails_one_call_and_changes_nothing() {
    let store = MemStore::new();
    store.faults().arm("commit.put.before", 2, Fault::NoSpace);
    store.put(CommitSeq(0), b"a").unwrap();
    assert_eq!(
        store.put(CommitSeq(1), b"b").unwrap_err().kind(),
        ErrorKind::NoSpace
    );
    assert_eq!(store.tip().unwrap(), Some(CommitSeq(0)));
    store.put(CommitSeq(1), b"b").unwrap();
    store.faults().arm("commit.put.after", 1, Fault::Crash);
    assert!(store.put(CommitSeq(2), b"c").is_err());
    let after = MemStore::from_image(&store.crash(0));
    assert_eq!(
        after.tip().unwrap(),
        Some(CommitSeq(2)),
        "durable, never acknowledged"
    );
}

#[test]
fn capacity_refuses_creates_and_deletes_free_space() {
    let store = MemStore::with_config(MemStoreConfig {
        capacity_bytes: Some(2_000),
        ..MemStoreConfig::default()
    });
    store.put(CommitSeq(0), &[0u8; 900]).unwrap();
    store.put(CommitSeq(1), &[0u8; 900]).unwrap();
    assert_eq!(
        store.put(CommitSeq(2), &[0u8; 900]).unwrap_err().kind(),
        ErrorKind::NoSpace
    );
    assert_eq!(store.used_bytes(), 1_800);
    assert_eq!(
        commits(&store).delete(CommitSeq(0), CommitSeq(2)).unwrap(),
        900
    );
    store.put(CommitSeq(2), &[0u8; 900]).unwrap();
    // A segment too big for the budget fails at append, leaving no file.
    let mut w = store.open_segment().unwrap();
    let id = cid(1);
    let e = w
        .append_extent(&id, None, &data_extent(&id, 1_500))
        .unwrap_err();
    assert_eq!(e.kind(), ErrorKind::NoSpace);
    drop(w);
    store.set_capacity(None);
    assert_eq!(segs(&store).list().unwrap().len(), 0);
}

#[test]
fn packs_have_footers_and_deleted_packs_are_idempotent() {
    let store = MemStore::new();
    let codec = PlainCodec::new();
    let hash = [7u8; 32];
    let desc = EncodingDesc {
        codec: Codec::Raw,
        level: Level(0),
        ulen: 500,
        dict_id: None,
    };
    let mut ext = Vec::new();
    ExtentCodec::seal(
        &codec,
        ExtentKind::Node,
        &ChunkId(hash),
        &desc,
        &[1u8; 500],
        &mut ext,
    )
    .unwrap();
    let mut p = store.open_pack().unwrap();
    assert_eq!(
        p.append_node(&[8u8; 32], &ext).unwrap_err().kind(),
        ErrorKind::Invalid,
        "the node hash must be the extent's id"
    );
    let loc = p.append_node(&hash, &ext).unwrap();
    assert_eq!(loc.len as usize, ext.len());
    let sealed = p.seal().unwrap();
    use lunix_iface::store::PackRead;
    let f = store.pack_footer(sealed.pack).unwrap();
    assert_eq!(f.container_kind, ContainerKind::MapPack);
    assert_eq!(f.entries.len(), 1);
    assert_eq!(store.delete_pack(sealed.pack).unwrap(), sealed.size_bytes);
    assert_eq!(
        store.pack_footer(sealed.pack).unwrap_err().kind(),
        ErrorKind::NotFound
    );
}

#[test]
fn the_checkpoint_falls_back_when_the_newest_slot_is_torn() {
    let store = MemStore::new();
    let slot = |n: u64| lunix_format::local::CheckpointSlot {
        slot_seq: 0,
        volume_uuid: store.uuid(),
        last_commit_seq: n,
        id_high: n,
        lane_tail_ordinals: vec![0],
        stats: vec![],
    };
    store.store(&slot(1)).unwrap();
    store.store(&slot(2)).unwrap();
    store.store(&slot(3)).unwrap();
    assert_eq!(store.load().unwrap().unwrap().slot_seq, 3);
    store.tear_newest_checkpoint();
    let got = store.load().unwrap().unwrap();
    assert_eq!((got.slot_seq, got.last_commit_seq), (2, 2));
    store.tear_newest_checkpoint();
    assert_eq!(store.load().unwrap(), None);
}

#[test]
fn set_aside_keeps_the_commit_out_of_the_listing() {
    let store = MemStore::new();
    for n in 0..3u64 {
        store.put(CommitSeq(n), b"x").unwrap();
    }
    store.set_aside(CommitSeq(2)).unwrap();
    assert_eq!(commits(&store).list().unwrap().len(), 2);
    assert_eq!(
        store.set_aside(CommitSeq(2)).unwrap_err().kind(),
        ErrorKind::Refused
    );
    store.put(CommitSeq(2), b"again").unwrap();
    assert_eq!(store.get(CommitSeq(2)).unwrap(), b"again");
}

#[test]
fn pins_gc_evidence_and_the_volume_files() {
    let store = MemStore::new();
    assert!(store.roots().unwrap().is_empty());
    let root = PinRoot {
        kind: PinKind::Snapshot,
        name: "s1".into(),
        commit_seq: CommitSeq(4),
        deadline: None,
    };
    store.set_pins(vec![root.clone()]);
    assert_eq!(store.roots().unwrap(), vec![root]);
    store.fail_pins(true);
    assert_eq!(store.roots().unwrap_err().kind(), ErrorKind::Corrupt);
    assert_eq!(
        store.put_evidence(0, b"x").unwrap_err().kind(),
        ErrorKind::Invalid
    );

    assert_eq!(
        store.volume_bytes().unwrap(),
        MemStoreConfig::default().volume_bytes
    );
    let mut json = VolumeJson::new("renamed", "/data");
    json.retain_last = 5;
    store.store_volume_json(&json).unwrap();
    assert_eq!(store.volume_json().unwrap(), json);
    assert!(store.device_record().unwrap().is_none());
    assert!(store.local_pins().unwrap().is_none());
    let image: StoreImage = store.close();
    let again = MemStore::from_image(&image);
    assert_eq!(again.volume_json().unwrap(), json);
    assert_eq!(again.uuid(), store.uuid());
}

#[test]
fn ids_are_unique_across_threads() {
    let store = MemStore::new();
    let ids = store.ids();
    let handles: Vec<_> = (0..4)
        .map(|_| {
            let ids = ids.clone();
            std::thread::spawn(move || {
                (0..500)
                    .map(|_| ids.allocate().unwrap().raw())
                    .collect::<Vec<_>>()
            })
        })
        .collect();
    let mut all: Vec<u64> = handles
        .into_iter()
        .flat_map(|h| h.join().unwrap())
        .collect();
    all.sort_unstable();
    all.dedup();
    assert_eq!(all.len(), 2_000);
}

#[test]
fn sweep_counts_leftovers_and_lists_unreferenced_packs() {
    let store = MemStore::new();
    store.add_leftover("segments/00000000000000ff.open", 10);
    store.add_leftover("state/commits/tmp.tmp", 3);
    let report = store
        .sweep_orphans(&SweepRequest {
            high_water_mark: 0,
            named_segments: &HashSet::new(),
            named_packs: &HashSet::new(),
        })
        .unwrap();
    assert_eq!(report.deleted_temp, 2);
    assert_eq!(report.highest_id_before, 0xff, "a leftover's id counts");
}

#[test]
fn the_sweep_refuses_a_mark_below_a_named_id_and_feeds_the_allocator() {
    let store = MemStore::new();
    let (seg, _) = sealed_segment(&store, 1);
    let named: HashSet<_> = [seg].into_iter().collect();
    let none = HashSet::new();
    let e = store
        .sweep_orphans(&SweepRequest {
            high_water_mark: seg.raw() - 1,
            named_segments: &named,
            named_packs: &none,
        })
        .unwrap_err();
    assert_eq!(e.kind(), ErrorKind::Invalid);
    store.add_leftover("segments/00000000000000ff.open", 1);
    let report = store
        .sweep_orphans(&SweepRequest {
            high_water_mark: seg.raw(),
            named_segments: &named,
            named_packs: &none,
        })
        .unwrap();
    assert_eq!(report.highest_id_before, 0xff);
    assert!(
        store.ids().high_water() >= 0xff,
        "the sweep observes the ids it saw"
    );
}

#[test]
fn a_hint_with_the_wrong_clen_is_corrupt_and_open_writers_are_not_leftovers() {
    let store = MemStore::new();
    let (_, h) = sealed_segment(&store, 1);
    let wrong = Hint {
        clen: h.clen - 1,
        ..h
    };
    assert_eq!(
        store
            .read_extent(&wrong, &mut Vec::new())
            .unwrap_err()
            .kind(),
        ErrorKind::Corrupt
    );
    let mut w = store.open_segment().unwrap();
    w.append_extent(&cid(2), None, &data_extent(&cid(2), 50))
        .unwrap();
    assert!(
        store.leftovers().unwrap().is_empty(),
        "the file of a live writer is in use"
    );
    assert_eq!(store.remove_leftovers().unwrap(), 0);
}

#[test]
fn the_checkpoint_belongs_to_one_volume_and_the_tail_guard_zeroes_stale_tails() {
    let store = MemStore::new();
    let slot = |uuid: [u8; 16], last: u64| lunix_format::local::CheckpointSlot {
        slot_seq: 0,
        volume_uuid: lunix_format::VolumeUuid(uuid),
        last_commit_seq: last,
        id_high: 9,
        lane_tail_ordinals: vec![3, 4],
        stats: vec![],
    };
    assert_eq!(
        store.store(&slot([9; 16], 0)).unwrap_err().kind(),
        ErrorKind::Invalid,
        "another volume's slot"
    );
    store.put(CommitSeq(0), b"c0").unwrap();
    store.put(CommitSeq(1), b"c1").unwrap();
    store.store(&slot(store.uuid().0, 1)).unwrap();
    let hint = store.load_checkpoint_hint().unwrap().unwrap();
    assert!(hint.tails_usable);
    assert_eq!(hint.slot.lane_tail_ordinals, vec![3, 4]);
    // After `--truncate-tip` the checkpoint names a commit the chain lost.
    store.set_aside(CommitSeq(1)).unwrap();
    let hint = store.load_checkpoint_hint().unwrap().unwrap();
    assert!(!hint.tails_usable);
    assert_eq!(hint.slot.lane_tail_ordinals, vec![0, 0]);
    assert_eq!(hint.slot.id_high, 9, "id_high stays usable");
    assert_eq!(
        store.load().unwrap().unwrap().lane_tail_ordinals,
        vec![3, 4],
        "load returns the slot as written"
    );
}

#[test]
fn open_observes_the_checkpoint_id_high() {
    let store = MemStore::new();
    let mut w = store.open_segment().unwrap();
    w.append_extent(&cid(1), None, &data_extent(&cid(1), 50))
        .unwrap();
    let abandoned = w.id();
    store
        .store(&lunix_format::local::CheckpointSlot {
            slot_seq: 0,
            volume_uuid: store.uuid(),
            last_commit_seq: 0,
            id_high: abandoned.raw(),
            lane_tail_ordinals: vec![0],
            stats: vec![],
        })
        .unwrap();
    let image = store.close();
    drop(w);
    let again = MemStore::from_image(&image);
    assert!(
        again.open_segment().unwrap().id().raw() > abandoned.raw(),
        "an id the checkpoint recorded is never reissued"
    );
}
