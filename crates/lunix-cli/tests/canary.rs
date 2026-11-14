//! A secret passed through `--passphrase-fd` never appears in the process
//! arguments, the environment or any output of `lunix`; it does reach the
//! daemon, as the protocol requires.

mod common;

use std::io::Write;
use std::os::unix::net::UnixListener;
use std::process::{Command, Stdio};
use std::sync::mpsc;

use lunix_iface::ctl::{
    from_payload, read_frame, to_payload, write_frame, Request, RequestFrame, Response,
    ResponseFrame, VolumeCreated, VolumeMode, MAX_FRAME,
};

const CANARY: &str = "canary-7f3a91c2e5b84d06-Zq";

fn hex(s: &str) -> String {
    s.bytes().map(|b| format!("{b:02x}")).collect()
}

#[test]
fn canary_stays_off_argv_environment_and_output() {
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("canary.sock");
    let listener = UnixListener::bind(&sock).unwrap();
    let (seen_tx, seen_rx) = mpsc::channel::<Vec<u8>>();
    let (go_tx, go_rx) = mpsc::channel::<()>();
    let server = std::thread::spawn(move || {
        let (mut conn, _) = listener.accept().unwrap();
        let payload = read_frame(&mut conn, MAX_FRAME).unwrap();
        let frame: RequestFrame = from_payload(&payload).unwrap();
        let Request::VolumeCreate(c) = frame.call else {
            panic!("not a create")
        };
        seen_tx
            .send(
                c.passphrase
                    .expect("passphrase on the wire")
                    .expose()
                    .to_vec(),
            )
            .unwrap();
        // Hold the reply until the test has looked at the live process.
        go_rx.recv().unwrap();
        let reply = ResponseFrame::ok(
            frame.id,
            Response::VolumeCreated(VolumeCreated {
                name: "c1".into(),
                uuid: "u".into(),
                dir: "/data/x/u".into(),
                mode: VolumeMode::Encrypted,
                key_file: None,
            }),
        );
        write_frame(&mut conn, &to_payload(&reply).unwrap(), MAX_FRAME).unwrap();
    });

    let mut child = Command::new(common::BIN)
        .arg("--socket")
        .arg(&sock)
        .args(["volume", "create", "c1", "--size", "1G", "--dir", "/data/x"])
        .args(["--encrypt", "--passphrase-fd", "0"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let pid = child.id();
    {
        let mut si = child.stdin.take().unwrap();
        si.write_all(CANARY.as_bytes()).unwrap();
        si.write_all(b"\n").unwrap();
    }
    let on_wire = seen_rx.recv().unwrap();
    assert_eq!(on_wire, CANARY.as_bytes(), "the daemon must get the secret");

    let cmdline = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap();
    let environ = std::fs::read(format!("/proc/{pid}/environ")).unwrap_or_default();
    go_tx.send(()).unwrap();
    let out = common::finish(child.wait_with_output().unwrap());
    server.join().unwrap();

    let h = hex(CANARY);
    assert_eq!(out.code, 0, "{}", out.stderr);
    for (what, hay) in [
        ("argv", String::from_utf8_lossy(&cmdline).into_owned()),
        (
            "environment",
            String::from_utf8_lossy(&environ).into_owned(),
        ),
        ("stdout", out.stdout.clone()),
        ("stderr", out.stderr.clone()),
    ] {
        assert!(!hay.contains(CANARY), "canary in {what}");
        assert!(!hay.contains(&h), "hex canary in {what}");
    }
    assert!(String::from_utf8_lossy(&cmdline).contains("--passphrase-fd"));
}

#[test]
fn a_failing_request_does_not_echo_the_secret() {
    let env = common::Env::new();
    env.err(
        "VolumeCreate",
        lunix_iface::ctl::ErrorCode::Busy,
        "volume exists",
    );
    let o = env.run_stdin(
        &[
            "volume",
            "create",
            "c1",
            "--size",
            "1G",
            "--dir",
            "/d",
            "--encrypt",
            "--passphrase-fd",
            "0",
        ],
        CANARY.as_bytes(),
    );
    assert_eq!(o.code, 3);
    assert!(!o.stderr.contains(CANARY) && !o.stdout.contains(CANARY));
    assert!(!o.stderr.contains(&hex(CANARY)));
}

#[test]
fn the_environment_is_never_a_passphrase_source() {
    let env = common::Env::new();
    // No --passphrase flag: a request must carry no passphrase whatever the
    // environment holds.
    env.ok(
        "VolumeCreate",
        Response::VolumeCreated(VolumeCreated {
            name: "c1".into(),
            uuid: "u".into(),
            dir: "/d/u".into(),
            mode: VolumeMode::Integrity,
            key_file: None,
        }),
    );
    let o = Command::new(common::BIN)
        .arg("--socket")
        .arg(env.socket())
        .args(["volume", "create", "c1", "--size", "1G", "--dir", "/d"])
        .env("LUNIX_PASSPHRASE", CANARY)
        .env("PASSPHRASE", CANARY)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(o.status.success());
    let Request::VolumeCreate(c) = &env.server.requests()[0] else {
        panic!("not a create")
    };
    assert!(c.passphrase.is_none());
}
