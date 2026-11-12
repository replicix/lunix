//! `lunix-ublk-testd`: the root test daemon of the ublk layer (M10).
//!
//! A file-backed [`BlockEngine`](lunix_iface::block::BlockEngine) behind a real
//! ublk device, with the subcommands the phase 0 harness scripts used
//! (`run`, `recover`, `quiesce`, `del`, `features`, `grow`) and fault toggles
//! for the negative controls (`--no-reissue`, the drop-writes toggle).
//!
//! **Root only; it creates real devices.** Never run it by hand on a shared
//! host: the scripts in `tests/root/` run it under `tools/remote.sh root`.
//!
//! ```text
//! lunix-ublk-testd features
//! lunix-ublk-testd run     --state DIR --backing FILE [--create-gib N] [options]
//! lunix-ublk-testd recover --state DIR --backing FILE [options]
//! lunix-ublk-testd quiesce --state DIR [--timeout-ms N]
//! lunix-ublk-testd grow    --state DIR --bytes N
//! lunix-ublk-testd detach  --state DIR
//! lunix-ublk-testd cmd     --state DIR counters | fault on|off | stuck-ctl MS | info
//! lunix-ublk-testd del     --id N [--del-timeout-s N]
//! lunix-ublk-testd crashpoints
//!
//! options: --queues N --depth N --buf-bytes N --cpus LIST --max-gib N
//!          --chunk-kib N --workers N --user-copy --no-reissue --drop-writes
//!          --no-unshare --no-mlock
//! ```
//!
//! A running daemon listens on `DIR/ctl.sock` (one text command per
//! connection, one reply line) and takes signals: `SIGUSR1` prints counters,
//! `SIGHUP` toggles drop-writes, `SIGUSR2` quiesces and exits, `SIGTERM` and
//! `SIGINT` detach (`BUSY` keeps it serving).

mod engine;

use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use engine::FileEngine;
use lunix_format::local::DeviceRecord;
use lunix_format::VolumeUuid;
use lunix_ublk::config::Timeouts;
use lunix_ublk::ctl::{CtlHook, CtlOp, STATE_QUIESCED};
use lunix_ublk::{
    harden_process, ControlPlane, DataPath, Features, Harden, RecoveryPolicy, RingControl,
    UblkConfig, UblkDevice, UblkError,
};

const VOLUME: VolumeUuid = VolumeUuid(*b"lunix-ublk-testd");
const BOOL_FLAGS: [&str; 7] = [
    "user-copy",
    "no-reissue",
    "drop-writes",
    "no-unshare",
    "no-mlock",
    "recover",
    "help",
];

struct Args {
    sub: String,
    kv: HashMap<String, String>,
    flags: HashSet<String>,
    rest: Vec<String>,
}

fn parse_args() -> Result<Args, String> {
    let mut it = std::env::args().skip(1);
    let sub = it.next().ok_or("missing subcommand (try `help`)")?;
    let (mut kv, mut flags, mut rest) = (HashMap::new(), HashSet::new(), Vec::new());
    while let Some(a) = it.next() {
        if let Some(name) = a.strip_prefix("--") {
            if BOOL_FLAGS.contains(&name) {
                flags.insert(name.to_string());
            } else {
                let v = it.next().ok_or_else(|| format!("--{name} needs a value"))?;
                kv.insert(name.to_string(), v);
            }
        } else {
            rest.push(a);
        }
    }
    Ok(Args {
        sub,
        kv,
        flags,
        rest,
    })
}

impl Args {
    fn flag(&self, k: &str) -> bool {
        self.flags.contains(k)
    }

    fn req(&self, k: &str) -> Result<&str, String> {
        self.kv
            .get(k)
            .map(String::as_str)
            .ok_or_else(|| format!("--{k} is required"))
    }

