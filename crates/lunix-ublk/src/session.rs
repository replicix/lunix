//! The sequences of control commands, written against [`ControlPlane`] so that
//! the order, the retries and the refusals are testable without a kernel.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crate::config::{build_params, DataPath, RecoveryPolicy, UblkConfig, BLOCK_BYTES};
use crate::ctl::{ControlPlane, DevInfo, STATE_DEAD, STATE_QUIESCED};
use crate::error::{Errno, Result, SizeError, UblkError};
use crate::features::Features;
use crate::identity::{check_identity, IdentityMismatch, KernelView};
use lunix_format::local::DeviceRecord;

const EBUSY: i32 = 16;
const EINTR: i32 = 4;
const ENOENT: i32 = 2;
const ENODEV: i32 = 19;

/// What a size request amounts to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SizeChange {
    /// The requested size is the current one: nothing to do.
    Same,
    /// A real grow.
    Grow,
}

/// Validates sizes before they reach the kernel. The driver accepts unaligned
/// sizes and shrinks (ADR-20), so this is the only check there is.
#[derive(Debug)]
pub struct SizeGuard {
    max: u64,
    current: AtomicU64,
}

impl SizeGuard {
    /// A guard for a device of `current` bytes that may grow to `max`.
    pub fn new(current: u64, max: u64) -> Self {
        Self {
            max,
            current: AtomicU64::new(current),
        }
    }

    /// The size the kernel was last told, in bytes.
    pub fn current(&self) -> u64 {
        self.current.load(Ordering::Acquire)
    }

    /// Records a size the kernel now has.
    pub fn set(&self, bytes: u64) {
        self.current.store(bytes, Ordering::Release);
    }

    /// Accepts a multiple of 4096 that is positive, not below the current size
    /// and not above the maximum. The current size itself is always accepted.
    ///
    /// # Errors
    /// [`UblkError::InvalidSize`] with the rule that failed.
    pub fn validate(&self, new_bytes: u64) -> Result<SizeChange> {
        let bad = |why| UblkError::InvalidSize {
            requested: new_bytes,
            why,
        };
        if new_bytes == 0 {
            return Err(bad(SizeError::Zero));
        }
        if !new_bytes.is_multiple_of(BLOCK_BYTES) {
            return Err(bad(SizeError::Unaligned));
        }
        let current = self.current();
        if new_bytes < current {
            return Err(bad(SizeError::Shrink { current }));
        }
        if new_bytes == current {
            return Ok(SizeChange::Same);
        }
        if new_bytes > self.max {
            return Err(bad(SizeError::AboveMax { max: self.max }));
        }
        Ok(SizeChange::Grow)
    }
}

/// `GET_FEATURES`, refusing a driver without the mandatory flags.
///
/// # Errors
/// [`UblkError::MissingFeatures`] or the command's error.
pub fn probe(ctl: &dyn ControlPlane) -> Result<Features> {
    let f = ctl.features()?;
    f.require_mandatory()?;
    Ok(f)
}

/// `ADD_DEV`, then `SET_PARAMS`, for a device of `size_bytes`. The device is
/// not started. If `SET_PARAMS` fails the new (never started) device is
/// deleted again.
///
/// # Errors
/// A flag or configuration problem, or the failing command.
pub fn add_device(
    ctl: &dyn ControlPlane,
    features: Features,
    cfg: &UblkConfig,
    size_bytes: u64,
) -> Result<DevInfo> {
    let flags = features.add_flags(
        cfg.data_path == DataPath::UserCopy,
        cfg.recovery == RecoveryPolicy::Reissue,
    )?;
    let params = build_params(cfg, size_bytes)?;
    let want = DevInfo {
        nr_hw_queues: cfg.queues,
        queue_depth: cfg.depth,
        max_io_buf_bytes: cfg.max_io_buf_bytes,
        dev_id: cfg.dev_id.unwrap_or(u32::MAX),
        ublksrv_pid: i32::try_from(std::process::id()).unwrap_or(0),
        flags,
        ..Default::default()
    };
    let added = ctl.add_dev(want)?;
    if let Err(e) = ctl.set_params(added.dev_id, params) {
        // Never started, no queue attached: deleting it cannot block.
        let _ = ctl.del_dev(added.dev_id);
        return Err(e);
    }
    ctl.dev_info(added.dev_id)
}

