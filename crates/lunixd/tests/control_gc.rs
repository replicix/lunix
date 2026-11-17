//! `Gc` and `GcStatus` over the control socket (plan M12 step 12), against a
//! `lunixd` child with `device = "none"`: a churn volume (written in process
//! through `lunixd::testsupport`, then attached by the daemon), a dry run
//! that reports dead objects and deletes nothing, a pass on request that
//! frees them, and the status fields after it. The volume then reads back
//! equal to what was written last and `lunix-fsck` (when built) finds no
//! error.
//!
//! The volume's `volume.json` gets the smallest grace it takes (12 s with
//! `read_timeout_ms` 1000), so the pass on request takes about that long.
//!
//! ```text
//! cargo build -p lunix-fsck
//! cargo test -p lunixd --features testsupport --test control_gc
//! ```

#![cfg(feature = "testsupport")]

mod common;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Instant;

use common::{scratch, Daemon};
use lunix_format::local::VolumeJson;
use lunix_format::paths::local as lpath;
use lunix_format::CryptoMode;
use lunix_iface::ctl::{
    Attach, Compress, ErrorCode, GcRequest, NameParam, Outcome, Request, Response, VolumeCreate,
    VolumeMode,
};
use lunix_iface::gc::GcPassSummary;
use lunixd::testsupport::{close, fsck, payload, Driver, TestVolume, VolumeBuilder};

const SIZE: u64 = 64 << 20;
const CS: u32 = 64 << 10;
/// The churned region, rewritten whole in every round.
const REGION: u64 = 4 << 20;
/// Rounds: GC keeps three FULLs (`retain_last` 2 plus the floor's own
/// floor), so the first rounds' segments die whole.
const ROUNDS: u64 = 8;
const QUEUES: u16 = 2;

#[track_caller]
fn ok(o: Outcome) -> Response {
    match o {
        Outcome::Ok(r) => r,
        Outcome::Err(e) => panic!("expected ok, got {e:?}"),
    }
}

fn gc(d: &mut Daemon, dry_run: bool) -> GcPassSummary {
    match ok(d.call(Request::Gc(GcRequest {
        name: "g".into(),
        dry_run,
    }))) {
        Response::Gc(s) => s,
        r => panic!("{r:?}"),
    }
}

fn segment_files(voldir: &Path) -> BTreeSet<(String, u64)> {
    std::fs::read_dir(voldir.join("segments"))
        .expect("segments dir")
        .flatten()
        .map(|e| {
            (
                e.file_name().to_string_lossy().into_owned(),
                e.metadata().map(|m| m.len()).unwrap_or(0),
            )
        })
        .collect()
}

fn bytes(set: &BTreeSet<(String, u64)>) -> u64 {
    set.iter().map(|(_, n)| n).sum()
}

fn builder(parent: &Path) -> VolumeBuilder {
    VolumeBuilder::new(parent)
        .manual()
        .mode(CryptoMode::Plain)
        .chunk_size(CS)
        .size_bytes(SIZE)
        .queues(QUEUES)
}

/// Rewrites the region `ROUNDS` times, a commit after each round; returns
/// the region's last content.
fn churn(vol: &TestVolume) -> Vec<u8> {
    let stack = vol.open().expect("open");
    let mut d = Driver::new(&stack).expect("driver");
    let mut last = Vec::new();
    for r in 0..ROUNDS {
        last.clear();
        for c in 0..REGION / u64::from(CS) {
            let data = payload(r << 32 | c, CS as usize);
            let st = d.write(
                (c % u64::from(QUEUES)) as usize,
                c * u64::from(CS),
                &data,
                false,
            );
            assert!(st.is_ok(), "write: {st:?}");
            last.extend_from_slice(&data);
        }
        assert!(d.flush(0).is_ok() && d.flush(1).is_ok());
        stack.engine.commit_now(false).expect("commit");
    }
    drop(d);
    close(stack).expect("close");
    last
}

/// The smallest grace `volume.json` takes: 10 s plus twice `read_timeout`.
fn short_grace(voldir: &Path) {
    let path = voldir.join(lpath::volume_json());
    let mut j = VolumeJson::from_json(&std::fs::read_to_string(&path).unwrap()).unwrap();
    j.read_timeout_ms = 1000;
    j.gc_grace_ms = 12_000;
    j.validate().unwrap();
    std::fs::write(&path, j.to_json_pretty().unwrap()).unwrap();
}

fn voldir_of(d: &mut Daemon) -> PathBuf {
    match ok(d.call(Request::VolumeList)) {
        Response::VolumeList { volumes } => {
            PathBuf::from(&volumes.iter().find(|v| v.name == "g").expect("listed").dir)
        }
        r => panic!("{r:?}"),
    }
}

