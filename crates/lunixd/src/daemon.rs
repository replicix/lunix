//! The daemon's state and the control-call handlers (plan M12a steps 3-8).
//!
//! Every handler blocks (file I/O, ublk control commands, engine commits) and
//! runs on a blocking thread of the control server, never on a tokio worker.
//! One mutex per volume serialises the calls on that volume (except `Gc`,
//! which holds it only to find the engine, and `GcStatus`, which does not take
//! it); the registry has its own.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use lunix_crypto::{Argon2Cost, AttachError};
use lunix_engine::{GcStatusHandle, OnJournalDamage, Pressure};
use lunix_format::commit::Commit;
use lunix_format::local::{DeviceRecord, RegistryEntry, RollbackFloor, VolumeJson};
use lunix_format::paths::local as lpath;
use lunix_format::volume::{CodecPolicy, VolumeDescriptor};
use lunix_format::{CommitSeq, CryptoMode, IdLen};
use lunix_iface::block::BlockEngine;
use lunix_iface::clock::{Clock, SystemClock};
use lunix_iface::crypto::CommitCodec;
use lunix_iface::ctl::{
    Attach, Attached, Compress, CtlError, ErrorCode, GcPressure, GcRequest, JournalDamagePolicy,
    NameParam, Request, Resize, Response, RollbackFloorView, SpaceBody, SpaceFigures, SpaceRequest,
    ThrottleStatus, VolumeCreate, VolumeCreated, VolumeMode, VolumeState, VolumeStatusBody,
    VolumeSummary, WritePathStatus, PROTOCOL_VERSION,
};
use lunix_iface::gc::GcControl;
use lunix_iface::space::{SpaceEvents, SpaceGate};
use lunix_iface::store::VolumeDir;
use lunix_iface::ErrorKind;
use lunix_ublk::config::Timeouts;
use lunix_ublk::{current_boot_id, UblkConfig, UblkError, DEFAULT_DEPTH};

use crate::audit::{Audit, Peer};
use crate::config::{DaemonConfig, DeviceKind};
use crate::device::{
    delete_dead, kernel_has_device, probe_recorded, start_ublk, Device, DeviceError, DeviceParams,
    NullDevice, RecordedDevice, Started,
};
use crate::registry::Registry;
use crate::stack::{
    create_volume, effective_space, lock_volume, open_volume, read_pins_unlocked, unlock,
    AttachCredential, CreateParams, OpenParams, StackError, VolumeStack,
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
    /// A start failed after the kernel device existed and the device was not
    /// deleted (M10: a `START_DEV` timeout may still be pending). The queue
    /// threads may hold the engine and `LOCK`; a daemon restart recovers or
    /// replaces the device.
    Orphan {
        dev_id: u32,
        why: String,
    },
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
    /// The collector status of each attached volume: `GcStatus` reads it
    /// without the volume's mutex (an attach may hold that for long) and
    /// without the engine. Set at attach, removed at detach.
    gc_status: Mutex<HashMap<String, GcStatusHandle>>,
    /// Volumes with a `Gc` call waiting for a pass on request.
    gc_running: Mutex<std::collections::HashSet<String>>,
    audit: Audit,
    /// Set before a quiesce takes its snapshot: mutating calls are refused
    /// (cleared again when the quiesce fails without freezing anything).
    quiescing: AtomicBool,
    exiting: AtomicBool,
    /// Where an exit request goes (the server's main loop). The exit never
    /// depends on a reply being written (M12a review M2).
    exit_tx: Mutex<Option<tokio::sync::mpsc::UnboundedSender<i32>>>,
}

impl std::fmt::Debug for Daemon {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Daemon")
            .field("socket", &self.cfg.socket)
            .finish_non_exhaustive()
    }
}

/// How long a detach waits for a call to let go of the engine.
const ENGINE_RELEASE_WAIT: std::time::Duration = std::time::Duration::from_secs(30);

fn err(code: ErrorCode, msg: impl Into<String>) -> CtlError {
    CtlError::new(code, msg)
}

