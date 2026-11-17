//! Shared helpers of the lunixd tests: a queue driver over the engine's
//! handlers (no block device), volume creation, and a child daemon on a
//! temporary socket.
#![allow(dead_code)]

pub mod volume;

use std::collections::HashMap;
use std::io::Write;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use lunix_engine::Engine;
use lunix_iface::block::{
    BlockEngine, IoOp, IoRequest, IoStatus, QueueConfig, QueueHandler, QueueId, Tag,
};
use lunix_iface::ctl::{
    read_frame, to_payload, write_frame, Outcome, Request, RequestFrame, ResponseFrame, MAX_FRAME,
};
use lunix_testkit::block::TestCompleter;

/// Deterministic bytes, compressible by half: a 16-byte pattern from the seed
/// followed by as many zero bytes, repeated.
pub fn pattern(seed: u64, len: usize) -> Vec<u8> {
    let mut x = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
    let mut v = Vec::with_capacity(len);
    while v.len() < len {
        for _ in 0..16 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            v.push(x as u8);
        }
        v.extend_from_slice(&[0u8; 16]);
    }
    v.truncate(len);
    v
}

/// Drives an engine's queue handlers directly, as a single-depth ublk queue
/// per call would.
pub struct Driver {
    pub queues: Vec<Box<dyn QueueHandler>>,
    done: TestCompleter,
    next: u16,
    reads: HashMap<Tag, usize>,
    completed: HashMap<Tag, (IoStatus, Vec<u8>)>,
}

impl Driver {
    pub fn new(engine: &Arc<Engine>, queues: u16, buf: u32) -> Self {
        let mut v = Vec::new();
        for q in 0..queues {
            let cfg = QueueConfig {
                queue: QueueId(q),
                nr_queues: queues,
                depth: 64,
                max_io_buf_bytes: buf,
            };
            v.push(engine.queue_handler(&cfg).expect("queue handler"));
        }
        Self {
            queues: v,
            done: TestCompleter::new(buf),
            next: 0,
            reads: HashMap::new(),
            completed: HashMap::new(),
        }
    }

    fn collect(&mut self, inline: Option<(Tag, &[u8])>) {
        for (t, s) in self.done.take_completed() {
            let data = match self.reads.remove(&t) {
                Some(len) if s.is_ok() => match inline {
                    Some((it, b)) if it == t => b.to_vec(),
                    _ => self.done.buf_of(t, len),
                },
                _ => Vec::new(),
            };
            assert!(
                self.completed.insert(t, (s, data)).is_none(),
                "tag {t:?} twice"
            );
        }
    }

    pub fn submit(
        &mut self,
        q: usize,
        op: IoOp,
        fua: bool,
        off: u64,
        len: u32,
        data: &[u8],
    ) -> Tag {
        self.next = self.next.wrapping_add(1);
        let tag = Tag(self.next);
        let req = IoRequest {
            tag,
            op,
            fua,
            offset_bytes: off,
            len,
        };
        let mut buf = match op {
            IoOp::Write => data.to_vec(),
            IoOp::Read => vec![0u8; len as usize],
            _ => Vec::new(),
        };
        if op == IoOp::Read {
            self.reads.insert(tag, len as usize);
        }
        let qi = q % self.queues.len();
        self.queues[qi].on_request(req, &mut buf, &mut self.done);
        self.collect(Some((tag, &buf)));
        tag
    }

    pub fn wait(&mut self, tag: Tag) -> (IoStatus, Vec<u8>) {
        let start = Instant::now();
        loop {
            if let Some(r) = self.completed.remove(&tag) {
                return r;
            }
            for q in &mut self.queues {
                q.poll(&mut self.done);
            }
            self.collect(None);
            if !self.completed.contains_key(&tag) {
                std::thread::sleep(Duration::from_micros(200));
            }
            assert!(
                start.elapsed() < Duration::from_secs(60),
                "request {tag:?} never completed"
            );
        }
    }

    pub fn write(&mut self, off: u64, data: &[u8], fua: bool) -> IoStatus {
        let t = self.submit(
            0,
            IoOp::Write,
            fua,
            off,
            u32::try_from(data.len()).unwrap(),
            data,
        );
        self.wait(t).0
    }

    pub fn write_on(&mut self, q: usize, off: u64, data: &[u8]) -> IoStatus {
        let t = self.submit(
            q,
            IoOp::Write,
            false,
            off,
            u32::try_from(data.len()).unwrap(),
            data,
        );
        self.wait(t).0
    }

    pub fn op(&mut self, op: IoOp, off: u64, len: u32) -> IoStatus {
        let t = self.submit(0, op, false, off, len, &[]);
        self.wait(t).0
    }

    pub fn read_status(&mut self, off: u64, len: u32) -> (IoStatus, Vec<u8>) {
        let t = self.submit(0, IoOp::Read, false, off, len, &[]);
        self.wait(t)
    }

    pub fn read(&mut self, off: u64, len: u32) -> Vec<u8> {
        let t = self.submit(0, IoOp::Read, false, off, len, &[]);
        let (s, d) = self.wait(t);
        assert!(s.is_ok(), "read at {off} failed: {s:?}");
        d
    }

    pub fn flush(&mut self) -> IoStatus {
        let t = self.submit(0, IoOp::Flush, false, 0, 0, &[]);
        self.wait(t).0
    }
}

