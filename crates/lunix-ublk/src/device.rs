//! [`UblkDevice`]: one attached ublk device with its queue threads.
//!
//! The device is added (or recovered) through the raw control ring, libublk
//! attaches the queues afterwards, and the layer starts it with its own
//! commands. The lifecycle commands are:
//!
//! | operation | commands | notes |
//! |---|---|---|
//! | [`UblkDevice::add_persisting`] | `GET_FEATURES`, `ADD_DEV`, `SET_PARAMS`, *persist the record*, `START_DEV` | parameters per DESIGN §3 |
//! | [`UblkDevice::recover`] | identity check, `START_USER_RECOVERY`, `END_USER_RECOVERY` | no `SET_PARAMS` |
//! | [`UblkDevice::quiesce_and_exit`] | `QUIESCE_DEV`, drain, then the process exits | never a stop command (W1) |
//! | [`UblkDevice::detach`] | `TRY_STOP_DEV`, drain, `DEL_DEV` | `Busy` while any opener exists |
//! | [`UblkDevice::grow`] | `UPDATE_SIZE` after the engine's durable commit | validated first |
//!
//! Dropping an [`UblkDevice`] neither stops nor deletes anything: the queue
//! threads keep serving, and a daemon that exits does so through one of the
//! operations above or through `abort()` (recoverable like `kill -9`).
//!
//! # What the daemon (M12) must call
//!
//! * First start of a volume: [`UblkDevice::add_persisting`] with a closure
//!   that writes `state/device` durably. The closure runs between `SET_PARAMS`
//!   and `START_DEV`, so there is no moment with a started device and no
//!   record. If it fails the new (DEAD) device is deleted.
//! * Later starts: [`UblkDevice::recover`] with the record. On
//!   [`UblkError::Identity`] (nothing was touched) add a new device.
//! * After every grow the daemon rewrites the record from
//!   [`UblkDevice::record`]; the order of engine, kernel and record does not
//!   matter for recovery (see [`crate::check_identity`]).
//! * Errors after the kernel device exists are [`UblkError::Device`] and carry
//!   its id; [`UblkDevice::warnings`] lists non-fatal problems of the attach.
//! * Before exiting abnormally the process must have closed `/dev/ublkcN`
//!   (exit or abort); another process then deletes the device with
//!   [`UblkDevice::delete_by_id`] (`force` only for a device without a server).
//! * Process setup: [`crate::harden_process`] first (mount namespace, mlock,
//!   no core dumps).
//! * **A crash between `persist` and `START_DEV`** leaves a DEAD device that
//!   carries this volume's tag and was never started. The next start gets
//!   `Identity { State { 0 } }` from `recover` and adds a new device; the old
//!   one stays until reboot. M12 should call
//!   `delete_by_id(recorded_id, .., false)` on it (a DEAD device is not LIVE,
//!   so the delete is neither refused nor a stop).
//! * libublk prints `device reload json failed` once per attach (its control
//!   constructor); it is harmless.

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::Duration;

use libublk::ctrl::UblkCtrlBuilder;
use libublk::io::UblkDev;
use libublk::UblkFlags;
use lunix_format::local::DeviceRecord;
use lunix_iface::block::{BlockEngine, ResizeHook};
use lunix_iface::wake::WakeFd;

use crate::config::{DataPath, UblkConfig};
use crate::ctl::{ControlPlane, DevInfo, RingControl, STATE_LIVE};
use crate::error::{Result, UblkError};
use crate::features::flag;
use crate::identity::{current_boot_id, make_record, other_process_alive};
use crate::queue::{
    fatal_device, queue_thread, Ended, QueueArgs, QueueCounters, QueueSnapshot, Ready, StartGate,
};
use crate::session::{self, SizeGuard};
use crate::watchdog::run_with_timeout;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Lifecycle {
    Serving,
    /// `QUIESCE_DEV` was sent: the device is canceling. Only `quiesce` (to
    /// retry) and `crash_for_recovery` are accepted.
    Quiescing,
    Stopping,
    Deleted,
}

