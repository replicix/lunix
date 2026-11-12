//! Crash sweep: for every durable step, a child process (this test binary,
//! run again) is killed at the step's crash point; the parent reopens the
//! directory and checks the state is exactly the old one or the new one.
//!
//! The child is the test `child_process`, which does nothing unless the
//! environment names a directory and an operation.

#[cfg(feature = "crashpoints")]
use std::collections::BTreeSet;
#[cfg(feature = "crashpoints")]
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};

use lunix_format::gcfile::Condemned;
use lunix_format::local::{CheckpointSlot, DeviceRecord};
use lunix_format::{CommitSeq, CryptoMode, VolumeUuid};
use lunix_iface::store::{
    CheckpointStore, CommitStore, GcStateStore, PackRead, PackStore, PinSource, SegmentStore,
    SweepRequest, VolumeDir,
};
#[cfg(feature = "crashpoints")]
use lunix_iface::ErrorKind;

use super::{cid, create, data_extent, node_extent, open, tempdir, test_opts, UUID};
use crate::LocalVolume;

const CHILD_TEST: &str = "tests::crash::child_process";
const MODE: CryptoMode = CryptoMode::Plain;
const CHUNK: u32 = 16 * 1024;

const SEG: [&str; 4] = [
    "before_fdatasync",
    "after_fdatasync",
    "after_rename",
    "after_dirsync",
];
const DEL: [&str; 3] = ["before_unlink", "after_unlink", "after_dirsync"];

/// `(operation, crash points it passes)`: the sweep's table.
fn table() -> Vec<(&'static str, Vec<String>)> {
    let family = |p: &str| SEG.iter().map(|s| format!("{p}.{s}")).collect::<Vec<_>>();
    let del = |p: &str| DEL.iter().map(|s| format!("{p}.{s}")).collect::<Vec<_>>();
    vec![
        ("segment", family("store.segment")),
        ("pack", family("store.pack")),
        ("commit", family("store.commit")),
        ("evidence", family("store.evidence")),
        ("state", family("store.state")),
        ("condemned", family("store.state")),
        (
            "checkpoint",
            ["before_write", "before_fdatasync", "after_fdatasync"]
                .iter()
                .map(|s| format!("store.checkpoint.{s}"))
                .collect(),
        ),
        (
            "checkpoint_first",
            [
                "before_write",
                "before_fdatasync",
                "after_fdatasync",
                "after_dirsync",
            ]
            .iter()
            .map(|s| format!("store.checkpoint.{s}"))
            .collect(),
        ),
        ("delete_segment", del("store.delete")),
        ("delete_pack", del("store.delete")),
        ("delete_commit", del("store.delete")),
        ("leftovers", del("store.leftover")),
        ("sweep", del("store.delete")),
        (
            "set_aside",
            ["before_rename", "after_rename", "after_dirsync"]
                .iter()
                .map(|s| format!("store.set_aside.{s}"))
                .collect(),
        ),
    ]
}

fn slot(seq: u64, last_commit: u64) -> CheckpointSlot {
    CheckpointSlot {
        slot_seq: seq,
        volume_uuid: VolumeUuid(UUID),
        last_commit_seq: last_commit,
        id_high: 10 * seq,
        lane_tail_ordinals: vec![1, 2],
        stats: vec![seq],
    }
}

fn device(dev_id: u32) -> DeviceRecord {
    DeviceRecord {
        nr_queues: 2,
        queue_depth: 64,
        dev_id,
        max_io_buf_bytes: 1 << 20,
        boot_id: [3; 16],
        volume_uuid: VolumeUuid(UUID),
        dev_sectors: 2048,
        flags: 0,
    }
}

fn condemned(body: &[u8]) -> Condemned {
    Condemned {
        body_ver: 1,
        body: body.to_vec(),
    }
}

fn seal_segment(vol: &LocalVolume, ids: &[u32]) -> lunix_format::SegId {
    let mut w = vol.open_segment().expect("open");
    for &i in ids {
        w.append_extent(&cid(i), None, &data_extent(MODE, CHUNK, &cid(i), 120))
            .expect("append");
    }
    let id = w.id();
    w.seal(None).expect("seal");
    id
}