/// What the kernel reports about `dev_id`.
///
/// # Errors
/// [`UblkError::Identity`] with `NotFound` when the device does not exist,
/// otherwise the command's error.
pub fn read_view(ctl: &dyn ControlPlane, dev_id: u32) -> Result<KernelView> {
    let info = match ctl.dev_info(dev_id) {
        Ok(i) => i,
        Err(UblkError::Ctl { errno, .. }) if errno.0 == ENOENT || errno.0 == ENODEV => {
            return Err(UblkError::Identity {
                dev_id,
                why: IdentityMismatch::NotFound,
            });
        }
        Err(e) => return Err(e),
    };
    let params = ctl.params(dev_id)?;
    Ok(KernelView {
        info,
        dev_sectors: params.basic.dev_sectors,
    })
}

/// Everything up to the point where queues attach: identity check, then
/// `START_USER_RECOVERY` (retried on `EBUSY` every `recovery_retry` up to
/// `recovery_wait`), then a re-fetch. **No `SET_PARAMS`**: the driver retains
/// the parameters. A failed identity check returns before any command that
/// changes the device.
///
/// # Errors
/// [`UblkError::Identity`] when the device is not the recorded one,
/// [`UblkError::Busy`] when the driver still reports `EBUSY` after the wait,
/// otherwise the failing command.
pub fn begin_recovery(
    ctl: &dyn ControlPlane,
    record: &DeviceRecord,
    cfg: &UblkConfig,
    boot_id_now: [u8; 16],
    engine_bytes: u64,
) -> Result<KernelView> {
    let dev_id = record.dev_id;
    let refuse = |why| UblkError::Identity { dev_id, why };
    // The boot comes first: after a reboot the id may belong to anyone.
    if record.boot_id != boot_id_now {
        return Err(refuse(IdentityMismatch::BootId));
    }
    let view = read_view(ctl, dev_id)?;
    check_identity(record, &cfg.volume, &view, boot_id_now, engine_bytes).map_err(refuse)?;

    let t = cfg.timeouts;
    let started = Instant::now();
    loop {
        match ctl.start_user_recovery(dev_id) {
            Ok(()) => break,
            Err(UblkError::Ctl { errno, .. }) if errno.0 == EBUSY => {
                if started.elapsed() >= t.recovery_wait {
                    return Err(UblkError::Busy {
                        what: "START_USER_RECOVERY (a daemon may still serve the device)",
                    });
                }
                std::thread::sleep(t.recovery_retry);
            }
            Err(e) => return Err(e),
        }
    }
    let after = read_view(ctl, dev_id)?;
    if after.info.state != STATE_QUIESCED {
        return Err(refuse(IdentityMismatch::State {
            state: after.info.state,
        }));
    }
    if after.dev_sectors != view.dev_sectors {
        return Err(refuse(IdentityMismatch::DevSectors {
            recorded: view.dev_sectors,
            actual: after.dev_sectors,
        }));
    }
    Ok(after)
}

/// `UPDATE_SIZE` to `new_bytes`, after the checks the driver does not make:
/// the device must have a disk (state not `DEAD`: older drivers dereference a
/// null disk), and the size must be valid for `guard`. Updates the guard.
///
/// # Errors
/// A size error before anything is sent, `ENODEV` for a device without a
/// disk, or the command's error.
pub fn update_size(
    ctl: &dyn ControlPlane,
    dev_id: u32,
    guard: &SizeGuard,
    new_bytes: u64,
) -> Result<()> {
    if guard.validate(new_bytes)? == SizeChange::Same {
        return Ok(());
    }
    let info = ctl.dev_info(dev_id)?;
    if info.state == STATE_DEAD {
        return Err(UblkError::Ctl {
            op: crate::ctl::CtlOp::UpdateSize,
            errno: Errno(ENODEV),
        });
    }
    ctl.update_size(dev_id, new_bytes >> 9)?;
    guard.set(new_bytes);
    Ok(())
}

/// `TRY_STOP_DEV`: `EBUSY` becomes [`UblkError::Busy`] and the device stays.
///
/// # Errors
/// [`UblkError::Busy`], or the command's error.
pub fn try_stop(ctl: &dyn ControlPlane, dev_id: u32) -> Result<()> {
    match ctl.try_stop_dev(dev_id) {
        Err(UblkError::Ctl { errno, .. }) if errno.0 == EBUSY => Err(UblkError::Busy {
            what: "the device (a file system is mounted or the device is open)",
        }),
        other => other,
    }
}

