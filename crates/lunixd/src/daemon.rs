//! The daemon's state and the control-call handlers (plan M12a steps 3-8).
//!
//! Every handler blocks (file I/O, ublk control commands, engine commits) and
//! runs on a blocking thread of the control server, never on a tokio worker.
//! One mutex per volume serialises the calls on that volume; the registry has
//! its own.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

use lunix_crypto::{Argon2Cost, AttachError};
use lunix_format::commit::Commit;
use lunix_format::local::{DeviceRecord, RegistryEntry, VolumeJson};
use lunix_format::paths::local as lpath;
use lunix_format::volume::{CodecPolicy, VolumeDescriptor};
use lunix_format::{CommitSeq, CryptoMode, IdLen};
use lunix_iface::block::BlockEngine;
use lunix_iface::crypto::CommitCodec;
use lunix_iface::ctl::{
    Attach, Attached, Compress, CtlError, ErrorCode, GcRequest, NameParam, Request, Resize,
    Response, SpaceBody, SpaceFigures, SpaceRequest, VolumeCreate, VolumeCreated, VolumeMode,
    VolumeState, VolumeStatusBody, VolumeSummary, PROTOCOL_VERSION,
};
use lunix_iface::gc::GcStatusSnapshot;
use lunix_iface::space::{SpaceEvents, SpaceGate};
use lunix_iface::store::VolumeDir;
use lunix_iface::ErrorKind;
use lunix_ublk::config::Timeouts;
use lunix_ublk::{current_boot_id, UblkConfig, UblkError, DEFAULT_DEPTH};

use crate::audit::{Audit, Peer};
use crate::config::{DaemonConfig, DeviceKind};
use crate::device::{start_ublk, Device, DeviceError, DeviceParams, NullDevice, Started};
use crate::registry::Registry;
use crate::stack::{
    create_volume, open_volume, unlock, AttachCredential, CreateParams, OpenParams, StackError,
    VolumeStack,
};
use crate::{fsio, lock};

/// A volume the daemon serves.
struct Live {
    // Dropped first: the device holds the engine.
    device: Device,
    stack: VolumeStack,
    link: Option<PathBuf>,
}

#[derive(Default)]
enum Slot {
    #[default]
    Detached,
    Attached(Box<Live>),
}

/// What a handled call asks the server to do after replying.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum After {
    /// Keep serving.
    Continue,
    /// Exit with this code once the reply is written (`QuiesceAll`).
    Exit(i32),
}

/// The daemon.
pub struct Daemon {
    cfg: DaemonConfig,
    registry: Mutex<Registry>,
    slots: Mutex<HashMap<String, Arc<Mutex<Slot>>>>,
    audit: Audit,
    exiting: AtomicBool,
}

impl std::fmt::Debug for Daemon {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Daemon")
            .field("socket", &self.cfg.socket)
            .finish_non_exhaustive()
    }
}

fn err(code: ErrorCode, msg: impl Into<String>) -> CtlError {
    CtlError::new(code, msg)
}

/// The control error for a stack failure. Rollback and fork have no code of
/// their own in the protocol yet (see the M12a report, iface gaps): they use
/// `downgrade` with messages that start with `rollback:` and `fork:`.
pub fn stack_error(e: &StackError) -> CtlError {
    let code = match e {
        StackError::Attach(a) => match a {
            AttachError::Downgrade | AttachError::BadMac | AttachError::KeyringRollback => {
                ErrorCode::Downgrade
            }
            AttachError::WrongKmk | AttachError::WrongCredential => ErrorCode::WrongKmk,
            AttachError::NeedCredential
            | AttachError::ModeMismatch
            | AttachError::Unsupported(_) => ErrorCode::Invalid,
            AttachError::Format(_) => ErrorCode::Io,
            _ => ErrorCode::Internal,
        },
        StackError::Rollback { .. } | StackError::Fork { .. } => ErrorCode::Downgrade,
        StackError::JournalDamage => ErrorCode::Io,
        StackError::Invalid(_) => ErrorCode::Invalid,
        StackError::Store(s) => store_code(s.kind()),
        StackError::Open(o) => match o {
            lunix_engine::OpenError::Config(_) => ErrorCode::Invalid,
            _ => ErrorCode::Io,
        },
    };
    err(code, e.to_string())
}

fn store_code(k: ErrorKind) -> ErrorCode {
    match k {
        ErrorKind::Busy => ErrorCode::Busy,
        ErrorKind::NotFound => ErrorCode::NotFound,
        ErrorKind::NoSpace => ErrorCode::NoSpace,
        ErrorKind::Invalid | ErrorKind::Unsupported => ErrorCode::Invalid,
        _ => ErrorCode::Io,
    }
}