struct Shared {
    ctl: Arc<dyn ControlPlane>,
    dev_id: u32,
    info: DevInfo,
    cfg: UblkConfig,
    boot_id: [u8; 16],
    size: SizeGuard,
    /// Serialises `UPDATE_SIZE` between `grow` and the engine's resize hook.
    resize_lock: Mutex<()>,
    life: Mutex<Lifecycle>,
    warnings: Mutex<Vec<String>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// Tells the kernel about a grow the engine has made durable.
struct ResizeBridge(Arc<Shared>);

impl ResizeHook for ResizeBridge {
    fn resized(&self, new_size_bytes: u64) -> lunix_iface::Result<()> {
        // Only a serving device is told about a grow: after a detach or a
        // quiesce the id may belong to another device by the time the engine
        // grows.
        if *lock(&self.0.life) != Lifecycle::Serving {
            return Err(lunix_iface::Error::Io(std::io::Error::other(
                "the device is no longer serving; UPDATE_SIZE not sent",
            )));
        }
        let _g = lock(&self.0.resize_lock);
        session::update_size(&*self.0.ctl, self.0.dev_id, &self.0.size, new_size_bytes)
            .map_err(|e| lunix_iface::Error::Io(std::io::Error::other(e.to_string())))
    }
}

/// The hook of an engine whose device is gone (detached, or never started):
/// nothing is sent, so a later `engine.grow` cannot reach a reused device id.
struct DetachedHook;

impl ResizeHook for DetachedHook {
    fn resized(&self, _new_size_bytes: u64) -> lunix_iface::Result<()> {
        Ok(())
    }
}

/// Writes the identity record of a new device durably. Called between
/// `SET_PARAMS` and `START_DEV`.
pub type Persist = Box<dyn FnOnce(&DeviceRecord) -> Result<()>>;

/// How to bring the device up.
enum Mode<'a> {
    New(Option<Persist>),
    Recover(&'a DeviceRecord),
}

/// What `UblkDevice::start` needs once the device exists in the kernel.
struct StartPlan {
    ctl: Arc<dyn ControlPlane>,
    engine: Arc<dyn BlockEngine>,
    cfg: UblkConfig,
    info: DevInfo,
    kernel_bytes: u64,
    boot_id: [u8; 16],
    recovering: bool,
}

/// What to do with the kernel device when `start` fails.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Cleanup {
    /// A fresh device that never started and has no queue attached: delete it.
    Delete,
    /// Leave it (a recovering device, or a command may still be pending).
    Leave,
}

/// A device that has been quiesced and may now be left by exiting.
#[derive(Debug)]
pub struct Quiesced {
    engine_error: Option<lunix_iface::Error>,
}

impl Quiesced {
    /// The failure of the engine's final commit, if there was one. The next
    /// start recovers from the journal.
    pub fn engine_error(&self) -> Option<&lunix_iface::Error> {
        self.engine_error.as_ref()
    }

    /// Exits the process: code 0, or 70 when the engine's final commit failed
    /// (the message is printed first). Runs no destructors and sends no stop
    /// command: the exit closes `/dev/ublkcN`, which moves the device to
    /// `QUIESCED` (W1).
    pub fn exit(self) -> ! {
        match self.engine_error {
            None => std::process::exit(0),
            Some(e) => {
                eprintln!(
                    "lunix-ublk: engine quiesce failed after QUIESCE_DEV: {e}; exiting for recovery"
                );
                std::process::exit(70)
            }
        }
    }
}

/// An attached device. See the module documentation.
pub struct UblkDevice {
    shared: Arc<Shared>,
    engine: Arc<dyn BlockEngine>,
    libublk_dev: Mutex<Option<Arc<UblkDev>>>,
    threads: Mutex<Vec<JoinHandle<()>>>,
    ended: Arc<Ended>,
    counters: Vec<Arc<QueueCounters>>,
    /// One per queue: signalled to take the queue down (drain) without
    /// waiting for an `ABORT` that a fully busy queue never gets.
    stops: Vec<WakeFd>,
    /// An engine quiesce is running (or timed out and may still run).
    engine_quiescing: Arc<AtomicBool>,
}

impl fmt::Debug for UblkDevice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UblkDevice")
            .field("dev_id", &self.shared.dev_id)
            .field("queues", &self.counters.len())
            .finish_non_exhaustive()
    }
}