/// `QUIESCE_DEV` with the driver waiting up to `timeout` for in-flight
/// requests. `EBUSY` (not idle in time) and `EINTR` become
/// [`UblkError::QuiesceTimeout`].
///
/// # Errors
/// [`UblkError::QuiesceTimeout`], or the command's error.
pub fn quiesce(ctl: &dyn ControlPlane, dev_id: u32, timeout: Duration) -> Result<()> {
    let ms = u32::try_from(timeout.as_millis().max(1)).unwrap_or(u32::MAX);
    match ctl.quiesce_dev(dev_id, ms) {
        Err(UblkError::Ctl { errno, .. }) if errno.0 == EBUSY || errno.0 == EINTR => {
            Err(UblkError::QuiesceTimeout)
        }
        other => other,
    }
}

#[cfg(test)]
pub(crate) mod fake {
    //! A control plane that is only a state machine, for unit tests.

    use std::collections::HashMap;
    use std::sync::Mutex;

    use super::*;
    use crate::config::build_params;
    use crate::ctl::{CtlOp, Params, STATE_LIVE};

    pub(crate) struct State {
        pub features: u64,
        pub dev: Option<DevInfo>,
        pub params: Params,
        pub log: Vec<&'static str>,
        /// `START_USER_RECOVERY` answers `EBUSY` this many more times; the
        /// device then flips from `LIVE` to `QUIESCED`.
        pub ebusy_left: u32,
        pub fail: HashMap<CtlOp, i32>,
        pub try_stop_busy: bool,
    }

    pub(crate) struct Fake(pub Mutex<State>);

    impl Fake {
        pub(crate) fn new(features: u64) -> Self {
            Self(Mutex::new(State {
                features,
                dev: None,
                params: Params::default(),
                log: Vec::new(),
                ebusy_left: 0,
                fail: HashMap::new(),
                try_stop_busy: false,
            }))
        }

        pub(crate) fn with_device(features: u64, info: DevInfo, params: Params) -> Self {
            let f = Self::new(features);
            {
                let mut s = f.0.lock().unwrap();
                s.dev = Some(info);
                s.params = params;
            }
            f
        }

        pub(crate) fn log(&self) -> Vec<&'static str> {
            self.0.lock().unwrap().log.clone()
        }

        pub(crate) fn fail(&self, op: CtlOp, errno: i32) {
            self.0.lock().unwrap().fail.insert(op, errno);
        }

