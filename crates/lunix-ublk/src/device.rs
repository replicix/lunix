//! [`UblkDevice`]: one attached ublk device with its queue threads.
//!
//! The device is added (or recovered) through the raw control ring, libublk
//! attaches the queues afterwards, and the layer starts it with its own
//! commands. The lifecycle commands are:
//!
//! | operation | commands | notes |
//! |---|---|---|
//! | [`UblkDevice::add`] | `GET_FEATURES`, `ADD_DEV`, `SET_PARAMS`, `START_DEV` | parameters per DESIGN §3 |
//! | [`UblkDevice::recover`] | identity check, `START_USER_RECOVERY`, `END_USER_RECOVERY` | no `SET_PARAMS` |
//! | [`UblkDevice::quiesce_and_exit`] | `QUIESCE_DEV`, then the process exits | never a stop command (W1) |
//! | [`UblkDevice::detach`] | `TRY_STOP_DEV`, drain, `DEL_DEV` | `Busy` while mounted |
//! | [`UblkDevice::grow`] | `UPDATE_SIZE` after the engine's durable commit | validated first |
//!
//! Dropping an [`UblkDevice`] neither stops nor deletes anything: the queue
//! threads keep serving, and a daemon that exits does so through one of the
//! operations above or through `abort()` (recoverable like `kill -9`).

use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::Duration;

use libublk::ctrl::UblkCtrlBuilder;
use libublk::io::UblkDev;
use libublk::UblkFlags;
use lunix_format::local::DeviceRecord;
use lunix_iface::block::{BlockEngine, ResizeHook};