fn build_libublk_dev(info: DevInfo, size_bytes: u64) -> Result<UblkDev> {
    let id = i32::try_from(info.dev_id).map_err(|_| UblkError::Config("device id too large"))?;
    // No `ADD_DEV` flag: libublk neither adds nor (on drop) deletes anything.
    let ctrl = UblkCtrlBuilder::default()
        .name("lunix")
        .id(id)
        .nr_queues(info.nr_hw_queues)
        .depth(info.queue_depth)
        .io_buf_bytes(info.max_io_buf_bytes)
        .dev_flags(UblkFlags::empty())
        .build()
        .map_err(|e| UblkError::Libublk(format!("control: {e}")))?;
    let depth = info.queue_depth;
    UblkDev::new(
        "lunix".to_string(),
        move |dev| {
            dev.tgt.dev_size = size_bytes;
            // One more SQE than tags for the wake poll, a CQ of twice that.
            dev.tgt.sq_depth = depth.saturating_add(8);
            dev.tgt.cq_depth = depth.saturating_mul(2).saturating_add(8);
            Ok(())
        },
        &ctrl,
    )
    .map_err(|e| UblkError::Libublk(format!("device: {e}")))
}

/// The engine's final commit, bounded by a watchdog. The outer error is the
/// timeout, the inner option the engine's own failure.
///
/// `running` is set while a call is in progress, also after its watchdog
/// fired: a retry then reports instead of calling `engine.quiesce()` a second
/// time while the first may still run.
fn engine_quiesce(
    engine: &Arc<dyn BlockEngine>,
    running: &Arc<AtomicBool>,
    bound: Duration,
) -> Result<Option<lunix_iface::Error>> {
    if running.swap(true, Ordering::AcqRel) {
        return Err(UblkError::State(
            "an earlier engine quiesce is still running",
        ));
    }
    let (e, r) = (Arc::clone(engine), Arc::clone(running));
    let out = run_with_timeout("engine quiesce", bound, move || {
        let res = e.quiesce();
        r.store(false, Ordering::Release);
        res
    })?;
    Ok(out.err())
}

impl UblkDevice {
    /// Creates a device for `engine` (its size and handlers), starts it and
    /// returns once `START_DEV` has succeeded. **No record is persisted before
    /// the start**: a daemon that dies right after `START_DEV` leaves a device
    /// nobody can recover. Production callers use
    /// [`UblkDevice::add_persisting`].
    ///
    /// # Errors
    /// As [`UblkDevice::add_persisting`].
    pub fn add(engine: Arc<dyn BlockEngine>, cfg: UblkConfig) -> Result<Self> {
        let ctl: Arc<dyn ControlPlane> = Arc::new(RingControl::new(cfg.timeouts));
        Self::attach(ctl, engine, cfg, Mode::New(None))
    }

    /// [`UblkDevice::add`] with the record written by `persist` after
    /// `SET_PARAMS` and before `START_DEV`. The device is DEAD until
    /// `START_DEV`, so a failing `persist` deletes it harmlessly.
    ///
    /// The caller prepares the process first ([`crate::harden_process`]).
    ///
    /// # Errors
    /// A configuration or feature problem, the error of `persist`, a failing
    /// command, or a queue that could not start. Errors after `ADD_DEV` are
    /// [`UblkError::Device`] (carry the id). A fresh device that never
    /// started is deleted again; if `START_DEV` itself times out the command
    /// may still be pending and the caller reports and exits (the id is in
    /// the error; another process deletes the device).
    pub fn add_persisting(
        engine: Arc<dyn BlockEngine>,
        cfg: UblkConfig,
        persist: Persist,
    ) -> Result<Self> {
        let ctl: Arc<dyn ControlPlane> = Arc::new(RingControl::new(cfg.timeouts));
        Self::attach(ctl, engine, cfg, Mode::New(Some(persist)))
    }

    /// [`UblkDevice::add_persisting`] over a given control plane.
    ///
    /// # Errors
    /// As [`UblkDevice::add_persisting`].
    pub fn add_with(
        ctl: Arc<dyn ControlPlane>,
        engine: Arc<dyn BlockEngine>,
        cfg: UblkConfig,
        persist: Option<Persist>,
    ) -> Result<Self> {
        Self::attach(ctl, engine, cfg, Mode::New(persist))
    }

