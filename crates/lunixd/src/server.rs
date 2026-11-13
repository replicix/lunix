//! The control server (plan M12a step 3) and the signals (step 7).
//!
//! The tokio runtime exists only here: it accepts connections and waits for
//! signals. Each connection is one blocking task that reads frames with the
//! `lunix-iface` framing (`read_frame` keeps the payload in zeroizing memory:
//! it may carry a passphrase), checks the version and `Request::validate`,
//! and runs the call on a separate thread bounded by the per-call timeout. A
//! call that outlives its timeout gets an error reply and keeps running; the
//! volume's mutex makes the next call on that volume wait for it.
//!
//! `SIGTERM`, `SIGINT` and `SIGUSR2` quiesce every device and exit without a
//! stop command (W1); when a quiesce fails the daemon keeps serving and logs.

use std::io;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream as StdStream;
use std::path::Path;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use lunix_iface::ctl::{
    from_payload, read_frame, to_payload, write_frame, CtlError, ErrorCode, FrameError,
    RequestFrame, Response, ResponseFrame, MAX_FRAME, PROTOCOL_VERSION,
};
use tokio::net::UnixListener;
use tokio::signal::unix::{signal, SignalKind};

use crate::audit::Peer;
use crate::daemon::{After, Daemon};

/// Prepares the socket path: refuses when another daemon answers on it,
/// removes a stale socket file, creates the parent directory.
///
/// # Errors
/// A live daemon on the path, or the file system.
pub fn bind(path: &Path, group: Option<u32>) -> io::Result<std::os::unix::net::UnixListener> {
    if StdStream::connect(path).is_ok() {
        return Err(io::Error::new(
            io::ErrorKind::AddrInUse,
            "another daemon answers on the control socket",
        ));
    }
    if let Ok(m) = std::fs::symlink_metadata(path) {
        use std::os::unix::fs::FileTypeExt;
        if m.file_type().is_socket() {
            std::fs::remove_file(path)?;
        } else {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "the control socket path exists and is not a socket",
            ));
        }
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let l = std::os::unix::net::UnixListener::bind(path)?;
    let mode = if group.is_some() { 0o660 } else { 0o600 };
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))?;
    if let Some(gid) = group {
        std::os::unix::fs::chown(path, None, Some(gid))?;
    }
    Ok(l)
}

fn reply(stream: &mut StdStream, frame: &ResponseFrame) -> Result<(), FrameError> {
    let payload = to_payload(frame)?;
    write_frame(stream, &payload, MAX_FRAME)
}

fn peer_of(stream: &tokio::net::UnixStream) -> Peer {
    match stream.peer_cred() {
        Ok(c) => Peer {
            uid: Some(c.uid()),
            gid: Some(c.gid()),
            pid: c.pid(),
        },
        Err(_) => Peer::default(),
    }
}

/// Serves one connection until it closes. Returns the exit code when a call
/// asked the daemon to exit.
fn connection(daemon: &Arc<Daemon>, mut stream: StdStream, peer: Peer) -> Option<i32> {
    loop {
        let payload = match read_frame(&mut stream, MAX_FRAME) {
            Ok(p) => p,
            Err(FrameError::Eof) => return None,
            Err(e) => {
                // Oversized, empty or truncated: say so once and close.
                let _ = reply(
                    &mut stream,
                    &ResponseFrame::err(0, CtlError::new(ErrorCode::Invalid, e.to_string())),
                );
                return None;
            }
        };
        let frame: RequestFrame = match from_payload(&payload) {
            Ok(f) => f,
            Err(_) => {
                let id = serde_json::from_slice::<serde_json::Value>(&payload)
                    .ok()
                    .and_then(|v| v.get("id").and_then(serde_json::Value::as_u64))
                    .unwrap_or(0);
                drop(payload);
                if reply(
                    &mut stream,
                    &ResponseFrame::err(id, CtlError::new(ErrorCode::Invalid, "malformed request")),
                )
                .is_err()
                {
                    return None;
                }
                continue;
            }
        };
        drop(payload);
        let id = frame.id;
        if frame.v != PROTOCOL_VERSION {
            let e = CtlError::new(
                ErrorCode::Invalid,
                format!("protocol version {} is not {PROTOCOL_VERSION}", frame.v),
            );
            if reply(&mut stream, &ResponseFrame::err(id, e)).is_err() {
                return None;
            }
            continue;
        }
        if let Err(e) = frame.call.validate() {
            if reply(
                &mut stream,
                &ResponseFrame::err(id, CtlError::new(ErrorCode::Invalid, e.to_string())),
            )
            .is_err()
            {
                return None;
            }
            continue;
        }
        let (res, after) = run_call(daemon, peer, frame);
        let out = match res {
            Ok(r) => ResponseFrame::ok(id, r),
            Err(e) => ResponseFrame::err(id, e),
        };
        let sent = reply(&mut stream, &out);
        if let After::Exit(code) = after {
            return Some(code);
        }
        if sent.is_err() {
            return None;
        }
    }
}

