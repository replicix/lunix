//! M12a review M1, M2 and m1: the shutdown path (`QuiesceAll`, `SIGTERM`)
//! against a child `lunixd` with `device = "none"`. The null device's marker
//! files stand in for the kernel: `null-device-quiesce-fails` is a
//! `QUIESCE_DEV` timeout, `null-device-quiesce-ms` a slow quiesce.

mod common;

use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use common::{scratch, Daemon};
use lunix_iface::ctl::{
    Attach, Compress, ErrorCode, Outcome, Request, Response, VolumeCreate, VolumeMode,
};

fn create(d: &mut Daemon, name: &str, dir: &Path) -> PathBuf {
    match d.call(Request::VolumeCreate(VolumeCreate {
        name: name.into(),
        dir: dir.to_string_lossy().into_owned(),
        size_bytes: 64 << 20,
        chunk_size: 64 << 10,
        mode: VolumeMode::Plain,
        compress: Compress::None,
        id_len: 16,
        map_fanout: None,
        key_file: None,
        passphrase: None,
    })) {
        Outcome::Ok(Response::VolumeCreated(c)) => PathBuf::from(c.dir),
        o => panic!("{o:?}"),
    }
}

fn attach(d: &mut Daemon, name: &str) -> Outcome {
    d.call(Request::Attach(Attach {
        name: name.into(),
        queues: Some(1),
        depth: None,
        cpus: None,
        max_io_buf_bytes: None,
        credential: None,
        on_journal_damage: None,
        allow_unverifiable_fork: false,
    }))
}

#[track_caller]
fn attached(o: Outcome) {
    assert!(matches!(o, Outcome::Ok(Response::Attached(_))), "{o:?}");
}

#[test]
fn two_volumes_quiesce_and_the_daemon_exits() {
    let Some(t) = scratch("sd2") else { return };
    let mut d = Daemon::start(t.path(), "");
    let vols = t.path().join("vols");
    create(&mut d, "a", &vols);
    create(&mut d, "b", &vols);
    attached(attach(&mut d, "a"));
    attached(attach(&mut d, "b"));
    assert!(matches!(
        d.call(Request::QuiesceAll),
        Outcome::Ok(Response::Quiesced { devices: 2 })
    ));
    assert_eq!(d.wait_exit(), 0);
}

#[test]
fn a_failed_quiesce_with_nothing_else_quiesced_keeps_serving() {
    let Some(t) = scratch("sd1") else { return };
    let mut d = Daemon::start(t.path(), "");
    let a = create(&mut d, "a", &t.path().join("vols"));
    attached(attach(&mut d, "a"));
    std::fs::write(a.join("null-device-quiesce-fails"), b"").unwrap();
    match d.call(Request::QuiesceAll) {
        Outcome::Err(e) => {
            assert_eq!(e.code, ErrorCode::Io);
            assert!(e.message.contains("refuses new I/O"), "{}", e.message);
        }
        o => panic!("{o:?}"),
    }
    assert!(matches!(
        d.call(Request::Ping),
        Outcome::Ok(Response::Pong { .. })
    ));
    assert!(d.alive());
    assert!(d
        .log_text()
        .contains("refuses new I/O until QuiesceAll is retried"));
    // Mutating calls work again (the quiescing flag was cleared); the retry
    // succeeds and exits.
    std::fs::remove_file(a.join("null-device-quiesce-fails")).unwrap();
    assert!(matches!(
        d.call(Request::QuiesceAll),
        Outcome::Ok(Response::Quiesced { devices: 1 })
    ));
    assert_eq!(d.wait_exit(), 0);
}

#[test]
fn a_quiesce_failing_after_another_was_quiesced_aborts_for_recovery() {
    let Some(t) = scratch("sda") else { return };
    let mut d = Daemon::start(t.path(), "");
    let vols = t.path().join("vols");
    create(&mut d, "a", &vols);
    let b = create(&mut d, "b", &vols);
    attached(attach(&mut d, "a"));
    attached(attach(&mut d, "b"));
    // "a" is quiesced first (name order), "b" fails twice.
    std::fs::write(b.join("null-device-quiesce-fails"), b"").unwrap();
    let pid = d.child.as_ref().unwrap().id();
    let sent = std::process::Command::new("kill")
        .arg("-TERM")
        .arg(pid.to_string())
        .status()
        .unwrap();
    assert!(sent.success());
    let st = d.wait_status();
    assert_eq!(st.signal(), Some(libc::SIGABRT), "{st:?}");
    let log = d.log_text();
    assert!(log.contains("retrying once"), "{log}");
    assert!(log.contains("aborting for recovery"), "{log}");
}

#[test]
fn quiesce_all_outliving_the_call_timeout_still_replies_and_exits() {
    let Some(t) = scratch("sdt") else { return };
    let mut d = Daemon::start(t.path(), "call_timeout_ms = 15000");
    let vols = t.path().join("vols");
    let a = create(&mut d, "a", &vols);
    create(&mut d, "b", &vols);
    attached(attach(&mut d, "a"));
    std::fs::write(a.join("null-device-quiesce-ms"), b"20000").unwrap();
    // While the quiesce runs, a mutating call is refused (review m1).
    let sock = d.socket.clone();
    let waiter = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(3000));
        let frame = lunix_iface::ctl::RequestFrame::new(
            99,
            Request::Attach(Attach {
                name: "b".into(),
                queues: Some(1),
                depth: None,
                cpus: None,
                max_io_buf_bytes: None,
                credential: None,
                on_journal_damage: None,
                allow_unverifiable_fork: false,
            }),
        );
        let payload = lunix_iface::ctl::to_payload(&frame).unwrap();
        let mut s = std::os::unix::net::UnixStream::connect(&sock).unwrap();
        lunix_iface::ctl::write_frame(&mut s, &payload, lunix_iface::ctl::MAX_FRAME).unwrap();
        let r = lunix_iface::ctl::read_frame(&mut s, lunix_iface::ctl::MAX_FRAME).unwrap();
        serde_json::from_slice::<lunix_iface::ctl::ResponseFrame>(&r).unwrap()
    });
    let start = Instant::now();
    let o = d.call(Request::QuiesceAll);
    assert!(
        start.elapsed() >= Duration::from_secs(3),
        "the quiesce took its time"
    );
    assert!(
        matches!(o, Outcome::Ok(Response::Quiesced { devices: 1 })),
        "no timeout reply for QuiesceAll: {o:?}"
    );
    let r = waiter.join().unwrap();
    assert!(
        matches!(r.result, Outcome::Err(ref e) if e.code == ErrorCode::Busy),
        "attach during the quiesce: {r:?}"
    );
    assert_eq!(d.wait_exit(), 0);
}

#[test]
fn a_slow_quiesce_on_sigterm_exits() {
    let Some(t) = scratch("sds") else { return };
    let mut d = Daemon::start(t.path(), "call_timeout_ms = 4000");
    let a = create(&mut d, "a", &t.path().join("vols"));
    attached(attach(&mut d, "a"));
    std::fs::write(a.join("null-device-quiesce-ms"), b"6000").unwrap();
    let pid = d.child.as_ref().unwrap().id();
    assert!(std::process::Command::new("kill")
        .arg("-TERM")
        .arg(pid.to_string())
        .status()
        .unwrap()
        .success());
    assert_eq!(d.wait_exit(), 0);
}