    /// Recovers the device `record` describes after a crash or a quiesce, if
    /// and only if it passes the identity checks of DESIGN §3
    /// ([`crate::check_identity`]). `cfg.queues`, `cfg.depth` and
    /// `cfg.max_io_buf_bytes` are ignored: the device's own values (equal to
    /// the record's) rule. A failed check is [`UblkError::Identity`] and
    /// leaves the device alone; the caller then adds a new device.
    ///
    /// # Errors
    /// [`UblkError::Identity`], [`UblkError::Busy`] when a daemon still
    /// serves the device after the wait, or any failing command.
    pub fn recover(
        engine: Arc<dyn BlockEngine>,
        cfg: UblkConfig,
        record: &DeviceRecord,
    ) -> Result<Self> {
        let ctl: Arc<dyn ControlPlane> = Arc::new(RingControl::new(cfg.timeouts));
        Self::recover_with(ctl, engine, cfg, record)
    }

    /// [`UblkDevice::recover`] over a given control plane.
    ///
    /// # Errors
    /// As [`UblkDevice::recover`].
    pub fn recover_with(
        ctl: Arc<dyn ControlPlane>,
        engine: Arc<dyn BlockEngine>,
        cfg: UblkConfig,
        record: &DeviceRecord,
    ) -> Result<Self> {
        Self::attach(ctl, engine, cfg, Mode::Recover(record))
    }

    /// Deletes a device by id (the `del` of the harness) under the `DEL_DEV`
    /// watchdog. Blocks while a file system is mounted on it (W5): the
    /// watchdog reports, nothing is killed.
    ///
    /// **`DEL_DEV` of a LIVE device is a `STOP_DEV`** (the kernel stops it and
    /// waits for every started request): sent by the process that serves the
    /// device it is the phase 0 deadlock. This function therefore refuses a
    /// `LIVE` device unless `force` is set, and `force` is for another process
    /// deleting a device whose server is gone. A serving process detaches with
    /// [`UblkDevice::detach`] and never calls this on its own device.
    ///
    /// # Errors
    /// [`UblkError::State`] for a LIVE device without `force`, the command's
    /// error, or [`UblkError::Timeout`].
    pub fn delete_by_id(dev_id: u32, timeouts: crate::config::Timeouts, force: bool) -> Result<()> {
        let ctl = RingControl::new(timeouts);
        if !force && ctl.dev_info(dev_id)?.state == STATE_LIVE {
            return Err(UblkError::State(
                "DEL_DEV of a LIVE device is a stop: refused (use detach, or force it from another process once the server is gone)",
            ));
        }
        ctl.del_dev(dev_id)
    }

    fn attach(
        ctl: Arc<dyn ControlPlane>,
        engine: Arc<dyn BlockEngine>,
        mut cfg: UblkConfig,
        mode: Mode<'_>,
    ) -> Result<Self> {
        cfg.validate()?;
        let engine_bytes = engine.size_bytes();
        let features = session::probe(&*ctl)?;
        let boot_id = current_boot_id()?;
        let (info, kernel_bytes, recovering) = match mode {
            Mode::New(persist) => {
                let info = session::add_device(&*ctl, features, &cfg, engine_bytes)?;
                if let Some(persist) = persist {
                    let rec = make_record(&info, cfg.volume, boot_id, engine_bytes >> 9);
                    if let Err(e) = persist(&rec) {
                        // Never started: deleting cannot block.
                        let _ = ctl.del_dev(info.dev_id);
                        return Err(UblkError::Device {
                            dev_id: info.dev_id,
                            source: Box::new(e),
                        });
                    }
                }
                (info, engine_bytes, false)
            }
            Mode::Recover(rec) => {
                let view = session::begin_recovery(
                    &*ctl,
                    rec,
                    &cfg,
                    boot_id,
                    engine_bytes,
                    &other_process_alive,
                )?;
                (view.info, view.dev_sectors << 9, true)
            }
        };
        // The device's own values rule from here on.
        cfg.queues = info.nr_hw_queues;
        cfg.depth = info.queue_depth;
        cfg.max_io_buf_bytes = info.max_io_buf_bytes;
        cfg.data_path = if info.flags & flag::USER_COPY != 0 {
            DataPath::UserCopy
        } else {
            DataPath::Copy
        };
        let plan = StartPlan {
            ctl: Arc::clone(&ctl),
            engine,
            cfg,
            info,
            kernel_bytes,
            boot_id,
            recovering,
        };
        match Self::start(plan) {
            Ok(d) => Ok(d),
            Err((e, cleanup)) => {
                if cleanup == Cleanup::Delete {
                    // Never started, no queue attached: deleting is safe.
                    let _ = ctl.del_dev(info.dev_id);
                }
                Err(UblkError::Device {
                    dev_id: info.dev_id,
                    source: Box::new(e),
                })
            }
        }
    }

