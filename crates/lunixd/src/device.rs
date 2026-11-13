//! The block device in front of an engine (plan M12a steps 5-7; DESIGN §3).
//!
//! [`Device::Ublk`] is the real `lunix-ublk` device. [`Device::Null`] serves
//! nothing: its queue handlers are built (so the journal lanes open and the
//! stale-tail wipe happens, as with a real device) on one thread that keeps
//! them until the device goes away. It exists for the control-plane tests
//! that run without root (`device = "none"` in the config). Marker files in
//! the volume directory stand in for kernel behaviour: `null-device-busy`
//! (detach answers `Busy`, as for a mounted file system),
//! `null-device-quiesce-fails` and `null-device-quiesce-ms` (see
//! `NullDevice::quiesce`).
//!
//! # ublk rules kept here (W1, W1a, W2)
//!
//! * No stop command is ever sent: detach is `UblkDevice::detach`
//!   (`TRY_STOP_DEV`, drain, final commit, `DEL_DEV` of a stopped device),
//!   upgrade and `SIGTERM` are `UblkDevice::quiesce` and then the process
//!   exits.
//! * `delete_by_id` is called only on a device that is DEAD (never started,
//!   M10 re-review R4) and carries this volume's tag: never on a LIVE one.
//! * Queue handlers are dropped only after a drain (the layer's own; for the
//!   null device nothing is ever in flight).

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use lunix_engine::Engine;
use lunix_format::local::DeviceRecord;
use lunix_iface::block::{BlockEngine, QueueConfig, QueueId};
use lunix_iface::store::VolumeDir;
use lunix_store::LocalVolume;
use lunix_ublk::config::Timeouts;
use lunix_ublk::ctl::STATE_DEAD;
use lunix_ublk::{
    current_boot_id, volume_tag, ControlPlane, IdentityMismatch, Persist, RingControl, UblkConfig,
    UblkDevice, UblkError,
};

/// How to build a device.
#[derive(Clone, Debug)]
pub struct DeviceParams {
    /// Queues (equal to the engine's journal lanes).
    pub queues: u16,
    /// Queue depth.
    pub depth: u16,
    /// `max_io_buf_bytes`.
    pub max_io_buf_bytes: u32,
    /// The volume chunk size (`io_opt`).
    pub chunk_bytes: u32,
    /// Queue thread CPUs.
    pub cpus: Vec<usize>,
    /// Watchdogs.
    pub timeouts: Timeouts,
}

/// Why a device operation failed.
#[derive(Debug, thiserror::Error)]
pub enum DeviceError {
    /// The device has openers (mounted, open, udev probe): nothing changed.
    #[error("the device is busy (mounted or open); nothing changed")]
    Busy,
    /// The ublk layer failed.
    #[error(transparent)]
    Ublk(#[from] UblkError),
    /// The engine failed.
    #[error("engine: {0}")]
    Engine(#[from] lunix_iface::Error),
    /// The null device's thread failed.
    #[error("{0}")]
    Other(String),
}

/// A device serving one engine.
pub enum Device {
    /// A real ublk device.
    Ublk(UblkDevice),
    /// No block device (tests without root).
    Null(NullDevice),
}

impl std::fmt::Debug for Device {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ublk(d) => f.debug_tuple("Ublk").field(&d.dev_id()).finish(),
            Self::Null(_) => f.write_str("Null"),
        }
    }
}

/// What starting a device did.
pub struct Started {
    /// The device.
    pub device: Device,
    /// An existing device was recovered.
    pub recovered: bool,
}

impl Device {
    /// The ublk id (`None` for the null device).
    pub fn dev_id(&self) -> Option<u32> {
        match self {
            Self::Ublk(d) => Some(d.dev_id()),
            Self::Null(_) => None,
        }
    }

    /// `/dev/ublkbN`.
    pub fn bdev_path(&self) -> Option<String> {
        match self {
            Self::Ublk(d) => Some(d.bdev_path()),
            Self::Null(_) => None,
        }
    }