    fn num<T: std::str::FromStr>(&self, k: &str, default: T) -> Result<T, String> {
        match self.kv.get(k) {
            None => Ok(default),
            Some(v) => v.parse().map_err(|_| format!("--{k}: bad number {v:?}")),
        }
    }
}

fn parse_cpus(s: &str) -> Result<Vec<usize>, String> {
    let bad = || format!("bad cpu list {s:?}");
    let mut v = Vec::new();
    for part in s.split(',') {
        if let Some((a, b)) = part.split_once('-') {
            let (a, b): (usize, usize) =
                (a.parse().map_err(|_| bad())?, b.parse().map_err(|_| bad())?);
            v.extend(a..=b);
        } else {
            v.push(part.parse().map_err(|_| bad())?);
        }
    }
    Ok(v)
}

/// What the main thread is asked to do, from a signal or the socket.
enum Cmd {
    Grow(u64, Reply),
    Quiesce {
        timeout_ms: u64,
        attempts: u32,
        reply: Reply,
    },
    Detach(Reply),
    Counters(Reply),
    Fault(Option<bool>, Reply),
    StuckCtl(u64, Reply),
    Info(Reply),
    QueuesEnded,
}

/// Where the answer goes: the socket client, or stdout for a signal.
struct Reply(Option<UnixStream>);

impl Reply {
    fn send(mut self, text: &str) {
        println!("REPLY {text}");
        if let Some(s) = self.0.as_mut() {
            let _ = writeln!(s, "{text}");
            let _ = s.flush();
        }
    }
}

fn state_dir(a: &Args) -> Result<PathBuf, String> {
    Ok(PathBuf::from(a.req("state")?))
}

fn write_record(dir: &Path, rec: &DeviceRecord) -> Result<(), String> {
    let mut bytes = Vec::new();
    rec.encode(&mut bytes).map_err(|e| e.to_string())?;
    let tmp = dir.join("device.tmp");
    let mut f = std::fs::File::create(&tmp).map_err(|e| e.to_string())?;
    f.write_all(&bytes)
        .and_then(|()| f.sync_all())
        .map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, dir.join("device")).map_err(|e| e.to_string())
}

fn block_signals() {
    // SAFETY: a valid, initialised signal set and a null old-set pointer.
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        for s in [
            libc::SIGHUP,
            libc::SIGUSR1,
            libc::SIGUSR2,
            libc::SIGTERM,
            libc::SIGINT,
        ] {
            libc::sigaddset(&mut set, s);
        }
        libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
    }
}

fn spawn_signal_thread(tx: Sender<Cmd>) {
    let _ = std::thread::Builder::new()
        .name("testd-signals".into())
        .spawn(move || {
            // SAFETY: as in `block_signals`; `sigwait` writes the received number.
            let set = unsafe {
                let mut set: libc::sigset_t = std::mem::zeroed();
                libc::sigemptyset(&mut set);
                for s in [
                    libc::SIGHUP,
                    libc::SIGUSR1,
                    libc::SIGUSR2,
                    libc::SIGTERM,
                    libc::SIGINT,
                ] {
                    libc::sigaddset(&mut set, s);
                }
                set
            };
            loop {
                let mut sig = 0;
                // SAFETY: valid set, valid out pointer.
                if unsafe { libc::sigwait(&set, &mut sig) } != 0 {
                    continue;
                }
                let none = || Reply(None);
                let cmd = match sig {
                    libc::SIGUSR1 => Cmd::Counters(none()),
                    libc::SIGHUP => Cmd::Fault(None, none()),
                    libc::SIGUSR2 => Cmd::Quiesce {
                        timeout_ms: 5000,
                        attempts: 3,
                        reply: none(),
                    },
                    _ => Cmd::Detach(none()),
                };
                if tx.send(cmd).is_err() {
                    return;
                }
            }
        });
}

/// A parsed socket command, before it has a reply channel.
enum Req {
    Grow(u64),
    Quiesce(u64),
    Detach,
    Counters,
    Fault(bool),
    StuckCtl(u64),
    Info,
}