    /// Takes queue threads that were gated through but whose device failed to
    /// start down again: they leave once nothing is in flight (nothing ever
    /// was). Returns whether all of them ended in time.
    fn release_queues(
        stops: &[WakeFd],
        ended: &Ended,
        handles: &mut Vec<JoinHandle<()>>,
        wait: Duration,
    ) -> bool {
        for s in stops {
            s.signal();
        }
        let all = ended.wait_for(handles.len(), wait);
        if all {
            for h in handles.drain(..) {
                let _ = h.join();
            }
        }
        all
    }

    /// Builds the queues and starts the device.
    fn start(plan: StartPlan) -> std::result::Result<Self, (UblkError, Cleanup)> {
        let StartPlan {
            ctl,
            engine,
            cfg,
            info,
            kernel_bytes,
            boot_id,
            recovering,
        } = plan;
        let before_queues = if recovering {
            Cleanup::Leave
        } else {
            Cleanup::Delete
        };
        let t = cfg.timeouts;
        let dev = run_with_timeout("attach", t.ctl, move || {
            build_libublk_dev(info, kernel_bytes)
        })
        .and_then(|r| r)
        .map(Arc::new)
        .map_err(|e| (e, before_queues))?;

        let n = usize::from(cfg.queues);
        let gate = Arc::new(StartGate::new());
        let ended = Arc::new(Ended::default());
        let (tx, rx) = std::sync::mpsc::channel::<Ready>();
        let counters: Vec<Arc<QueueCounters>> =
            (0..n).map(|_| Arc::new(QueueCounters::default())).collect();
        let stops: Vec<WakeFd> = (0..n)
            .map(|_| WakeFd::new())
            .collect::<std::io::Result<_>>()
            .map_err(|e| (UblkError::Io(e), before_queues))?;
        let user_copy = cfg.data_path == DataPath::UserCopy;
        let mut handles = Vec::with_capacity(n);
        let mut spawn_err = None;
        for (q, counter) in counters.iter().enumerate() {
            let qid = u16::try_from(q).unwrap_or(u16::MAX);
            let args = QueueArgs {
                qid,
                dev: Arc::clone(&dev),
                engine: Arc::clone(&engine),
                nr_queues: cfg.queues,
                depth: cfg.depth,
                buf_bytes: cfg.max_io_buf_bytes,
                user_copy,
                cpu: if cfg.cpus.is_empty() {
                    None
                } else {
                    Some(cfg.cpus[q % cfg.cpus.len()])
                },
                counters: Arc::clone(counter),
                gate: Arc::clone(&gate),
                ready: tx.clone(),
                ended: Arc::clone(&ended),
                stop: stops[q].clone(),
            };
            match std::thread::Builder::new()
                .name(format!("lunix-q{q}"))
                .spawn(move || queue_thread(args))
            {
                Ok(h) => handles.push(h),
                Err(e) => {
                    spawn_err = Some(UblkError::Io(e));
                    break;
                }
            }
        }
        drop(tx);

        // Wait for every started thread to report; any failure abandons the
        // attach before a single FETCH was submitted.
        let mut failure = spawn_err;
        let mut reported = 0usize;
        let deadline = std::time::Instant::now() + t.startup;
        while reported < handles.len() {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            match rx.recv_timeout(left) {
                Ok((_, Ok(()))) => reported += 1,
                Ok((queue, Err(why))) => {
                    reported += 1;
                    failure.get_or_insert(UblkError::QueueStart { queue, why });
                }
                Err(_) => {
                    failure.get_or_insert(UblkError::Timeout {
                        op: "queue start",
                        after: t.startup,
                    });
                    break;
                }
            }
        }
        if let Some(e) = failure {
            gate.decide(false);
            // The threads exit without having fetched anything.
            let all_gone = ended.wait_for(handles.len(), t.drain);
            if all_gone {
                for h in handles {
                    let _ = h.join();
                }
                drop(dev);
                return Err((e, before_queues));
            }
            // A thread is stuck in the engine or the kernel: leave everything
            // as it is and report (nothing may be deleted under it).
            return Err((e, Cleanup::Leave));
        }

        let shared = Arc::new(Shared {
            ctl: Arc::clone(&ctl),
            dev_id: info.dev_id,
            info,
            boot_id,
            size: SizeGuard::new(kernel_bytes, cfg.max_size_bytes.max(engine.size_bytes())),
            cfg,
            resize_lock: Mutex::new(()),
            life: Mutex::new(Lifecycle::Serving),
            warnings: Mutex::new(Vec::new()),
        });
        engine.set_on_resize(Arc::new(ResizeBridge(Arc::clone(&shared))));
        gate.decide(true);

        let pid = i32::try_from(std::process::id()).unwrap_or(0);
        let started = if recovering {
            ctl.end_user_recovery(info.dev_id, pid)
        } else {
            ctl.start_dev(info.dev_id, pid)
        };
        if let Err(e) = started {
            engine.set_on_resize(Arc::new(DetachedHook));
            // A timeout leaves the command pending in the kernel: touch
            // nothing. Any other error means the device did not start: take
            // the queues down, close the char device, and for a fresh device
            // delete it (state DEAD, the delete cannot block).
            if matches!(e, UblkError::Timeout { .. }) {
                return Err((e, Cleanup::Leave));
            }
            if Self::release_queues(&stops, &ended, &mut handles, t.drain) {
                drop(dev);
                return Err((e, before_queues));
            }
            return Err((e, Cleanup::Leave));
        }

        let device = Self {
            shared,
            engine,
            libublk_dev: Mutex::new(Some(dev)),
            threads: Mutex::new(handles),
            ended,
            counters,
            stops,
            engine_quiescing: Arc::new(AtomicBool::new(false)),
        };
        // A grow that was durable in the engine but not yet in the kernel when
        // the old daemon died: catch the device up. A failure here is a
        // warning next to a serving device, not an error.
        let engine_bytes = device.engine.size_bytes();
        if engine_bytes > device.shared.size.current() {
            let _g = lock(&device.shared.resize_lock);
            if let Err(e) =
                session::update_size(&*ctl, info.dev_id, &device.shared.size, engine_bytes)
            {
                lock(&device.shared.warnings).push(format!(
                    "catching the device up to the engine's size failed: {e}"
                ));
            }
        }
        Ok(device)
    }