/// A scratch directory on a real file system (`LUNIX_SPIKE_DIR` on the test
/// host), or `None` when it is tmpfs (the journal needs `O_DIRECT`).
pub fn scratch(tag: &str) -> Option<tempfile::TempDir> {
    let base = std::env::var_os("LUNIX_SPIKE_DIR").map_or_else(std::env::temp_dir, PathBuf::from);
    std::fs::create_dir_all(&base).ok()?;
    let t = tempfile::Builder::new()
        .prefix(&format!("m12a-{tag}-"))
        .tempdir_in(&base)
        .ok()?;
    let st = rustix::fs::statfs(t.path()).ok()?;
    #[allow(clippy::unnecessary_cast)]
    // A test that cannot run must not pass silently (M12a review): on tmpfs
    // it fails, unless LUNIXD_TESTS_ALLOW_SKIP=1 says skips are acceptable.
    if st.f_type as u64 == 0x0102_1994 {
        assert!(
            std::env::var_os("LUNIXD_TESTS_ALLOW_SKIP").is_some(),
            "{} is tmpfs: the journal needs O_DIRECT; set LUNIX_SPIKE_DIR to a real file system",
            t.path().display()
        );
        eprintln!("SKIP: {} is tmpfs", t.path().display());
        return None;
    }
    Some(t)
}

// ------------------------------------------------------------------ daemon

pub const LUNIXD: &str = env!("CARGO_BIN_EXE_lunixd");

/// A `lunixd` child with `device = "none"`, its state and socket in `dir`.
pub struct Daemon {
    pub child: Option<Child>,
    pub dir: PathBuf,
    pub socket: PathBuf,
    pub log: PathBuf,
    next_id: u64,
}

impl Daemon {
    pub fn config_text(dir: &Path, extra: &str) -> String {
        format!(
            "state_dir = \"{d}/state\"\nsocket = \"{d}/ctl.sock\"\ndev_dir = \"{d}/dev\"\ndevice = \"none\"\nharden = false\nqueues = 2\nargon2_m_kib = 19456\nargon2_t = 2\njournal_max = 268435456\nlog_level = \"debug\"\ncall_timeout_ms = 120000\n{extra}\n",
            d = dir.display()
        )
    }

    pub fn start(dir: &Path, extra: &str) -> Self {
        let cfg = dir.join("lunixd.toml");
        std::fs::write(&cfg, Self::config_text(dir, extra)).unwrap();
        let log = dir.join(format!(
            "lunixd-{}.log",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let out = std::fs::File::create(&log).unwrap();
        let child = Command::new(LUNIXD)
            .arg("--config")
            .arg(&cfg)
            .stdout(Stdio::null())
            .stderr(out)
            .spawn()
            .expect("spawn lunixd");
        let socket = dir.join("ctl.sock");
        let mut d = Self {
            child: Some(child),
            dir: dir.to_path_buf(),
            socket,
            log,
            next_id: 1,
        };
        let start = Instant::now();
        loop {
            if UnixStream::connect(&d.socket).is_ok() {
                break;
            }
            if let Some(st) = d.child.as_mut().unwrap().try_wait().unwrap() {
                panic!("lunixd exited at start ({st}): {}", d.log_text());
            }
            assert!(
                start.elapsed() < Duration::from_secs(60),
                "lunixd did not start: {}",
                d.log_text()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        d
    }

    pub fn log_text(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }

    /// One call on a fresh connection.
    pub fn call(&mut self, req: Request) -> Outcome {
        let t0 = Instant::now();
        let what = format!("{req:?}").chars().take(40).collect::<String>();
        let out = self.call_inner(req);
        if std::env::var_os("LUNIXD_TEST_TIMING").is_some() {
            eprintln!("call {what} took {:?}", t0.elapsed());
        }
        out
    }

    fn call_inner(&mut self, req: Request) -> Outcome {
        self.next_id += 1;
        let frame = RequestFrame::new(self.next_id, req);
        let payload = to_payload(&frame).unwrap();
        let mut s = UnixStream::connect(&self.socket).expect("connect");
        write_frame(&mut s, &payload, MAX_FRAME).unwrap();
        let reply = read_frame(&mut s, MAX_FRAME).expect("reply");
        let r: ResponseFrame = serde_json::from_slice(&reply).expect("response");
        assert_eq!(r.id, self.next_id);
        r.result
    }

    /// Raw bytes in, raw reply out (for malformed-frame tests).
    pub fn raw(&self, bytes: &[u8]) -> Vec<u8> {
        let mut s = UnixStream::connect(&self.socket).expect("connect");
        s.write_all(bytes).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
        // One reply frame (the server keeps the connection open after a
        // recoverable error).
        read_frame(&mut s, MAX_FRAME)
            .map(|z| z.to_vec())
            .unwrap_or_default()
    }

    /// Waits for the child to exit and returns its status.
    pub fn wait_status(&mut self) -> std::process::ExitStatus {
        let mut c = self.child.take().unwrap();
        let start = Instant::now();
        loop {
            if let Some(st) = c.try_wait().unwrap() {
                return st;
            }
            assert!(
                start.elapsed() < Duration::from_secs(120),
                "lunixd did not exit"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// True while the child runs.
    pub fn alive(&mut self) -> bool {
        self.child
            .as_mut()
            .is_some_and(|c| c.try_wait().unwrap().is_none())
    }

    /// Waits for the child to exit and returns its code.
    pub fn wait_exit(&mut self) -> i32 {
        let mut c = self.child.take().unwrap();
        let start = Instant::now();
        loop {
            if let Some(st) = c.try_wait().unwrap() {
                return st.code().unwrap_or(-1);
            }
            assert!(
                start.elapsed() < Duration::from_secs(60),
                "lunixd did not exit"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    pub fn kill(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        self.kill();
    }
}