fn parse_req(words: &[&str]) -> Option<Req> {
    match words {
        ["grow", n] => n.parse().ok().map(Req::Grow),
        ["quiesce", ms] => ms.parse().ok().map(Req::Quiesce),
        ["detach"] => Some(Req::Detach),
        ["counters"] => Some(Req::Counters),
        ["fault", "on"] => Some(Req::Fault(true)),
        ["fault", "off"] => Some(Req::Fault(false)),
        ["stuck-ctl", ms] => ms.parse().ok().map(Req::StuckCtl),
        ["info"] => Some(Req::Info),
        _ => None,
    }
}

impl Req {
    fn into_cmd(self, reply: Reply) -> Cmd {
        match self {
            Req::Grow(n) => Cmd::Grow(n, reply),
            Req::Quiesce(ms) => Cmd::Quiesce {
                timeout_ms: ms,
                attempts: 1,
                reply,
            },
            Req::Detach => Cmd::Detach(reply),
            Req::Counters => Cmd::Counters(reply),
            Req::Fault(on) => Cmd::Fault(Some(on), reply),
            Req::StuckCtl(ms) => Cmd::StuckCtl(ms, reply),
            Req::Info => Cmd::Info(reply),
        }
    }
}

fn spawn_socket_thread(path: PathBuf, tx: Sender<Cmd>) -> Result<(), String> {
    let _ = std::fs::remove_file(&path);
    let listener =
        UnixListener::bind(&path).map_err(|e| format!("bind {}: {e}", path.display()))?;
    std::thread::Builder::new()
        .name("testd-socket".into())
        .spawn(move || {
            for conn in listener.incoming() {
                let Ok(mut s) = conn else { continue };
                let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
                let mut line = String::new();
                let mut byte = [0u8; 1];
                while let Ok(1) = s.read(&mut byte) {
                    if byte[0] == b'\n' || line.len() > 256 {
                        break;
                    }
                    line.push(char::from(byte[0]));
                }
                let words: Vec<&str> = line.split_whitespace().collect();
                match parse_req(&words) {
                    Some(req) => {
                        if tx.send(req.into_cmd(Reply(Some(s)))).is_err() {
                            return;
                        }
                    }
                    None => Reply(Some(s)).send(&format!("ERR unknown command {line:?}")),
                }
            }
        })
        .map(|_| ())
        .map_err(|e| e.to_string())
}

fn counters_line(dev: &UblkDevice, eng: &FileEngine) -> String {
    let q = dev.queue_counters();
    let sum = |f: fn(&lunix_ublk::QueueSnapshot) -> u64| q.iter().map(f).sum::<u64>();
    format!(
        "COUNTERS reads={} writes={} fua_writes={} flushes={} discards={} write_zeroes={} rejected={} started={} completed={} in_flight={} dropped_writes={} size={}",
        sum(|s| s.reads),
        sum(|s| s.writes),
        sum(|s| s.fua_writes),
        sum(|s| s.flushes),
        sum(|s| s.discards),
        sum(|s| s.write_zeroes),
        sum(|s| s.rejected),
        sum(|s| s.started),
        sum(|s| s.completed),
        dev.in_flight(),
        eng.dropped_writes(),
        dev.size_bytes()
    )
}

fn open_backing(path: &str, create_gib: u64) -> Result<(std::fs::File, u64), String> {
    let f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(create_gib > 0)
        .truncate(false)
        .custom_flags(libc::O_DIRECT)
        .open(path)
        .map_err(|e| format!("open {path}: {e}"))?;
    let mut size = f.metadata().map_err(|e| e.to_string())?.len();
    if create_gib > 0 && size < create_gib << 30 {
        f.set_len(create_gib << 30).map_err(|e| e.to_string())?; // sparse
        size = create_gib << 30;
    }
    if size == 0 || size % 4096 != 0 {
        return Err(format!(
            "backing file size {size} is not a positive multiple of 4096"
        ));
    }
    Ok((f, size))
}

