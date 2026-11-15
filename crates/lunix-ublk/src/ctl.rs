//! The raw control ring (phase 0 P0.3): `io_uring` `URING_CMD` commands on
//! `/dev/ublk-control`.
//!
//! libublk 0.4.8 rejects control flags outside its own list
//! (`NO_AUTO_PART_SCAN`, `SAFE_STOP_DEV`, `FAIL_IO`, `PER_IO_DAEMON`) and has
//! no `TRY_STOP_DEV`, so every control command of the layer goes through this
//! module. `CMD_IOCTL_ENCODE` opcodes are always used.
//!
//! # No `STOP_DEV`
//!
//! [`CtlOp`] has no variant for `STOP_DEV`. After `QUIESCE_DEV` a `STOP_DEV`
//! deadlocked the kernel thread in `ublk_wait_tagset_rqs_idle` and wedged a
//! device twice (ADR-16, W1), and a stop with a mounted file system yanks the
//! device from under it. The only stopping command is `TRY_STOP_DEV`, which
//! refuses while the device has openers.
//!
//! # Watchdogs (W4)
//!
//! Each command of [`RingControl`] runs through
//! [`run_with_timeout`](crate::watchdog::run_with_timeout) with its own
//! `io_uring` and its own descriptor on the control device, so a command that
//! never returns blocks neither the caller nor the next command. None of this
//! is ever done on a queue thread.

use std::fmt;
use std::fs::OpenOptions;
use std::os::fd::AsRawFd;
use std::sync::Arc;
use std::time::Duration;

use io_uring::{cqueue, opcode, squeue, types, IoUring};

use crate::config::Timeouts;
use crate::error::{Errno, Result, UblkError};
use crate::features::Features;
use crate::watchdog::run_with_timeout;

/// `ublksrv_ctrl_dev_info`: the 64-byte device description.
pub type DevInfo = libublk::sys::ublksrv_ctrl_dev_info;

/// `ublk_params`: what `SET_PARAMS` and `GET_PARAMS` carry.
pub type Params = libublk::sys::ublk_params;

/// The control device.
pub const CONTROL_PATH: &str = "/dev/ublk-control";

/// `UBLK_S_DEV_DEAD`: added, not started (or stopped).
pub const STATE_DEAD: u16 = 0;
/// `UBLK_S_DEV_LIVE`.
pub const STATE_LIVE: u16 = 1;
/// `UBLK_S_DEV_QUIESCED`.
pub const STATE_QUIESCED: u16 = 2;
/// `UBLK_S_DEV_FAIL_IO`: not recoverable.
pub const STATE_FAIL_IO: u16 = 3;

/// `sizeof(struct ublksrv_ctrl_cmd)`, part of every encoded opcode.
const CTRL_CMD_SIZE: u32 = 32;
const DIR_R: u32 = 2;
const DIR_RW: u32 = 3;
/// The original `GET_DEV_INFO` (0x02), for kernels without `GET_DEV_INFO2`.
const LEGACY_GET_DEV_INFO: u32 =
    (DIR_R << 30) | (CTRL_CMD_SIZE << 16) | ((b'u' as u32) << 8) | 0x02;

/// A control command the layer sends. There is no `STOP_DEV` (see the module
/// documentation).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[non_exhaustive]
pub enum CtlOp {
    /// `GET_FEATURES`.
    GetFeatures,
    /// `ADD_DEV`.
    AddDev,
    /// `GET_DEV_INFO2` (falling back to `GET_DEV_INFO`).
    GetDevInfo,
    /// `GET_PARAMS`.
    GetParams,
    /// `SET_PARAMS`.
    SetParams,
    /// `START_DEV`.
    StartDev,
    /// `START_USER_RECOVERY`.
    StartUserRecovery,
    /// `END_USER_RECOVERY`.
    EndUserRecovery,
    /// `QUIESCE_DEV`.
    QuiesceDev,
    /// `TRY_STOP_DEV`.
    TryStopDev,
    /// `DEL_DEV`.
    DelDev,
    /// `UPDATE_SIZE`.
    UpdateSize,
}

impl CtlOp {
    /// Every command of the layer.
    pub const ALL: [CtlOp; 12] = [
        Self::GetFeatures,
        Self::AddDev,
        Self::GetDevInfo,
        Self::GetParams,
        Self::SetParams,
        Self::StartDev,
        Self::StartUserRecovery,
        Self::EndUserRecovery,
        Self::QuiesceDev,
        Self::TryStopDev,
        Self::DelDev,
        Self::UpdateSize,
    ];

