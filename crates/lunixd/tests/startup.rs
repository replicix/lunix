//! M12a re-review R4: a signal while startup recovery runs ends the daemon.
//! In-process: `server::run` with a startup step that never finishes, then a
//! `SIGUSR2` to this test process (its own binary, so no other test sees it).

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use lunixd::config::{DaemonConfig, DeviceKind};
use lunixd::daemon::Daemon;

#[test]
fn a_signal_during_startup_recovery_exits() {
    let Some(t) = common::scratch("startup") else {
        return;
    };
    let cfg = DaemonConfig {
        state_dir: t.path().join("state"),
        socket: t.path().join("ctl.sock"),
        dev_dir: t.path().join("dev"),
        device: DeviceKind::None,
        harden: false,
        ..DaemonConfig::default()
    };
    let daemon = Arc::new(Daemon::new(cfg).unwrap());
    let listener =
        lunixd::server::bind(&daemon.config().socket, daemon.config().socket_group).unwrap();
    let d = Arc::clone(&daemon);
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let code = lunixd::server::run(d, listener, |_| loop {
            // A recovery that does not end (a slow device, a lock held).
            std::thread::sleep(Duration::from_secs(1));
        });
        let _ = tx.send(code.map_err(|e| e.to_string()));
    });
    // Let the runtime install its signal handlers.
    std::thread::sleep(Duration::from_millis(500));
    let start = Instant::now();
    let pid = std::process::id();
    assert!(std::process::Command::new("kill")
        .arg("-USR2")
        .arg(pid.to_string())
        .status()
        .unwrap()
        .success());
    let code = rx
        .recv_timeout(Duration::from_secs(30))
        .expect("run returned while startup was still running");
    assert_eq!(code, Ok(0));
    assert!(start.elapsed() < Duration::from_secs(30));
    assert!(daemon.is_exiting());
}