fn run(a: &Args, recover: bool) -> Result<(), String> {
    let t0 = Instant::now();
    let dir = state_dir(a)?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    // Signals are handled by one thread; block them before any other exists.
    block_signals();
    harden_process(Harden {
        mlock: !a.flag("no-mlock"),
        io_flusher: true,
        private_mount_ns: !a.flag("no-unshare"),
    })
    .map_err(|e| format!("hardening the process: {e}"))?;

    let (file, size) = open_backing(a.req("backing")?, a.num("create-gib", 0u64)?)?;
    let eng = FileEngine::new(file, size, a.num("workers", 4usize)?);
    eng.set_drop_writes(a.flag("drop-writes"));

    let mut cfg = UblkConfig::new(VOLUME);
    cfg.queues = a.num("queues", 4u16)?;
    cfg.depth = a.num("depth", 128u16)?;
    cfg.max_io_buf_bytes = a.num("buf-bytes", 1u32 << 20)?;
    cfg.chunk_bytes = a.num("chunk-kib", 64u32)? << 10;
    cfg.max_size_bytes = a.num("max-gib", 64u64)? << 30;
    if a.flag("user-copy") {
        cfg.data_path = DataPath::UserCopy;
    }
    if a.flag("no-reissue") {
        cfg.recovery = RecoveryPolicy::NoReissueNegativeControl;
    }
    if let Some(c) = a.kv.get("cpus") {
        cfg.cpus = parse_cpus(c)?;
    }

    let engine: Arc<FileEngine> = Arc::new(eng.clone());
    let device = if recover {
        let bytes =
            std::fs::read(dir.join("device")).map_err(|e| format!("reading state/device: {e}"))?;
        let rec = DeviceRecord::decode(&bytes).map_err(|e| format!("state/device: {e}"))?;
        println!("RECOVER_FOUND id={}", rec.dev_id);
        match UblkDevice::recover(engine, cfg, &rec) {
            Ok(d) => d,
            Err(UblkError::Identity { dev_id, why }) => {
                println!("REFUSED identity dev_id={dev_id}: {why}");
                std::process::exit(3);
            }
            Err(e) => return Err(format!("recover: {e}")),
        }
    } else {
        UblkDevice::add(engine, cfg).map_err(|e| format!("add: {e}"))?
    };
    write_record(&dir, &device.record())?;
    std::fs::write(dir.join("pid"), std::process::id().to_string()).map_err(|e| e.to_string())?;

    let (tx, rx) = channel::<Cmd>();
    spawn_signal_thread(tx.clone());
    spawn_socket_thread(dir.join("ctl.sock"), tx.clone())?;
    let device = Arc::new(device);
    {
        let (d, tx) = (Arc::clone(&device), tx);
        let _ = std::thread::Builder::new()
            .name("testd-watch".into())
            .spawn(move || {
                d.wait_queues_ended(Duration::from_secs(10 * 365 * 86_400));
                let _ = tx.send(Cmd::QueuesEnded);
            });
    }
    println!(
        "READY id={} bdev={} recovered={} ms={}",
        device.dev_id(),
        device.bdev_path(),
        u8::from(recover),
        t0.elapsed().as_millis()
    );
    let _ = std::io::stdout().flush();

    while let Ok(cmd) = rx.recv() {
        match cmd {
            Cmd::Counters(r) => {
                let line = counters_line(&device, &eng);
                println!("{line}");
                r.send(&line);
            }
            Cmd::Fault(v, r) => {
                let on = v.unwrap_or(!eng.drop_writes());
                eng.set_drop_writes(on);
                println!("FAULT drop_writes={on}");
                r.send(&format!("OK drop_writes={on}"));
            }
            Cmd::Info(r) => r.send(&format!(
                "OK id={} size={} flags={:#x} queues={} depth={}",
                device.dev_id(),
                device.size_bytes(),
                device.info().flags,
                device.info().nr_hw_queues,
                device.info().queue_depth
            )),
            Cmd::Grow(bytes, r) => match device.grow(bytes) {
                Ok(()) => {
                    let rec = write_record(&dir, &device.record());
                    r.send(&format!(
                        "OK size={} record={}",
                        device.size_bytes(),
                        rec.is_ok()
                    ));
                }
                Err(e) => r.send(&format!("ERR {e}")),
            },
            Cmd::StuckCtl(ms, r) => {
                let hook: CtlHook = Arc::new(|op| {
                    if op == CtlOp::GetDevInfo {
                        loop {
                            std::thread::park();
                        }
                    }
                });
                let t = Timeouts {
                    ctl: Duration::from_millis(ms),
                    ..Timeouts::default()
                };
                let stuck = RingControl::new(t).with_hook(hook);
                let started = Instant::now();
                let out = match stuck.dev_info(device.dev_id()) {
                    Err(UblkError::Timeout { op, .. }) => {
                        format!("TIMEOUT op={op} after_ms={}", started.elapsed().as_millis())
                    }
                    other => format!("UNEXPECTED {other:?}"),
                };
                // The daemon keeps serving; show it.
                println!("{out} in_flight={}", device.in_flight());
                r.send(&out);
            }
            Cmd::Quiesce {
                timeout_ms,
                attempts,
                reply,
            } => {
                let mut last = String::new();
                for n in 1..=attempts {
                    match device.quiesce(Duration::from_millis(timeout_ms)) {
                        Ok(q) => {
                            reply.send(&format!(
                                "OK quiesced engine_error={}",
                                q.engine_error().is_some()
                            ));
                            println!("QUIESCE ok attempt={n} ms={}", t0.elapsed().as_millis());
                            let _ = std::io::stdout().flush();
                            // No stop command, ever: the exit closes the cdev.
                            q.exit();
                        }
                        Err(e) => {
                            last = e.to_string();
                            println!("QUIESCE attempt={n} failed: {last}");
                        }
                    }
                }
                if attempts > 1 {
                    // A signal asked for it and the device keeps draining:
                    // crash like kill -9, which REISSUE recovers.
                    println!("QUIESCE giving up; crashing for recovery");
                    device.crash_for_recovery();
                }
                reply.send(&format!("ERR {last}"));
            }
            Cmd::Detach(r) => match device.detach() {
                Ok(()) => {
                    r.send("OK detached");
                    println!("DETACHED");
                    let _ = std::fs::remove_file(dir.join("ctl.sock"));
                    let _ = std::fs::remove_file(dir.join("pid"));
                    #[cfg(feature = "crashpoints")]
                    let _ = lunix_iface::crashpoint::dump_counts();
                    std::process::exit(0);
                }
                Err(UblkError::Busy { .. }) => {
                    println!("BUSY");
                    r.send("BUSY");
                }
                Err(e) => {
                    println!("DETACH failed: {e}");
                    r.send(&format!("ERR {e}"));
                }
            },
            Cmd::QueuesEnded => {
                println!("QUEUES_ENDED (the device was stopped from outside)");
                std::process::exit(0);
            }
        }
    }
    Ok(())
}