    /// The kernel's device id (`/dev/ublkbN`).
    pub fn dev_id(&self) -> u32 {
        self.shared.dev_id
    }

    /// `/dev/ublkbN`.
    pub fn bdev_path(&self) -> String {
        format!("/dev/ublkb{}", self.shared.dev_id)
    }

    /// The device as the kernel reported it after the attach.
    pub fn info(&self) -> DevInfo {
        self.shared.info
    }

    /// Non-fatal problems of the attach (for example a failed catch-up
    /// `UPDATE_SIZE`: the device serves with the older size until retried
    /// through [`UblkDevice::grow`]).
    pub fn warnings(&self) -> Vec<String> {
        lock(&self.shared.warnings).clone()
    }

    /// The identity record for `state/device`, with the size the kernel has
    /// now. Persist it after every grow.
    pub fn record(&self) -> DeviceRecord {
        make_record(
            &self.shared.info,
            self.shared.cfg.volume,
            self.shared.boot_id,
            self.shared.size.current() >> 9,
        )
    }

    /// The engine the device serves.
    pub fn engine(&self) -> &Arc<dyn BlockEngine> {
        &self.engine
    }

    /// Per-queue counters.
    pub fn queue_counters(&self) -> Vec<QueueSnapshot> {
        self.counters.iter().map(|c| c.snapshot()).collect()
    }

    /// Requests taken from the driver and not yet committed, over all queues.
    pub fn in_flight(&self) -> u64 {
        self.counters.iter().map(|c| c.in_flight()).sum()
    }

    /// The size the kernel has, in bytes.
    pub fn size_bytes(&self) -> u64 {
        self.shared.size.current()
    }

