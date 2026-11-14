//! Lock, checkpoint, pins, leftovers, the orphan sweep and the id rule.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use lunix_format::gcfile::Condemned;
use lunix_format::local::{CheckpointSlot, LocalPins, LocalPinsFile, Pin, RollbackFloor};
use lunix_format::pins::{Consistency, HoldPin, ReaderPin, SnapshotPin};
use lunix_format::{paths, CommitSeq, CryptoMode, FamilyId, ObjectKind, TimeMs, VolumeUuid};
use lunix_iface::crypto::MetaCodec;
use lunix_iface::store::{
    CheckpointStore, CommitStore, GcStateStore, PackStore, PinKind, PinSource, SegmentStore,
    SweepRequest, VolumeDir,
};
use lunix_iface::ErrorKind;

use super::codec::TestCodec;
use super::crash::spawn_child;
use super::{
    cid, create, data_extent, json, node_extent, open, tempdir, test_opts, try_open, RecordingFs,
    UUID,
};
use crate::{read_volume_file, tails_usable, LocalVolume, StoreOptions};

const MODE: CryptoMode = CryptoMode::Plain;

fn plain(dir: &std::path::Path) -> LocalVolume {
    create(dir, MODE, 16 * 1024, test_opts())
}

fn seal_one(vol: &LocalVolume, n: u32) -> lunix_format::SegId {
    let mut w = vol.open_segment().expect("open");
    w.append_extent(&cid(n), None, &data_extent(MODE, 16 * 1024, &cid(n), 100))
        .expect("append");
    let id = w.id();
    w.seal(None).expect("seal");
    id
}

fn slot(seq: u64, last: u64) -> CheckpointSlot {
    CheckpointSlot {
        slot_seq: seq,
        volume_uuid: VolumeUuid(UUID),
        last_commit_seq: last,
        id_high: 10 * seq,
        lane_tail_ordinals: vec![4, 9],
        stats: vec![seq],
    }
}

// ---------------------------------------------------------------- the lock

#[test]
fn a_live_holder_blocks_a_second_opener_with_a_typed_error() {
    let dir = tempdir();
    let vol = plain(dir.path());
    let err = try_open(dir.path(), MODE, test_opts()).expect_err("locked");
    assert_eq!(err.kind(), ErrorKind::Busy);
    // The lock lives as long as any writer does, not just the volume handle.
    let writer = vol.open_segment().expect("writer");
    drop(vol);
    assert_eq!(
        try_open(dir.path(), MODE, test_opts())
            .expect_err("writer holds")
            .kind(),
        ErrorKind::Busy
    );
    drop(writer);
    open(dir.path(), MODE, test_opts()).expect("free again");
}

#[test]
fn a_lock_left_by_a_killed_process_is_taken_over() {
    let dir = tempdir();
    drop(plain(dir.path()));
    let mut child = spawn_child(dir.path(), "hold", None);
    let ready = dir.path().join("ready");
    let deadline = Instant::now() + Duration::from_secs(60);
    while !ready.exists() {
        assert!(Instant::now() < deadline, "the child never took the lock");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        try_open(dir.path(), MODE, test_opts())
            .expect_err("held by the child")
            .kind(),
        ErrorKind::Busy
    );
    child.kill().expect("kill -9");
    child.wait().expect("reap");
    assert!(
        dir.path().join("LOCK").exists(),
        "the stale LOCK file is still there"
    );
    open(dir.path(), MODE, test_opts()).expect("stale lock taken over");
}

// ------------------------------------------------------ create, open, state

#[test]
fn create_open_and_the_small_state_files_round_trip() {
    let dir = tempdir();
    let bytes = super::descriptor(MODE, 16 * 1024);
    let vol = plain(dir.path());
    assert_eq!(vol.volume_bytes().expect("volume"), bytes);
    assert_eq!(vol.uuid(), VolumeUuid(UUID));
    assert_eq!(vol.volume_json().expect("json"), json(dir.path()));
    drop(vol);
    assert_eq!(read_volume_file(dir.path()).expect("read"), bytes);
    for sub in [
        "journal",
        "segments",
        "map",
        "index",
        "cache",
        "state/commits",
        "state/gc",
        "snapshots",
        "readers",
        "holds",
        "gc",
    ] {
        assert!(dir.path().join(sub).is_dir(), "{sub}");
    }
    let vol = open(dir.path(), MODE, test_opts()).expect("open");

    let mut j = json(dir.path());
    j.retain_last = 3;
    vol.store_volume_json(&j).expect("store json");
    assert_eq!(vol.volume_json().expect("json"), j);

    assert_eq!(vol.device_record().expect("none"), None);
    let rec = lunix_format::local::DeviceRecord {
        nr_queues: 2,
        queue_depth: 32,
        dev_id: 4,
        max_io_buf_bytes: 1 << 20,
        boot_id: [1; 16],
        volume_uuid: VolumeUuid(UUID),
        dev_sectors: 99,
        flags: 3,
    };
    vol.store_device_record(&rec).expect("store");
    assert_eq!(vol.device_record().expect("rec"), Some(rec));
    vol.clear_device_record().expect("clear");
    vol.clear_device_record().expect("clear is idempotent");
    assert_eq!(vol.device_record().expect("none"), None);

    assert_eq!(vol.local_pins().expect("none"), None);
    let pins = LocalPins {
        volume_uuid: VolumeUuid(UUID),
        family_id: FamilyId([9; 16]),
        max_keyring_seq: 3,
        max_epoch: 0,
        pin: Some(Pin {
            mode: CryptoMode::Integrity,
            fingerprint: [5; 16],
        }),
    };
    vol.store_local_pins(&pins).expect("pins");
    assert_eq!(vol.local_pins().expect("pins"), Some(pins));
}