        fn step(&self, op: CtlOp) -> Result<std::sync::MutexGuard<'_, State>> {
            let mut s = self.0.lock().unwrap();
            s.log.push(op.name());
            if let Some(errno) = s.fail.get(&op).copied() {
                return Err(UblkError::Ctl {
                    op,
                    errno: Errno(errno),
                });
            }
            Ok(s)
        }

        fn ctl_err(op: CtlOp, errno: i32) -> UblkError {
            UblkError::Ctl {
                op,
                errno: Errno(errno),
            }
        }
    }

    impl ControlPlane for Fake {
        fn features(&self) -> Result<Features> {
            Ok(Features(self.step(CtlOp::GetFeatures)?.features))
        }

        fn add_dev(&self, mut info: DevInfo) -> Result<DevInfo> {
            let mut s = self.step(CtlOp::AddDev)?;
            if info.dev_id == u32::MAX {
                info.dev_id = 7;
            }
            info.state = STATE_DEAD;
            info.flags |= crate::features::flag::URING_CMD_COMP_IN_TASK;
            s.dev = Some(info);
            Ok(info)
        }

        fn dev_info(&self, dev_id: u32) -> Result<DevInfo> {
            let s = self.step(CtlOp::GetDevInfo)?;
            match s.dev {
                Some(d) if d.dev_id == dev_id => Ok(d),
                _ => Err(Self::ctl_err(CtlOp::GetDevInfo, ENOENT)),
            }
        }

        fn params(&self, dev_id: u32) -> Result<Params> {
            let s = self.step(CtlOp::GetParams)?;
            match s.dev {
                Some(d) if d.dev_id == dev_id => Ok(s.params),
                _ => Err(Self::ctl_err(CtlOp::GetParams, ENOENT)),
            }
        }

        fn set_params(&self, _dev_id: u32, params: Params) -> Result<()> {
            let mut s = self.step(CtlOp::SetParams)?;
            s.params = params;
            Ok(())
        }

        fn start_dev(&self, _dev_id: u32, _pid: i32) -> Result<()> {
            let mut s = self.step(CtlOp::StartDev)?;
            if let Some(d) = s.dev.as_mut() {
                d.state = STATE_LIVE;
            }
            Ok(())
        }

        fn start_user_recovery(&self, _dev_id: u32) -> Result<()> {
            let mut s = self.step(CtlOp::StartUserRecovery)?;
            if s.ebusy_left > 0 {
                s.ebusy_left -= 1;
                if s.ebusy_left == 0 {
                    if let Some(d) = s.dev.as_mut() {
                        d.state = STATE_QUIESCED;
                    }
                }
                return Err(Self::ctl_err(CtlOp::StartUserRecovery, EBUSY));
            }
            match s.dev.as_ref().map(|d| d.state) {
                Some(STATE_QUIESCED) => Ok(()),
                _ => Err(Self::ctl_err(CtlOp::StartUserRecovery, EBUSY)),
            }
        }

        fn end_user_recovery(&self, _dev_id: u32, _pid: i32) -> Result<()> {
            let mut s = self.step(CtlOp::EndUserRecovery)?;
            if let Some(d) = s.dev.as_mut() {
                d.state = STATE_LIVE;
            }
            Ok(())
        }

        fn quiesce_dev(&self, _dev_id: u32, _timeout_ms: u32) -> Result<()> {
            let mut s = self.step(CtlOp::QuiesceDev)?;
            if let Some(d) = s.dev.as_mut() {
                d.state = STATE_QUIESCED;
            }
            Ok(())
        }

        fn try_stop_dev(&self, _dev_id: u32) -> Result<()> {
            let mut s = self.step(CtlOp::TryStopDev)?;
            if s.try_stop_busy {
                return Err(Self::ctl_err(CtlOp::TryStopDev, EBUSY));
            }
            if let Some(d) = s.dev.as_mut() {
                d.state = STATE_DEAD;
            }
            Ok(())
        }

        fn del_dev(&self, _dev_id: u32) -> Result<()> {
            let mut s = self.step(CtlOp::DelDev)?;
            s.dev = None;
            Ok(())
        }

        fn update_size(&self, _dev_id: u32, sectors: u64) -> Result<()> {
            let mut s = self.step(CtlOp::UpdateSize)?;
            s.params.basic.dev_sectors = sectors;
            Ok(())
        }
    }

    /// A quiesced device recorded for `volume`, as the daemon would have left it.
    pub(crate) fn quiesced(cfg: &UblkConfig, size: u64, features: u64) -> (Fake, DeviceRecord) {
        let info = DevInfo {
            nr_hw_queues: cfg.queues,
            queue_depth: cfg.depth,
            max_io_buf_bytes: cfg.max_io_buf_bytes,
            dev_id: 5,
            state: STATE_QUIESCED,
            flags: 0x6745a,
            ..Default::default()
        };
        let params = build_params(cfg, size).unwrap();
        let rec = crate::identity::make_record(&info, cfg.volume, BOOT, params.basic.dev_sectors);
        (Fake::with_device(features, info, params), rec)
    }

    pub(crate) const BOOT: [u8; 16] = [3; 16];
}

#[cfg(test)]
mod tests {
    use super::fake::*;
    use super::*;
    use crate::config::UblkConfig;
    use crate::ctl::{CtlOp, STATE_LIVE};
    use lunix_format::VolumeUuid;

    const HOST: u64 = 0x7ffff;
    const GIB16: u64 = 16 << 30;

    fn cfg() -> UblkConfig {
        let mut c = UblkConfig::new(VolumeUuid([4; 16]));
        c.timeouts.recovery_retry = Duration::from_millis(1);
        c.timeouts.recovery_wait = Duration::from_millis(60);
        c
    }

    const MUTATING: [&str; 7] = [
        "ADD_DEV",
        "SET_PARAMS",
        "START_DEV",
        "START_USER_RECOVERY",
        "END_USER_RECOVERY",
        "DEL_DEV",
        "UPDATE_SIZE",
    ];