fn seal_pack(vol: &LocalVolume) -> lunix_format::PackId {
    let mut p = vol.open_pack().expect("pack");
    for i in 0..2u32 {
        let h = cid(i).0;
        p.append_node(&h, &node_extent(MODE, &h, 90)).expect("node");
    }
    let id = p.id();
    p.seal().expect("seal");
    id
}

/// State the child's operation starts from.
fn setup(op: &str, vol: &LocalVolume) {
    match op {
        "segment" | "pack" => {
            seal_segment(vol, &[100]);
        }
        "commit" => vol.put(CommitSeq(0), b"zero").expect("put"),
        "evidence" => vol.put_evidence(1, b"{}\n").expect("evidence"),
        "state" => vol.store_device_record(&device(1)).expect("device"),
        "condemned" => vol.store_condemned(&condemned(b"old")).expect("condemned"),
        "checkpoint" => vol.store(&slot(1, 0)).expect("checkpoint"),
        "delete_segment" => {
            seal_segment(vol, &[1]);
            seal_segment(vol, &[2]);
        }
        "delete_pack" => {
            seal_pack(vol);
        }
        "delete_commit" | "set_aside" => {
            for n in 0..3 {
                vol.put(CommitSeq(n), format!("c{n}").as_bytes())
                    .expect("put");
            }
        }
        "leftovers" => {
            for rel in [
                "segments/a.tmp",
                "state/b.tmp",
                "segments/0000000000000009.open",
            ] {
                std::fs::write(vol.dir().join(rel), b"junk").expect("junk");
            }
        }
        "sweep" => {
            seal_segment(vol, &[1]);
            seal_segment(vol, &[2]);
            std::fs::write(vol.dir().join("map/x.tmp"), b"junk").expect("junk");
        }
        _ => {}
    }
}

/// The operation the child runs (and dies in).
fn run_op(op: &str, vol: &LocalVolume, dir: &Path) {
    match op {
        "segment" => {
            seal_segment(vol, &[1, 2]);
        }
        "pack" => {
            seal_pack(vol);
        }
        "commit" => vol.put(CommitSeq(1), b"one").expect("put"),
        "evidence" => vol.put_evidence(2, b"{\"p\":2}\n").expect("evidence"),
        "state" => vol.store_device_record(&device(2)).expect("device"),
        "condemned" => vol.store_condemned(&condemned(b"new")).expect("condemned"),
        "checkpoint" => vol.store(&slot(2, 1)).expect("checkpoint"),
        "checkpoint_first" => vol.store(&slot(1, 1)).expect("checkpoint"),
        "delete_segment" => {
            let first = SegmentStore::list(vol).expect("list")[0].seg;
            SegmentStore::delete(vol, first).expect("delete");
        }
        "delete_pack" => {
            let first = vol.list_packs().expect("list")[0].pack;
            PackStore::delete_pack(vol, first).expect("delete");
        }
        "delete_commit" => {
            CommitStore::delete(vol, CommitSeq(0), CommitSeq(2)).expect("delete");
        }
        "set_aside" => vol.set_aside(CommitSeq(2)).expect("aside"),
        "leftovers" => {
            vol.remove_leftovers().expect("remove");
        }
        "sweep" => {
            let segs = SegmentStore::list(vol).expect("list");
            let named = [segs[0].seg].into_iter().collect();
            vol.sweep_orphans(&SweepRequest {
                high_water_mark: segs[0].seg.raw(),
                named_segments: &named,
                named_packs: &Default::default(),
            })
            .expect("sweep");
        }
        "hold" => {
            std::fs::write(dir.join("ready"), b"1").expect("ready");
            std::thread::sleep(std::time::Duration::from_secs(120));
        }
        other => panic!("unknown child operation {other}"),
    }
}

/// The child half: does nothing in a normal test run.
#[test]
fn child_process() {
    let Some(dir) = std::env::var_os("LUNIX_STORE_CHILD_DIR") else {
        return;
    };
    let op = std::env::var("LUNIX_STORE_CHILD_OP").expect("operation");
    let dir = std::path::PathBuf::from(dir);
    let vol = open(&dir, MODE, test_opts()).expect("child opens the volume");
    run_op(&op, &vol, &dir);
}