/// The control error for a stack failure.
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
        StackError::Rollback { .. } => ErrorCode::Rollback,
        StackError::Fork { .. } => ErrorCode::Fork,
        StackError::ForkUnverifiable { .. } => ErrorCode::ForkUnverifiable,
        StackError::JournalDamage => ErrorCode::JournalDamage,
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
    on_journal_damage: Option<OnJournalDamage>,
    allow_unverifiable_fork: bool,
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
            credential.mode = c.mode.map(CryptoMode::from);
        }
        Ok(Self {
            queues: a.queues,
            depth: a.depth,
            cpus: a.cpus,
            max_io_buf_bytes: a.max_io_buf_bytes,
            credential,
            on_journal_damage: a.on_journal_damage.map(|p| match p {
                JournalDamagePolicy::Wipe => OnJournalDamage::Wipe,
                JournalDamagePolicy::Refuse => OnJournalDamage::Refuse,
            }),
            allow_unverifiable_fork: a.allow_unverifiable_fork,
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

/// `volume.json`, read without the volume lock (the locked-buffer bound is
/// checked before the volume is opened).
fn volume_json_unlocked(voldir: &Path) -> Result<VolumeJson, CtlError> {
    let bytes = fsio::read_capped(&voldir.join(lpath::volume_json()), 1 << 20)
        .map_err(|e| err(ErrorCode::Io, format!("volume.json: {e}")))?
        .ok_or_else(|| err(ErrorCode::NotFound, "volume.json is missing"))?;
    let text =
        String::from_utf8(bytes).map_err(|_| err(ErrorCode::Io, "volume.json is not UTF-8"))?;
    VolumeJson::from_json(&text).map_err(|e| err(ErrorCode::Io, format!("volume.json: {e}")))
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
    /// A daemon over `cfg`, with the registry loaded. A registry that does
    /// not parse is renamed aside and rebuilt from `volume_roots`; with no
    /// roots configured the daemon refuses to start (an empty registry would
    /// leave the devices of every volume unrecovered). An I/O error is never
    /// "rebuild": the file is left alone and the start fails.
    ///
    /// # Errors
    /// The registry could neither be read nor rebuilt.
    pub fn new(cfg: DaemonConfig) -> anyhow::Result<Self> {
        let path = cfg.registry();
        let registry = match Registry::load(&path) {
            Ok(r) => r,
            Err(crate::registry::RegistryError::Format(e)) => {
                if cfg.volume_roots.is_empty() {
                    anyhow::bail!(
                        "registry {} does not parse ({e}) and no volume_roots are configured to rebuild it from; fix or remove it",
                        path.display()
                    );
                }
                let ts = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_secs());
                let aside = path.with_extension(format!("json.bad-{ts}"));
                std::fs::rename(&path, &aside)?;
                tracing::error!(error = %e, aside = %aside.display(), "registry does not parse; kept aside, rebuilding it from volume_roots");
                let (r, skipped) = Registry::rebuild(&path, &cfg.volume_roots);
                for s in skipped {
                    tracing::warn!(entry = %s, "registry rebuild skipped");
                }
                r.save()?;
                r
            }
            Err(e) => anyhow::bail!("registry {}: {e}", path.display()),
        };
        // Every registered volume has its slot from the start, so a quiesce
        // snapshot sees it (M12a review m1).
        let slots = registry
            .entries()
            .iter()
            .map(|e| (e.name.clone(), Arc::new(Mutex::new(Slot::Detached))))
            .collect();
        Ok(Self {
            audit: Audit::new(cfg.audit_log()),
            cfg,
            registry: Mutex::new(registry),
            slots: Mutex::new(slots),
            gc_status: Mutex::new(HashMap::new()),
            gc_running: Mutex::new(std::collections::HashSet::new()),
            quiescing: AtomicBool::new(false),
            exiting: AtomicBool::new(false),
            exit_tx: Mutex::new(None),
        })
    }

    /// Where exit requests go (the server's main loop).
    pub fn set_exit_sender(&self, tx: tokio::sync::mpsc::UnboundedSender<i32>) {
        *lock(&self.exit_tx) = Some(tx);
    }

    /// Asks the server to exit with `code` after `delay` (the delay lets a
    /// reply to the caller go out first; the exit does not wait for it).
    fn request_exit(&self, code: i32, delay: std::time::Duration) {
        let Some(tx) = lock(&self.exit_tx).clone() else {
            // No server (tests of the library): exit directly.
            std::thread::sleep(delay);
            std::process::exit(code);
        };
        std::thread::spawn(move || {
            std::thread::sleep(delay);
            let _ = tx.send(code);
        });
    }

    /// Audits a mutating request that `validate` refused.
    pub fn audit_refused(&self, peer: Peer, req: &Request, message: &str) {
        if crate::audit::is_mutating(req) {
            self.audit
                .record(peer, req, Err((ErrorCode::Invalid, message)));
        }
    }

    /// The configuration.
    pub fn config(&self) -> &DaemonConfig {
        &self.cfg
    }

    fn slot(&self, name: &str) -> Arc<Mutex<Slot>> {
        Arc::clone(lock(&self.slots).entry(name.to_owned()).or_default())
    }

    /// Remembers the collector status of a volume being attached (for
    /// `GcStatus`).
    fn note_engine(&self, name: &str, live: &Live) {
        lock(&self.gc_status).insert(name.to_owned(), live.stack.engine.gc_status_handle());
    }

    /// Drops a detached volume: the device first (it holds the engine), then
    /// the stack, after the calls that borrowed the engine let go of it (a
    /// `Gc` call, the space ladder's compact step: both end at once on a
    /// quiesced engine, at most one collector unit later). The borrow gate
    /// is closed first, so no new borrow starts. Dropping the last handle
    /// stops the engine's threads and releases `LOCK`, so the volume can be
    /// attached or checked again as soon as the detach answered.
    ///
    /// Exception: a borrow still running after [`ENGINE_RELEASE_WAIT`] (a
    /// collector unit stuck in I/O) keeps the engine; the detach answers all
    /// the same, with a warning, and `LOCK` is released only when that borrow
    /// ends.
    fn release(name: &str, live: Live) {
        let Live {
            device,
            stack,
            link,
        } = live;
        drop(device);
        drop(link);
        if !stack.borrows.close_and_wait(ENGINE_RELEASE_WAIT) {
            tracing::warn!(volume = %name, "a call still holds the engine after the detach; LOCK is released when it ends");
        }
        drop(stack);
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
        // An override is audited before it can take effect, so a crash during
        // the attach still leaves the line (M12a re-review nit).
        if matches!(&req, Request::Attach(a) if a.allow_unverifiable_fork) {
            self.audit.record_intent(peer, &req);
        }
        let (res, after) = if self.exiting.load(Ordering::Acquire) {
            (
                Err(err(ErrorCode::Busy, "the daemon is shutting down")),
                After::Continue,
            )
        } else if mutating
            && !matches!(req, Request::QuiesceAll)
            && self.quiescing.load(Ordering::Acquire)
        {
            (
                Err(err(ErrorCode::Busy, "a quiesce of every device is running")),
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
        let percent = c.store_max_percent.unwrap_or(self.cfg.store_max_percent);
        if percent != 0 && !(100..=10_000).contains(&percent) {
            return Err(err(
                ErrorCode::Invalid,
                "store_max_percent must be 0, or 100 to 10000",
            ));
        }
        if (1..200).contains(&percent) {
            tracing::warn!(
                volume = %c.name,
                store_max_percent = percent,
                "a cap below 200 % of the size may not hold a full volume at the default compaction threshold (ADR-32)"
            );
        }
        let tweak = move |j: &mut VolumeJson| {
            if let Some(m) = jmax {
                j.journal_max = m;
            }
            if let Some(i) = interval {
                j.commit.interval_ms = i;
            }
            // The percentage, not its bytes: a grow recomputes the cap
            // (FREEZE F2). 0: no cap.
            j.space.cache_max = None;
            j.space.cache_max_percent = (percent != 0).then_some(percent);
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
        drop(self.slot(&c.name));
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
                Slot::Orphan { dev_id, .. } => match peek(Path::new(&e.directory)) {
                    Ok(p) => (
                        p.size,
                        p.desc.crypto_mode,
                        Some(format!("/dev/ublkb{dev_id}")),
                    ),
                    Err(_) => continue,
                },
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

    fn floor_view(f: Option<RollbackFloor>) -> Option<RollbackFloorView> {
        f.map(|f| RollbackFloorView {
            max_commit_seq: f.max_commit_seq.0,
            tip_hash: f.tip_hash.iter().map(|b| format!("{b:02x}")).collect(),
        })
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
                let tip = s.engine.tip_view();
                let cuts = s.engine.cut_stats();
                let wstats = s.engine.write_stats();
                let win = s.journal.window_stats();
                let read_only = s.space.is_read_only() || s.engine.is_stopped();
                let written = stats.chunks_total.saturating_sub(stats.chunks_zero);
                let snap = s.space.snapshot();
                let throttle = ThrottleStatus::from_snapshot(
                    &snap,
                    u64::try_from(SystemClock.monotonic().as_millis()).unwrap_or(u64::MAX),
                    match s.engine.gc_pressure() {
                        Pressure::Normal => GcPressure::Normal,
                        Pressure::Raised => GcPressure::Raised,
                        Pressure::Urgent => GcPressure::Urgent,
                    },
                    stats.ops_discard.saturating_add(stats.ops_write_zeroes),
                    s.engine.gc_status_handle().get().stats.live_bytes,
                );
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
                    space: SpaceFigures::from(&snap),
                    throttle,
                    journal_used: fill.used_bytes,
                    journal_capacity: fill.capacity_bytes,
                    journal_marks_written: s.journal.marks_written(),
                    journal_marks_skipped: s.journal.marks_skipped(),
                    write_path: WritePathStatus {
                        ring_waiters: cuts.ring_waiters,
                        ring_parked_writes: cuts.ring_parked_writes,
                        ring_wait_max_since_attach_ms: cuts.ring_wait_max_since_attach_ms,
                        ring_wait_max_60s_ms: cuts.ring_wait_max_60s_ms,
                        freed_files_by_lane: wstats.freed_files_by_lane,
                        write_latency_log2_us: wstats.write_latency_log2_us.to_vec(),
                        ring_pace_rate: snap.throttle.ring_pace_rate,
                        ring_paced_writes: snap.throttle.ring_paced_writes,
                        ring_paced_ms: snap.throttle.ring_paced_ms,
                        ring_pace_max_ms: snap.throttle.ring_pace_max_ms,
                        ring_pace_capped: snap.throttle.ring_pace_capped,
                        ring_pace_bypassed_fua: snap.throttle.ring_pace_bypassed_fua,
                        window_resident_bytes: win.resident_bytes,
                        window_hits: win.hits,
                        window_misses: win.misses,
                        last_cut_bytes: cuts.last_cut_bytes,
                        last_cut_ms: cuts.last_cut_ms,
                    },
                    locked_bytes: l.device.locked_bytes() + s.journal.locked_bytes(),
                    last_commit_seq: tip.commit_seq.0,
                    last_commit_ms: Some(tip.created_at.0),
                    dedup_ratio: ratio(written, written.saturating_sub(stats.dedup_hits)),
                    compression_ratio: ratio(stats.stored_ulen, stats.stored_clen),
                    stats,
                    rollback_floor: Self::floor_view(s.store.rollback_floor().ok().flatten()),
                }
            }
            Slot::Detached | Slot::Orphan { .. } => {
                let dir = Path::new(&e.directory);
                let pk = peek(dir)?;
                let floor = read_pins_unlocked(dir)
                    .map_err(|x| stack_error(&x))?
                    .and_then(|f| f.floor);
                let orphan = match &*g {
                    Slot::Orphan { dev_id, .. } => Some(*dev_id),
                    _ => None,
                };
                VolumeStatusBody {
                    name: p.name.clone(),
                    uuid: e.uuid,
                    // A device left in the kernel after a failed start: not
                    // attached, not usable until a restart recovers it.
                    state: if orphan.is_some() {
                        VolumeState::ReadOnly
                    } else {
                        VolumeState::Detached
                    },
                    size_bytes: pk.size,
                    chunk_size: pk.desc.chunk_size,
                    mode: pk.desc.crypto_mode.into(),
                    device: orphan.map(|id| format!("/dev/ublkb{id}")),
                    stats: Default::default(),
                    space: SpaceFigures::default(),
                    throttle: ThrottleStatus::default(),
                    journal_used: 0,
                    journal_capacity: 0,
                    journal_marks_written: 0,
                    journal_marks_skipped: 0,
                    write_path: WritePathStatus::default(),
                    locked_bytes: 0,
                    last_commit_seq: pk.tip.map_or(0, |t| t.0),
                    last_commit_ms: None,
                    dedup_ratio: None,
                    compression_ratio: None,
                    rollback_floor: Self::floor_view(floor),
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
        if self.quiescing.load(Ordering::Acquire) {
            return Err(err(ErrorCode::Busy, "a quiesce of every device is running"));
        }
        match &*g {
            Slot::Attached(_) => {
                return Err(err(ErrorCode::Busy, format!("{name} is already attached")));
            }
            Slot::Orphan { dev_id, why } => {
                return Err(err(
                    ErrorCode::Busy,
                    format!("device {dev_id} of {name} was left in the kernel after a failed start ({why}); restart lunixd, which recovers or replaces it"),
                ));
            }
            Slot::Detached => {}
        }
        match self.bring_up(&name, Path::new(&e.directory), opts) {
            Ok((live, recovered, first_attach)) => {
                let (queues, depth) = live.device.geometry();
                let resp = Attached {
                    device: self.device_path(&name, &live).unwrap_or_default(),
                    dev_id: live.device.dev_id().unwrap_or(0),
                    queues,
                    depth,
                    recovered,
                    first_attach,
                };
                tracing::info!(
                    volume = %name,
                    locked_bytes = live.device.locked_bytes() + live.stack.journal.locked_bytes(),
                    "attached; queue and journal I/O memory locked"
                );
                self.note_engine(&name, &live);
                *g = Slot::Attached(Box::new(live));
                Ok(Response::Attached(resp))
            }
            Err(Failed::Orphan { dev_id, error }) => {
                *g = Slot::Orphan {
                    dev_id,
                    why: error.message.clone(),
                };
                Err(error)
            }
            Err(Failed::Plain(e)) => Err(e),
        }
    }

    /// Opens the volume and puts a device in front of it (create or recover).
    /// Returns the live volume, whether the device was recovered, and whether
    /// this host had no floor (`first_attach`).
    fn bring_up(
        &self,
        name: &str,
        voldir: &Path,
        o: AttachOpts,
    ) -> Result<(Live, bool, bool), Failed> {
        let started_at = Instant::now();
        let record = device_record_unlocked(voldir)
            .filter(|r| current_boot_id().is_ok_and(|b| b == r.boot_id));
        // The record's geometry rules only for a device that can actually be
        // recovered (M12a review m10): a dead or vanished device's record is
        // not a reason to repeat its geometry. The probe is read-only; a DEAD
        // device of this volume is deleted only after LOCK and the credential
        // check (re-review R5), below.
        let mut dead_own = None;
        let record = match (&record, self.cfg.device) {
            (Some(r), DeviceKind::Ublk) => match probe_recorded(r) {
                RecordedDevice::Recoverable | RecordedDevice::Unknown => record,
                RecordedDevice::DeadOwn => {
                    dead_own = Some(r.clone());
                    None
                }
                RecordedDevice::Gone => None,
            },
            _ => None,
        };
        let on_disk = journal_lanes(voldir);
        let default_q = self.cfg.queues.unwrap_or_else(|| {
            UblkConfig::default_queues(
                std::thread::available_parallelism().map_or(4, std::num::NonZeroUsize::get),
            )
        });
        if let (Some(r), Some(q)) = (&record, o.queues) {
            if q != r.nr_queues {
                tracing::warn!(
                    volume = name,
                    requested = q,
                    recorded = r.nr_queues,
                    "the device to recover keeps its recorded queue count"
                );
            }
        }
        // A device to recover keeps its geometry (the kernel's values rule).
        let queues = match (&record, o.queues) {
            (Some(r), _) => r.nr_queues,
            (None, Some(q)) => q,
            (None, None) => on_disk.unwrap_or(default_q),
        };
        if on_disk.is_some_and(|l| l > queues) {
            return Err(Failed::Plain(err(
                ErrorCode::Invalid,
                format!(
                    "the journal has {} lanes; attach with at least that many queues (lanes never shrink in phase 1)",
                    on_disk.unwrap_or(0)
                ),
            )));
        }
        let depth = record
            .as_ref()
            .map(|r| r.queue_depth)
            .or(o.depth)
            .unwrap_or(DEFAULT_DEPTH);
        let cpus = match o.cpus.as_deref().or(self.cfg.cpus.as_deref()) {
            Some(s) => parse_cpus(s).map_err(Failed::Plain)?,
            None => Vec::new(),
        };
        let buf = record
            .as_ref()
            .map(|r| r.max_io_buf_bytes)
            .or(o.max_io_buf_bytes);
        // The locked-buffer bound before anything is opened (re-review R5):
        // the buffer size comes from the request, the record or volume.json.
        let max_buf = match buf {
            Some(b) => b,
            None => {
                volume_json_unlocked(voldir)
                    .map_err(Failed::Plain)?
                    .max_io_buf_bytes
            }
        };
        let locked = u64::from(queues) * u64::from(depth) * u64::from(max_buf);
        if locked > self.cfg.max_locked_buffers {
            return Err(Failed::Plain(err(
                ErrorCode::Invalid,
                format!(
                    "{queues} queues x depth {depth} x {max_buf} bytes = {} MiB of locked buffers exceeds max_locked_buffers ({} MiB)",
                    locked >> 20,
                    self.cfg.max_locked_buffers >> 20
                ),
            )));
        }
        let mut op = OpenParams::new(voldir.to_path_buf(), queues);
        op.credential = o.credential;
        op.max_io_buf_bytes = Some(max_buf);
        op.on_journal_damage = o.on_journal_damage.unwrap_or(self.cfg.on_journal_damage);
        op.lock_memory = self.cfg.harden;
        op.destage_workers = self.cfg.destage_workers;
        op.journal_resident_max = self.cfg.journal_resident_max;
        op.cut_target = self.cfg.cut_target;
        op.allow_unverifiable_fork = o.allow_unverifiable_fork;
        op.full_timeout = self.cfg.full_timeout;
        op.throttle = self.cfg.throttle;
        op.gc_pressure_rate_factor = self.cfg.gc_pressure_rate_factor;
        if o.allow_unverifiable_fork {
            tracing::warn!(
                volume = name,
                "attach with the operator override allow_unverifiable_fork (audited)"
            );
        }
        let stack = open_volume(op).map_err(|e| Failed::Plain(stack_error(&e)))?;
        // LOCK is held and the credential passed: now a DEAD device of this
        // volume may be deleted (M10 re-review R4).
        if let Some(r) = dead_own {
            delete_dead(&r);
        }
        let rep = stack.engine.recovery_report();
        let first_attach = rep.first_attach;
        if first_attach {
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
            max_io_buf_bytes: max_buf,
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
                let ctl = device_error(&e);
                if let DeviceError::Ublk(u) = &e {
                    if let Some(id) = u.dev_id() {
                        if kernel_has_device(id) {
                            tracing::error!(volume = name, dev_id = id, error = %u, "a failed start left the device in the kernel; the volume is unusable until lunixd restarts (another process deletes or recovers it)");
                            // The queue threads may hold the engine: keep it
                            // (and LOCK) rather than drop it under them.
                            std::mem::forget(stack);
                            return Err(Failed::Orphan {
                                dev_id: id,
                                error: ctl,
                            });
                        }
                        tracing::warn!(volume = name, dev_id = id, error = %u, "the start failed; the new device was deleted again");
                    }
                }
                return Err(Failed::Plain(ctl));
            }
        };
        for w in device.warnings() {
            tracing::warn!(volume = name, warning = %w, "device");
        }
        let link = device.bdev_path().and_then(|b| self.make_link(name, &b));
        // No commit here: the coordinator's own triggers destage what the
        // journal replayed, in the background (M12a review m6). After a crash
        // between C6 and C9 `Journal::fill()` overstates the ring until the
        // first commit; that is accepted.
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
            first_attach,
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
            // The layer knows whether TRY_STOP_DEV got through; the quiesce
            // asks it (Device::needs_quiesce), not this error.
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
            tracing::warn!(volume = %p.name, error = %e, "state/device not removed (the next start sees the device gone and clears it)");
        }
        lock(&self.gc_status).remove(&p.name);
        Self::release(&p.name, *live);
        match res {
            Ok(()) => Ok(Response::Detached),
            Err(e) => Err(device_error(&e)),
        }
    }

    // ------------------------------------------------------------ resize, space

    fn attached_live<'a>(g: &'a MutexGuard<'_, Slot>, name: &str) -> Result<&'a Live, CtlError> {
        match &**g {
            Slot::Attached(l) => Ok(l),
            _ => Err(err(
                ErrorCode::Invalid,
                format!("attach {name} first (phase 1 grows online only)"),
            )),
        }
    }

    fn resize(&self, r: &Resize) -> Result<Response, CtlError> {
        self.entry(&r.name)?;
        let slot = self.slot(&r.name);
        let mut g = lock(&slot);
        let live = Self::attached_live(&g, &r.name)?;
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
        let grown = live.device.grow(r.size_bytes);
        // Whatever the grow reported, the engine may have committed a new
        // size: the cap follows it before any later step can fail (review
        // m2 of p1/bstop-fu; `grow_cache_max` writes nothing, cannot fail).
        let new = live.stack.engine.size_bytes();
        if new > cur {
            if let Slot::Attached(live) = &mut *g {
                self.grow_cache_max(&r.name, &mut live.stack, cur, new);
            }
        }
        let live = Self::attached_live(&g, &r.name)?;
        grown.map_err(|e| device_error(&e))?;
        if let Some(rec) = live.device.record() {
            live.stack
                .store
                .store_device_record(&rec)
                .map_err(|e| err(ErrorCode::Io, format!("state/device: {e}")))?;
        }
        Ok(Response::Resized { size_bytes: new })
    }

    /// After a grow from `old` to `new` bytes (FREEZE F2): a percentage cap
    /// (`space.cache_max_percent`) is recomputed from the new size and the
    /// gate reconfigured; `volume.json` keeps the percentage, so it needs no
    /// write. An absolute `cache_max` stays, with a warning when it is below
    /// the new size plus two commits. No cap stays no cap. The grow itself
    /// is done: a failure here is logged, not returned.
    fn grow_cache_max(&self, name: &str, st: &mut VolumeStack, old: u64, new: u64) {
        if new <= old {
            return;
        }
        let dirty = st.json.commit.dirty_bytes;
        if let Some(pct) = st.json.space.cache_max_percent {
            let eff = effective_space(&st.json, new);
            match st.space.reconfigure(&eff) {
                Ok(()) => tracing::info!(
                    volume = name,
                    cache_max_percent = pct,
                    cache_max = eff.cache_max,
                    "grown: the cap follows the size"
                ),
                Err(e) => {
                    tracing::warn!(volume = name, error = %e, "grown, but the cap was not raised")
                }
            }
            return;
        }
        let Some(cap) = st.json.space.cache_max else {
            return;
        };
        if cap < new.saturating_add(dirty.saturating_mul(2)) {
            tracing::warn!(
                volume = name,
                cache_max = cap,
                size = new,
                "grown; the absolute cache_max stays and is below the new size plus two commits: the volume will refuse writes before it is full (raise it with lunix space --cache-max)"
            );
        } else {
            tracing::info!(
                volume = name,
                cache_max = cap,
                "grown; the absolute cache_max stays"
            );
        }
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
                // An absolute cap replaces a percentage (FREEZE F2).
                json.space.cache_max = Some(c);
                json.space.cache_max_percent = None;
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
                    // The file first: a gate changed for a policy the file
                    // does not hold would be undone silently by the next
                    // grow or restart (review m3 of p1/bstop-fu).
                    st.store
                        .store_volume_json(&json)
                        .map_err(|e| err(ErrorCode::Io, e.to_string()))?;
                    st.json = json;
                    st.space
                        .reconfigure(&effective_space(&st.json, st.engine.size_bytes()))
                        .map_err(|e| {
                            err(
                                ErrorCode::Invalid,
                                format!("volume.json is written, the gate kept its policy: {e}"),
                            )
                        })?;
                    // The ring-full wait follows the new full_timeout too.
                    st.engine.set_full_timeout(st.space.full_timeout());
                }
                Ok(Response::Space(SpaceBody {
                    settings: st.json.space.clone(),
                    figures: SpaceFigures::from(&st.space.snapshot()),
                }))
            }
            Slot::Detached | Slot::Orphan { .. } => {
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

    /// `Gc`: a pass now (or a dry run) on the engine's coordinator
    /// (`GcControl::run_pass`; `--dry-run` takes the same path and changes
    /// nothing). The volume's mutex is held only to find the engine: a pass
    /// waits out its grace, and a detach or a quiesce must not wait for it.
    /// A quiesce of the engine (detach, `QuiesceAll`) ends the call with
    /// `Closed` (answered `Busy`) at once, and no collector call starts after
    /// it; the call holds a borrow (`stack::Borrows`) the detach waits for. One
    /// non-dry call per volume at a time: a second one answers `Busy`.
    fn gc(&self, g: &GcRequest) -> Result<Response, CtlError> {
        self.entry(&g.name)?;
        let (engine, borrows) = {
            let slot = self.slot(&g.name);
            let guard = lock(&slot);
            match &*guard {
                Slot::Attached(l) => (Arc::clone(&l.stack.engine), Arc::clone(&l.stack.borrows)),
                _ => {
                    return Err(err(
                        ErrorCode::Invalid,
                        format!("attach {} first: gc runs on an attached volume", g.name),
                    ))
                }
            }
        };
        let detaching = || {
            err(
                ErrorCode::Busy,
                format!("{} was detached or quiesced while the gc pass ran", g.name),
            )
        };
        let Some(_borrow) = borrows.enter() else {
            return Err(detaching());
        };
        // One pass on request per volume: a client that gave up and retries
        // does not stack call threads (a pass lasts at least its grace).
        let _running = if g.dry_run {
            None
        } else {
            Some(Running::enter(&self.gc_running, &g.name).ok_or_else(|| {
                err(
                    ErrorCode::Busy,
                    format!("a gc pass on request for {} is already running", g.name),
                )
            })?)
        };
        let started = Instant::now();
        let res = engine.run_pass(g.dry_run);
        drop(engine);
        match res {
            Ok(s) => {
                tracing::info!(
                    volume = %g.name,
                    dry_run = s.dry_run,
                    pass = s.pass,
                    bytes_freed = s.bytes_freed,
                    segments_deleted = s.segments_deleted,
                    segments_compacted = s.segments_compacted,
                    ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                    "gc pass on request"
                );
                Ok(Response::Gc(s))
            }
            Err(lunix_iface::Error::Closed) => Err(detaching()),
            Err(e) => Err(err(store_code(e.kind()), format!("gc: {e}"))),
        }
    }

    /// `GcStatus`: the collector's last published status, without waiting
    /// for the volume's mutex. A volume that is not attached is idle.
    fn gc_status(&self, p: &NameParam) -> Result<Response, CtlError> {
        self.entry(&p.name)?;
        let st = lock(&self.gc_status).get(&p.name).map(GcStatusHandle::get);
        Ok(Response::GcStatus(st.unwrap_or_default()))
    }

    // ------------------------------------------------------------ quiesce, startup

    /// Quiesces every device (W1: `QUIESCE_DEV`, a true drain, the final
    /// commit; never a stop) and asks the server to exit.
    ///
    /// Every device is walked, in name order, and failures are collected
    /// (M12a review M1). A device whose detach already got past
    /// `TRY_STOP_DEV` is stopping and has no started request: nothing to do.
    /// When some device failed:
    /// * if nothing else was quiesced, the daemon keeps running and logs; a
    ///   device whose `QUIESCE_DEV` timed out refuses new I/O until a retry
    ///   or a crash for recovery (M10), and the log says so;
    /// * if other devices are already quiesced, the failed ones are retried
    ///   once, and if they still fail the process aborts (DESIGN §3: retry,
    ///   then `abort()`, recoverable like `kill -9`), so it never serves on
    ///   with frozen devices.
    ///
    /// On success the slots stay locked and the exit is requested directly,
    /// not through the reply (M12a review M2).
    pub fn quiesce_all(&self) -> (Result<Response, CtlError>, After) {
        if self.quiescing.swap(true, Ordering::AcqRel) {
            return (
                Err(err(
                    ErrorCode::Busy,
                    "a quiesce of every device is already running",
                )),
                After::Continue,
            );
        }
        let mut slots: Vec<(String, Arc<Mutex<Slot>>)> = lock(&self.slots)
            .iter()
            .map(|(k, v)| (k.clone(), Arc::clone(v)))
            .collect();
        slots.sort_by(|a, b| a.0.cmp(&b.0));
        let guards: Vec<(String, MutexGuard<'_, Slot>)> =
            slots.iter().map(|(n, s)| (n.clone(), lock(s))).collect();
        let t = self.cfg.quiesce_timeout;
        let mut quiesced = 0u32;
        let mut engine_failures = 0u32;
        let mut orphans = Vec::new();
        let mut failed: Vec<(usize, String)> = Vec::new();
        for (i, (name, g)) in guards.iter().enumerate() {
            match &**g {
                Slot::Attached(live) if !live.device.needs_quiesce() => {
                    tracing::info!(volume = %name, "device is stopping or deleted (a detach got past TRY_STOP_DEV): no started request, nothing to quiesce");
                }
                Slot::Attached(live) => {
                    // The quiesce rule (ADR-32, M5): requests waiting for
                    // space fail with ENOSPC and nothing parks, so the drain
                    // can finish.
                    live.stack.space.begin_close();
                    if t <= COORD_PERIOD {
                        tracing::warn!(
                            volume = %name,
                            quiesce_timeout_ms = u64::try_from(t.as_millis()).unwrap_or(u64::MAX),
                            "quiesce_timeout_ms is 100 ms or less: writes parked for a full journal ring fail with EIO at once"
                        );
                    }
                    live.stack.engine.begin_close(close_ring_bound(t));
                    match live.device.quiesce(t) {
                        Ok(None) => quiesced += 1,
                        Ok(Some(e)) => {
                            quiesced += 1;
                            engine_failures += 1;
                            tracing::error!(volume = %name, error = %e, "final commit after QUIESCE_DEV failed; the next start recovers from the journal");
                        }
                        Err(e) => failed.push((i, e.to_string())),
                    }
                }
                Slot::Orphan { dev_id, .. } => orphans.push(*dev_id),
                Slot::Detached => {}
            }
        }
        if !failed.is_empty() {
            if quiesced == 0 {
                for (i, e) in &failed {
                    tracing::error!(volume = %guards[*i].0, error = %e, "quiesce failed: after a QUIESCE_DEV timeout the device refuses new I/O until QuiesceAll is retried or the daemon crashes for recovery; nothing else was quiesced, the daemon keeps running");
                }
                drop(guards);
                self.quiescing.store(false, Ordering::Release);
                let names: Vec<&str> = failed
                    .iter()
                    .map(|(i, _)| guards_name(&slots, *i))
                    .collect();
                return (
                    Err(err(
                        ErrorCode::Io,
                        format!(
                            "quiesce failed for {}: retry QuiesceAll (the device refuses new I/O meanwhile)",
                            names.join(", ")
                        ),
                    )),
                    After::Continue,
                );
            }
            for (i, e) in &failed {
                let (name, g) = &guards[*i];
                tracing::warn!(volume = %name, error = %e, "quiesce failed while other devices are quiesced; retrying once");
                let retried = match &**g {
                    Slot::Attached(live) => live.device.quiesce(t),
                    _ => Ok(None),
                };
                match retried {
                    Ok(engine) => {
                        quiesced += 1;
                        if let Some(e) = engine {
                            engine_failures += 1;
                            tracing::error!(volume = %name, error = %e, "final commit after QUIESCE_DEV failed; the next start recovers from the journal");
                        }
                    }
                    Err(e) => {
                        tracing::error!(volume = %name, error = %e, "quiesce failed again with other devices already quiesced: aborting for recovery (DESIGN §3; recovered like kill -9)");
                        std::process::abort();
                    }
                }
            }
        }
        self.exiting.store(true, Ordering::Release);
        // Nothing may touch a quiesced device again: the slots stay locked
        // until the process exits.
        for (_, g) in guards {
            std::mem::forget(g);
        }
        if !orphans.is_empty() {
            // M10's contract for a device left after a failed start: such a
            // device may have gone live late and hold started requests, so
            // the only sanctioned exit is abort() (recovered like kill -9).
            tracing::error!(?orphans, "devices left in the kernel after a failed start cannot be quiesced: aborting for recovery (the other devices are quiesced)");
            std::process::abort();
        }
        let code = if engine_failures > 0 { 70 } else { 0 };
        tracing::info!(
            devices = quiesced,
            code,
            "quiesced; exiting without a stop command"
        );
        self.request_exit(code, std::time::Duration::from_secs(2));
        (
            Ok(Response::Quiesced { devices: quiesced }),
            After::Exit(code),
        )
    }

    /// Plan step 8 (with step 5): every registered volume that has a device
    /// record is brought back before the socket accepts calls.
    ///
    /// * Same boot, the recorded device QUIESCED or LIVE with this volume's
    ///   tag: it is recovered.
    /// * Same boot, the device gone, another volume's, or DEAD with this tag:
    ///   a detach had completed (or the start never did). The volume stays
    ///   detached; the record is cleared, and a DEAD device of this volume
    ///   deleted, only after `LOCK` is held and the credential check passed
    ///   (M12a re-review R5), so a second daemon on the same directory cannot
    ///   delete a device the first is starting.
    /// * Another boot (lead decision of 2026-11-14, following the plan): the
    ///   volume was attached when the host went down, so it is attached again
    ///   with a **new** device.
    ///
    /// A volume that needs a credential (encrypted, or integrity without its
    /// own key file) is left for an explicit attach. Recovery stops as soon as
    /// a quiesce starts, and re-checks that under each slot lock.
    pub fn recover_on_start(&self) {
        let entries: Vec<RegistryEntry> = lock(&self.registry).entries().to_vec();
        for e in entries {
            if self.quiescing.load(Ordering::Acquire) {
                tracing::info!("a quiesce started: recovery stops here");
                return;
            }
            let voldir = PathBuf::from(&e.directory);
            let Some(rec) = device_record_unlocked(&voldir) else {
                continue;
            };
            if self.cfg.device != DeviceKind::Ublk {
                continue;
            }
            let same_boot = current_boot_id().is_ok_and(|b| b == rec.boot_id);
            if same_boot {
                match probe_recorded(&rec) {
                    RecordedDevice::Recoverable => {}
                    RecordedDevice::Unknown => {
                        tracing::warn!(volume = %e.name, dev_id = rec.dev_id, "the recorded device cannot be inspected; not recovered (attach it to retry)");
                        continue;
                    }
                    probe @ (RecordedDevice::DeadOwn | RecordedDevice::Gone) => {
                        match lock_volume(&voldir, &AttachCredential::default()) {
                            Ok(store) => {
                                if probe == RecordedDevice::DeadOwn {
                                    delete_dead(&rec);
                                }
                                if let Err(x) = store.clear_device_record() {
                                    tracing::warn!(volume = %e.name, error = %x, "stale state/device not removed");
                                }
                                tracing::info!(volume = %e.name, dev_id = rec.dev_id, "the recorded device is gone (a detach completed): record cleared, the volume stays detached");
                            }
                            Err(x) => {
                                tracing::warn!(volume = %e.name, error = %x, "the recorded device is gone; the record stays until an attach with the volume's credential clears it");
                            }
                        }
                        continue;
                    }
                }
            } else {
                tracing::info!(volume = %e.name, "device record from another boot: the volume was attached; attaching it again with a new device");
            }
            let started = Instant::now();
            let slot = self.slot(&e.name);
            let mut g = lock(&slot);
            if self.quiescing.load(Ordering::Acquire) {
                tracing::info!("a quiesce started: recovery stops here");
                return;
            }
            match self.bring_up(&e.name, &voldir, AttachOpts::default()) {
                Ok((live, recovered, _)) => {
                    tracing::info!(volume = %e.name, recovered, stall_ms = started.elapsed().as_millis() as u64, "volume recovered at start");
                    self.note_engine(&e.name, &live);
                    *g = Slot::Attached(Box::new(live));
                }
                Err(Failed::Orphan { dev_id, error }) => {
                    tracing::error!(volume = %e.name, dev_id, error = %error.message, "volume not recovered at start: its device is left in the kernel");
                    *g = Slot::Orphan {
                        dev_id,
                        why: error.message,
                    };
                }
                Err(Failed::Plain(c)) => {
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

    /// Test hook: the engine of an attached volume (a test holds its
    /// coordinator mutex to show that status does not wait for it).
    #[cfg(feature = "testsupport")]
    pub fn engine_of(&self, name: &str) -> Option<Arc<lunix_engine::Engine>> {
        let slot = self.slot(name);
        let g = lock(&slot);
        match &*g {
            Slot::Attached(l) => Some(Arc::clone(&l.stack.engine)),
            _ => None,
        }
    }

    /// The volume directory of `name`, from the registry.
    pub fn volume_dir(&self, name: &str) -> Option<PathBuf> {
        lock(&self.registry)
            .find(name)
            .map(|e| PathBuf::from(&e.directory))
    }

    /// A number that changes whenever any attached volume does I/O or
    /// commits, and whether every slot could be looked at (a slot busy with
    /// a control call counts as activity).
    fn activity(&self) -> (u64, bool) {
        let slots: Vec<Arc<Mutex<Slot>>> = lock(&self.slots).values().cloned().collect();
        let mut sum = 0u64;
        for s in slots {
            let Ok(g) = s.try_lock() else {
                return (0, false);
            };
            if let Slot::Attached(l) = &*g {
                let st = l.stack.engine.stats();
                sum = sum
                    .wrapping_add(st.ops_read)
                    .wrapping_add(st.ops_write)
                    .wrapping_add(st.ops_flush)
                    .wrapping_add(st.ops_discard)
                    .wrapping_add(st.ops_write_zeroes)
                    .wrapping_add(st.commits)
                    .wrapping_add(1);
            }
        }
        (sum, true)
    }

    /// Empties the read cache of every attached volume whose slot is free
    /// (a slot busy with a control call is skipped); returns the bytes.
    fn release_read_caches(&self) -> u64 {
        let slots: Vec<Arc<Mutex<Slot>>> = lock(&self.slots).values().cloned().collect();
        let mut bytes = 0u64;
        for s in slots {
            let Ok(g) = s.try_lock() else { continue };
            if let Slot::Attached(l) = &*g {
                bytes = bytes.saturating_add(l.stack.engine.release_read_cache());
            }
        }
        bytes
    }

    /// Starts the idle-trim thread (decision 2 of 2026-11-13): once every
    /// volume has been idle for `idle_trim_ms`, the heap is trimmed once, off
    /// the queue threads; activity re-arms it.
    pub fn spawn_idle_trim(self: &Arc<Self>) {
        let Some(idle) = self.cfg.idle_trim else {
            return;
        };
        let weak = Arc::downgrade(self);
        let period = (idle / 4).clamp(
            std::time::Duration::from_millis(50),
            std::time::Duration::from_secs(2),
        );
        let spawned = std::thread::Builder::new()
            .name("lunixd-trim".into())
            .spawn(move || {
                let mut last = (u64::MAX, Instant::now());
                let mut trimmed = false;
                loop {
                    std::thread::sleep(period);
                    let Some(d) = weak.upgrade() else { return };
                    if d.is_exiting() {
                        return;
                    }
                    let (sig, complete) = d.activity();
                    if !complete || sig != last.0 {
                        last = (sig, Instant::now());
                        trimmed = false;
                        continue;
                    }
                    if !trimmed && last.1.elapsed() >= idle {
                        let before = crate::trim::rss_bytes();
                        // The read caches are what a workload leaves behind
                        // (M12a memory gate, 2026-11-15): release them first,
                        // the trim then returns the pages.
                        let released = d.release_read_caches();
                        crate::trim::trim();
                        let after = crate::trim::rss_bytes();
                        tracing::debug!(
                            before = ?before,
                            after = ?after,
                            read_cache_released = released,
                            "idle: read caches released, heap trimmed"
                        );
                        trimmed = true;
                    }
                }
            });
        if let Err(e) = spawned {
            tracing::warn!(error = %e, "idle trim thread not started");
        }
    }
}

fn guards_name(slots: &[(String, Arc<Mutex<Slot>>)], i: usize) -> &str {
    slots.get(i).map_or("?", |(n, _)| n.as_str())
}

/// Why `bring_up` failed.
enum Failed {
    /// A device was left in the kernel (a tombstone is kept).
    Orphan { dev_id: u32, error: CtlError },
    /// Anything else; nothing is left.
    Plain(CtlError),
}

/// A volume's entry in `Daemon::gc_running` while a `Gc` call waits.
struct Running<'a> {
    set: &'a Mutex<std::collections::HashSet<String>>,
    name: String,
}

impl<'a> Running<'a> {
    fn enter(set: &'a Mutex<std::collections::HashSet<String>>, name: &str) -> Option<Self> {
        lock(set).insert(name.to_owned()).then(|| Self {
            set,
            name: name.to_owned(),
        })
    }
}

impl Drop for Running<'_> {
    fn drop(&mut self) {
        lock(self.set).remove(&self.name);
    }
}

/// How long a write parked for a full journal ring may still wait for its
/// cut once a quiesce began (review N3): two thirds of `quiesce_timeout`,
/// and at most the timeout less one coordinator period (100 ms, the latency
/// of the answer), so the answer comes before `QUIESCE_DEV` gives up (final
/// review m-B, p1/bstop-fu review m1). A timeout of 100 ms or less (the root
/// fault scripts use 1 ms to make `QUIESCE_DEV` fail) gives 0: every ring
/// wait is answered at once.
pub(crate) fn close_ring_bound(quiesce_timeout: Duration) -> Duration {
    (quiesce_timeout * 2 / 3).min(quiesce_timeout.saturating_sub(COORD_PERIOD))
}

/// The engine's `background_period` (`EngineConfig::new`): how often the
/// coordinator wakes the queues that hold a ring-parked write.
const COORD_PERIOD: Duration = Duration::from_millis(100);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ring_bound_at_quiesce_is_two_thirds_of_quiesce_timeout() {
        assert_eq!(
            close_ring_bound(Duration::from_secs(30)),
            Duration::from_secs(20)
        );
        // The answer (the bound plus one coordinator period) comes inside
        // the timeout whenever the timeout is above that period.
        for ms in [150u64, 300, 600, 1000, 3000] {
            let t = Duration::from_millis(ms);
            assert!(close_ring_bound(t) + COORD_PERIOD <= t, "{ms} ms");
            assert!(close_ring_bound(t) <= t * 2 / 3, "{ms} ms");
        }
        assert_eq!(close_ring_bound(Duration::from_millis(1)), Duration::ZERO);
    }

    #[test]
    fn a_percentage_cap_is_its_share_of_the_size_with_a_floor() {
        const GIB: u64 = 1 << 30;
        let default_cache_max =
            |size, pct, dirty| (pct != 0).then(|| crate::stack::cache_cap(size, pct, dirty));
        // ADR-32 M2: 200 % of the size...
        assert_eq!(default_cache_max(32 * GIB, 200, GIB), Some(64 * GIB));
        assert_eq!(default_cache_max(32 * GIB, 150, GIB), Some(48 * GIB));
        // ...never below the size plus two commits' worth of journal.
        assert_eq!(default_cache_max(GIB, 200, GIB), Some(3 * GIB));
        assert_eq!(
            default_cache_max(64 << 20, 200, GIB),
            Some((64 << 20) + 2 * GIB)
        );
        // 0: no cap.
        assert_eq!(default_cache_max(32 * GIB, 0, GIB), None);
    }

    #[test]
    fn a_percentage_cap_follows_the_size_and_an_absolute_one_stays() {
        // FREEZE F2: what the gate gets for each size.
        const GIB: u64 = 1 << 30;
        let mut j = VolumeJson::new("v", "/d");
        j.space.cache_max_percent = Some(200);
        let at = |j: &VolumeJson, size| effective_space(j, size).cache_max;
        assert_eq!(at(&j, 32 * GIB), Some(64 * GIB));
        assert_eq!(at(&j, 100 * GIB), Some(200 * GIB), "a grow recomputes it");
        assert_eq!(effective_space(&j, GIB).cache_max_percent, None, "resolved");
        j.space.cache_max_percent = None;
        j.space.cache_max = Some(20 * GIB);
        assert_eq!(at(&j, 32 * GIB), Some(20 * GIB));
        assert_eq!(at(&j, 100 * GIB), Some(20 * GIB), "an absolute cap stays");
        j.space.cache_max = None;
        assert_eq!(at(&j, 100 * GIB), None);
    }

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