fn device_error(e: &DeviceError) -> CtlError {
    match e {
        DeviceError::Busy => err(ErrorCode::Busy, e.to_string()),
        DeviceError::Ublk(u) => {
            let code = match u.root() {
                UblkError::Busy { .. } => ErrorCode::Busy,
                UblkError::InvalidSize { .. } | UblkError::Config(_) => ErrorCode::Invalid,
                _ => ErrorCode::Io,
            };
            err(code, e.to_string())
        }
        DeviceError::Engine(x) => err(store_code(x.kind()), e.to_string()),
        DeviceError::Other(_) => err(ErrorCode::Internal, e.to_string()),
    }
}

/// Options of an attach.
#[derive(Default, Debug)]
pub struct AttachOpts {
    queues: Option<u16>,
    depth: Option<u16>,
    cpus: Option<String>,
    max_io_buf_bytes: Option<u32>,
    credential: AttachCredential,
}

impl AttachOpts {
    fn from_request(a: Attach) -> Result<Self, CtlError> {
        let mut credential = AttachCredential::default();
        if let Some(c) = a.credential {
            credential.passphrase = c.passphrase;
            credential.key_file = c.key_file.map(PathBuf::from);
            credential.fingerprint = match c.fingerprint {
                None => None,
                Some(hex) => Some(parse_fingerprint(&hex)?),
            };
        }
        Ok(Self {
            queues: a.queues,
            depth: a.depth,
            cpus: a.cpus,
            max_io_buf_bytes: a.max_io_buf_bytes,
            credential,
        })
    }
}

fn parse_fingerprint(hex: &str) -> Result<[u8; 16], CtlError> {
    let bad = || {
        err(
            ErrorCode::Invalid,
            "fingerprint must be 32 lowercase hex digits",
        )
    };
    if hex.len() != 32 {
        return Err(bad());
    }
    let mut out = [0u8; 16];
    for (i, o) in out.iter_mut().enumerate() {
        let pair = hex.get(2 * i..2 * i + 2).ok_or_else(bad)?;
        *o = u8::from_str_radix(pair, 16).map_err(|_| bad())?;
    }
    Ok(out)
}

fn parse_cpus(s: &str) -> Result<Vec<usize>, CtlError> {
    let bad = || err(ErrorCode::Invalid, "bad cpu list");
    let mut v = Vec::new();
    for part in s.split(',') {
        if let Some((a, b)) = part.split_once('-') {
            let (a, b): (usize, usize) =
                (a.parse().map_err(|_| bad())?, b.parse().map_err(|_| bad())?);
            if b < a || b - a > 4096 {
                return Err(bad());
            }
            v.extend(a..=b);
        } else {
            v.push(part.parse().map_err(|_| bad())?);
        }
    }
    Ok(v)
}

/// The lanes the journal directory already has (`q<lane:02x>-*.j`).
fn journal_lanes(voldir: &Path) -> Option<u16> {
    let rd = std::fs::read_dir(voldir.join("journal")).ok()?;
    let mut max: Option<u16> = None;
    for e in rd.flatten() {
        let name = e.file_name();
        let Some(n) = name.to_str() else { continue };
        let Some(rest) = n.strip_prefix('q') else {
            continue;
        };
        if !n.ends_with(".j") {
            continue;
        }
        let Some(lane) = rest.get(..2).and_then(|h| u16::from_str_radix(h, 16).ok()) else {
            continue;
        };
        max = Some(max.map_or(lane, |m| m.max(lane)));
    }
    max.map(|m| m + 1)
}

/// `state/device`, read without the volume lock (the queue count must be
/// known before the journal is opened).
fn device_record_unlocked(voldir: &Path) -> Option<DeviceRecord> {
    let bytes = fsio::read_capped(&voldir.join(lpath::state_device()), 4096).ok()??;
    DeviceRecord::decode(&bytes).ok()
}

/// The newest commit in `state/commits/`.
fn tip_on_disk(voldir: &Path) -> Option<CommitSeq> {
    let rd = std::fs::read_dir(voldir.join("state/commits")).ok()?;
    rd.flatten()
        .filter_map(|e| {
            let n = e.file_name();
            let s = n.to_str()?;
            (s.len() == 16).then(|| u64::from_str_radix(s, 16).ok())?
        })
        .max()
        .map(CommitSeq)
}

/// What can be learned about a detached volume without its lock.
struct Peek {
    desc: VolumeDescriptor,
    size: u64,
    tip: Option<CommitSeq>,
}