pub(crate) fn spawn_child(dir: &Path, op: &str, crash_at: Option<&str>) -> Child {
    let mut cmd = Command::new(std::env::current_exe().expect("exe"));
    cmd.args(["--exact", CHILD_TEST, "--nocapture", "--test-threads=1"])
        .env("LUNIX_STORE_CHILD_DIR", dir)
        .env("LUNIX_STORE_CHILD_OP", op)
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    if let Some(p) = crash_at {
        cmd.env("LUNIX_CRASH_AT", p);
    }
    let _g = super::write_guard();
    cmd.spawn().expect("spawn the child")
}

/// Everything on disk is whole: each sealed segment and pack verifies, each
/// commit reads, the pin directories decode.
fn assert_consistent(vol: &LocalVolume) {
    for s in SegmentStore::list(vol).expect("list") {
        vol.footer(s.seg).expect("footer");
        vol.verify_segment(s.seg).expect("segment hash");
    }
    for p in vol.list_packs().expect("packs") {
        vol.verify_pack(p.pack).expect("pack hash");
    }
    for c in CommitStore::list(vol).expect("commits") {
        let bytes = vol.get(c.seq).expect("commit");
        assert_eq!(bytes.len() as u64, c.size_bytes);
    }
    vol.roots().expect("pins");
}

#[cfg(feature = "crashpoints")]
fn check(op: &str, point: &str, vol: &LocalVolume) {
    let step = point.rsplit('.').next().expect("step");
    // Create-style steps: the rename is what makes the new state visible.
    let new = matches!(step, "after_rename" | "after_dirsync");
    let ctx = format!("{op} killed at {point}");
    match op {
        "segment" => {
            let n = SegmentStore::list(vol).expect("list").len();
            assert_eq!(n, if new { 2 } else { 1 }, "{ctx}");
        }
        "pack" => {
            let n = vol.list_packs().expect("list").len();
            assert_eq!(n, if new { 1 } else { 0 }, "{ctx}");
        }
        "commit" => {
            let tip = vol.tip().expect("tip");
            assert_eq!(tip, Some(CommitSeq(if new { 1 } else { 0 })), "{ctx}");
            if new {
                assert_eq!(vol.get(CommitSeq(1)).expect("get"), b"one");
            }
        }
        "evidence" => {
            let passes = vol.evidence_passes().expect("passes");
            assert_eq!(passes, if new { vec![1, 2] } else { vec![1] }, "{ctx}");
            let again = vol.put_evidence(2, b"x");
            if new {
                assert_eq!(again.expect_err("exists").kind(), ErrorKind::AlreadyExists);
            } else {
                again.expect("the pass can still be written");
            }
        }
        "state" => {
            let want = if new { 2 } else { 1 };
            assert_eq!(
                vol.device_record().expect("rec"),
                Some(device(want)),
                "{ctx}"
            );
        }
        "condemned" => {
            let want = if new { &b"new"[..] } else { &b"old"[..] };
            let got = vol.load_condemned().expect("load").expect("present");
            assert_eq!(got.body, want, "{ctx}");
        }
        "checkpoint" => {
            let got = vol.load().expect("load").expect("a slot survives");
            let old = slot(1, 0);
            let newer = slot(2, 1);
            match step {
                "before_write" => assert_eq!(got, old, "{ctx}"),
                // The slot is written but maybe not synced: a kill leaves it
                // in the page cache, a power cut may not. Either is whole.
                "before_fdatasync" => assert!(got == old || got == newer, "{ctx}"),
                _ => assert_eq!(got, newer, "{ctx}"),
            }
        }
        "checkpoint_first" => {
            let got = vol.load().expect("load");
            match step {
                "before_write" => assert_eq!(got, None, "{ctx}"),
                "before_fdatasync" => assert!(got.is_none() || got == Some(slot(1, 1)), "{ctx}"),
                _ => assert_eq!(got, Some(slot(1, 1)), "{ctx}"),
            }
        }
        "delete_segment" => {
            let n = SegmentStore::list(vol).expect("list").len();
            assert_eq!(n, if step == "before_unlink" { 2 } else { 1 }, "{ctx}");
        }
        "delete_pack" => {
            let n = vol.list_packs().expect("list").len();
            assert_eq!(n, if step == "before_unlink" { 1 } else { 0 }, "{ctx}");
        }
        "delete_commit" => {
            let seqs: Vec<u64> = CommitStore::list(vol)
                .expect("list")
                .iter()
                .map(|c| c.seq.0)
                .collect();
            assert_eq!(
                seqs,
                if step == "before_unlink" {
                    vec![0, 1, 2]
                } else {
                    vec![1, 2]
                },
                "{ctx}"
            );
        }
        "set_aside" => {
            let tip = vol.tip().expect("tip");
            assert_eq!(
                tip,
                Some(CommitSeq(if step == "before_rename" { 2 } else { 1 })),
                "{ctx}"
            );
            let aside = vol.dir().join("state/commits/0000000000000002.damaged");
            assert_eq!(aside.exists(), step != "before_rename", "{ctx}");
        }
        "leftovers" => {
            let left = vol.leftovers().expect("leftovers").len();
            let want = match step {
                "before_unlink" => 3,
                "after_unlink" => 2,
                _ => 0,
            };
            assert_eq!(left, want, "{ctx}");
        }
        "sweep" => {
            let n = SegmentStore::list(vol).expect("list").len();
            // The temp file goes first (its own crash points), then the orphan
            // segment, which is what these points guard.
            assert_eq!(n, if step == "before_unlink" { 2 } else { 1 }, "{ctx}");
            assert!(vol.leftovers().expect("leftovers").is_empty(), "{ctx}");
        }
        other => panic!("no check for {other}"),
    }
    assert_consistent(vol);
    // Whatever the crash left, the next open sweeps it and never reuses an id.
    let high = vol.ids().high_water();
    vol.remove_leftovers().expect("remove leftovers");
    assert!(vol.leftovers().expect("left").is_empty());
    let w = vol.open_segment().expect("open segment");
    assert!(w.id().raw() > high, "{ctx}: id reused");
}

