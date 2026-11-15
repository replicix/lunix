//! `lunixd`: the Lunix daemon.
//!
//! ```text
//! lunixd [--config PATH]          serve (default config /etc/lunix/lunixd.toml)
//! lunixd --list-crashpoints       print the crash points of this build
//! lunixd ctl --socket PATH JSON   send one raw control request, print the reply
//! lunixd check --dir VOLDIR       offline store self-check of a detached volume
//! ```
//!
//! `ctl` is a minimal client for scripts and tests until `lunix` (M13a) is
//! merged: `JSON` is a request object such as `{"method":"ping"}`; `v` and
//! `id` are filled in when absent. A request that carries a secret
//! (`passphrase`) is refused on the command line, where the process list
//! shows it: pass `-` (or nothing) and write the request to stdin. The exit
//! code is 0 on success, else the error code's (`ErrorCode::exit_code`).

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
        /// The request, as JSON; `-` or absent reads it from stdin.
        request: Option<String>,
    },
    /// Offline self-check of a detached volume (stand-in for `lunix fsck`).
    Check {
        /// The volume directory (`<dir>/<uuid>`).
        #[arg(long)]
        dir: PathBuf,
        /// A key file (encrypted volumes, or integrity volumes without
        /// their own `integrity.key`).
        #[arg(long)]
        key_file: Option<PathBuf>,
    },
}

fn ctl(socket: &std::path::Path, request: Option<&str>) -> anyhow::Result<u8> {
    let text = match request {
        Some(r) if r != "-" => {
            if r.contains("passphrase") {
                anyhow::bail!("a request with a passphrase must come on stdin (`lunixd ctl -`), not the command line");
            }
            r.to_owned()
        }
        _ => {
            let mut t = String::new();
            std::io::Read::read_to_string(&mut std::io::stdin(), &mut t)?;
            t
        }
    };
    let mut v: serde_json::Value = serde_json::from_str(&text).context("request is not JSON")?;
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
    // flag are inherited. Core dumps are off in every build (a dump keeps
    // /dev/ublkcN open and holds key material). No mlockall (maintainer,
    // 2026-11-15): the I/O path's regions are locked as they are created
    // (queue buffers and stacks, journal buffers and flush-thread stack), the
    // key arena here; a process that cannot lock them does not start.
    harden_process(Harden {
        mlock: cfg.harden,
        io_flusher: cfg.harden,
        private_mount_ns: cfg.harden,
        no_core_dumps: true,
    })
    .context(
        "process hardening (memory locking needs CAP_IPC_LOCK or an unlimited RLIMIT_MEMLOCK)",
    )?;
    if cfg.harden {
        let arena = lunix_crypto::require_locked()
            .map_err(|e| anyhow::anyhow!(e))
            .context("key arena")?;
        tracing::info!(
            key_arena_locked_bytes = arena.locked_bytes(),
            "memory locking: key arena locked; queue and journal buffers are locked at attach"
        );
    }
    if cfg.metrics.is_some() {
        tracing::warn!(
            "metrics are deferred in phase 1 (plan M12a step 9); the address is ignored"
        );
    }
    std::fs::create_dir_all(&cfg.state_dir)
        .with_context(|| format!("state dir {}", cfg.state_dir.display()))?;
    let daemon = Arc::new(Daemon::new(cfg)?);
    // Bind first (nothing is accepted yet), then the signal handlers, then
    // recovery (M12a review m2): a failed bind recovers nothing, and a
    // SIGTERM during recovery is a quiesce.
    let listener = lunixd::server::bind(&daemon.config().socket, daemon.config().socket_group)
        .context("control socket")?;
    tracing::info!(socket = %daemon.config().socket.display(), "lunixd started");
    daemon.spawn_idle_trim();
    let code = lunixd::server::run(Arc::clone(&daemon), listener, |d| d.recover_on_start())?;
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
        Some(Cmd::Check { dir, key_file }) => {
            StderrLog::install(tracing::Level::WARN);
            let cred = lunixd::stack::AttachCredential {
                key_file: key_file.clone(),
                ..Default::default()
            };
            match lunixd::check::check_volume(dir, &cred) {
                Ok(r) => {
                    println!(
                        "check: tip {} commits {} segments {} packs {} extents {} problems {}",
                        r.tip,
                        r.commits,
                        r.segments,
                        r.packs,
                        r.extents,
                        r.problems.len()
                    );
                    for p in &r.problems {
                        println!("problem: {p}");
                    }
                    if r.is_clean() {
                        println!("check: clean");
                        ExitCode::SUCCESS
                    } else {
                        ExitCode::from(4)
                    }
                }
                Err(e) => {
                    eprintln!("lunixd check: {e}");
                    ExitCode::from(1)
                }
            }
        }
        Some(Cmd::Ctl { socket, request }) => match ctl(socket, request.as_deref()) {
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