    /// Queues and depth in use.
    pub fn geometry(&self) -> (u16, u16) {
        match self {
            Self::Ublk(d) => {
                let i = d.info();
                (i.nr_hw_queues, i.queue_depth)
            }
            Self::Null(n) => (n.queues, n.depth),
        }
    }

    /// Non-fatal problems of the attach.
    pub fn warnings(&self) -> Vec<String> {
        match self {
            Self::Ublk(d) => d.warnings(),
            Self::Null(_) => Vec::new(),
        }
    }

    /// Detach: `TRY_STOP_DEV` (retried while `Busy` for at most
    /// `busy_retry`, for udev's probe of a new device), drain, the engine's
    /// final commit, `DEL_DEV`. `Busy` changes nothing.
    ///
    /// # Errors
    /// [`DeviceError::Busy`], or the layer's error (the engine's final commit
    /// failing still deletes the device).
    pub fn detach(&self, busy_retry: Duration) -> Result<(), DeviceError> {
        match self {
            Self::Ublk(d) => {
                let start = Instant::now();
                loop {
                    match d.detach() {
                        Ok(()) => return Ok(()),
                        Err(UblkError::Busy { .. }) => {
                            if start.elapsed() >= busy_retry {
                                return Err(DeviceError::Busy);
                            }
                            std::thread::sleep(Duration::from_millis(200));
                        }
                        Err(e) => return Err(e.into()),
                    }
                }
            }
            Self::Null(n) => n.detach(),
        }
    }

    /// Grows the device and the engine; the caller persists the record.
    ///
    /// # Errors
    /// The layer's or the engine's error.
    pub fn grow(&self, bytes: u64) -> Result<(), DeviceError> {
        match self {
            Self::Ublk(d) => Ok(d.grow(bytes)?),
            Self::Null(n) => Ok(n.engine.grow(bytes)?),
        }
    }

    /// The record to persist after a grow (`None` for the null device).
    pub fn record(&self) -> Option<DeviceRecord> {
        match self {
            Self::Ublk(d) => Some(d.record()),
            Self::Null(_) => None,
        }
    }

    /// `QUIESCE_DEV`, a true drain, then the engine's final commit. After
    /// `Ok` the process must exit without touching the device again (W1).
    /// The inner value is the final commit's failure, if any (the next start
    /// recovers from the journal).
    ///
    /// # Errors
    /// A quiesce or drain timeout (the device is latched quiescing; the
    /// daemon keeps running and logs) or a command error.
    pub fn quiesce(&self, timeout: Duration) -> Result<Option<String>, DeviceError> {
        match self {
            Self::Ublk(d) => {
                let q = d.quiesce(timeout)?;
                Ok(q.engine_error().map(ToString::to_string))
            }
            Self::Null(n) => n.quiesce(),
        }
    }
}

/// The device of `device = "none"`: queue handlers on one parked thread.
pub struct NullDevice {
    engine: Arc<Engine>,
    voldir: PathBuf,
    queues: u16,
    depth: u16,
    stop: Mutex<Option<mpsc::Sender<()>>>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl NullDevice {
    /// Builds every queue handler on a new thread and keeps them there.
    ///
    /// # Errors
    /// A handler that could not be built (bad lane count, journal error).
    pub fn start(
        engine: Arc<Engine>,
        voldir: &Path,
        p: &DeviceParams,
    ) -> Result<Self, DeviceError> {
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();
        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        let e = Arc::clone(&engine);
        let (queues, depth, buf) = (p.queues, p.depth, p.max_io_buf_bytes);
        let thread = std::thread::Builder::new()
            .name("lunix-nulldev".into())
            .spawn(move || {
                let mut handlers = Vec::new();
                for q in 0..queues {
                    let cfg = QueueConfig {
                        queue: QueueId(q),
                        nr_queues: queues,
                        depth,
                        max_io_buf_bytes: buf,
                    };
                    match e.queue_handler(&cfg) {
                        Ok(h) => handlers.push(h),
                        Err(err) => {
                            let _ = ready_tx.send(Err(err.to_string()));
                            return;
                        }
                    }
                }
                let _ = ready_tx.send(Ok(()));
                // Nothing is ever in flight: dropping after the stop is a drain.
                let _ = stop_rx.recv();
                drop(handlers);
            })
            .map_err(|e| DeviceError::Other(e.to_string()))?;
        match ready_rx.recv() {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                let _ = thread.join();
                return Err(DeviceError::Other(e));
            }
            Err(_) => {
                let _ = thread.join();
                return Err(DeviceError::Other("null device thread died".into()));
            }
        }
        Ok(Self {
            engine,
            voldir: voldir.to_path_buf(),
            queues,
            depth,
            stop: Mutex::new(Some(stop_tx)),
            thread: Mutex::new(Some(thread)),
        })
    }