fn client(a: &Args, line: &str) -> Result<(), String> {
    let path = state_dir(a)?.join("ctl.sock");
    let mut s =
        UnixStream::connect(&path).map_err(|e| format!("connect {}: {e}", path.display()))?;
    s.set_read_timeout(Some(Duration::from_secs(120)))
        .map_err(|e| e.to_string())?;
    writeln!(s, "{line}").map_err(|e| e.to_string())?;
    let mut out = String::new();
    s.read_to_string(&mut out).map_err(|e| e.to_string())?;
    print!("{out}");
    if out.starts_with("ERR") || out.is_empty() {
        std::process::exit(1);
    }
    if out.starts_with("BUSY") {
        std::process::exit(16);
    }
    Ok(())
}

fn info(id: u32) -> Result<(), String> {
    let ctl = RingControl::new(Timeouts::default());
    let i = ctl.dev_info(id).map_err(|e| e.to_string())?;
    let p = ctl.params(id).map_err(|e| e.to_string())?;
    println!(
        "DEVINFO id={} state={}{} queues={} depth={} max_io_buf={} flags={:#x}",
        i.dev_id,
        i.state,
        if i.state == STATE_QUIESCED {
            "(quiesced)"
        } else {
            ""
        },
        i.nr_hw_queues,
        i.queue_depth,
        i.max_io_buf_bytes,
        i.flags
    );
    println!(
        "PARAMS types={:#x} attrs={:#x} lbs_shift={} pbs_shift={} io_min_shift={} io_opt_shift={} max_sectors={} dev_sectors={} discard_granularity={} max_discard_sectors={}",
        p.types,
        p.basic.attrs,
        p.basic.logical_bs_shift,
        p.basic.physical_bs_shift,
        p.basic.io_min_shift,
        p.basic.io_opt_shift,
        p.basic.max_sectors,
        p.basic.dev_sectors,
        p.discard.discard_granularity,
        p.discard.max_discard_sectors
    );
    Ok(())
}