fn peek(voldir: &Path) -> Result<Peek, CtlError> {
    let bytes = lunix_store::read_volume_file(voldir)
        .map_err(|e| err(store_code(e.kind()), format!("volume descriptor: {e}")))?;
    let desc = VolumeDescriptor::decode(&bytes)
        .map_err(|e| err(ErrorCode::Io, format!("volume descriptor: {e}")))?;
    let tip = tip_on_disk(voldir);
    // The size lives in the commits; it is read when the volume unlocks
    // without a credential (plain, or integrity with its own key file).
    let size = tip
        .and_then(|t| {
            let ok = unlock(voldir, &desc, &AttachCredential::default()).ok()?;
            let b = std::fs::read(voldir.join(lpath::commit(t))).ok()?;
            match ok.crypto.meta().open_commit(t, &b).ok()? {
                Commit::Full(f) => Some(f.size),
                _ => None,
            }
        })
        .unwrap_or(0);
    Ok(Peek { desc, size, tip })
}

fn ratio(num: u64, den: u64) -> Option<f64> {
    (den > 0 && num > 0).then(|| num as f64 / den as f64)
}

impl Daemon {
    /// A daemon over `cfg`, with the registry loaded (or rebuilt from
    /// `volume_roots` when it does not parse).
    ///
    /// # Errors
    /// The registry could neither be read nor rebuilt.
    pub fn new(cfg: DaemonConfig) -> anyhow::Result<Self> {
        let path = cfg.registry();
        let registry = match Registry::load(&path) {
            Ok(r) => r,
            Err(e) => {
                tracing::error!(error = %e, "registry unreadable; rebuilding it from volume_roots");
                let (r, skipped) = Registry::rebuild(&path, &cfg.volume_roots);
                for s in skipped {
                    tracing::warn!(entry = %s, "registry rebuild skipped");
                }
                r.save()?;
                r
            }
        };
        Ok(Self {
            audit: Audit::new(cfg.audit_log()),
            cfg,
            registry: Mutex::new(registry),
            slots: Mutex::new(HashMap::new()),
            exiting: AtomicBool::new(false),
        })
    }

    /// The configuration.
    pub fn config(&self) -> &DaemonConfig {
        &self.cfg
    }

    fn slot(&self, name: &str) -> Arc<Mutex<Slot>> {
        Arc::clone(lock(&self.slots).entry(name.to_owned()).or_default())
    }

    fn entry(&self, name: &str) -> Result<RegistryEntry, CtlError> {
        lock(&self.registry)
            .find(name)
            .cloned()
            .ok_or_else(|| err(ErrorCode::NotFound, format!("no volume named {name:?}")))
    }

    /// Handles one validated call and audit-logs it when it changes
    /// something.
    pub fn handle(&self, peer: Peer, req: Request) -> (Result<Response, CtlError>, After) {
        let mutating = crate::audit::is_mutating(&req);
        let audit_req = mutating.then(|| req.clone());
        let (res, after) = if self.exiting.load(Ordering::Acquire) {
            (
                Err(err(ErrorCode::Busy, "the daemon is shutting down")),
                After::Continue,
            )
        } else {
            self.dispatch(req)
        };
        if let Some(r) = audit_req {
            self.audit.record(
                peer,
                &r,
                res.as_ref()
                    .map(|_| ())
                    .map_err(|e| (e.code, e.message.as_str())),
            );
        }
        (res, after)
    }

    fn dispatch(&self, req: Request) -> (Result<Response, CtlError>, After) {
        let res = match req {
            Request::Ping => Ok(Response::Pong {
                version: PROTOCOL_VERSION,
                daemon: env!("CARGO_PKG_VERSION").to_owned(),
            }),
            Request::VolumeCreate(c) => self.create(c),
            Request::VolumeList => self.list(),
            Request::VolumeStatus(p) => self.status(&p),
            Request::Attach(a) => self.attach(a),
            Request::Detach(p) => self.detach(&p),
            Request::Resize(r) => self.resize(&r),
            Request::Space(s) => self.space(&s),
            Request::QuiesceAll => return self.quiesce_all(),
            Request::Gc(g) => self.gc(&g),
            Request::GcStatus(p) => self.gc_status(&p),
        };
        (res, After::Continue)
    }

    // ------------------------------------------------------------ create