use crate::config::{DataPath, UblkConfig};
use crate::ctl::{ControlPlane, DevInfo, RingControl};
use crate::error::{Result, UblkError};
use crate::features::{flag, Features};
use crate::identity::{current_boot_id, make_record};
use crate::queue::{
    fatal_device, queue_thread, Ended, QueueArgs, QueueCounters, QueueSnapshot, Ready, StartGate,
};
use crate::session::{self, SizeGuard};
use crate::watchdog::run_with_timeout;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Lifecycle {
    Serving,
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
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// Tells the kernel about a grow the engine has made durable.
struct ResizeBridge(Arc<Shared>);

impl ResizeHook for ResizeBridge {
    fn resized(&self, new_size_bytes: u64) -> lunix_iface::Result<()> {
        let _g = lock(&self.0.resize_lock);
        session::update_size(&*self.0.ctl, self.0.dev_id, &self.0.size, new_size_bytes)
            .map_err(|e| lunix_iface::Error::Io(std::io::Error::other(e.to_string())))
    }
}

/// How to bring the device up.
enum Mode<'a> {
    New,
    Recover(&'a DeviceRecord),
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
                eprintln!("lunix-ublk: engine quiesce failed after QUIESCE_DEV: {e}; exiting for recovery");
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

impl UblkDevice {
    /// Creates a device for `engine` (its size and handlers), starts it and
    /// returns once `START_DEV` has succeeded.
    ///
    /// The caller prepares the process first ([`crate::harden_process`]) and
    /// persists [`UblkDevice::record`] to `state/device`.
    ///
    /// # Errors
    /// A configuration or feature problem, a failing command, or a queue that
    /// could not start. If the failure comes before `START_DEV` the new device
    /// is deleted again; if `START_DEV` itself fails the error says so and the
    /// caller reports and exits (the device id is in the error text of the
    /// command; `lunix-ublk-testd del` removes it).
    pub fn add(engine: Arc<dyn BlockEngine>, cfg: UblkConfig) -> Result<Self> {
        let ctl: Arc<dyn ControlPlane> = Arc::new(RingControl::new(cfg.timeouts));
        Self::add_with(ctl, engine, cfg)
    }

    /// [`UblkDevice::add`] over a given control plane.
    ///
    /// # Errors
    /// As [`UblkDevice::add`].
    pub fn add_with(
        ctl: Arc<dyn ControlPlane>,
        engine: Arc<dyn BlockEngine>,
        cfg: UblkConfig,
    ) -> Result<Self> {
        Self::attach(ctl, engine, cfg, Mode::New)
    }

    /// Recovers the device `record` describes after a crash or a quiesce, if
    /// and only if it passes the identity checks of DESIGN §3. `cfg.queues`,
    /// `cfg.depth` and `cfg.max_io_buf_bytes` are ignored: the device's own
    /// values (equal to the record's) rule. A failed check is
    /// [`UblkError::Identity`] and leaves the device alone; the caller then
    /// adds a new device.
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

    /// Deletes a device by id (the `del` of the harness): `DEL_DEV` under the
    /// `DEL_DEV` watchdog. Works on devices in any state, including a
    /// recovering one. Blocks while a file system is mounted on it (W5): the
    /// watchdog reports, nothing is killed.
    ///
    /// # Errors
    /// The command's error or [`UblkError::Timeout`].
    pub fn delete_by_id(dev_id: u32, timeouts: crate::config::Timeouts) -> Result<()> {
        RingControl::new(timeouts).del_dev(dev_id)
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
            Mode::New => {
                let info = session::add_device(&*ctl, features, &cfg, engine_bytes)?;
                (info, engine_bytes, false)
            }
            Mode::Recover(rec) => {
                let view = session::begin_recovery(&*ctl, rec, &cfg, boot_id, engine_bytes)?;
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
        match Self::start(
            ctl.clone(),
            engine,
            cfg,
            features,
            info,
            kernel_bytes,
            boot_id,
            recovering,
        ) {
            Ok(d) => Ok(d),
            Err((e, delete_fresh)) => {
                if delete_fresh {
                    // Never started, no queue attached: deleting is safe.
                    let _ = ctl.del_dev(info.dev_id);
                }
                Err(e)
            }
        }
    }

    /// Builds the queues and starts the device. The error carries whether the
    /// device is a fresh one that never started and can be deleted again.
    #[allow(clippy::too_many_arguments)]
    fn start(
        ctl: Arc<dyn ControlPlane>,
        engine: Arc<dyn BlockEngine>,
        cfg: UblkConfig,
        _features: Features,
        info: DevInfo,
        kernel_bytes: u64,
        boot_id: [u8; 16],
        recovering: bool,
    ) -> std::result::Result<Self, (UblkError, bool)> {
        let fresh = !recovering;
        let fail = |e: UblkError| (e, fresh);
        let t = cfg.timeouts;
        let dev = run_with_timeout("attach", t.ctl, move || {
            build_libublk_dev(info, kernel_bytes)
        })
        .and_then(|r| r)
        .map(Arc::new)
        .map_err(fail)?;

        let n = usize::from(cfg.queues);
        let gate = Arc::new(StartGate::new());
        let ended = Arc::new(Ended::default());
        let (tx, rx) = std::sync::mpsc::channel::<Ready>();
        let counters: Vec<Arc<QueueCounters>> =
            (0..n).map(|_| Arc::new(QueueCounters::default())).collect();
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
                return Err((e, fresh));
            }
            // A thread is stuck in the engine or the kernel: leave everything
            // as it is and report (nothing may be deleted under it).
            return Err((e, false));
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
        });
        engine.set_on_resize(Arc::new(ResizeBridge(Arc::clone(&shared))));
        gate.decide(true);

        let pid = i32::try_from(std::process::id()).unwrap_or(0);
        let started = if recovering {
            ctl.end_user_recovery(info.dev_id, pid)
        } else {
            ctl.start_dev(info.dev_id, pid)
        };
        // Queues have fetched: whatever happens now, the device is not
        // deleted from here.
        started.map_err(|e| (e, false))?;

        let device = Self {
            shared,
            engine,
            libublk_dev: Mutex::new(Some(dev)),
            threads: Mutex::new(handles),
            ended,
            counters,
        };
        // A grow that was durable in the engine but not yet in the kernel when
        // the old daemon died: catch the device up.
        let engine_bytes = device.engine.size_bytes();
        if engine_bytes > device.shared.size.current() {
            let _g = lock(&device.shared.resize_lock);
            session::update_size(&*ctl, info.dev_id, &device.shared.size, engine_bytes)
                .map_err(|e| (e, false))?;
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

    /// The identity record for `state/device`, with the size the kernel has
    /// now. Persist it after the attach and after every grow.
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
            Lifecycle::Stopping => Err(UblkError::Config("the device is stopping")),
            Lifecycle::Deleted => Err(UblkError::Config("the device was deleted")),
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

    /// `QUIESCE_DEV`, then the engine's final commit; the caller may then
    /// exit through [`Quiesced::exit`]. No stop command is ever sent.
    ///
    /// The driver waits up to `timeout` for in-flight requests; queue threads
    /// keep completing them meanwhile. On success no request is delivered any
    /// more and none is in flight (checked; a violation aborts).
    ///
    /// **A timeout is not harmless**: the driver marks the device canceling
    /// before it waits and does not undo that, so after
    /// [`UblkError::QuiesceTimeout`] the device refuses new I/O and its queues
    /// wind down. Retry, or call [`UblkDevice::crash_for_recovery`].
    ///
    /// # Errors
    /// [`UblkError::QuiesceTimeout`] or the command's error.
    pub fn quiesce(&self, timeout: Duration) -> Result<Quiesced> {
        self.check_serving()?;
        session::quiesce(&*self.shared.ctl, self.shared.dev_id, timeout)?;
        // The kernel saw every request committed, and our counters run ahead
        // of the kernel by construction; anything else is a bug (W2).
        let left = self.in_flight();
        if left != 0 {
            fatal_device(&format!(
                "{left} request(s) in flight after a successful QUIESCE_DEV"
            ));
        }
        let engine_error = self.engine.quiesce().err();
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

    /// `TRY_STOP_DEV` alone: [`UblkError::Busy`] while the device is mounted
    /// or open (the device stays and keeps serving). After success the queue
    /// threads end by themselves; [`UblkDevice::detach`] drains them.
    ///
    /// # Errors
    /// [`UblkError::Busy`], [`UblkError::Unsupported`] without
    /// `SAFE_STOP_DEV`, or the command's error.
    pub fn try_stop(&self) -> Result<()> {
        self.check_serving()?;
        if self.shared.info.flags & flag::SAFE_STOP_DEV == 0 {
            return Err(UblkError::Unsupported(
                "the device has no SAFE_STOP_DEV (kernel 7.0 or newer); detach would need a stop command, which is never sent",
            ));
        }
        session::try_stop(&*self.shared.ctl, self.shared.dev_id)?;
        *lock(&self.shared.life) = Lifecycle::Stopping;
        Ok(())
    }

    /// Detach: [`UblkDevice::try_stop`], wait for the queue threads to end,
    /// the engine's final commit, then `DEL_DEV` under its 60 s watchdog.
    ///
    /// Everything after a successful `TRY_STOP_DEV` is bounded by a wait that
    /// reports instead of killing. If the engine's final commit fails the
    /// device is still deleted and the error is returned.
    ///
    /// # Errors
    /// [`UblkError::Busy`] (nothing changed), [`UblkError::DrainTimeout`],
    /// the engine's final-commit error, or the `DEL_DEV` error.
    pub fn detach(&self) -> Result<()> {
        if *lock(&self.shared.life) == Lifecycle::Serving {
            self.try_stop()?;
        } else if *lock(&self.shared.life) == Lifecycle::Deleted {
            return Err(UblkError::Config("the device was deleted"));
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
        let engine_result = self.engine.quiesce();
        // Close /dev/ublkcN: DEL_DEV waits for the last reference.
        drop(lock(&self.libublk_dev).take());
        self.shared.ctl.del_dev(self.shared.dev_id)?;
        *lock(&self.shared.life) = Lifecycle::Deleted;
        engine_result.map_err(UblkError::from)
    }
}