/// Runs a call on its own thread, bounded by the configured timeout.
fn run_call(
    daemon: &Arc<Daemon>,
    peer: Peer,
    frame: RequestFrame,
) -> (Result<Response, CtlError>, After) {
    let timeout = daemon.config().call_timeout;
    let (tx, rx) = mpsc::channel();
    let d = Arc::clone(daemon);
    let spawned = std::thread::Builder::new()
        .name("lunixd-call".into())
        .spawn(move || {
            let _ = tx.send(d.handle(peer, frame.call));
        });
    if spawned.is_err() {
        return (
            Err(CtlError::new(
                ErrorCode::Internal,
                "cannot start a call thread",
            )),
            After::Continue,
        );
    }
    match rx.recv_timeout(timeout) {
        Ok(r) => r,
        Err(_) => (
            Err(CtlError::new(
                ErrorCode::Internal,
                format!(
                    "the call did not finish within {} ms; it may still complete",
                    timeout.as_millis()
                ),
            )),
            After::Continue,
        ),
    }
}

/// Accepts connections and handles signals until a call or a signal asks to
/// exit; returns the exit code.
///
/// # Errors
/// The listener or the signal setup failed.
pub fn run(daemon: Arc<Daemon>, listener: std::os::unix::net::UnixListener) -> io::Result<i32> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .max_blocking_threads(64)
        .thread_name("lunixd-ctl")
        .enable_all()
        .build()?;
    let code = rt.block_on(async move {
        listener.set_nonblocking(true)?;
        let listener = UnixListener::from_std(listener)?;
        let (exit_tx, mut exit_rx) = tokio::sync::mpsc::channel::<i32>(4);
        for kind in [
            SignalKind::terminate(),
            SignalKind::interrupt(),
            SignalKind::user_defined2(),
        ] {
            let mut sig = signal(kind)?;
            let d = Arc::clone(&daemon);
            let tx = exit_tx.clone();
            tokio::spawn(async move {
                while sig.recv().await.is_some() {
                    tracing::info!("signal: quiescing every device");
                    let d2 = Arc::clone(&d);
                    let out = tokio::task::spawn_blocking(move || d2.quiesce_all()).await;
                    match out {
                        Ok((_, After::Exit(code))) => {
                            let _ = tx.send(code).await;
                            return;
                        }
                        Ok((Err(e), _)) => {
                            tracing::error!(error = %e.message, "quiesce on signal failed; still serving");
                        }
                        _ => {}
                    }
                }
            });
        }
        tracing::info!("control socket ready");
        loop {
            tokio::select! {
                Some(code) = exit_rx.recv() => return io::Result::Ok(code),
                acc = listener.accept() => {
                    let (stream, _) = match acc {
                        Ok(s) => s,
                        Err(e) => {
                            tracing::warn!(error = %e, "accept failed");
                            tokio::time::sleep(Duration::from_millis(50)).await;
                            continue;
                        }
                    };
                    let peer = peer_of(&stream);
                    let std = stream.into_std()?;
                    std.set_nonblocking(false)?;
                    let d = Arc::clone(&daemon);
                    let tx = exit_tx.clone();
                    tokio::task::spawn_blocking(move || {
                        if let Some(code) = connection(&d, std, peer) {
                            let _ = tx.blocking_send(code);
                        }
                    });
                }
            }
        }
    });
    // Connections may be blocked reading an idle client, and after a quiesce
    // the volume mutexes stay locked: do not wait for any of them.
    rt.shutdown_background();
    code
}