fn real_main() -> Result<(), String> {
    let a = parse_args()?;
    match a.sub.as_str() {
        "features" => {
            let ctl = RingControl::new(Timeouts::default());
            let f: Features = ctl.features().map_err(|e| e.to_string())?;
            print!("{}", f.render_table());
            match f.require_mandatory() {
                Ok(()) => println!("\nmandatory flags: all present"),
                Err(e) => {
                    println!("\nREFUSED: {e}");
                    std::process::exit(2);
                }
            }
            Ok(())
        }
        "run" => run(&a, a.flag("recover")),
        "recover" => run(&a, true),
        "quiesce" => client(&a, &format!("quiesce {}", a.num("timeout-ms", 5000u64)?)),
        "grow" => client(&a, &format!("grow {}", a.req("bytes")?)),
        "detach" => client(&a, "detach"),
        "cmd" => client(&a, &a.rest.join(" ")),
        "del" => {
            let id: u32 = a.num("id", u32::MAX)?;
            if id == u32::MAX {
                return Err("--id is required".into());
            }
            let t = Timeouts {
                del: Duration::from_secs(a.num("del-timeout-s", 60u64)?),
                ..Timeouts::default()
            };
            match UblkDevice::delete_by_id(id, t) {
                Ok(()) => {
                    println!("DEL_DEV result=0");
                    Ok(())
                }
                Err(e) => {
                    println!("DEL_DEV failed: {e}");
                    std::process::exit(if matches!(e, UblkError::Timeout { .. }) {
                        124
                    } else {
                        1
                    });
                }
            }
        }
        "info" => info(a.num("id", 0u32)?),
        "crashpoints" => {
            for n in lunix_iface::crashpoint::registered() {
                println!("{n}");
            }
            Ok(())
        }
        "help" | "--help" => {
            println!("see the module documentation of lunix-ublk-testd");
            Ok(())
        }
        other => Err(format!("unknown subcommand {other:?}")),
    }
}

fn main() {
    if let Err(e) = real_main() {
        eprintln!("lunix-ublk-testd: {e}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lunix_ublk::IdentityMismatch;

    #[test]
    fn cpu_lists_parse() {
        assert_eq!(parse_cpus("0-3").unwrap(), vec![0, 1, 2, 3]);
        assert_eq!(parse_cpus("4,6-7").unwrap(), vec![4, 6, 7]);
        assert!(parse_cpus("a").is_err());
    }

    #[test]
    fn identity_mismatch_text_is_printable() {
        assert!(!IdentityMismatch::BootId.to_string().is_empty());
    }
}