    /// Test hooks of the shutdown tests: `null-device-quiesce-fails` makes the
    /// quiesce fail as a `QUIESCE_DEV` timeout would (the device is then
    /// latched quiescing, like a real one), `null-device-quiesce-ms` (a
    /// number) makes it take that long.
    fn quiesce(&self) -> Result<Option<String>, DeviceError> {
        if let Ok(t) = std::fs::read_to_string(self.voldir.join("null-device-quiesce-ms")) {
            if let Ok(ms) = t.trim().parse::<u64>() {
                std::thread::sleep(Duration::from_millis(ms));
            }
        }
        if self.voldir.join("null-device-quiesce-fails").exists() {
            return Err(DeviceError::Ublk(UblkError::QuiesceTimeout));
        }
        Ok(self.engine.quiesce().err().map(|e| e.to_string()))
    }

    fn detach(&self) -> Result<(), DeviceError> {
        if self.voldir.join("null-device-busy").exists() {
            return Err(DeviceError::Busy);
        }
        if let Some(tx) = crate::lock(&self.stop).take() {
            let _ = tx.send(());
        }
        if let Some(t) = crate::lock(&self.thread).take() {
            let _ = t.join();
        }
        self.engine.quiesce()?;
        Ok(())
    }
}

impl Drop for NullDevice {
    fn drop(&mut self) {
        if let Some(tx) = crate::lock(&self.stop).take() {
            let _ = tx.send(());
        }
        if let Some(t) = crate::lock(&self.thread).take() {
            let _ = t.join();
        }
    }
}

fn ublk_config(store: &LocalVolume, p: &DeviceParams) -> UblkConfig {
    let mut cfg = UblkConfig::new(store.uuid());
    cfg.queues = p.queues;
    cfg.depth = p.depth;
    cfg.max_io_buf_bytes = p.max_io_buf_bytes;
    cfg.chunk_bytes = p.chunk_bytes;
    cfg.cpus.clone_from(&p.cpus);
    cfg.timeouts = p.timeouts;
    cfg
}

/// M10 re-review R4: a crash between persisting `state/device` and
/// `START_DEV` leaves a DEAD device with this volume's tag that nothing
/// deletes. Delete it, but only after re-reading it: DEAD (never LIVE, so the
/// delete is not a stop, W1a) and carrying this volume's tag.
fn delete_dead_own_device(rec: &DeviceRecord, timeouts: Timeouts) {
    let ctl = RingControl::new(timeouts);
    let info = match ctl.dev_info(rec.dev_id) {
        Ok(i) => i,
        Err(e) => {
            tracing::warn!(dev_id = rec.dev_id, error = %e, "cannot read the dead device; left alone");
            return;
        }
    };
    if info.state != STATE_DEAD {
        return;
    }
    if info.ublksrv_flags != volume_tag(&rec.volume_uuid) {
        tracing::warn!(
            dev_id = rec.dev_id,
            "dead device carries another tag; left alone"
        );
        return;
    }
    match UblkDevice::delete_by_id(rec.dev_id, timeouts, false) {
        Ok(()) => tracing::info!(
            dev_id = rec.dev_id,
            "deleted this volume's never-started (dead) device"
        ),
        Err(e) => {
            tracing::warn!(dev_id = rec.dev_id, error = %e, "could not delete this volume's dead device; it stays until reboot")
        }
    }
}

