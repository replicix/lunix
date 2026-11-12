//! fsync accounting: an instrumented file system proves the durable order of
//! every create and delete.

use lunix_format::{CommitSeq, CryptoMode};
use lunix_iface::store::{CheckpointStore, CommitStore, GcStateStore, PackStore, SegmentStore};

use super::{cid, create, data_extent, node_extent, tempdir, test_opts, Ev, RecordingFs, UUID};
use lunix_format::local::CheckpointSlot;
use lunix_format::VolumeUuid;

fn ren(from: &str, to: &str) -> Ev {
    Ev::Rename {
        from: from.into(),
        to: to.into(),
    }
}

/// Every rename follows a data sync and is followed by a directory sync before
/// anything else is renamed or the log ends.
fn assert_durable_order(events: &[Ev]) {
    let mut synced = false;
    let mut renamed_unsynced = false;
    for e in events {
        match e {
            Ev::Fdatasync => synced = true,
            Ev::Rename { .. } => {
                assert!(synced, "rename before fdatasync: {events:?}");
                assert!(
                    !renamed_unsynced,
                    "two renames without a dir sync: {events:?}"
                );
                synced = false;
                renamed_unsynced = true;
            }
            Ev::FsyncDir(_) => renamed_unsynced = false,
            Ev::Unlink(_) => {}
        }
    }
    assert!(!renamed_unsynced, "rename without a dir sync: {events:?}");
}

#[test]
fn every_create_syncs_data_then_renames_then_syncs_the_directory() {
    for mode in [CryptoMode::Plain, CryptoMode::Encrypted] {
        let fs = RecordingFs::new();
        let mut opts = test_opts();
        opts.fs = fs.clone();
        let dir = tempdir();
        let vol = create(dir.path(), mode, 16 * 1024, opts);
        assert_durable_order(&fs.take());

        // Segment: the appends write but do not sync; the seal does it all.
        let mut w = vol.open_segment().expect("open");
        for i in 0..3 {
            w.append_extent(&cid(i), None, &data_extent(mode, 16 * 1024, &cid(i), 100))
                .expect("append");
        }
        assert_eq!(fs.take(), vec![], "appending never syncs or renames");
        let seg = w.id();
        w.seal(None).expect("seal");
        assert_eq!(
            fs.take(),
            vec![
                Ev::Fdatasync,
                ren(&format!("{seg}.open"), &format!("{seg}.seg")),
                Ev::FsyncDir("segments".into()),
            ]
        );

        // Pack.
        let mut p = vol.open_pack().expect("pack");
        p.append_node(&[1; 32], &node_extent(mode, &[1; 32], 80))
            .expect("node");
        let pack = p.id();
        p.seal().expect("seal");
        assert_eq!(
            fs.take(),
            vec![
                Ev::Fdatasync,
                ren(&format!("{pack}.open"), &format!("{pack}")),
                Ev::FsyncDir("map".into()),
            ]
        );

        // Commit.
        vol.put(CommitSeq(0), b"genesis").expect("put");
        assert_eq!(
            fs.take(),
            vec![
                Ev::Fdatasync,
                ren("0000000000000000.tmp", "0000000000000000"),
                Ev::FsyncDir("commits".into()),
            ]
        );

        // Evidence.
        vol.put_evidence(1, b"{}\n").expect("evidence");
        assert_eq!(
            fs.take(),
            vec![
                Ev::Fdatasync,
                ren("0000000000000001.tmp", "0000000000000001"),
                Ev::FsyncDir("gc".into()),
            ]
        );

        // Checkpoint: the first write creates the file (data sync, then the
        // directory), later ones only sync the slot they wrote.
        let slot = CheckpointSlot {
            slot_seq: 0,
            volume_uuid: VolumeUuid(UUID),
            last_commit_seq: 0,
            id_high: 3,
            lane_tail_ordinals: vec![0],
            stats: vec![],
        };
        vol.store(&slot).expect("checkpoint");
        assert_eq!(fs.take(), vec![Ev::Fdatasync, Ev::FsyncDir("state".into())]);
        vol.store(&slot).expect("checkpoint");
        assert_eq!(fs.take(), vec![Ev::Fdatasync]);

        // Deletes: unlink, then the directory.
        SegmentStore::delete(&vol, seg).expect("delete");
        assert_eq!(
            fs.take(),
            vec![
                Ev::Unlink(format!("{seg}.seg")),
                Ev::FsyncDir("segments".into())
            ]
        );
        PackStore::delete_pack(&vol, pack).expect("delete pack");
        assert_eq!(
            fs.take(),
            vec![Ev::Unlink(format!("{pack}")), Ev::FsyncDir("map".into())]
        );
        vol.put(CommitSeq(1), b"one").expect("put");
        fs.take();
        CommitStore::delete(&vol, CommitSeq(0), CommitSeq(1)).expect("delete commit");
        assert_eq!(
            fs.take(),
            vec![
                Ev::Unlink("0000000000000000".into()),
                Ev::FsyncDir("commits".into())
            ]
        );
    }
}

#[test]
fn a_refused_or_failed_create_leaves_no_temp_file_and_no_rename() {
    let fs = RecordingFs::new();
    let mut opts = test_opts();
    opts.fs = fs.clone();
    let dir = tempdir();
    let vol = create(dir.path(), CryptoMode::Plain, 16 * 1024, opts);
    vol.put(CommitSeq(5), b"five").expect("put");
    fs.take();
    let err = vol.put(CommitSeq(5), b"other").expect_err("exists");
    assert_eq!(err.kind(), lunix_iface::ErrorKind::AlreadyExists);
    assert_eq!(vol.get(CommitSeq(5)).expect("get"), b"five");
    assert!(
        lunix_iface::store::VolumeDir::leftovers(&vol)
            .expect("leftovers")
            .is_empty(),
        "the refused write cleaned up its temp file"
    );
}

#[test]
fn durable_order_holds_for_the_whole_volume_creation() {
    let fs = RecordingFs::new();
    let mut opts = test_opts();
    opts.fs = fs.clone();
    let dir = tempdir();
    let _vol = create(dir.path(), CryptoMode::Integrity, 16 * 1024, opts);
    let events = fs.take();
    assert_durable_order(&events);
    // `volume` is renamed last: a crash earlier leaves a directory that does
    // not open as a volume.
    let last_rename = events
        .iter()
        .rev()
        .find_map(|e| match e {
            Ev::Rename { to, .. } => Some(to.clone()),
            _ => None,
        })
        .expect("a rename");
    assert_eq!(last_rename, "volume");
}