    /// The kernel's name of the command.
    pub fn name(self) -> &'static str {
        match self {
            Self::GetFeatures => "GET_FEATURES",
            Self::AddDev => "ADD_DEV",
            Self::GetDevInfo => "GET_DEV_INFO",
            Self::GetParams => "GET_PARAMS",
            Self::SetParams => "SET_PARAMS",
            Self::StartDev => "START_DEV",
            Self::StartUserRecovery => "START_USER_RECOVERY",
            Self::EndUserRecovery => "END_USER_RECOVERY",
            Self::QuiesceDev => "QUIESCE_DEV",
            Self::TryStopDev => "TRY_STOP_DEV",
            Self::DelDev => "DEL_DEV",
            Self::UpdateSize => "UPDATE_SIZE",
        }
    }

    /// The command number (`_IOC_NR`).
    fn nr(self) -> u32 {
        match self {
            Self::GetFeatures => 0x13,
            Self::AddDev => 0x04,
            Self::GetDevInfo => 0x12,
            Self::GetParams => 0x09,
            Self::SetParams => 0x08,
            Self::StartDev => 0x06,
            Self::StartUserRecovery => 0x10,
            Self::EndUserRecovery => 0x11,
            Self::QuiesceDev => 0x16,
            Self::TryStopDev => 0x17,
            Self::DelDev => 0x05,
            Self::UpdateSize => 0x15,
        }
    }

    /// The transfer direction (`_IOC_READ` is 2, `_IOC_WRITE` is 1).
    fn dir(self) -> u32 {
        match self {
            Self::GetFeatures | Self::GetDevInfo | Self::GetParams => DIR_R,
            _ => DIR_RW,
        }
    }

    /// The encoded opcode (`_IOWR('u', nr, struct ublksrv_ctrl_cmd)`).
    pub fn opcode(self) -> u32 {
        (self.dir() << 30) | (CTRL_CMD_SIZE << 16) | (u32::from(b'u') << 8) | self.nr()
    }
}

impl fmt::Display for CtlOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Everything the layer asks of the kernel's control plane. [`RingControl`] is
/// the real one; the unit tests use a fake. All methods block and never run
/// on a queue thread.
pub trait ControlPlane: Send + Sync + 'static {
    /// `GET_FEATURES`.
    fn features(&self) -> Result<Features>;
    /// `ADD_DEV`: returns the device with the id the driver chose and the
    /// flags it set.
    fn add_dev(&self, info: DevInfo) -> Result<DevInfo>;
    /// `GET_DEV_INFO2`.
    fn dev_info(&self, dev_id: u32) -> Result<DevInfo>;
    /// `GET_PARAMS`.
    fn params(&self, dev_id: u32) -> Result<Params>;
    /// `SET_PARAMS`.
    fn set_params(&self, dev_id: u32, params: Params) -> Result<()>;
    /// `START_DEV` for the daemon with this pid.
    fn start_dev(&self, dev_id: u32, pid: i32) -> Result<()>;
    /// `START_USER_RECOVERY`. `EBUSY` is returned as [`UblkError::Ctl`].
    fn start_user_recovery(&self, dev_id: u32) -> Result<()>;
    /// `END_USER_RECOVERY` for the daemon with this pid.
    fn end_user_recovery(&self, dev_id: u32, pid: i32) -> Result<()>;
    /// `QUIESCE_DEV`: the driver waits up to `timeout_ms` for in-flight
    /// requests to complete.
    fn quiesce_dev(&self, dev_id: u32, timeout_ms: u32) -> Result<()>;
    /// `TRY_STOP_DEV`: `EBUSY` while the device has openers.
    fn try_stop_dev(&self, dev_id: u32) -> Result<()>;
    /// `DEL_DEV`.
    fn del_dev(&self, dev_id: u32) -> Result<()>;
    /// `UPDATE_SIZE` to `sectors` 512-byte sectors.
    fn update_size(&self, dev_id: u32, sectors: u64) -> Result<()>;
}

/// A function called on the helper thread before a command is submitted. A
/// hook that never returns is the stuck control call of the watchdog test.
pub type CtlHook = Arc<dyn Fn(CtlOp) + Send + Sync>;

/// The real control plane: one `io_uring` per command.
#[derive(Clone)]
pub struct RingControl {
    timeouts: Timeouts,
    hook: Option<CtlHook>,
}

impl fmt::Debug for RingControl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RingControl")
            .field("timeouts", &self.timeouts)
            .field("hook", &self.hook.is_some())
            .finish()
    }
}