    /// Waits until every queue thread has ended (the device was stopped or
    /// deleted from outside, or [`UblkDevice::detach`] drained them). `false`
    /// on timeout.
    pub fn wait_queues_ended(&self, timeout: Duration) -> bool {
        self.ended.wait_for(self.counters.len(), timeout)
    }

    fn check_serving(&self) -> Result<()> {
        match *lock(&self.shared.life) {
            Lifecycle::Serving => Ok(()),
            Lifecycle::Quiescing => Err(UblkError::State(
                "the device is quiescing: retry quiesce or crash for recovery",
            )),
            Lifecycle::Stopping => Err(UblkError::State("the device is stopping")),
            Lifecycle::Deleted => Err(UblkError::State("the device was deleted")),
        }
    }

    /// Grows the device to `new_bytes`.
    ///
    /// The size is validated first (a multiple of 4096, not below the current
    /// size, not above the configured maximum; the driver would accept
    /// anything, ADR-20). The engine then commits the new size durably and
    /// calls the resize hook, which sends `UPDATE_SIZE`. Growing to the
    /// current size is a no-op. Control thread only.
    ///
    /// # Errors
    /// [`UblkError::InvalidSize`] without any engine or kernel call, the
    /// engine's error, or the `UPDATE_SIZE` error.
    pub fn grow(&self, new_bytes: u64) -> Result<()> {
        self.check_serving()?;
        if self.shared.size.validate(new_bytes)? == session::SizeChange::Same {
            return Ok(());
        }
        self.engine.grow(new_bytes)?;
        // The hook has normally sent UPDATE_SIZE already (then this is a
        // no-op); an engine that did not call it is caught up here.
        let _g = lock(&self.shared.resize_lock);
        session::update_size(
            &*self.shared.ctl,
            self.shared.dev_id,
            &self.shared.size,
            new_bytes,
        )
    }

    /// `QUIESCE_DEV`, then a true drain, then the engine's final commit; the
    /// caller may then exit through [`Quiesced::exit`]. No stop command is
    /// ever sent.
    ///
    /// The driver is satisfied once every queue has an idle command left to
    /// notify the server with; requests the engine still holds stay with it.
    /// The queue threads keep running: they complete those requests and commit
    /// them with commands of the layer's own (libublk would drop them), then
    /// end. This call waits for that (`Timeouts::drain`; once more after a
    /// second `QUIESCE_DEV`, which also cancels idle commands the driver left
    /// alone), so the kernel has seen every commit and the successor has
    /// nothing to replay. Only then does the engine commit finally (bounded by
    /// `Timeouts::engine`).
    ///
    /// **A timeout is not harmless**: the driver marks the device canceling
    /// before it waits and does not undo that, so after
    /// [`UblkError::QuiesceTimeout`] or [`UblkError::DrainTimeout`] the device
    /// refuses new I/O. The device is latched `Quiescing`: only this call
    /// again, or [`UblkDevice::crash_for_recovery`], is accepted; it is never
    /// detached or deleted in this state.
    ///
    /// # Errors
    /// [`UblkError::QuiesceTimeout`], [`UblkError::DrainTimeout`] or the
    /// command's error.
    pub fn quiesce(&self, timeout: Duration) -> Result<Quiesced> {
        {
            let life = *lock(&self.shared.life);
            if !matches!(life, Lifecycle::Serving | Lifecycle::Quiescing) {
                self.check_serving()?;
            }
        }
        let t = self.shared.cfg.timeouts;
        let sent = session::quiesce(&*self.shared.ctl, self.shared.dev_id, timeout);
        if matches!(
            sent,
            Ok(()) | Err(UblkError::QuiesceTimeout | UblkError::Timeout { .. })
        ) {
            // The driver marked the device canceling, or the command is still
            // pending and will.
            *lock(&self.shared.life) = Lifecycle::Quiescing;
        }
        sent?;
        // The driver cancels only idle commands. A queue whose every tag is
        // held by the engine has none, never sees an `ABORT`, and would keep
        // waiting in libublk: tell every queue it is down.
        for s in &self.stops {
            s.signal();
        }
        if !self.ended.wait_for(self.counters.len(), t.drain) {
            // The driver cancels only idle commands; a second QUIESCE_DEV
            // cancels the rest.
            session::quiesce(&*self.shared.ctl, self.shared.dev_id, timeout)?;
            if !self.ended.wait_for(self.counters.len(), t.drain) {
                return Err(UblkError::DrainTimeout { after: t.drain });
            }
        }
        // Queue threads end only with nothing in flight; anything else is a bug.
        let left = self.in_flight();
        if left != 0 {
            fatal_device(&format!(
                "{left} request(s) in flight after the queues ended"
            ));
        }
        let engine_error = engine_quiesce(&self.engine, &self.engine_quiescing, t.engine)?;
        Ok(Quiesced { engine_error })
    }