#[test]
fn gc_on_request_frees_space_dry_run_deletes_nothing_status_reports() {
    let Some(t) = scratch("gcctl") else { return };
    let vols = t.path().join("vols");
    let mut d = Daemon::start(t.path(), "");
    ok(d.call(Request::VolumeCreate(VolumeCreate {
        name: "g".into(),
        dir: vols.to_string_lossy().into_owned(),
        size_bytes: SIZE,
        chunk_size: CS,
        mode: VolumeMode::Plain,
        compress: Compress::Auto,
        id_len: 16,
        map_fanout: None,
        key_file: None,
        passphrase: None,
    })));
    let voldir = voldir_of(&mut d);
    let parent = voldir.parent().expect("parent").to_path_buf();
    let vol = TestVolume::at(&voldir, builder(&parent), None).expect("volume");
    let last = churn(&vol);
    short_grace(&voldir);

    // Not attached: no pass, status idle.
    match d.call(Request::Gc(GcRequest {
        name: "g".into(),
        dry_run: true,
    })) {
        Outcome::Err(e) => assert_eq!(e.code, ErrorCode::Invalid, "{e:?}"),
        o => panic!("{o:?}"),
    }

    ok(d.call(Request::Attach(Attach {
        name: "g".into(),
        queues: None,
        depth: None,
        cpus: None,
        max_io_buf_bytes: None,
        credential: None,
        on_journal_damage: None,
        allow_unverifiable_fork: false,
    })));
    let attached = Instant::now();

    // The dry run reports the dead rounds and deletes nothing (no grace has
    // run since the attach: nothing can be deleted yet by anyone).
    let before = segment_files(&voldir);
    let dry = gc(&mut d, true);
    assert!(
        attached.elapsed().as_secs() < 10,
        "the dry run took too long"
    );
    assert!(dry.dry_run && dry.pass == 0, "{dry:?}");
    assert!(dry.segments_deleted > 0 && dry.bytes_freed > 0, "{dry:?}");
    assert_eq!(segment_files(&voldir), before, "a dry run deleted segments");
    match ok(d.call(Request::GcStatus(NameParam { name: "g".into() }))) {
        Response::GcStatus(s) => assert!(s.last_pass.is_none() && s.stats.passes == 0, "{s:?}"),
        r => panic!("{r:?}"),
    }

    // The pass on request: condemn, grace, recheck, delete.
    let started = Instant::now();
    let pass = gc(&mut d, false);
    eprintln!("gc pass on request: {pass:?} in {:?}", started.elapsed());
    assert!(!pass.dry_run && pass.pass >= 1, "{pass:?}");
    assert!(
        pass.segments_deleted > 0 && pass.bytes_freed > 0,
        "{pass:?}"
    );
    let after = segment_files(&voldir);
    assert!(
        bytes(&after) < bytes(&before),
        "segments {} -> {} bytes",
        bytes(&before),
        bytes(&after)
    );
    let gone = before.difference(&after).count() as u64;
    assert!(gone >= pass.segments_deleted, "{gone} files gone, {pass:?}");

    match ok(d.call(Request::GcStatus(NameParam { name: "g".into() }))) {
        Response::GcStatus(s) => {
            assert_eq!(s.state, None, "{s:?}");
            assert!(s.stats.passes >= 1, "{s:?}");
            assert_eq!(
                s.last_pass.as_ref().map(|p| p.pass),
                Some(pass.pass),
                "{s:?}"
            );
            assert!(s.last_finished_ms.is_some(), "{s:?}");
            assert!(s.stats.bytes_freed >= pass.bytes_freed, "{s:?}");
            assert!(s.stats.live_bytes > 0, "{s:?}");
        }
        r => panic!("{r:?}"),
    }

    // A second dry run finds the deleted rounds gone.
    let dry2 = gc(&mut d, true);
    assert!(dry2.bytes_freed < dry.bytes_freed, "{dry2:?} after {dry:?}");

    // Detached: the data is the last round's, fsck finds no error.
    assert!(matches!(
        ok(d.call(Request::Detach(NameParam { name: "g".into() }))),
        Response::Detached
    ));
    if let Some(f) = fsck(&vol) {
        assert!(f.no_errors(), "lunix-fsck:\n{}", f.text);
    }
    let stack = vol.open().expect("reopen");
    let mut dr = Driver::new(&stack).expect("driver");
    let all = dr.read_all(SIZE).expect("read");
    assert!(all[..REGION as usize] == last[..], "the region differs");
    assert!(all[REGION as usize..].iter().all(|&b| b == 0));
    drop(dr);
    close(stack).expect("close");
    d.kill();
}
