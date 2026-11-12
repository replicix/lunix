//! [`FakeCtlServer`]: a control-protocol server on a Unix socket, for testing
//! `lunix` (M13) without a daemon.
//!
//! It speaks the real framing and message types of [`lunix_iface::ctl`]: it
//! reads length-prefixed JSON frames, checks the protocol version, runs
//! [`Request::validate`], records the request and answers from a script.
//! Anything the script has no answer for gets an `internal` error naming the
//! method, so a test notices a call it did not expect.
//!
//! Methods are named as the `Request` variants are (`"Ping"`, `"VolumeCreate"`,
//! `"Attach"`, ...). [`respond`](FakeCtlServer::respond) answers every call of a
//! method the same way; [`respond_once`](FakeCtlServer::respond_once) queues
//! answers, consumed in order, after which the permanent answer (if any) applies.
//! `Ping` is answered with a `Pong` unless scripted.

use std::collections::{HashMap, VecDeque};
use std::io;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;

use lunix_iface::ctl::{
    from_payload, read_frame, to_payload, write_frame, CtlError, ErrorCode, FrameError, Outcome,
    Request, RequestFrame, Response, ResponseFrame, MAX_FRAME, PROTOCOL_VERSION,
};

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// The variant name of a request (`"VolumeList"`), taken from its `Debug`
/// output: it relies on the derived `Debug` starting with the variant name.
pub fn method_name(req: &Request) -> String {
    let dbg = format!("{req:?}");
    dbg.split(|c: char| !c.is_ascii_alphanumeric())
        .next()
        .unwrap_or_default()
        .to_owned()
}

#[derive(Default)]
struct Script {
    always: HashMap<String, Outcome>,
    once: HashMap<String, VecDeque<Outcome>>,
}

struct Shared {
    script: Mutex<Script>,
    log: Mutex<Vec<Request>>,
    conns: Mutex<Vec<(u64, UnixStream)>>,
    next_conn: AtomicU64,
    stop: AtomicBool,
}

/// A running fake server. Dropping it stops it and closes its connections.
pub struct FakeCtlServer {
    path: PathBuf,
    shared: Arc<Shared>,
    accept: Option<JoinHandle<()>>,
}

impl FakeCtlServer {
    /// Listens on `path` (which must not exist).
    ///
    /// # Errors
    /// The bind error.
    pub fn start(path: &Path) -> io::Result<Self> {
        let listener = UnixListener::bind(path)?;
        let shared = Arc::new(Shared {
            script: Mutex::new(Script::default()),
            log: Mutex::new(Vec::new()),
            conns: Mutex::new(Vec::new()),
            next_conn: AtomicU64::new(0),
            stop: AtomicBool::new(false),
        });
        let s = Arc::clone(&shared);
        let accept = std::thread::Builder::new()
            .name("fake-ctl-accept".into())
            .spawn(move || {
                for conn in listener.incoming() {
                    if s.stop.load(Ordering::SeqCst) {
                        return;
                    }
                    let Ok(conn) = conn else { continue };
                    let id = s.next_conn.fetch_add(1, Ordering::Relaxed);
                    if let Ok(clone) = conn.try_clone() {
                        lock(&s.conns).push((id, clone));
                    }
                    let s2 = Arc::clone(&s);
                    let _ = std::thread::Builder::new()
                        .name("fake-ctl-conn".into())
                        .spawn(move || {
                            serve(&s2, conn);
                            lock(&s2.conns).retain(|(i, _)| *i != id);
                        });
                }
            })?;
        Ok(Self {
            path: path.to_owned(),
            shared,
            accept: Some(accept),
        })
    }

    /// The socket path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Answers every call of `method` with `outcome`.
    pub fn respond(&self, method: &str, outcome: Outcome) {
        lock(&self.shared.script)
            .always
            .insert(method.to_owned(), outcome);
    }

    /// Answers the next call of `method` with `outcome` (queued).
    pub fn respond_once(&self, method: &str, outcome: Outcome) {
        lock(&self.shared.script)
            .once
            .entry(method.to_owned())
            .or_default()
            .push_back(outcome);
    }

    /// Every valid request received so far, in order.
    pub fn requests(&self) -> Vec<Request> {
        lock(&self.shared.log).clone()
    }
}

impl Drop for FakeCtlServer {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        for (_, c) in lock(&self.shared.conns).drain(..) {
            let _ = c.shutdown(std::net::Shutdown::Both);
        }
        // Wake the blocking accept.
        let _ = UnixStream::connect(&self.path);
        if let Some(h) = self.accept.take() {
            let _ = h.join();
        }
        let _ = std::fs::remove_file(&self.path);
    }
}

fn answer(s: &Shared, req: &Request) -> Outcome {
    let name = method_name(req);
    let mut script = lock(&s.script);
    if let Some(o) = script.once.get_mut(&name).and_then(VecDeque::pop_front) {
        return o;
    }
    if let Some(o) = script.always.get(&name) {
        return o.clone();
    }
    match req {
        Request::Ping => Outcome::Ok(Response::Pong {
            version: PROTOCOL_VERSION,
            daemon: "fake-lunixd".into(),
        }),
        _ => Outcome::Err(CtlError::new(
            ErrorCode::Internal,
            format!("fake server has no answer for {name}"),
        )),
    }
}

fn serve(s: &Shared, mut conn: UnixStream) {
    run(s, &mut conn);
    // The accept loop keeps a duplicate of the descriptor, so closing ours
    // would not end the connection; shut it down explicitly.
    let _ = conn.shutdown(std::net::Shutdown::Both);
}

fn run(s: &Shared, conn: &mut UnixStream) {
    loop {
        let payload = match read_frame(conn, MAX_FRAME) {
            Ok(p) => p,
            Err(FrameError::Eof | FrameError::Io(_) | FrameError::Truncated { .. }) => return,
            Err(_) => {
                // Oversized or empty frame: say so, then drop the connection.
                let _ = send(
                    conn,
                    &ResponseFrame::err(0, CtlError::new(ErrorCode::Invalid, "bad frame")),
                );
                return;
            }
        };
        let reply = match from_payload::<RequestFrame>(&payload) {
            Err(_) => ResponseFrame::err(0, CtlError::new(ErrorCode::Invalid, "malformed request")),
            Ok(f) if f.v != PROTOCOL_VERSION => ResponseFrame::err(
                f.id,
                CtlError::new(ErrorCode::Invalid, "unsupported protocol version"),
            ),
            Ok(f) => match f.call.validate() {
                Err(e) => {
                    ResponseFrame::err(f.id, CtlError::new(ErrorCode::Invalid, e.to_string()))
                }
                Ok(()) => {
                    lock(&s.log).push(f.call.clone());
                    ResponseFrame {
                        v: PROTOCOL_VERSION,
                        id: f.id,
                        result: answer(s, &f.call),
                    }
                }
            },
        };
        if send(conn, &reply).is_err() {
            return;
        }
    }
}

fn send(conn: &mut UnixStream, frame: &ResponseFrame) -> Result<(), FrameError> {
    let payload = to_payload(frame)?;
    write_frame(conn, &payload, MAX_FRAME)
}