#[cfg(feature = "crashpoints")]
#[test]
fn a_kill_at_every_durable_step_leaves_the_old_state_or_the_new_one() {
    for (op, points) in table() {
        for point in points {
            let dir = tempdir();
            {
                let vol = create(dir.path(), MODE, CHUNK, test_opts());
                setup(op, &vol);
            }
            let child = spawn_child(dir.path(), op, Some(&point));
            let out = child.wait_with_output().expect("wait");
            assert_eq!(
                out.status.signal(),
                Some(9),
                "{op} at {point}: the child did not die at the crash point ({:?}): {}",
                out.status,
                String::from_utf8_lossy(&out.stderr)
            );
            let vol = open(dir.path(), MODE, test_opts())
                .unwrap_or_else(|e| panic!("{op} at {point}: reopen failed: {e}"));
            check(op, &point, &vol);
        }
    }
}

#[cfg(feature = "crashpoints")]
#[test]
fn the_sweep_table_covers_every_registered_store_crash_point() {
    let covered: BTreeSet<String> = table().into_iter().flat_map(|(_, p)| p).collect();
    let registered: BTreeSet<String> = lunix_iface::crashpoint::registered()
        .into_iter()
        .filter(|n| n.starts_with("store."))
        .map(str::to_owned)
        .collect();
    assert_eq!(registered, covered);
    lunix_iface::crashpoint::check_registry().expect("names are unique and well formed");
}

#[test]
fn a_run_without_a_crash_point_completes_every_operation() {
    // The same operations, run to the end: they must succeed (so a child that
    // died for another reason is not mistaken for a crash).
    for (op, _) in table() {
        let dir = tempdir();
        {
            let vol = create(dir.path(), MODE, CHUNK, test_opts());
            setup(op, &vol);
        }
        let out = spawn_child(dir.path(), op, None)
            .wait_with_output()
            .expect("wait");
        assert!(
            out.status.success(),
            "{op}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let vol = open(dir.path(), MODE, test_opts()).expect("reopen");
        assert_consistent(&vol);
    }
}