    fn touched(log: &[&str]) -> Vec<&'static str> {
        MUTATING.into_iter().filter(|m| log.contains(m)).collect()
    }

    #[test]
    fn adding_a_device_is_add_then_set_params_then_a_read_back() {
        let fake = Fake::new(HOST);
        let features = probe(&fake).unwrap();
        let info = add_device(&fake, features, &cfg(), GIB16).unwrap();
        assert_eq!(
            fake.log(),
            ["GET_FEATURES", "ADD_DEV", "SET_PARAMS", "GET_DEV_INFO"]
        );
        let flags = info.flags;
        // Mandatory flags, SAFE_STOP_DEV and NO_AUTO_PART_SCAN, no USER_COPY.
        assert_eq!(flags & 0x6745a, 0x6745a & flags);
        assert_ne!(flags & crate::features::flag::SAFE_STOP_DEV, 0);
        assert_eq!(flags & crate::features::flag::USER_COPY, 0);
        let s = fake.0.lock().unwrap();
        assert_eq!(s.params.basic.dev_sectors, GIB16 >> 9);
    }

    #[test]
    fn a_driver_without_a_mandatory_flag_is_refused_before_any_add() {
        let fake = Fake::new(HOST & !crate::features::flag::QUIESCE);
        let err = probe(&fake).unwrap_err();
        assert!(matches!(err, UblkError::MissingFeatures(_)), "{err}");
        assert_eq!(fake.log(), ["GET_FEATURES"]);
    }

    #[test]
    fn a_failed_set_params_deletes_the_never_started_device() {
        let fake = Fake::new(HOST);
        fake.fail(CtlOp::SetParams, 22);
        let features = probe(&fake).unwrap();
        let err = add_device(&fake, features, &cfg(), GIB16).unwrap_err();
        assert_eq!(err.errno(), Some(22));
        assert_eq!(
            fake.log(),
            ["GET_FEATURES", "ADD_DEV", "SET_PARAMS", "DEL_DEV"]
        );
    }

    #[test]
    fn recovery_sends_no_set_params_and_re_fetches() {
        let (fake, rec) = quiesced(&cfg(), GIB16, HOST);
        let view = begin_recovery(&fake, &rec, &cfg(), BOOT, GIB16).unwrap();
        assert_eq!(view.info.state, STATE_QUIESCED);
        assert_eq!(
            fake.log(),
            [
                "GET_DEV_INFO",
                "GET_PARAMS",
                "START_USER_RECOVERY",
                "GET_DEV_INFO",
                "GET_PARAMS"
            ]
        );
        assert!(!fake.log().contains(&"SET_PARAMS"));
    }

    #[test]
    fn start_user_recovery_is_retried_on_ebusy_until_the_kernel_quiesced() {
        let (fake, rec) = quiesced(&cfg(), GIB16, HOST);
        {
            // A kill -9 leaves the device LIVE for a moment.
            let mut s = fake.0.lock().unwrap();
            s.dev.as_mut().unwrap().state = STATE_LIVE;
            s.ebusy_left = 3;
        }
        begin_recovery(&fake, &rec, &cfg(), BOOT, GIB16).unwrap();
        let starts = fake
            .log()
            .iter()
            .filter(|o| **o == "START_USER_RECOVERY")
            .count();
        assert_eq!(starts, 4);
    }

    #[test]
    fn a_device_that_stays_busy_is_reported_not_forced() {
        let (fake, rec) = quiesced(&cfg(), GIB16, HOST);
        fake.0.lock().unwrap().dev.as_mut().unwrap().state = STATE_LIVE;
        let err = begin_recovery(&fake, &rec, &cfg(), BOOT, GIB16).unwrap_err();
        assert!(matches!(err, UblkError::Busy { .. }), "{err}");
        assert!(!fake.log().contains(&"DEL_DEV"));
        assert!(!fake.log().contains(&"SET_PARAMS"));
    }

    #[test]
    fn a_stale_boot_id_is_refused_without_touching_the_kernel() {
        let (fake, rec) = quiesced(&cfg(), GIB16, HOST);
        let err = begin_recovery(&fake, &rec, &cfg(), [9; 16], GIB16).unwrap_err();
        assert!(
            matches!(
                err,
                UblkError::Identity {
                    why: IdentityMismatch::BootId,
                    ..
                }
            ),
            "{err}"
        );
        assert!(fake.log().is_empty(), "{:?}", fake.log());
    }

    #[test]
    fn a_wrong_queue_count_or_size_is_refused_and_the_device_is_untouched() {
        let c = cfg();
        // The record says 8 queues; the device has 4.
        let (fake, mut rec) = quiesced(&c, GIB16, HOST);
        rec.nr_queues = 8;
        let err = begin_recovery(&fake, &rec, &c, BOOT, GIB16).unwrap_err();
        assert!(
            matches!(
                err,
                UblkError::Identity {
                    why: IdentityMismatch::QueueCount { .. },
                    ..
                }
            ),
            "{err}"
        );
        assert!(touched(&fake.log()).is_empty(), "{:?}", fake.log());

        // The record says 32 GiB; the device is 16 GiB.
        let (fake, mut rec) = quiesced(&c, GIB16, HOST);
        rec.dev_sectors = (32u64 << 30) >> 9;
        let err = begin_recovery(&fake, &rec, &c, BOOT, 32 << 30).unwrap_err();
        assert!(
            matches!(
                err,
                UblkError::Identity {
                    why: IdentityMismatch::DevSectors { .. },
                    ..
                }
            ),
            "{err}"
        );
        assert!(touched(&fake.log()).is_empty(), "{:?}", fake.log());

        // The device is gone.
        let (fake, mut rec) = quiesced(&c, GIB16, HOST);
        rec.dev_id = 99;
        let err = begin_recovery(&fake, &rec, &c, BOOT, GIB16).unwrap_err();
        assert!(
            matches!(
                err,
                UblkError::Identity {
                    why: IdentityMismatch::NotFound,
                    ..
                }
            ),
            "{err}"
        );
        assert!(touched(&fake.log()).is_empty());

        // Another volume.
        let (fake, rec) = quiesced(&c, GIB16, HOST);
        let mut other = c.clone();
        other.volume = VolumeUuid([5; 16]);
        let err = begin_recovery(&fake, &rec, &other, BOOT, GIB16).unwrap_err();
        assert!(
            matches!(
                err,
                UblkError::Identity {
                    why: IdentityMismatch::Volume,
                    ..
                }
            ),
            "{err}"
        );
        assert!(touched(&fake.log()).is_empty());
    }

    #[test]
    fn a_device_in_fail_io_state_is_not_recovered() {
        let c = cfg();
        let (fake, rec) = quiesced(&c, GIB16, HOST);
        fake.0.lock().unwrap().dev.as_mut().unwrap().state = crate::ctl::STATE_FAIL_IO;
        let err = begin_recovery(&fake, &rec, &c, BOOT, GIB16).unwrap_err();
        assert!(
            matches!(
                err,
                UblkError::Identity {
                    why: IdentityMismatch::State { state: 3 },
                    ..
                }
            ),
            "{err}"
        );
        assert!(touched(&fake.log()).is_empty());
    }

    #[test]
    fn size_rules() {
        let g = SizeGuard::new(16 << 30, 64 << 30);
        assert_eq!(g.validate(16 << 30).unwrap(), SizeChange::Same);
        assert_eq!(g.validate(64 << 30).unwrap(), SizeChange::Grow);
        assert_eq!(g.validate((16 << 30) + 4096).unwrap(), SizeChange::Grow);
        let why = |n: u64| match g.validate(n).unwrap_err() {
            UblkError::InvalidSize { why, .. } => why,
            e => panic!("unexpected {e}"),
        };
        assert_eq!(why(0), SizeError::Zero);
        // 64 GiB + 12 KiB + 512 B: the driver accepts it, the daemon does not.
        assert_eq!(why((64 << 30) + 12288 + 512), SizeError::Unaligned);
        assert_eq!(why(1), SizeError::Unaligned);
        assert_eq!(why(8 << 30), SizeError::Shrink { current: 16 << 30 });
        assert_eq!(
            why((16 << 30) - 4096),
            SizeError::Shrink { current: 16 << 30 }
        );
        assert_eq!(
            why((64 << 30) + 4096),
            SizeError::AboveMax { max: 64 << 30 }
        );
        // An unaligned size is refused even when it is also too large.
        assert_eq!(why((128 << 30) + 1), SizeError::Unaligned);
    }

    #[test]
    fn a_refused_size_never_reaches_the_kernel() {
        let (fake, _) = quiesced(&cfg(), GIB16, HOST);
        fake.0.lock().unwrap().dev.as_mut().unwrap().state = STATE_LIVE;
        let g = SizeGuard::new(GIB16, 64 << 30);
        for bad in [0, 1, 8 << 30, (64 << 30) + 4096, GIB16 + 100] {
            assert!(update_size(&fake, 5, &g, bad).is_err(), "{bad}");
        }
        assert!(fake.log().is_empty(), "{:?}", fake.log());
        assert_eq!(g.current(), GIB16);
    }

    #[test]
    fn a_grow_checks_the_disk_exists_then_updates_the_size() {
        let (fake, _) = quiesced(&cfg(), GIB16, HOST);
        fake.0.lock().unwrap().dev.as_mut().unwrap().state = STATE_LIVE;
        let g = SizeGuard::new(GIB16, 64 << 30);
        update_size(&fake, 5, &g, 64 << 30).unwrap();
        assert_eq!(fake.log(), ["GET_DEV_INFO", "UPDATE_SIZE"]);
        assert_eq!(g.current(), 64 << 30);
        assert_eq!(
            fake.0.lock().unwrap().params.basic.dev_sectors,
            (64u64 << 30) >> 9
        );
        // The same size again is a no-op and sends nothing.
        update_size(&fake, 5, &g, 64 << 30).unwrap();
        assert_eq!(fake.log().len(), 2);
    }

    #[test]
    fn update_size_on_a_device_without_a_disk_is_refused_locally() {
        let (fake, _) = quiesced(&cfg(), GIB16, HOST);
        fake.0.lock().unwrap().dev.as_mut().unwrap().state = STATE_DEAD;
        let g = SizeGuard::new(GIB16, 64 << 30);
        let err = update_size(&fake, 5, &g, 32 << 30).unwrap_err();
        assert_eq!(err.errno(), Some(ENODEV));
        assert!(!fake.log().contains(&"UPDATE_SIZE"));
        assert_eq!(g.current(), GIB16);
    }

    #[test]
    fn try_stop_while_mounted_is_busy_and_the_device_stays() {
        let (fake, _) = quiesced(&cfg(), GIB16, HOST);
        fake.0.lock().unwrap().try_stop_busy = true;
        let err = try_stop(&fake, 5).unwrap_err();
        assert!(matches!(err, UblkError::Busy { .. }), "{err}");
        assert_eq!(fake.log(), ["TRY_STOP_DEV"]);
        // After the unmount it succeeds.
        fake.0.lock().unwrap().try_stop_busy = false;
        try_stop(&fake, 5).unwrap();
    }

    #[test]
    fn a_quiesce_that_times_out_is_a_typed_error() {
        let (fake, _) = quiesced(&cfg(), GIB16, HOST);
        fake.fail(CtlOp::QuiesceDev, EBUSY);
        let err = quiesce(&fake, 5, Duration::from_secs(1)).unwrap_err();
        assert!(matches!(err, UblkError::QuiesceTimeout), "{err}");
        fake.fail(CtlOp::QuiesceDev, 22);
        let err = quiesce(&fake, 5, Duration::from_secs(1)).unwrap_err();
        assert_eq!(err.errno(), Some(22));
    }

    #[test]
    fn user_copy_is_requested_only_when_configured() {
        let fake = Fake::new(HOST);
        let features = probe(&fake).unwrap();
        let mut c = cfg();
        c.data_path = DataPath::UserCopy;
        let info = add_device(&fake, features, &c, GIB16).unwrap();
        assert_ne!(info.flags & crate::features::flag::USER_COPY, 0);
    }

    #[test]
    fn the_negative_control_omits_reissue_only_when_asked() {
        let fake = Fake::new(HOST);
        let features = probe(&fake).unwrap();
        let mut c = cfg();
        c.recovery = RecoveryPolicy::NoReissueNegativeControl;
        let info = add_device(&fake, features, &c, GIB16).unwrap();
        assert_eq!(info.flags & crate::features::flag::USER_RECOVERY_REISSUE, 0);
        assert_ne!(info.flags & crate::features::flag::USER_RECOVERY, 0);
        let fake = Fake::new(HOST);
        let info = add_device(&fake, features, &cfg(), GIB16).unwrap();
        assert_ne!(info.flags & crate::features::flag::USER_RECOVERY_REISSUE, 0);
    }
}