fn floor(seq: u64, h: u8) -> RollbackFloor {
    RollbackFloor {
        max_commit_seq: CommitSeq(seq),
        tip_hash: [h; 32],
    }
}

fn read_pins(dir: &std::path::Path) -> LocalPinsFile {
    let bytes = std::fs::read(dir.join("state/pins")).expect("read state/pins");
    LocalPinsFile::decode(&bytes).expect("decode state/pins")
}

#[test]
fn the_rollback_floor_is_raise_only_and_refuses_a_fork() {
    let dir = tempdir();
    let vol = plain(dir.path());
    assert_eq!(vol.rollback_floor().expect("none"), None);
    vol.raise_rollback_floor(&floor(5, 1)).expect("first");
    vol.raise_rollback_floor(&floor(5, 1))
        .expect("equal is a no-op");

    let lower = vol.raise_rollback_floor(&floor(4, 1)).expect_err("lower");
    assert_eq!(lower.kind(), ErrorKind::Refused);
    let fork = vol.raise_rollback_floor(&floor(5, 2)).expect_err("fork");
    assert_eq!(fork.kind(), ErrorKind::Refused);
    assert_eq!(vol.rollback_floor().expect("floor"), Some(floor(5, 1)));
    assert_eq!(
        read_pins(dir.path()).floor,
        Some(floor(5, 1)),
        "on disk too"
    );

    vol.raise_rollback_floor(&floor(6, 3)).expect("raise");
    drop(vol);
    let vol = open(dir.path(), MODE, test_opts()).expect("reopen");
    assert_eq!(vol.rollback_floor().expect("floor"), Some(floor(6, 3)));
}

#[test]
fn raising_the_floor_creates_the_pins_with_the_volumes_identity() {
    let dir = tempdir();
    let vol = plain(dir.path());
    assert_eq!(vol.local_pins().expect("none"), None);
    vol.raise_rollback_floor(&floor(1, 7)).expect("raise");
    let pins = vol.local_pins().expect("pins").expect("created");
    assert_eq!(pins.volume_uuid, VolumeUuid(UUID));
    assert_eq!(
        (pins.max_keyring_seq, pins.max_epoch, pins.pin),
        (0, 0, None)
    );
    assert_eq!(read_pins(dir.path()).floor, Some(floor(1, 7)));
}

#[test]
fn storing_the_pins_keeps_the_floor_and_the_floor_keeps_the_pins() {
    let dir = tempdir();
    let vol = plain(dir.path());
    let mut pins = LocalPins {
        volume_uuid: VolumeUuid(UUID),
        family_id: FamilyId([9; 16]),
        max_keyring_seq: 3,
        max_epoch: 1,
        pin: Some(Pin {
            mode: CryptoMode::Integrity,
            fingerprint: [5; 16],
        }),
    };
    vol.store_local_pins(&pins).expect("pins");
    vol.raise_rollback_floor(&floor(9, 4)).expect("raise");
    assert_eq!(vol.local_pins().expect("pins"), Some(pins.clone()));

    pins.max_keyring_seq = 4;
    vol.store_local_pins(&pins).expect("replace the pins");
    assert_eq!(vol.local_pins().expect("pins"), Some(pins));
    assert_eq!(
        vol.rollback_floor().expect("floor"),
        Some(floor(9, 4)),
        "replacing the pins never drops the floor"
    );
}

#[test]
fn an_unreadable_pins_file_is_an_error_not_an_empty_floor() {
    let dir = tempdir();
    let vol = plain(dir.path());
    vol.raise_rollback_floor(&floor(2, 2)).expect("raise");
    let path = dir.path().join("state/pins");
    let mut bytes = std::fs::read(&path).expect("read");
    let last = bytes.len() - 1;
    bytes[last] ^= 0xff;
    std::fs::write(&path, &bytes).expect("damage");
    assert!(vol.rollback_floor().is_err());
    assert!(vol.raise_rollback_floor(&floor(3, 3)).is_err());
    let pins = LocalPins {
        volume_uuid: VolumeUuid(UUID),
        family_id: FamilyId([9; 16]),
        max_keyring_seq: 0,
        max_epoch: 0,
        pin: None,
    };
    assert!(vol.store_local_pins(&pins).is_err());
}

#[test]
fn create_refuses_an_existing_volume_and_open_refuses_a_missing_one() {
    let dir = tempdir();
    drop(plain(dir.path()));
    let again = super::create_raw(
        dir.path(),
        &super::descriptor(MODE, 16 * 1024),
        &json(dir.path()),
        MODE,
        test_opts(),
    );
    assert_eq!(again.expect_err("exists").kind(), ErrorKind::AlreadyExists);
    let empty = tempdir();
    assert_eq!(
        open(empty.path(), MODE, test_opts())
            .expect_err("no volume")
            .kind(),
        ErrorKind::NotFound
    );
    assert_eq!(
        open(&empty.path().join("nope"), MODE, test_opts())
            .expect_err("no dir")
            .kind(),
        ErrorKind::NotFound
    );
    // The codec's mode must be the descriptor's.
    assert_eq!(
        open(dir.path(), CryptoMode::Encrypted, test_opts())
            .expect_err("mode")
            .kind(),
        ErrorKind::Invalid
    );
}