/// Margin added to the driver's own wait of `QUIESCE_DEV` for the watchdog.
const QUIESCE_MARGIN: Duration = Duration::from_secs(10);

impl RingControl {
    /// A control plane with these watchdogs.
    pub fn new(timeouts: Timeouts) -> Self {
        Self {
            timeouts,
            hook: None,
        }
    }

    /// Installs a hook that runs on the helper thread before each command.
    /// Test use: a hook that blocks forever makes the command time out.
    #[must_use]
    pub fn with_hook(mut self, hook: CtlHook) -> Self {
        self.hook = Some(hook);
        self
    }

    fn watchdog(&self, op: CtlOp) -> Duration {
        match op {
            CtlOp::DelDev => self.timeouts.del,
            _ => self.timeouts.ctl,
        }
    }

    /// Runs one command that carries a buffer. The buffer is owned by the
    /// helper thread for the duration, so a timeout leaves it alive.
    fn exec_buf<T: Send + 'static>(
        &self,
        op: CtlOp,
        dev_id: u32,
        data: T,
        data0: u64,
        watchdog: Duration,
    ) -> Result<(i32, T)> {
        self.exec_buf_code(op, op.opcode(), dev_id, data, data0, watchdog)
    }

    /// [`Self::exec_buf`] with an explicit opcode (the legacy `GET_DEV_INFO`).
    fn exec_buf_code<T: Send + 'static>(
        &self,
        op: CtlOp,
        code: u32,
        dev_id: u32,
        data: T,
        data0: u64,
        watchdog: Duration,
    ) -> Result<(i32, T)> {
        let hook = self.hook.clone();
        let out = run_with_timeout(op.name(), watchdog, move || {
            if let Some(h) = &hook {
                h(op);
            }
            let ptr = Box::into_raw(Box::new(data));
            let len = u16::try_from(std::mem::size_of::<T>()).unwrap_or(u16::MAX);
            match raw_cmd(code, dev_id, ptr as u64, len, data0) {
                Ok(r) => {
                    // SAFETY: `ptr` came from `Box::into_raw` above and the
                    // completion was reaped, so the kernel is done with it.
                    let boxed = unsafe { Box::from_raw(ptr) };
                    Ok((r, *boxed))
                }
                // After a submit the command may still be pending in the
                // kernel's io-wq and write into the buffer later: on any
                // error the box is leaked on purpose.
                Err(e) => Err(e),
            }
        })?;
        Ok(out?)
    }

    /// Runs one command that carries no buffer.
    fn exec_plain(&self, op: CtlOp, dev_id: u32, data0: u64, watchdog: Duration) -> Result<i32> {
        let hook = self.hook.clone();
        let out = run_with_timeout(op.name(), watchdog, move || {
            if let Some(h) = &hook {
                h(op);
            }
            raw_cmd(op.opcode(), dev_id, 0, 0, data0)
        })?;
        Ok(out?)
    }

    fn check(op: CtlOp, res: i32) -> Result<i32> {
        if res < 0 {
            Err(UblkError::Ctl {
                op,
                errno: Errno(-res),
            })
        } else {
            Ok(res)
        }
    }
}

/// Submits one `URING_CMD` and waits for its completion. Returns the raw
/// result (negative errno on failure). `EINTR` while waiting re-enters the
/// wait (the command is not submitted again); any other error is returned with
/// the command possibly still pending, so callers must not free its buffer.
fn raw_cmd(code: u32, dev_id: u32, addr: u64, len: u16, data0: u64) -> std::io::Result<i32> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(CONTROL_PATH)?;
    let mut ring = IoUring::<squeue::Entry128, cqueue::Entry>::builder().build(2)?;
    let mut raw = [0u8; 80];
    raw[0..4].copy_from_slice(&dev_id.to_le_bytes());
    raw[4..6].copy_from_slice(&0xffff_u16.to_le_bytes());
    raw[6..8].copy_from_slice(&len.to_le_bytes());
    raw[8..16].copy_from_slice(&addr.to_le_bytes());
    raw[16..24].copy_from_slice(&data0.to_le_bytes());
    let sqe = opcode::UringCmd80::new(types::Fd(file.as_raw_fd()), code)
        .cmd(raw)
        .build()
        .user_data(1);
    // SAFETY: `addr` and `len` describe memory owned by the caller until the
    // completion is reaped; on any error the caller leaks it (see above).
    unsafe { ring.submission().push(&sqe) }
        .map_err(|_| std::io::Error::other("control submission queue full"))?;
    loop {
        match ring.submit_and_wait(1) {
            Ok(_) => break,
            Err(e) if e.raw_os_error() == Some(libc::EINTR) => {}
            Err(e) => return Err(e),
        }
    }
    let cqe = ring
        .completion()
        .next()
        .ok_or_else(|| std::io::Error::other("control command left no completion"))?;
    Ok(cqe.result())
}