    /// [`UblkDevice::quiesce`], then [`Quiesced::exit`]. Returns only when the
    /// quiesce failed; on success the process is gone.
    pub fn quiesce_and_exit(&self, timeout: Duration) -> UblkError {
        match self.quiesce(timeout) {
            Ok(q) => q.exit(),
            Err(e) => e,
        }
    }

    /// Ends the process like `kill -9` (`abort()`), which phase 0 proved
    /// recoverable with `USER_RECOVERY_REISSUE`. The sanctioned way out when
    /// a quiesce keeps timing out.
    pub fn crash_for_recovery(&self) -> ! {
        fatal_device("crash for recovery requested")
    }

    /// `TRY_STOP_DEV` alone: [`UblkError::Busy`] while the block device has
    /// any opener (a mount, an open file descriptor, or udev's probe just
    /// after the device appeared; the device stays and keeps serving). After
    /// success the queue threads end by themselves; [`UblkDevice::detach`]
    /// drains them.
    ///
    /// # Errors
    /// [`UblkError::Busy`], [`UblkError::Unsupported`] without
    /// `SAFE_STOP_DEV`, [`UblkError::State`] when quiescing, or the
    /// command's error.
    pub fn try_stop(&self) -> Result<()> {
        self.check_serving()?;
        if self.shared.info.flags & flag::SAFE_STOP_DEV == 0 {
            return Err(UblkError::Unsupported(
                "the device has no SAFE_STOP_DEV (kernel 7.0 or newer); detach would need a stop command, which is never sent",
            ));
        }
        session::try_stop(&*self.shared.ctl, self.shared.dev_id)?;
        *lock(&self.shared.life) = Lifecycle::Stopping;
        for s in &self.stops {
            s.signal();
        }
        Ok(())
    }

    /// Detach: [`UblkDevice::try_stop`], wait for the queue threads to end,
    /// the engine's final commit, then `DEL_DEV` under its 60 s watchdog (the
    /// device is STOPPED by then, so the delete is not a stop).
    ///
    /// Everything after a successful `TRY_STOP_DEV` is bounded by a wait that
    /// reports instead of killing. If the engine's final commit fails the
    /// device is still deleted and the error is returned.
    ///
    /// # Errors
    /// [`UblkError::Busy`] (nothing changed), [`UblkError::DrainTimeout`],
    /// the engine's final-commit error, or the `DEL_DEV` error.
    pub fn detach(&self) -> Result<()> {
        let life = *lock(&self.shared.life);
        match life {
            Lifecycle::Serving => self.try_stop()?,
            Lifecycle::Stopping => {}
            _ => self.check_serving()?,
        }
        let t = self.shared.cfg.timeouts;
        if !self.ended.wait_for(self.counters.len(), t.drain) {
            return Err(UblkError::DrainTimeout { after: t.drain });
        }
        let handles = std::mem::take(&mut *lock(&self.threads));
        for h in handles {
            let _ = h.join();
        }
        let left = self.in_flight();
        if left != 0 {
            fatal_device(&format!(
                "{left} request(s) in flight after the queues ended"
            ));
        }
        let engine_error = engine_quiesce(&self.engine, &self.engine_quiescing, t.engine)?;
        // Close /dev/ublkcN: DEL_DEV waits for the last reference.
        drop(lock(&self.libublk_dev).take());
        self.shared.ctl.del_dev(self.shared.dev_id)?;
        *lock(&self.shared.life) = Lifecycle::Deleted;
        self.engine.set_on_resize(Arc::new(DetachedHook));
        engine_error.map_or(Ok(()), |e| Err(UblkError::from(e)))
    }
}