#[test]
fn a_volume_on_tmpfs_is_refused_unless_allowed() {
    let shm = std::path::Path::new("/dev/shm");
    let is_tmpfs = rustix::fs::statfs(shm).is_ok_and(|s| s.f_type as u64 == 0x0102_1994);
    if !is_tmpfs {
        eprintln!("SKIP: /dev/shm is not a tmpfs on this host");
        return;
    }
    let dir = tempfile::tempdir_in(shm).expect("tmpfs dir");
    let strict = StoreOptions {
        allow_tmpfs: false,
        ..test_opts()
    };
    let err = super::create_raw(
        dir.path(),
        &super::descriptor(MODE, 16 * 1024),
        &json(dir.path()),
        MODE,
        strict.clone(),
    )
    .expect_err("tmpfs");
    assert_eq!(err.kind(), ErrorKind::Refused);
    assert!(!dir.path().join("volume").exists(), "nothing was created");
    drop(create(dir.path(), MODE, 16 * 1024, test_opts()));
    assert_eq!(
        open(dir.path(), MODE, strict).expect_err("tmpfs").kind(),
        ErrorKind::Refused
    );
}

#[test]
fn a_crash_while_creating_leaves_no_volume() {
    let dir = tempdir();
    std::fs::create_dir_all(dir.path().join("segments")).expect("dirs");
    std::fs::write(dir.path().join("volume.json"), b"{}").expect("partial");
    assert_eq!(
        open(dir.path(), MODE, test_opts())
            .expect_err("no volume")
            .kind(),
        ErrorKind::NotFound
    );
    // Creating over the partial directory finishes the job.
    drop(plain(dir.path()));
    open(dir.path(), MODE, test_opts()).expect("opens");
}

// ---------------------------------------------------------------- checkpoint

#[test]
fn a_torn_or_stale_checkpoint_is_ignored_with_a_warning() {
    let dir = tempdir();
    let vol = plain(dir.path());
    vol.store(&slot(0, 1)).expect("slot 1 -> file slot 0");
    vol.store(&slot(0, 2)).expect("slot 2 -> file slot 1");
    let path = dir.path().join("state/checkpoint");
    let good = std::fs::read(&path).expect("file");
    assert_eq!(good.len(), 8192);
    assert_eq!(
        vol.load().expect("load").map(|s| s.last_commit_seq),
        Some(2)
    );

    // Tear the newest slot (file slot 1): the older one answers.
    let mut torn = good.clone();
    torn[4096 + 100] ^= 0xff;
    std::fs::write(&path, &torn).expect("write");
    assert_eq!(
        vol.load().expect("load").map(|s| s.last_commit_seq),
        Some(1)
    );
    // Both torn: no checkpoint, one warning.
    torn[100] ^= 0xff;
    std::fs::write(&path, &torn).expect("write");
    vol.take_warnings();
    assert_eq!(vol.load().expect("load"), None);
    assert_eq!(vol.take_warnings().len(), 1);
    // A wrong-length file is ignored; the next store rebuilds it.
    std::fs::write(&path, b"short").expect("write");
    assert_eq!(vol.load().expect("load"), None);
    assert!(!vol.take_warnings().is_empty());
    vol.store(&slot(0, 7)).expect("rebuild");
    assert_eq!(
        vol.load().expect("load").map(|s| s.last_commit_seq),
        Some(7)
    );
    // A slot written for another volume is stale.
    let mut other = slot(0, 8);
    other.volume_uuid = VolumeUuid([1; 16]);
    assert_eq!(
        vol.store(&other).expect_err("foreign").kind(),
        ErrorKind::Invalid
    );
}

#[test]
fn zero_leaves_no_checkpoint_and_no_warning() {
    let dir = tempdir();
    let vol = plain(dir.path());
    vol.store(&slot(0, 3)).expect("store");
    vol.store(&slot(0, 4)).expect("store");
    vol.zero().expect("zero");
    vol.take_warnings();
    assert_eq!(vol.load().expect("load"), None);
    assert!(
        vol.take_warnings().is_empty(),
        "a zeroed checkpoint is expected"
    );
    vol.store(&slot(0, 5)).expect("store after zero");
    assert_eq!(
        vol.load().expect("load").map(|s| s.last_commit_seq),
        Some(5)
    );
}

#[test]
fn lane_tails_are_used_only_for_a_checkpoint_at_or_below_the_tip() {
    let dir = tempdir();
    let vol = plain(dir.path());
    vol.put(CommitSeq(0), b"g").expect("put");
    vol.put(CommitSeq(1), b"c1").expect("put");
    vol.store(&slot(0, 1)).expect("store");
    let hint = vol.load_checkpoint_hint().expect("hint").expect("some");
    assert!(hint.tails_usable);
    assert_eq!(hint.slot.lane_tail_ordinals, vec![4, 9]);

    // Commit 2 is written, the checkpoint names it, then it is set aside
    // (`--truncate-tip`) without zeroing the checkpoint: the tails are stale.
    vol.put(CommitSeq(2), b"c2").expect("put");
    vol.store(&slot(2, 2)).expect("store");
    assert!(
        vol.load_checkpoint_hint()
            .expect("hint")
            .expect("some")
            .tails_usable
    );
    vol.set_aside(CommitSeq(2)).expect("aside");
    let hint = vol.load_checkpoint_hint().expect("hint").expect("some");
    assert!(!hint.tails_usable);
    assert_eq!(hint.slot.lane_tail_ordinals, vec![0, 0], "scan every file");
    assert_eq!(hint.slot.id_high, 20, "the rest of the hint still serves");
    assert!(!tails_usable(&hint.slot, None));
    assert!(tails_usable(&slot(1, 0), Some(CommitSeq(0))));
}