/// Starts the ublk device of `engine`: recovers the device `state/device`
/// names when it passes the identity checks (same boot, QUIESCED or LIVE
/// without a server, same geometry and tag), else adds a new one with the
/// record written between `SET_PARAMS` and `START_DEV`.
///
/// # Errors
/// The layer's error. `UblkError::Busy` (a live server still runs) is passed
/// on; an identity refusal falls back to a new device.
pub fn start_ublk(
    engine: &Arc<Engine>,
    store: &LocalVolume,
    p: &DeviceParams,
) -> Result<Started, DeviceError> {
    let cfg = ublk_config(store, p);
    let dyn_engine: Arc<dyn BlockEngine> = engine.clone();
    if let Some(rec) = store.device_record()? {
        let same_boot = current_boot_id().is_ok_and(|b| b == rec.boot_id);
        if rec.volume_uuid == store.uuid() && same_boot {
            match UblkDevice::recover(Arc::clone(&dyn_engine), cfg.clone(), &rec) {
                Ok(d) => {
                    return Ok(Started {
                        device: Device::Ublk(d),
                        recovered: true,
                    })
                }
                Err(UblkError::Identity { dev_id, why }) => {
                    tracing::warn!(dev_id, why = %why, "recorded device not recovered; adding a new one");
                    if matches!(why, IdentityMismatch::State { state } if state == STATE_DEAD) {
                        delete_dead_own_device(&rec, p.timeouts);
                    }
                }
                Err(e) => return Err(e.into()),
            }
        } else {
            tracing::info!(
                dev_id = rec.dev_id,
                "device record from another boot or volume; adding a new device"
            );
        }
    }
    let s = store.clone();
    let persist: Persist = Box::new(move |rec: &DeviceRecord| {
        s.store_device_record(rec)
            .map_err(|e| UblkError::Io(std::io::Error::other(e.to_string())))
    });
    let d = UblkDevice::add_persisting(dyn_engine, cfg, persist)?;
    Ok(Started {
        device: Device::Ublk(d),
        recovered: false,
    })
}

/// What the kernel holds for a device record of this boot.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RecordedDevice {
    /// QUIESCED or LIVE with this volume's tag: recover it.
    Recoverable,
    /// DEAD with this volume's tag (never started, or stopped and not deleted).
    DeadOwn,
    /// No such device, or another volume's: the record is stale (a detach
    /// completed, or the id was reused).
    Gone,
    /// The kernel could not be asked (no `ublk_drv`, an error): leave it.
    Unknown,
}

/// Looks at the device `rec` names (read-only, `GET_DEV_INFO`).
pub fn probe_recorded(rec: &DeviceRecord) -> RecordedDevice {
    let ctl = RingControl::new(Timeouts::default());
    match ctl.dev_info(rec.dev_id) {
        Ok(i) if i.ublksrv_flags != volume_tag(&rec.volume_uuid) => RecordedDevice::Gone,
        Ok(i) if i.state == STATE_DEAD => RecordedDevice::DeadOwn,
        Ok(i)
            if i.state == lunix_ublk::ctl::STATE_QUIESCED
                || i.state == lunix_ublk::ctl::STATE_LIVE =>
        {
            RecordedDevice::Recoverable
        }
        Ok(_) => RecordedDevice::Unknown,
        Err(UblkError::Ctl { errno, .. }) if errno.0 == libc::ENOENT || errno.0 == libc::ENODEV => {
            RecordedDevice::Gone
        }
        Err(_) => RecordedDevice::Unknown,
    }
}

/// Deletes this volume's DEAD device of `rec` (re-checked inside).
pub fn delete_dead(rec: &DeviceRecord) {
    delete_dead_own_device(rec, Timeouts::default());
}

/// True when the kernel has a device with this id (for the tombstone of a
/// device left after a failed start).
pub fn kernel_has_device(dev_id: u32) -> bool {
    !matches!(
        RingControl::new(Timeouts::default()).dev_info(dev_id),
        Err(UblkError::Ctl { errno, .. }) if errno.0 == libc::ENOENT || errno.0 == libc::ENODEV
    )
}