    fn create(&self, c: VolumeCreate) -> Result<Response, CtlError> {
        let mut reg = lock(&self.registry);
        if reg.find(&c.name).is_some() {
            return Err(err(
                ErrorCode::Invalid,
                format!("a volume named {:?} exists", c.name),
            ));
        }
        let (policy, level) = match c.compress {
            Compress::Auto => (CodecPolicy::Auto, 1),
            Compress::Lz4 => (CodecPolicy::Lz4, 0),
            Compress::Zstd { level } => (CodecPolicy::Zstd, level),
            Compress::None => (CodecPolicy::None, 0),
        };
        let id_len = IdLen::new(c.id_len).map_err(|_| err(ErrorCode::Invalid, "id_len"))?;
        let mode = CryptoMode::from(c.mode);
        let key_file = c.key_file.as_deref().map(Path::new);
        let pass = c.passphrase.as_ref().map(|s| s.expose());
        let (jmax, interval) = (self.cfg.journal_max, self.cfg.commit_interval_ms);
        let tweak = move |j: &mut VolumeJson| {
            if let Some(m) = jmax {
                j.journal_max = m;
            }
            if let Some(i) = interval {
                j.commit.interval_ms = i;
            }
        };
        let params = CreateParams {
            name: &c.name,
            parent: Path::new(&c.dir),
            size_bytes: c.size_bytes,
            chunk_size: c.chunk_size,
            mode,
            policy,
            level,
            id_len,
            map_fanout: c.map_fanout.unwrap_or(512),
            key_file,
            passphrase: pass,
            argon2: Argon2Cost {
                m_kib: self.cfg.argon2_m_kib,
                t: self.cfg.argon2_t,
                p: 1,
            },
            json: Some(&tweak),
        };
        let created = create_volume(&params).map_err(|e| stack_error(&e))?;
        let entry = RegistryEntry {
            name: c.name.clone(),
            uuid: created.uuid.to_string(),
            directory: created.voldir.to_string_lossy().into_owned(),
        };
        if let Err(e) = reg.insert(entry).and_then(|()| reg.save()) {
            reg.remove(&c.name);
            let _ = std::fs::remove_dir_all(&created.voldir);
            return Err(err(ErrorCode::Io, format!("registry: {e}")));
        }
        if mode == CryptoMode::Plain {
            tracing::warn!(volume = %c.name, "plain volume: corruption detection only, no protection against anyone who can write its storage");
        }
        Ok(Response::VolumeCreated(VolumeCreated {
            name: c.name,
            uuid: created.uuid.to_string(),
            dir: created.voldir.to_string_lossy().into_owned(),
            mode: c.mode,
            key_file: created
                .generated_key_file
                .map(|p| p.to_string_lossy().into_owned()),
        }))
    }

    // ------------------------------------------------------------ list, status

    fn device_path(&self, name: &str, live: &Live) -> Option<String> {
        live.link
            .as_ref()
            .map(|l| l.to_string_lossy().into_owned())
            .or_else(|| live.device.bdev_path())
            .or_else(|| (self.cfg.device == DeviceKind::None).then(|| format!("none:{name}")))
    }

    fn list(&self) -> Result<Response, CtlError> {
        let entries: Vec<RegistryEntry> = lock(&self.registry).entries().to_vec();
        let mut volumes = Vec::with_capacity(entries.len());
        for e in entries {
            let slot = self.slot(&e.name);
            let g = lock(&slot);
            let (size, mode, device) = match &*g {
                Slot::Attached(l) => (
                    l.stack.engine.size_bytes(),
                    l.stack.desc.crypto_mode,
                    self.device_path(&e.name, l),
                ),
                Slot::Detached => match peek(Path::new(&e.directory)) {
                    Ok(p) => (p.size, p.desc.crypto_mode, None),
                    Err(_) => continue,
                },
            };
            volumes.push(VolumeSummary {
                name: e.name,
                uuid: e.uuid,
                dir: e.directory,
                size_bytes: size,
                mode: VolumeMode::from(mode),
                device,
            });
        }
        Ok(Response::VolumeList { volumes })
    }

