//! `Gc` and `GcStatus` over the control socket (plan M12 step 12), against a
//! `lunixd` child with `device = "none"`: a churn volume (written in process
//! through `lunixd::testsupport`, then attached by the daemon), a dry run
//! that reports dead objects and deletes nothing, a detach while a pass on
//! request waits (the call ends, the volume attaches again at once), a pass
//! on request that frees them, and the status fields after it. The volume then reads back
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
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use common::{scratch, Daemon};
use lunix_format::local::VolumeJson;
use lunix_format::paths::local as lpath;
use lunix_format::CryptoMode;
use lunix_iface::ctl::{
    read_frame, to_payload, write_frame, Attach, Compress, ErrorCode, GcRequest, NameParam,
    Outcome, Request, RequestFrame, Response, ResponseFrame, VolumeCreate, VolumeMode, MAX_FRAME,
};
use lunix_iface::gc::{GcPassSummary, GcStatusSnapshot};
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

fn attach() -> Request {
    Request::Attach(Attach {
        name: "g".into(),
        queues: None,
        depth: None,
        cpus: None,
        max_io_buf_bytes: None,
        credential: None,
        on_journal_damage: None,
        allow_unverifiable_fork: false,
    })
}

fn status(d: &mut Daemon) -> GcStatusSnapshot {
    match ok(d.call(Request::GcStatus(NameParam { name: "g".into() }))) {
        Response::GcStatus(s) => s,
        r => panic!("{r:?}"),
    }
}

/// One call on its own connection (from another thread than `Daemon`'s).
fn call_on(socket: &Path, req: Request) -> Outcome {
    let frame = RequestFrame::new(1, req);
    let payload = to_payload(&frame).expect("payload");
    let mut s = UnixStream::connect(socket).expect("connect");
    write_frame(&mut s, &payload, MAX_FRAME).expect("write");
    let reply = read_frame(&mut s, MAX_FRAME).expect("reply");
    let r: ResponseFrame = serde_json::from_slice(&reply).expect("response");
    r.result
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
        store_max_percent: None,
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

    ok(d.call(attach()));

    // The dry run reports the dead rounds and deletes nothing. The pass the
    // attach started may delete meanwhile (after its 12 s grace, on a slow
    // host): the file sets are compared only when nothing was freed.
    let freed_before = status(&mut d).stats.bytes_freed;
    let before = segment_files(&voldir);
    let dry = gc(&mut d, true);
    let after_dry = segment_files(&voldir);
    assert!(dry.dry_run && dry.pass == 0, "{dry:?}");
    assert!(dry.segments_deleted > 0 && dry.bytes_freed > 0, "{dry:?}");
    if status(&mut d).stats.bytes_freed == freed_before {
        assert_eq!(after_dry, before, "a dry run deleted segments");
    } else {
        eprintln!("a background pass deleted during the dry run: file sets not compared");
    }

    // A detach while a pass on request waits (M12c review S-2): the call ends
    // `Busy`, the detach answers once the engine is gone, and the volume can
    // be attached again at once (`LOCK` released). The pass the attach
    // started is in its grace, so the call waits for the pass after it.
    let sock = d.socket.clone();
    let waiting = std::thread::spawn(move || {
        call_on(
            &sock,
            Request::Gc(GcRequest {
                name: "g".into(),
                dry_run: false,
            }),
        )
    });
    // The detach comes once the call waits in the engine (`requests`), not
    // after a guessed delay.
    let start = Instant::now();
    while status(&mut d).requests == 0 {
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "the gc call never reached the engine"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    // One pass on request per volume: a second call answers at once.
    match d.call(Request::Gc(GcRequest {
        name: "g".into(),
        dry_run: false,
    })) {
        Outcome::Err(e) => assert_eq!(e.code, ErrorCode::Busy, "{e:?}"),
        o => panic!("a second gc call: {o:?}"),
    }
    assert!(matches!(
        ok(d.call(Request::Detach(NameParam { name: "g".into() }))),
        Response::Detached
    ));
    ok(d.call(attach()));
    // The waiting call needed at least one more grace: it ended `Busy`.
    match waiting.join().expect("gc call thread") {
        Outcome::Err(e) => assert_eq!(e.code, ErrorCode::Busy, "{e:?}"),
        o => panic!("the waiting gc call: {o:?}"),
    }
    let before = segment_files(&voldir);

    // The pass on request: condemn, grace, recheck, delete. Whether the pass
    // the attach started or the requested one deleted, the call's answer
    // covers both; the asserts use the engine's counters (new since the
    // re-attach) and the segment files, not one pass's summary.
    let started = Instant::now();
    let pass = gc(&mut d, false);
    eprintln!("gc pass on request: {pass:?} in {:?}", started.elapsed());
    assert!(!pass.dry_run, "{pass:?}");
    let after = segment_files(&voldir);
    assert!(
        bytes(&after) < bytes(&before),
        "segments {} -> {} bytes",
        bytes(&before),
        bytes(&after)
    );
    let s = status(&mut d);
    assert!(s.stats.passes >= 1, "{s:?}");
    assert!(s.stats.bytes_freed > 0, "{s:?}");
    assert!(s.stats.bytes_freed >= pass.bytes_freed, "{pass:?} {s:?}");
    assert!(s.last_finished_ms.is_some(), "{s:?}");
    assert!(s.stats.live_bytes > 0, "{s:?}");
    assert_eq!(s.requests, 0, "{s:?}");

    // A second dry run answers too.
    assert!(gc(&mut d, true).dry_run);

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