// ----------------------------------------------------------------------- pins

fn write_pin(vol: &LocalVolume, sub: &str, file: &str, kind: ObjectKind, name: &str, body: &[u8]) {
    let codec = TestCodec::new(MODE, UUID);
    let bytes = MetaCodec::seal(&*codec, kind, name, body).expect("seal");
    std::fs::write(vol.dir().join(sub).join(file), bytes).expect("write pin");
}

#[test]
fn pins_are_read_and_authenticated() {
    let dir = tempdir();
    let vol = plain(dir.path());
    assert!(vol.roots().expect("empty").is_empty());

    let mut body = Vec::new();
    SnapshotPin {
        consistency: Consistency::Crash,
        commit_seq: CommitSeq(5),
        created: TimeMs(1),
        label: "first".into(),
    }
    .encode(&mut body)
    .expect("encode");
    write_pin(
        &vol,
        "snapshots",
        "daily",
        ObjectKind::Snapshots,
        "snapshots/daily",
        &body,
    );

    let mut body = Vec::new();
    ReaderPin {
        commit_seq: CommitSeq(6),
        deadline: TimeMs(99),
    }
    .encode(&mut body);
    let reader = paths::reader("host-a", 0x1f).expect("name");
    write_pin(
        &vol,
        "readers",
        "host-a-000000000000001f",
        ObjectKind::Readers,
        &reader,
        &body,
    );

    let child = VolumeUuid([2; 16]);
    let mut body = Vec::new();
    HoldPin {
        base_seq: CommitSeq(7),
    }
    .encode(&mut body);
    write_pin(
        &vol,
        "holds",
        &child.to_string(),
        ObjectKind::Holds,
        &paths::hold(&child),
        &body,
    );
    // A crashed writer's temp file is not a pin.
    std::fs::write(dir.path().join("holds/junk.tmp"), b"half").expect("tmp");

    let roots = vol.roots().expect("roots");
    let summary: Vec<_> = roots
        .iter()
        .map(|r| {
            (
                r.kind,
                r.name.as_str(),
                r.commit_seq.0,
                r.deadline.map(|d| d.0),
            )
        })
        .collect();
    assert_eq!(
        summary,
        vec![
            (PinKind::Snapshot, "daily", 5, None),
            (PinKind::Reader, "host-a-000000000000001f", 6, Some(99)),
            (PinKind::Hold, child.to_string().as_str(), 7, None),
        ]
    );

    // An undecodable pin is an error, never a skip.
    let p = dir.path().join("snapshots/daily");
    let mut bytes = std::fs::read(&p).expect("read");
    let last = bytes.len() - 1;
    bytes[last] ^= 1;
    std::fs::write(&p, &bytes).expect("write");
    assert_eq!(vol.roots().expect_err("tampered").kind(), ErrorKind::Auth);
    // A pin moved to another name fails its binding.
    std::fs::rename(&p, dir.path().join("snapshots/other")).expect("move");
    assert!(vol.roots().is_err());
    std::fs::remove_file(dir.path().join("snapshots/other")).expect("rm");
    std::fs::write(dir.path().join("snapshots/garbage"), b"not an envelope").expect("write");
    assert_eq!(vol.roots().expect_err("garbage").kind(), ErrorKind::Corrupt);
}

// ------------------------------------------------------------------ gc state

#[test]
fn an_undecodable_condemned_list_is_absent() {
    let dir = tempdir();
    let vol = plain(dir.path());
    vol.store_condemned(&Condemned {
        body_ver: 1,
        body: b"x".to_vec(),
    })
    .expect("store");
    let p = dir.path().join("state/gc/condemned");
    let mut b = std::fs::read(&p).expect("read");
    b[20] ^= 1;
    std::fs::write(&p, b).expect("write");
    assert_eq!(vol.load_condemned().expect("load"), None);
}

#[test]
fn evidence_objects_are_sealed_whole_and_named_by_pass() {
    for mode in [CryptoMode::Plain, CryptoMode::Encrypted] {
        let dir = tempdir();
        let vol = create(dir.path(), mode, 16 * 1024, test_opts());
        vol.put_evidence(1, b"{\"pass\":1}\n").expect("put");
        assert_eq!(
            vol.put_evidence(0, b"x").expect_err("zero").kind(),
            ErrorKind::Invalid
        );
        let stored = std::fs::read(dir.path().join("gc/0000000000000001")).expect("object");
        let codec = TestCodec::new(mode, UUID);
        let body = MetaCodec::open(&*codec, ObjectKind::Gc, "gc/0000000000000001", &stored)
            .expect("opens");
        assert_eq!(
            lunix_format::gcfile::GcPassBody::decode(&body)
                .expect("body")
                .bytes,
            b"{\"pass\":1}\n"
        );
        // Moved to another pass number, it no longer authenticates.
        assert!(MetaCodec::open(&*codec, ObjectKind::Gc, "gc/0000000000000002", &stored).is_err());
        assert_eq!(vol.evidence_passes().expect("passes"), vec![1]);
    }
}