impl ControlPlane for RingControl {
    fn features(&self) -> Result<Features> {
        let op = CtlOp::GetFeatures;
        let (r, mask) = self.exec_buf(op, u32::MAX, 0u64, 0, self.watchdog(op))?;
        Self::check(op, r)?;
        Ok(Features(mask))
    }

    fn add_dev(&self, info: DevInfo) -> Result<DevInfo> {
        let op = CtlOp::AddDev;
        let (r, out) = self.exec_buf(op, u32::MAX, info, 0, self.watchdog(op))?;
        Self::check(op, r)?;
        Ok(out)
    }

    fn dev_info(&self, dev_id: u32) -> Result<DevInfo> {
        let op = CtlOp::GetDevInfo;
        let (r, out) = self.exec_buf(op, dev_id, DevInfo::default(), 0, self.watchdog(op))?;
        if r >= 0 {
            return Ok(out);
        }
        // Kernels without GET_DEV_INFO2 know only the original command (0x02).
        let (legacy, out) = self.exec_buf_code(
            op,
            LEGACY_GET_DEV_INFO,
            dev_id,
            DevInfo::default(),
            0,
            self.watchdog(op),
        )?;
        if legacy >= 0 {
            Ok(out)
        } else {
            // Report the error of the command we prefer.
            Self::check(op, r)?;
            Self::check(op, legacy)?;
            Ok(out)
        }
    }

    fn params(&self, dev_id: u32) -> Result<Params> {
        let op = CtlOp::GetParams;
        let p = Params {
            len: u32::try_from(std::mem::size_of::<Params>()).unwrap_or(u32::MAX),
            ..Default::default()
        };
        let (r, out) = self.exec_buf(op, dev_id, p, 0, self.watchdog(op))?;
        Self::check(op, r)?;
        Ok(out)
    }

    fn set_params(&self, dev_id: u32, params: Params) -> Result<()> {
        let op = CtlOp::SetParams;
        let (r, _) = self.exec_buf(op, dev_id, params, 0, self.watchdog(op))?;
        Self::check(op, r).map(|_| ())
    }

    fn start_dev(&self, dev_id: u32, pid: i32) -> Result<()> {
        let op = CtlOp::StartDev;
        // Fault hook (test only, `crashpoints` builds): while the file named
        // by `LUNIX_UBLK_FAULT_START_DEV` exists, `START_DEV` fails with EIO
        // without reaching the kernel, as a refused start would.
        if cfg!(feature = "crashpoints")
            && std::env::var_os("LUNIX_UBLK_FAULT_START_DEV")
                .is_some_and(|p| std::path::Path::new(&p).exists())
        {
            return Self::check(op, -libc::EIO).map(|_| ());
        }
        let r = self.exec_plain(op, dev_id, pid as u64, self.watchdog(op))?;
        Self::check(op, r).map(|_| ())
    }

    fn start_user_recovery(&self, dev_id: u32) -> Result<()> {
        let op = CtlOp::StartUserRecovery;
        let r = self.exec_plain(op, dev_id, 0, self.watchdog(op))?;
        Self::check(op, r).map(|_| ())
    }

    fn end_user_recovery(&self, dev_id: u32, pid: i32) -> Result<()> {
        let op = CtlOp::EndUserRecovery;
        let r = self.exec_plain(op, dev_id, pid as u64, self.watchdog(op))?;
        Self::check(op, r).map(|_| ())
    }

    fn quiesce_dev(&self, dev_id: u32, timeout_ms: u32) -> Result<()> {
        let op = CtlOp::QuiesceDev;
        let wait = Duration::from_millis(u64::from(timeout_ms)) + QUIESCE_MARGIN;
        let r = self.exec_plain(
            op,
            dev_id,
            u64::from(timeout_ms),
            wait.max(self.watchdog(op)),
        )?;
        Self::check(op, r).map(|_| ())
    }

    fn try_stop_dev(&self, dev_id: u32) -> Result<()> {
        let op = CtlOp::TryStopDev;
        let r = self.exec_plain(op, dev_id, 0, self.watchdog(op))?;
        Self::check(op, r).map(|_| ())
    }

