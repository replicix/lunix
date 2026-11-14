//! Shared helpers of the integration tests.
#![allow(dead_code)]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use lunix_iface::ctl::{
    Attached, CtlError, ErrorCode, Outcome, Response, SpaceFigures, VolumeMode, VolumeState,
    VolumeStatusBody,
};
use lunix_iface::stats::EngineStats;
use lunix_testkit::FakeCtlServer;
use tempfile::TempDir;

pub const BIN: &str = env!("CARGO_BIN_EXE_lunix");

/// A temp dir, a fake daemon in it and helpers to run the binary against it.
pub struct Env {
    // The server must drop first: its shutdown connects to the socket path.
    pub server: FakeCtlServer,
    pub dir: TempDir,
}

impl Env {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let server = FakeCtlServer::start(&dir.path().join("ctl.sock")).expect("fake server");
        Self { server, dir }
    }

    pub fn socket(&self) -> PathBuf {
        self.server.path().to_owned()
    }

    pub fn run(&self, args: &[&str]) -> Out {
        run_with(&self.socket(), args, None, self.dir.path())
    }

    pub fn run_stdin(&self, args: &[&str], stdin: &[u8]) -> Out {
        run_with(&self.socket(), args, Some(stdin), self.dir.path())
    }

    pub fn ok(&self, method: &str, body: Response) {
        self.server.respond(method, Outcome::Ok(body));
    }

    pub fn err(&self, method: &str, code: ErrorCode, msg: &str) {
        self.server
            .respond(method, Outcome::Err(CtlError::new(code, msg)));
    }
}

/// A finished run.
pub struct Out {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

pub fn run_with(socket: &Path, args: &[&str], stdin: Option<&[u8]>, cwd: &Path) -> Out {
    let mut cmd = Command::new(BIN);
    cmd.arg("--socket")
        .arg(socket)
        .args(args)
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });
    let mut child = cmd.spawn().expect("spawn lunix");
    if let Some(data) = stdin {
        let mut si = child.stdin.take().expect("stdin");
        si.write_all(data).expect("write stdin");
    }
    finish(child.wait_with_output().expect("wait"))
}

pub fn finish(o: Output) -> Out {
    Out {
        code: o.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
    }
}

/// Runs without a socket (for `--help` and usage errors).
pub fn run_bare(args: &[&str]) -> Out {
    let o = Command::new(BIN)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("run lunix");
    finish(o)
}

/// Compares `text` with `tests/snapshots/<name>`; `LUNIX_UPDATE_SNAPSHOTS=1`
/// rewrites it.
pub fn assert_snapshot(name: &str, text: &str) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/snapshots")
        .join(name);
    if std::env::var("LUNIX_UPDATE_SNAPSHOTS").as_deref() == Ok("1") {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, text).unwrap();
        return;
    }
    let want = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!("snapshot {name} missing ({e}); run with LUNIX_UPDATE_SNAPSHOTS=1")
    });
    assert_eq!(want, text, "snapshot {name} differs");
}

pub fn status_body(name: &str, mode: VolumeMode) -> VolumeStatusBody {
    VolumeStatusBody {
        name: name.to_owned(),
        uuid: "11111111-2222-3333-4444-555555555555".to_owned(),
        state: VolumeState::Attached,
        size_bytes: 200 << 30,
        chunk_size: 65536,
        mode,
        device: Some(format!("/dev/lunix/{name}")),
        stats: EngineStats::default(),
        space: SpaceFigures::default(),
        journal_used: 4096,
        journal_capacity: 1 << 30,
        last_commit_seq: 12,
        last_commit_ms: Some(1_794_652_830_000),
        dedup_ratio: Some(2.5),
        compression_ratio: None,
        rollback_floor: Some(lunix_iface::ctl::RollbackFloorView {
            max_commit_seq: 12,
            tip_hash: "ab".repeat(32),
        }),
    }
}

pub fn attached() -> Attached {
    Attached {
        device: "/dev/lunix/vol1".to_owned(),
        dev_id: 3,
        queues: 4,
        depth: 128,
        recovered: false,
        first_attach: false,
    }
}