    fn status(&self, p: &NameParam) -> Result<Response, CtlError> {
        let e = self.entry(&p.name)?;
        let slot = self.slot(&p.name);
        let g = lock(&slot);
        let body = match &*g {
            Slot::Attached(l) => {
                let s = &l.stack;
                let stats = s.engine.stats();
                let fill = lunix_iface::journal::Journal::fill(&*s.journal);
                let (tip, _) = s.engine.tip();
                let read_only = s.space.is_read_only() || s.engine.is_stopped();
                let written = stats.chunks_total.saturating_sub(stats.chunks_zero);
                VolumeStatusBody {
                    name: p.name.clone(),
                    uuid: e.uuid,
                    state: if read_only {
                        VolumeState::ReadOnly
                    } else {
                        VolumeState::Attached
                    },
                    size_bytes: s.engine.size_bytes(),
                    chunk_size: s.desc.chunk_size,
                    mode: s.desc.crypto_mode.into(),
                    device: self.device_path(&p.name, l),
                    space: SpaceFigures::from(&s.space.snapshot()),
                    journal_used: fill.used_bytes,
                    journal_capacity: fill.capacity_bytes,
                    last_commit_seq: tip.0,
                    last_commit_ms: None,
                    dedup_ratio: ratio(written, written.saturating_sub(stats.dedup_hits)),
                    compression_ratio: ratio(stats.stored_ulen, stats.stored_clen),
                    stats,
                }
            }
            Slot::Detached => {
                let pk = peek(Path::new(&e.directory))?;
                VolumeStatusBody {
                    name: p.name.clone(),
                    uuid: e.uuid,
                    state: VolumeState::Detached,
                    size_bytes: pk.size,
                    chunk_size: pk.desc.chunk_size,
                    mode: pk.desc.crypto_mode.into(),
                    device: None,
                    stats: Default::default(),
                    space: SpaceFigures::default(),
                    journal_used: 0,
                    journal_capacity: 0,
                    last_commit_seq: pk.tip.map_or(0, |t| t.0),
                    last_commit_ms: None,
                    dedup_ratio: None,
                    compression_ratio: None,
                }
            }
        };
        Ok(Response::VolumeStatus(Box::new(body)))
    }

    // ------------------------------------------------------------ attach

    fn attach(&self, a: Attach) -> Result<Response, CtlError> {
        let e = self.entry(&a.name)?;
        let name = a.name.clone();
        let opts = AttachOpts::from_request(a)?;
        let slot = self.slot(&name);
        let mut g = lock(&slot);
        if matches!(*g, Slot::Attached(_)) {
            return Err(err(ErrorCode::Busy, format!("{name} is already attached")));
        }
        let (live, recovered) = self.bring_up(&name, Path::new(&e.directory), opts)?;
        let (queues, depth) = live.device.geometry();
        let resp = Attached {
            device: self.device_path(&name, &live).unwrap_or_default(),
            dev_id: live.device.dev_id().unwrap_or(0),
            queues,
            depth,
            recovered,
        };
        *g = Slot::Attached(Box::new(live));
        Ok(Response::Attached(resp))
    }

    /// Opens the volume and puts a device in front of it (create or recover).
    fn bring_up(&self, name: &str, voldir: &Path, o: AttachOpts) -> Result<(Live, bool), CtlError> {
        let started_at = Instant::now();
        let record = device_record_unlocked(voldir)
            .filter(|r| current_boot_id().is_ok_and(|b| b == r.boot_id));
        let on_disk = journal_lanes(voldir);
        let default_q = self.cfg.queues.unwrap_or_else(|| {
            UblkConfig::default_queues(
                std::thread::available_parallelism().map_or(4, std::num::NonZeroUsize::get),
            )
        });
        // A device to recover keeps its geometry (the kernel's values rule).
        let queues = match (&record, o.queues) {
            (Some(r), _) => r.nr_queues,
            (None, Some(q)) => q,
            (None, None) => on_disk.unwrap_or(default_q),
        };
        if on_disk.is_some_and(|l| l > queues) {
            return Err(err(
                ErrorCode::Invalid,
                format!(
                    "the journal has {} lanes; attach with at least that many queues",
                    on_disk.unwrap_or(0)
                ),
            ));
        }
        let depth = record
            .as_ref()
            .map(|r| r.queue_depth)
            .or(o.depth)
            .unwrap_or(DEFAULT_DEPTH);
        let cpus = match o.cpus.as_deref().or(self.cfg.cpus.as_deref()) {
            Some(s) => parse_cpus(s)?,
            None => Vec::new(),
        };
        let buf = record
            .as_ref()
            .map(|r| r.max_io_buf_bytes)
            .or(o.max_io_buf_bytes);
        let mut op = OpenParams::new(voldir.to_path_buf(), queues);
        op.credential = o.credential;
        op.max_io_buf_bytes = buf;
        op.on_journal_damage = self.cfg.on_journal_damage;
        let stack = open_volume(op).map_err(|e| stack_error(&e))?;
        let rep = stack.engine.recovery_report();
        if stack.floor_before.is_none() {
            tracing::info!(volume = name, "first attach on this host: a rollback of history before it cannot be detected (ADR-31); the floor is recorded now");
        }
        tracing::info!(
            volume = name,
            tip = rep.tip.0,
            replayed = rep.replayed,
            swept_segments = rep.sweep.deleted_segments.len(),
            "volume opened"
        );
        let dp = DeviceParams {
            queues,
            depth,
            max_io_buf_bytes: buf.unwrap_or(stack.json.max_io_buf_bytes),
            chunk_bytes: stack.desc.chunk_size,
            cpus,
            timeouts: Timeouts::default(),
        };
        let started = match self.cfg.device {
            DeviceKind::Ublk => start_ublk(&stack.engine, &stack.store, &dp),
            DeviceKind::None => {
                NullDevice::start(Arc::clone(&stack.engine), voldir, &dp).map(|d| Started {
                    device: Device::Null(d),
                    recovered: false,
                })
            }
        };
        let Started { device, recovered } = match started {
            Ok(s) => s,
            Err(e) => {
                if let DeviceError::Ublk(u) = &e {
                    if let Some(id) = u.dev_id() {
                        tracing::error!(volume = name, dev_id = id, error = %u, "device left in the kernel; delete it once this daemon is gone");
                    }
                }
                return Err(device_error(&e));
            }
        };
        for w in device.warnings() {
            tracing::warn!(volume = name, warning = %w, "device");
        }
        let link = device.bdev_path().and_then(|b| self.make_link(name, &b));
        // An exact journal ledger after a crash between C6 and C9 (M11
        // re-review): commit the replayed records once the lanes are open.
        if stack.engine.recovery_report().replayed > 0 {
            if let Err(e) = stack.engine.commit_now(false) {
                tracing::warn!(volume = name, error = %e, "post-recovery commit failed; the next trigger retries");
            }
        }
        tracing::info!(
            volume = name,
            recovered,
            ms = started_at.elapsed().as_millis() as u64,
            "attached"
        );
        Ok((
            Live {
                device,
                stack,
                link,
            },
            recovered,
        ))
    }

