//! `lunixd`: the Lunix daemon.
//!
//! ```text
//! lunixd [--config PATH]          serve (default config /etc/lunix/lunixd.toml)
//! lunixd --list-crashpoints       print the crash points of this build
//! lunixd ctl --socket PATH JSON   send one raw control request, print the reply
//! ```
//!
//! `ctl` is a minimal client for scripts and tests until `lunix` (M13a) is
//! merged: `JSON` is a request object such as `{"method":"ping"}`; `v` and
//! `id` are filled in when absent. The exit code is 0 on success, else the
//! error code's (`ErrorCode::exit_code`).

#![forbid(unsafe_code)]

use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use anyhow::Context;
use clap::{Parser, Subcommand};
use lunix_iface::ctl::{
    read_frame, write_frame, Outcome, ResponseFrame, MAX_FRAME, PROTOCOL_VERSION,
};
use lunix_ublk::{harden_process, Harden};
use lunixd::config::{DaemonConfig, DEFAULT_CONFIG};
use lunixd::daemon::Daemon;
use lunixd::log::StderrLog;

#[derive(Parser, Debug)]
#[command(name = "lunixd", version, about = "The Lunix daemon")]
struct Cli {
    /// Configuration file.
    #[arg(long)]
    config: Option<PathBuf>,
    /// Print the crash points of this build and exit.
    #[arg(long)]
    list_crashpoints: bool,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Send one raw control request and print the reply.
    Ctl {
        /// The control socket.
        #[arg(long, default_value = "/run/lunix/ctl.sock")]
        socket: PathBuf,
        /// The request, as JSON.
        request: String,
    },
}

fn ctl(socket: &std::path::Path, request: &str) -> anyhow::Result<u8> {
    let mut v: serde_json::Value = serde_json::from_str(request).context("request is not JSON")?;
    let obj = v.as_object_mut().context("request must be a JSON object")?;
    obj.entry("v").or_insert(PROTOCOL_VERSION.into());
    obj.entry("id").or_insert(1.into());
    let payload = serde_json::to_vec(&v)?;
    let mut s =
        UnixStream::connect(socket).with_context(|| format!("connect {}", socket.display()))?;
    write_frame(&mut s, &payload, MAX_FRAME)?;
    let reply = read_frame(&mut s, MAX_FRAME)?;
    let text = String::from_utf8_lossy(&reply);
    println!("{text}");
    let frame: ResponseFrame = serde_json::from_slice(&reply).context("reply is not a response")?;
    Ok(match frame.result {
        Outcome::Ok(_) => 0,
        Outcome::Err(e) => e.code.exit_code(),
    })
}

fn serve(cli: &Cli) -> anyhow::Result<i32> {
    let (path, explicit) = match &cli.config {
        Some(p) => (p.clone(), true),
        None => (PathBuf::from(DEFAULT_CONFIG), false),
    };
    let cfg = DaemonConfig::load(&path, explicit)?;
    StderrLog::install(cfg.log_level);
    // Before any other thread exists: the mount namespace and the I/O-flusher
    // flag are inherited, mlockall covers what follows. Core dumps are off in
    // every build (a dump keeps /dev/ublkcN open and holds key material).
    harden_process(Harden {
        mlock: cfg.harden,
        io_flusher: cfg.harden,
        private_mount_ns: cfg.harden,
        no_core_dumps: true,
    })
    .context("process hardening")?;
    if cfg.metrics.is_some() {
        tracing::warn!(
            "metrics are deferred in phase 1 (plan M12a step 9); the address is ignored"
        );
    }
    std::fs::create_dir_all(&cfg.state_dir)
        .with_context(|| format!("state dir {}", cfg.state_dir.display()))?;
    let daemon = Arc::new(Daemon::new(cfg)?);
    daemon.recover_on_start();
    let listener = lunixd::server::bind(&daemon.config().socket, daemon.config().socket_group)
        .context("control socket")?;
    tracing::info!(socket = %daemon.config().socket.display(), "lunixd started");
    let code = lunixd::server::run(Arc::clone(&daemon), listener)?;
    let _ = std::fs::remove_file(&daemon.config().socket);
    Ok(code)
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    if cli.list_crashpoints {
        let points = lunixd::crash_points();
        if points.is_empty() {
            eprintln!("lunixd: built without the crashpoints feature");
            return ExitCode::from(2);
        }
        for p in points {
            println!("{p}");
        }
        return ExitCode::SUCCESS;
    }
    match &cli.cmd {
        Some(Cmd::Ctl { socket, request }) => match ctl(socket, request) {
            Ok(code) => ExitCode::from(code),
            Err(e) => {
                eprintln!("lunixd ctl: {e:#}");
                ExitCode::from(1)
            }
        },
        None => match serve(&cli) {
            // Exit without running destructors: after a quiesce nothing may
            // touch a device again, and engines must not commit on drop.
            Ok(code) => std::process::exit(code),
            Err(e) => {
                eprintln!("lunixd: {e:#}");
                ExitCode::from(1)
            }
        },
    }
}