// ------------------------------------------------------- leftovers and sweep

#[test]
fn leftovers_are_listed_and_removed_but_a_live_writer_keeps_its_file() {
    let dir = tempdir();
    let vol = plain(dir.path());
    let mut w = vol.open_segment().expect("writer");
    let id = cid(1);
    w.append_extent(&id, None, &data_extent(MODE, 16 * 1024, &id, 100))
        .expect("append");
    std::fs::write(
        dir.path().join("state/commits/00000000000000aa.tmp"),
        b"half",
    )
    .expect("tmp");
    std::fs::write(dir.path().join("cache/slab.tmp"), b"cache is not ours").expect("tmp");
    // What a crashed process left, under an id this process does not write.
    std::fs::write(dir.path().join("segments/00000000000000ff.open"), b"x").expect("open");
    let names: Vec<_> = vol
        .leftovers()
        .expect("list")
        .into_iter()
        .map(|l| l.name)
        .collect();
    assert_eq!(
        names,
        vec![
            "segments/00000000000000ff.open".to_owned(),
            "state/commits/00000000000000aa.tmp".to_owned()
        ],
        "the live writer's own file is not a leftover"
    );
    assert_eq!(vol.remove_leftovers().expect("remove"), 2);
    assert!(vol.leftovers().expect("list").is_empty());
    assert!(dir.path().join("cache/slab.tmp").exists());
    w.seal(None)
        .expect("the writer's file survived the removal");
}

#[test]
fn the_sweep_deletes_unnamed_sealed_files_above_the_mark_before_anything_else() {
    let dir = tempdir();
    let vol = plain(dir.path());
    let below = seal_one(&vol, 1); // kept: at or below the mark, left for GC
    let named = seal_one(&vol, 2);
    let pack_named = {
        let mut p = vol.open_pack().expect("pack");
        p.append_node(&[1; 32], &node_extent(MODE, &[1; 32], 70))
            .expect("node");
        let id = p.id();
        p.seal().expect("seal");
        id
    };
    // Orphans: sealed after the last commit that was made, as by a crash
    // between a seal and the commit that would have named it.
    let above = seal_one(&vol, 3);
    let pack_orphan = {
        let mut p = vol.open_pack().expect("pack");
        p.append_node(&[2; 32], &node_extent(MODE, &[2; 32], 70))
            .expect("node");
        let id = p.id();
        p.seal().expect("seal");
        id
    };
    drop(vol);

    let vol = open(dir.path(), MODE, test_opts()).expect("reopen after the crash");
    let segs: HashSet<_> = [named].into_iter().collect();
    let packs: HashSet<_> = [pack_named].into_iter().collect();
    let report = vol
        .sweep_orphans(&SweepRequest {
            high_water_mark: pack_named.raw(),
            named_segments: &segs,
            named_packs: &packs,
        })
        .expect("sweep");
    assert_eq!(report.highest_id_before, pack_orphan.raw());
    assert_eq!(report.deleted_segments, vec![above]);
    assert_eq!(report.deleted_packs, vec![pack_orphan]);
    assert!(report.kept_unreferenced.contains(&below));
    assert!(!report.kept_unreferenced.contains(&named));
    let left: Vec<_> = SegmentStore::list(&vol)
        .expect("list")
        .iter()
        .map(|i| i.seg)
        .collect();
    assert_eq!(left, vec![below, named]);
    assert_eq!(vol.list_packs_ids(), vec![pack_named]);
    // The ids of what was deleted are not handed out again.
    assert!(vol.ids().allocate().expect("id").raw() > pack_orphan.raw());
}

impl LocalVolume {
    fn list_packs_ids(&self) -> Vec<lunix_format::PackId> {
        lunix_iface::store::PackRead::list_packs(self)
            .expect("packs")
            .iter()
            .map(|p| p.pack)
            .collect()
    }
}

#[test]
fn ids_start_above_every_file_on_disk_even_unsealed_or_foreign_ones() {
    let dir = tempdir();
    drop(plain(dir.path()));
    // What a crash can leave: an unsealed segment, a stray pack temp, an
    // index run of M6's, none of them sealed segments.
    std::fs::write(dir.path().join("segments/0000000000000040.open"), b"x").expect("open");
    std::fs::write(dir.path().join("map/0000000000000050.tmp"), b"x").expect("tmp");
    std::fs::write(dir.path().join("index/0000000000000060"), b"x").expect("run");
    let vol = open(dir.path(), MODE, test_opts()).expect("open");
    assert_eq!(vol.ids().high_water(), 0x60);
    assert_eq!(vol.ids().allocate().expect("id").raw(), 0x61);
    // Callers raise the floor (checkpoint id_high, ids the chain names).
    vol.ids().observe(0x200);
    vol.ids().observe(0x10);
    assert_eq!(vol.ids().allocate().expect("id").raw(), 0x201);
}