    fn make_link(&self, name: &str, bdev: &str) -> Option<PathBuf> {
        let dir = &self.cfg.dev_dir;
        let link = dir.join(name);
        let res = std::fs::create_dir_all(dir).and_then(|()| {
            if std::fs::symlink_metadata(&link).is_ok_and(|m| m.file_type().is_symlink()) {
                std::fs::remove_file(&link)?;
            }
            std::os::unix::fs::symlink(bdev, &link)
        });
        match res {
            Ok(()) => Some(link),
            Err(e) => {
                tracing::warn!(volume = name, error = %e, "device symlink not created");
                None
            }
        }
    }

    fn remove_link(link: &Path) {
        if std::fs::symlink_metadata(link).is_ok_and(|m| m.file_type().is_symlink()) {
            if let Err(e) = std::fs::remove_file(link) {
                tracing::warn!(error = %e, "device symlink not removed");
            }
        }
    }

    // ------------------------------------------------------------ detach

    fn detach(&self, p: &NameParam) -> Result<Response, CtlError> {
        self.entry(&p.name)?;
        let slot = self.slot(&p.name);
        let mut g = lock(&slot);
        let Slot::Attached(live) = &*g else {
            return Err(err(
                ErrorCode::Invalid,
                format!("{} is not attached", p.name),
            ));
        };
        let res = live.device.detach(self.cfg.detach_busy_retry);
        let gone = match &res {
            Ok(()) => true,
            Err(DeviceError::Busy) => {
                return Err(err(
                    ErrorCode::Busy,
                    "the device is busy (mounted or open); nothing changed",
                ))
            }
            // The final commit failed but the device was deleted.
            Err(DeviceError::Ublk(UblkError::Engine(_)) | DeviceError::Engine(_)) => true,
            Err(_) => false,
        };
        if !gone {
            let e = res.err().map(|e| device_error(&e));
            return Err(e.unwrap_or_else(|| err(ErrorCode::Internal, "detach failed")));
        }
        let Slot::Attached(live) = std::mem::take(&mut *g) else {
            return Err(err(ErrorCode::Internal, "slot changed"));
        };
        if let Some(l) = &live.link {
            Self::remove_link(l);
        }
        if let Err(e) = live.stack.store.clear_device_record() {
            tracing::warn!(volume = %p.name, error = %e, "state/device not removed");
        }
        drop(live);
        match res {
            Ok(()) => Ok(Response::Detached),
            Err(e) => Err(device_error(&e)),
        }
    }

    // ------------------------------------------------------------ resize, space