    fn del_dev(&self, dev_id: u32) -> Result<()> {
        let op = CtlOp::DelDev;
        let r = self.exec_plain(op, dev_id, 0, self.watchdog(op))?;
        Self::check(op, r).map(|_| ())
    }

    fn update_size(&self, dev_id: u32, sectors: u64) -> Result<()> {
        let op = CtlOp::UpdateSize;
        let r = self.exec_plain(op, dev_id, sectors, self.watchdog(op))?;
        Self::check(op, r).map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use libublk::sys;

    #[test]
    fn opcodes_equal_the_kernel_header() {
        let pairs = [
            (CtlOp::AddDev, sys::UBLK_U_CMD_ADD_DEV),
            (CtlOp::DelDev, sys::UBLK_U_CMD_DEL_DEV),
            (CtlOp::StartDev, sys::UBLK_U_CMD_START_DEV),
            (CtlOp::SetParams, sys::UBLK_U_CMD_SET_PARAMS),
            (CtlOp::GetParams, sys::UBLK_U_CMD_GET_PARAMS),
            (
                CtlOp::StartUserRecovery,
                sys::UBLK_U_CMD_START_USER_RECOVERY,
            ),
            (CtlOp::EndUserRecovery, sys::UBLK_U_CMD_END_USER_RECOVERY),
            (CtlOp::GetDevInfo, sys::UBLK_U_CMD_GET_DEV_INFO2),
            (CtlOp::GetFeatures, sys::UBLK_U_CMD_GET_FEATURES),
            (CtlOp::UpdateSize, sys::UBLK_U_CMD_UPDATE_SIZE),
            (CtlOp::QuiesceDev, sys::UBLK_U_CMD_QUIESCE_DEV),
            (CtlOp::TryStopDev, sys::UBLK_U_CMD_TRY_STOP_DEV),
        ];
        assert_eq!(pairs.len(), CtlOp::ALL.len());
        for (op, header) in pairs {
            assert_eq!(op.opcode(), header, "{op}");
        }
    }

    #[test]
    fn the_legacy_get_dev_info_code_is_the_header_one() {
        assert_eq!(LEGACY_GET_DEV_INFO, sys::UBLK_U_CMD_GET_DEV_INFO);
    }

    #[test]
    fn no_command_encodes_stop_dev() {
        for op in CtlOp::ALL {
            assert_ne!(op.opcode(), sys::UBLK_U_CMD_STOP_DEV, "{op}");
            assert_ne!(op.nr(), 0x07, "{op}");
            assert!(!op.name().starts_with("STOP"), "{op}");
        }
    }

    #[test]
    fn every_command_is_distinct() {
        let mut codes: Vec<u32> = CtlOp::ALL.iter().map(|o| o.opcode()).collect();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), CtlOp::ALL.len());
    }

    #[test]
    fn the_buffers_have_the_kernel_sizes() {
        assert_eq!(std::mem::size_of::<DevInfo>(), 64);
        assert_eq!(std::mem::size_of::<u64>(), 8);
    }

    /// No source line of this crate may build a plain `STOP_DEV` command: every
    /// mention of the name is `TRY_STOP_DEV` or `SAFE_STOP_DEV`, or inside a comment or a string
    /// that documents the ban.
    #[test]
    fn no_source_line_issues_a_plain_stop_dev() {
        fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            for e in std::fs::read_dir(dir).unwrap() {
                let p = e.unwrap().path();
                if p.is_dir() {
                    walk(&p, out);
                } else if p.extension().is_some_and(|x| x == "rs") {
                    out.push(p);
                }
            }
        }
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        walk(&root, &mut files);
        assert!(files.len() >= 8, "found {} files", files.len());
        for f in files {
            // This file's tests name the command to forbid it.
            if f.ends_with("ctl.rs") {
                continue;
            }
            let text = std::fs::read_to_string(&f).unwrap();
            for (n, line) in text.lines().enumerate() {
                let t = line.trim_start();
                if t.starts_with("//") {
                    continue;
                }
                let stripped = line
                    .replace("TRY_STOP_DEV", "")
                    .replace("SAFE_STOP_DEV", "")
                    .replace("try_stop_dev", "");
                for bad in ["STOP_DEV", "stop_dev", "kill_dev", "run_target"] {
                    assert!(
                        !stripped.contains(bad),
                        "{}:{} mentions {bad}: {line}",
                        f.display(),
                        n + 1
                    );
                }
            }
        }
    }
}