#[test]
fn the_id_counter_never_repeats_under_threads_and_reports_exhaustion() {
    let dir = tempdir();
    let vol = plain(dir.path());
    let ids = vol.ids();
    let all: Vec<u64> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let ids = ids.clone();
                s.spawn(move || {
                    (0..200)
                        .map(|_| ids.allocate().expect("id").raw())
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().expect("join"))
            .collect()
    });
    let set: HashSet<_> = all.iter().collect();
    assert_eq!(set.len(), 1600);
    ids.observe(lunix_format::ObjId::MAX_COUNTER);
    assert_eq!(
        ids.allocate().expect_err("exhausted").kind(),
        ErrorKind::Refused
    );
}

// -------------------------------------------------------- reading and caching

#[test]
fn many_segments_read_through_a_small_descriptor_cache() {
    let dir = tempdir();
    let mut opts = test_opts();
    opts.max_open_files = 2;
    let vol = create(dir.path(), MODE, 16 * 1024, opts);
    let mut hints = Vec::new();
    for n in 0..6 {
        let mut w = vol.open_segment().expect("open");
        let e = data_extent(MODE, 16 * 1024, &cid(n), 90);
        hints.push((w.append_extent(&cid(n), None, &e).expect("append"), e));
        w.seal(None).expect("seal");
    }
    for _ in 0..2 {
        for (h, e) in &hints {
            let mut out = Vec::new();
            vol.read_extent(h, &mut out).expect("read");
            assert_eq!(&out, e);
        }
    }
    // A located descriptor stays valid after the segment is deleted.
    let (h, e) = &hints[0];
    let loc = vol.locate(h).expect("locate");
    SegmentStore::delete(&vol, h.seg).expect("delete");
    let mut buf = vec![0u8; loc.len as usize];
    crate::fsops::pread_exact(&loc.fd, &mut buf, loc.offset).expect("pread");
    assert_eq!(&buf, e);
    let mut out = Vec::new();
    assert_eq!(
        vol.read_extent(h, &mut out).expect_err("gone").kind(),
        ErrorKind::NotFound
    );
}

#[test]
fn concurrent_writers_get_distinct_segments() {
    let dir = tempdir();
    let vol = plain(dir.path());
    std::thread::scope(|s| {
        for t in 0..4u32 {
            let vol = vol.clone();
            s.spawn(move || {
                for i in 0..5 {
                    seal_one(&vol, t * 10 + i);
                }
            });
        }
    });
    let list = SegmentStore::list(&vol).expect("list");
    assert_eq!(list.len(), 20);
    assert!(list.windows(2).all(|w| w[0].seg < w[1].seg));
}

#[test]
fn a_full_disk_error_maps_to_no_space() {
    let e = crate::fsops::io_err(std::io::Error::from_raw_os_error(28));
    assert_eq!(e.kind(), ErrorKind::NoSpace);
    let e = crate::fsops::io_err(std::io::Error::from_raw_os_error(5));
    assert_eq!(e.kind(), ErrorKind::Io);
}

#[test]
fn recording_fs_is_only_a_wrapper() {
    // The recorder must not change behaviour: a volume built on it behaves.
    let fs = RecordingFs::new();
    let mut opts = test_opts();
    opts.fs = fs.clone();
    let dir = tempdir();
    let vol = create(dir.path(), MODE, 16 * 1024, opts);
    seal_one(&vol, 1);
    assert!(!fs.take().is_empty());
}

// --------------------------------------------------- review follow-ups

#[test]
fn replace_writes_from_two_threads_never_tear_the_file() {
    let dir = tempdir();
    let vol = plain(dir.path());
    let rec = |dev_id| lunix_format::local::DeviceRecord {
        nr_queues: 1,
        queue_depth: 8,
        dev_id,
        max_io_buf_bytes: 1 << 20,
        boot_id: [1; 16],
        volume_uuid: VolumeUuid(UUID),
        dev_sectors: 8,
        flags: 0,
    };
    std::thread::scope(|s| {
        for t in 0..2u32 {
            let vol = vol.clone();
            s.spawn(move || {
                for i in 0..200 {
                    vol.store_device_record(&rec(t * 1000 + i)).expect("store");
                    vol.store_condemned(&Condemned {
                        body_ver: 1,
                        body: vec![t as u8; 64],
                    })
                    .expect("condemned");
                    vol.store(&slot(0, 0)).expect("checkpoint");
                    let got = vol.device_record().expect("never torn").expect("present");
                    assert!(got.dev_id % 1000 < 200);
                }
            });
        }
    });
    assert!(vol.load_condemned().expect("load").is_some());
    assert!(vol.load().expect("load").is_some());
    assert!(vol.leftovers().expect("left").is_empty());
}

#[test]
fn a_stray_name_does_not_poison_the_id_scan() {
    let dir = tempdir();
    drop(plain(dir.path()));
    std::fs::write(dir.path().join("segments/0000000000000000.seg"), b"x").expect("zero id");
    std::fs::write(dir.path().join("segments/zzzz.seg"), b"x").expect("junk");
    std::fs::write(dir.path().join("index/manifest"), b"x").expect("manifest");
    std::fs::write(dir.path().join("map/0000000000000007"), b"x").expect("pack");
    let vol = open(dir.path(), MODE, test_opts()).expect("open");
    assert_eq!(vol.ids().high_water(), 7);
    let warnings = vol.take_warnings();
    assert_eq!(warnings.len(), 2, "{warnings:?}");
    // A bad observed value is reported as such, not as exhaustion.
    vol.ids().observe(0xff << 40);
    let e = vol.ids().allocate().expect_err("invalid");
    assert!(e.to_string().contains("not a valid id"), "{e}");
}