    fn resize(&self, r: &Resize) -> Result<Response, CtlError> {
        self.entry(&r.name)?;
        let slot = self.slot(&r.name);
        let g = lock(&slot);
        let Slot::Attached(live) = &*g else {
            return Err(err(
                ErrorCode::Invalid,
                "attach the volume first (phase 1 grows online only)",
            ));
        };
        let cur = live.stack.engine.size_bytes();
        if r.size_bytes < cur {
            return Err(err(ErrorCode::Invalid, "shrink is not supported"));
        }
        if !r
            .size_bytes
            .is_multiple_of(u64::from(live.stack.desc.chunk_size))
        {
            return Err(err(
                ErrorCode::Invalid,
                "the size must be a multiple of the chunk size",
            ));
        }
        live.device
            .grow(r.size_bytes)
            .map_err(|e| device_error(&e))?;
        if let Some(rec) = live.device.record() {
            live.stack
                .store
                .store_device_record(&rec)
                .map_err(|e| err(ErrorCode::Io, format!("state/device: {e}")))?;
        }
        Ok(Response::Resized {
            size_bytes: live.stack.engine.size_bytes(),
        })
    }

    fn space(&self, s: &SpaceRequest) -> Result<Response, CtlError> {
        let e = self.entry(&s.name)?;
        let slot = self.slot(&s.name);
        let mut g = lock(&slot);
        let change = crate::audit::is_mutating(&Request::Space(s.clone()));
        let apply = |json: &mut VolumeJson| -> Result<(), CtlError> {
            if let Some(f) = &s.free_floor {
                json.space.free_floor = f.clone();
            }
            if let Some(o) = s.on_full {
                json.space.on_full = o;
            }
            if let Some(t) = s.full_timeout_ms {
                json.space.full_timeout_ms = Some(t);
            }
            if let Some(c) = s.cache_max {
                json.space.cache_max = Some(c);
            }
            json.validate()
                .map_err(|e| err(ErrorCode::Invalid, format!("space: {e}")))
        };
        match &mut *g {
            Slot::Attached(live) => {
                let st = &mut live.stack;
                if change {
                    let mut json = st.json.clone();
                    apply(&mut json)?;
                    st.space
                        .reconfigure(&json.space)
                        .map_err(|e| err(ErrorCode::Invalid, e.to_string()))?;
                    st.store
                        .store_volume_json(&json)
                        .map_err(|e| err(ErrorCode::Io, e.to_string()))?;
                    st.json = json;
                }
                Ok(Response::Space(SpaceBody {
                    settings: st.json.space.clone(),
                    figures: SpaceFigures::from(&st.space.snapshot()),
                }))
            }
            Slot::Detached => {
                let path = Path::new(&e.directory).join(lpath::volume_json());
                let text = std::fs::read_to_string(&path)
                    .map_err(|e| err(ErrorCode::Io, format!("volume.json: {e}")))?;
                let mut json = VolumeJson::from_json(&text)
                    .map_err(|e| err(ErrorCode::Io, format!("volume.json: {e}")))?;
                if change {
                    apply(&mut json)?;
                    let out = json
                        .to_json_pretty()
                        .map_err(|e| err(ErrorCode::Invalid, e.to_string()))?;
                    fsio::replace_durable(&path, out.as_bytes(), 0o644)
                        .map_err(|e| err(ErrorCode::Io, format!("volume.json: {e}")))?;
                }
                Ok(Response::Space(SpaceBody {
                    settings: json.space,
                    figures: SpaceFigures::default(),
                }))
            }
        }
    }

    // ------------------------------------------------------------ gc

    fn gc(&self, g: &GcRequest) -> Result<Response, CtlError> {
        self.entry(&g.name)?;
        Err(err(
            ErrorCode::Invalid,
            "gc: this build has no collector (lunix-gc, M17, is not merged)",
        ))
    }

    fn gc_status(&self, p: &NameParam) -> Result<Response, CtlError> {
        self.entry(&p.name)?;
        Ok(Response::GcStatus(GcStatusSnapshot::default()))
    }

    // ------------------------------------------------------------ quiesce, startup