#[test]
fn open_observes_the_checkpoint_id_high() {
    let dir = tempdir();
    let vol = plain(dir.path());
    vol.store(&slot(50, 0)).expect("store"); // id_high = 500
    drop(vol);
    let vol = open(dir.path(), MODE, test_opts()).expect("open");
    assert_eq!(vol.ids().high_water(), 500);
}

#[test]
fn the_sweep_refuses_a_mark_below_a_named_id() {
    let dir = tempdir();
    let vol = plain(dir.path());
    let s = seal_one(&vol, 1);
    let named: HashSet<_> = [s].into_iter().collect();
    let e = vol
        .sweep_orphans(&SweepRequest {
            high_water_mark: s.raw() - 1,
            named_segments: &named,
            named_packs: &HashSet::new(),
        })
        .expect_err("inconsistent");
    assert_eq!(e.kind(), ErrorKind::Invalid);
    assert_eq!(SegmentStore::list(&vol).expect("list").len(), 1);
}

#[test]
fn a_refused_create_on_tmpfs_creates_nothing() {
    let shm = std::path::Path::new("/dev/shm");
    if !rustix::fs::statfs(shm).is_ok_and(|s| s.f_type as u64 == 0x0102_1994) {
        eprintln!("SKIP: /dev/shm is not a tmpfs on this host");
        return;
    }
    let base = tempfile::tempdir_in(shm).expect("tmpfs dir");
    let target = base.path().join("a/b");
    let strict = StoreOptions {
        allow_tmpfs: false,
        ..test_opts()
    };
    let err = super::create_raw(
        &target,
        &super::descriptor(MODE, 16 * 1024),
        &json(&target),
        MODE,
        strict,
    )
    .expect_err("tmpfs");
    assert_eq!(err.kind(), ErrorKind::Refused);
    assert!(!base.path().join("a").exists(), "nothing was created");
}

#[test]
fn a_cached_container_of_the_wrong_kind_is_not_returned() {
    let dir = tempdir();
    let vol = plain(dir.path());
    let seg = seal_one(&vol, 1);
    let hint = lunix_format::Hint {
        seg,
        off: 0,
        clen: 100,
    };
    let mut out = Vec::new();
    vol.read_extent(&hint, &mut out).expect("warms the cache");
    let loc = lunix_format::node::NodeLoc {
        pack: seg,
        off: 0,
        len: 168,
    };
    let e = lunix_iface::store::PackRead::read_node(&vol, loc, &mut out).expect_err("wrong kind");
    assert!(matches!(e.kind(), ErrorKind::Corrupt | ErrorKind::NotFound));
}

#[test]
fn warnings_are_capped_and_counted() {
    let dir = tempdir();
    let vol = plain(dir.path());
    std::fs::write(dir.path().join("state/checkpoint"), b"short").expect("junk");
    for _ in 0..200 {
        assert_eq!(vol.load().expect("load"), None);
    }
    let w = vol.take_warnings();
    assert_eq!(w.len(), 65, "64 kept plus the dropped-count line");
    assert!(w[0].contains("136 earlier warnings"), "{}", w[0]);
    assert!(vol.take_warnings().is_empty());
}

#[test]
fn oversized_state_files_are_refused_before_they_are_read() {
    let dir = tempdir();
    let vol = plain(dir.path());
    let big = vec![b' '; (2 << 20) + 1];
    std::fs::write(dir.path().join("volume.json"), &big).expect("write");
    assert_eq!(
        vol.volume_json().expect_err("big").kind(),
        ErrorKind::Corrupt
    );
    std::fs::write(dir.path().join("state/device"), vec![0u8; 5000]).expect("write");
    assert_eq!(
        vol.device_record().expect_err("big").kind(),
        ErrorKind::Corrupt
    );
    std::fs::write(dir.path().join("state/checkpoint"), vec![0u8; 100_000]).expect("write");
    assert_eq!(vol.load().expect("hint"), None);
    std::fs::write(dir.path().join("volume"), big).expect("write");
    assert_eq!(
        crate::read_volume_file(dir.path()).expect_err("big").kind(),
        ErrorKind::Corrupt
    );
}

#[test]
fn the_lock_file_names_the_holder() {
    let dir = tempdir();
    let _vol = plain(dir.path());
    assert_eq!(crate::lock_holder_pid(dir.path()), Some(std::process::id()));
    assert_eq!(crate::lock_holder_pid(&dir.path().join("nope")), None);
}

// ---------------------------------------------------------------------------
// reset_rollback_floor

fn reset_opts() -> StoreOptions {
    StoreOptions {
        allow_floor_reset: true,
        ..test_opts()
    }
}

fn commit_hash(bytes: &[u8]) -> [u8; 32] {
    *blake3::hash(bytes).as_bytes()
}

/// A volume with commits 0..=3 (arbitrary bytes: the store only hashes them).
fn with_commits(dir: &std::path::Path) -> (LocalVolume, Vec<[u8; 32]>) {
    let vol = create(dir, MODE, 16 * 1024, reset_opts());
    let mut hashes = Vec::new();
    for s in 0..=3u64 {
        let bytes = format!("commit {s}").into_bytes();
        vol.put(CommitSeq(s), &bytes).expect("put");
        hashes.push(commit_hash(&bytes));
    }
    (vol, hashes)
}

fn at(seq: u64, h: [u8; 32]) -> RollbackFloor {
    RollbackFloor {
        max_commit_seq: CommitSeq(seq),
        tip_hash: h,
    }
}

#[test]
fn a_floor_reset_is_refused_without_the_option() {
    let dir = tempdir();
    let vol = plain(dir.path());
    vol.put(CommitSeq(1), b"x").expect("put");
    vol.raise_rollback_floor(&at(1, commit_hash(b"x")))
        .expect("raise");
    for why in [
        lunix_iface::store::FloorReset::TruncateTip,
        lunix_iface::store::FloorReset::OperatorReset,
        lunix_iface::store::FloorReset::RepairPins,
    ] {
        let e = vol
            .reset_rollback_floor(&at(1, commit_hash(b"x")), why)
            .expect_err("refused");
        assert_eq!(e.kind(), ErrorKind::Refused);
    }
    assert!(!dir.path().join("state/floor-resets.log").exists());
}

#[test]
fn a_floor_reset_lowers_audits_and_verifies_the_commit() {
    use lunix_iface::store::FloorReset;
    let dir = tempdir();
    let (vol, h) = with_commits(dir.path());
    vol.raise_rollback_floor(&at(3, h[3])).expect("raise");

    // The commit must exist and hash to the named value.
    let e = vol
        .reset_rollback_floor(&at(2, [9; 32]), FloorReset::TruncateTip)
        .expect_err("wrong hash");
    assert_eq!(e.kind(), ErrorKind::Refused);
    let e = vol
        .reset_rollback_floor(&at(7, h[2]), FloorReset::OperatorReset)
        .expect_err("no such commit");
    assert_eq!(e.kind(), ErrorKind::Refused);
    assert_eq!(vol.rollback_floor().expect("floor"), Some(at(3, h[3])));
    assert!(
        !dir.path().join("state/floor-resets.log").exists(),
        "a refusal leaves no record"
    );

    // TruncateTip must go down.
    let e = vol
        .reset_rollback_floor(&at(3, h[3]), FloorReset::TruncateTip)
        .expect_err("not lower");
    assert_eq!(e.kind(), ErrorKind::Refused);

    let old = vol
        .reset_rollback_floor(&at(2, h[2]), FloorReset::TruncateTip)
        .expect("lower");
    assert_eq!(old, Some(at(3, h[3])));
    assert_eq!(vol.rollback_floor().expect("floor"), Some(at(2, h[2])));
    assert_eq!(read_pins(dir.path()).floor, Some(at(2, h[2])));

    // The operator reset may also raise, or set what is there.
    vol.reset_rollback_floor(&at(3, h[3]), FloorReset::OperatorReset)
        .expect("raise");
    vol.reset_rollback_floor(&at(3, h[3]), FloorReset::OperatorReset)
        .expect("same");
    let log = std::fs::read_to_string(dir.path().join("state/floor-resets.log")).expect("audit");
    let lines: Vec<&str> = log.lines().collect();
    assert_eq!(lines.len(), 3, "{log}");
    assert!(
        lines[0].contains("truncate_tip")
            && lines[0].contains("old=3:")
            && lines[0].contains("new=2:")
    );
    assert!(lines[1].contains("operator_reset"));
    drop(vol);
    let vol = open(dir.path(), MODE, reset_opts()).expect("reopen");
    assert_eq!(
        vol.rollback_floor().expect("floor"),
        Some(at(3, h[3])),
        "durable"
    );
}

#[test]
fn a_rejected_pins_file_is_replaced_only_by_the_repair_reason() {
    use lunix_iface::store::FloorReset;
    let dir = tempdir();
    let (vol, h) = with_commits(dir.path());
    vol.raise_rollback_floor(&at(3, h[3])).expect("raise");
    // A readable file is not repaired.
    let e = vol
        .reset_rollback_floor(&at(3, h[3]), FloorReset::RepairPins)
        .expect_err("not rejected");
    assert_eq!(e.kind(), ErrorKind::Refused);
    let path = dir.path().join("state/pins");
    let mut bytes = std::fs::read(&path).expect("read");
    let last = bytes.len() - 1;
    bytes[last] ^= 0xff;
    std::fs::write(&path, &bytes).expect("damage");
    for why in [FloorReset::TruncateTip, FloorReset::OperatorReset] {
        let e = vol
            .reset_rollback_floor(&at(2, h[2]), why)
            .expect_err("rejected");
        assert_eq!(e.kind(), ErrorKind::Corrupt, "never replaced silently");
    }
    assert_eq!(std::fs::read(&path).expect("read"), bytes, "untouched");
    let old = vol
        .reset_rollback_floor(&at(3, h[3]), FloorReset::RepairPins)
        .expect("repair");
    assert_eq!(old, None);
    let f = read_pins(dir.path());
    assert_eq!(f.floor, Some(at(3, h[3])));
    assert_eq!(
        (f.pins.max_keyring_seq, f.pins.max_epoch, f.pins.pin),
        (0, 0, None)
    );
    let log = std::fs::read_to_string(dir.path().join("state/floor-resets.log")).expect("audit");
    assert!(log.contains("repair_pins"), "{log}");
}