    /// Quiesces every device (W1: `QUIESCE_DEV`, a true drain, the final
    /// commit; never a stop). On success the slots stay locked and the
    /// caller exits; on a failure the daemon keeps serving and logs (a device
    /// that timed out stays latched quiescing until a retry).
    pub fn quiesce_all(&self) -> (Result<Response, CtlError>, After) {
        let slots: Vec<Arc<Mutex<Slot>>> = lock(&self.slots).values().cloned().collect();
        let guards: Vec<MutexGuard<'_, Slot>> = slots.iter().map(|s| lock(s)).collect();
        let mut devices = 0u32;
        let mut engine_failures = 0u32;
        for g in &guards {
            let Slot::Attached(live) = &**g else { continue };
            match live.device.quiesce(self.cfg.quiesce_timeout) {
                Ok(None) => devices += 1,
                Ok(Some(e)) => {
                    devices += 1;
                    engine_failures += 1;
                    tracing::error!(error = %e, "final commit after QUIESCE_DEV failed; the next start recovers from the journal");
                }
                Err(e) => {
                    tracing::error!(error = %e, "quiesce failed; the daemon keeps serving (retry QuiesceAll, or crash for recovery)");
                    return (Err(device_error(&e)), After::Continue);
                }
            }
        }
        self.exiting.store(true, Ordering::Release);
        // Nothing may touch a quiesced device again: the slots stay locked
        // until the process exits.
        for g in guards {
            std::mem::forget(g);
        }
        tracing::info!(devices, "quiesced; exiting without a stop command");
        let code = if engine_failures > 0 { 70 } else { 0 };
        (Ok(Response::Quiesced { devices }), After::Exit(code))
    }

    /// Plan step 8: recovers every registered volume that has a device record
    /// before the control socket accepts calls. A volume that needs a
    /// credential (encrypted, or an integrity volume without its own key file)
    /// is left for an explicit attach, which recovers its device.
    pub fn recover_on_start(&self) {
        let entries: Vec<RegistryEntry> = lock(&self.registry).entries().to_vec();
        for e in entries {
            let voldir = PathBuf::from(&e.directory);
            if !voldir.join(lpath::state_device()).exists() {
                continue;
            }
            let started = Instant::now();
            let slot = self.slot(&e.name);
            let mut g = lock(&slot);
            match self.bring_up(&e.name, &voldir, AttachOpts::default()) {
                Ok((live, recovered)) => {
                    tracing::info!(volume = %e.name, recovered, stall_ms = started.elapsed().as_millis() as u64, "volume recovered at start");
                    *g = Slot::Attached(Box::new(live));
                }
                Err(c) => {
                    tracing::error!(volume = %e.name, code = ?c.code, error = %c.message, "volume not recovered at start; attach it (with its credential)");
                }
            }
        }
    }

    /// True once `QuiesceAll` succeeded.
    pub fn is_exiting(&self) -> bool {
        self.exiting.load(Ordering::Acquire)
    }

    /// Volumes attached now.
    pub fn attached(&self) -> Vec<String> {
        let slots: Vec<(String, Arc<Mutex<Slot>>)> = lock(&self.slots)
            .iter()
            .map(|(k, v)| (k.clone(), Arc::clone(v)))
            .collect();
        slots
            .into_iter()
            .filter(|(_, s)| matches!(*lock(s), Slot::Attached(_)))
            .map(|(k, _)| k)
            .collect()
    }

    /// The volume directory of `name`, from the registry.
    pub fn volume_dir(&self, name: &str) -> Option<PathBuf> {
        lock(&self.registry)
            .find(name)
            .map(|e| PathBuf::from(&e.directory))
    }

    /// The volume's rollback floor as `VolumeStatus` should show it (the
    /// protocol has no field for it yet; the CLI or a test reads it here).
    pub fn rollback_floor(&self, name: &str) -> Option<(u64, [u8; 32])> {
        let slot = self.slot(name);
        let g = lock(&slot);
        match &*g {
            Slot::Attached(l) => l
                .stack
                .store
                .rollback_floor()
                .ok()
                .flatten()
                .map(|f| (f.max_commit_seq.0, f.tip_hash)),
            Slot::Detached => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprints_and_cpu_lists_parse() {
        assert_eq!(
            parse_fingerprint("000102030405060708090a0b0c0d0e0f").unwrap(),
            [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15]
        );
        assert!(parse_fingerprint("00").is_err());
        assert_eq!(parse_cpus("2-4,8").unwrap(), vec![2, 3, 4, 8]);
        assert!(parse_cpus("4-2").is_err());
        assert!(parse_cpus("x").is_err());
    }

    #[test]
    fn journal_lanes_counts_ring_files() {
        let t = tempfile::tempdir().unwrap();
        assert_eq!(journal_lanes(t.path()), None);
        std::fs::create_dir(t.path().join("journal")).unwrap();
        for n in [
            "q00-00000001.j",
            "q02-00000001.j",
            "q01-00000003.j.tmp",
            "x.j",
        ] {
            std::fs::write(t.path().join("journal").join(n), b"").unwrap();
        }
        assert_eq!(journal_lanes(t.path()), Some(3));
    }
}
