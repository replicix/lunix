//! Device settings and the parameters sent with `SET_PARAMS` (DESIGN §3).

use std::time::Duration;

use libublk::sys;
use lunix_format::VolumeUuid;

use crate::error::{Result, UblkError};

/// The logical and physical block size: 4096 bytes.
pub const BLOCK_BYTES: u64 = 4096;

/// `max_io_buf_bytes` default: 1 MiB.
pub const DEFAULT_IO_BUF_BYTES: u32 = 1 << 20;

/// `max_io_buf_bytes` minimum: 256 KiB (D14).
pub const MIN_IO_BUF_BYTES: u32 = 256 << 10;

/// The largest `max_io_buf_bytes` libublk accepts.
pub const MAX_IO_BUF_BYTES: u32 = 32 << 20;

/// Queue depth default.
pub const DEFAULT_DEPTH: u16 = 128;

/// `max_discard_sectors` and `max_write_zeroes_sectors`: just under 2 GiB, a
/// multiple of the 8-sector granularity, so that one request is at most
/// `i32::MAX` bytes (`lunix_iface::block::IoRequest::len`).
pub const MAX_DISCARD_SECTORS: u32 = (i32::MAX as u32 / 512) & !7;

/// How data reaches the daemon.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum DataPath {
    /// The kernel copies into and out of preallocated tag buffers (default).
    #[default]
    Copy,
    /// `UBLK_F_USER_COPY`: the queue thread `pread`s a write's data and
    /// `pwrite`s a read's data on `/dev/ublkcN` (ADR-17, opt-in per volume).
    UserCopy,
}

/// Recovery policy of the device. Only the first is for production.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum RecoveryPolicy {
    /// `USER_RECOVERY | USER_RECOVERY_REISSUE`: in-flight requests are
    /// reissued after a recovery (ADR-16).
    #[default]
    Reissue,
    /// `USER_RECOVERY` alone. **Negative control of the test harness only**:
    /// a recovery then fails the requests that were in flight.
    NoReissueNegativeControl,
}

/// Watchdogs and waits. Defaults follow rule W4.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Timeouts {
    /// Every control command except `DEL_DEV` and `QUIESCE_DEV`.
    pub ctl: Duration,
    /// `DEL_DEV` (blocks while a file system is mounted, W5).
    pub del: Duration,
    /// How long queue threads get to end after a successful `TRY_STOP_DEV`.
    pub drain: Duration,
    /// How long queue threads get to build their handlers and queues.
    pub startup: Duration,
    /// Pause between `START_USER_RECOVERY` attempts that answer `EBUSY`.
    pub recovery_retry: Duration,
    /// Total time spent retrying `START_USER_RECOVERY`.
    pub recovery_wait: Duration,
    /// The engine's final commit at quiesce and detach.
    pub engine: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self {
            ctl: Duration::from_secs(30),
            del: Duration::from_secs(60),
            drain: Duration::from_secs(30),
            startup: Duration::from_secs(30),
            recovery_retry: Duration::from_millis(100),
            recovery_wait: Duration::from_secs(30),
            engine: Duration::from_secs(120),
        }
    }
}

/// Everything the layer needs to create or recover one device.
#[derive(Clone, Debug)]
pub struct UblkConfig {
    /// The volume served; stored in the device record.
    pub volume: VolumeUuid,
    /// Hardware queues (1 ..= 16). See [`UblkConfig::default_queues`].
    pub queues: u16,
    /// Requests in flight per queue.
    pub depth: u16,
    /// Size of each tag buffer; `max_sectors` follows it (D14).
    pub max_io_buf_bytes: u32,
    /// The volume's chunk size in bytes, a power of two: `io_opt`.
    pub chunk_bytes: u32,
    /// The largest size a grow may reach.
    pub max_size_bytes: u64,
    /// `READ_ONLY` attribute (snapshot mounts).
    pub read_only: bool,
    /// Copy (default) or `USER_COPY`.
    pub data_path: DataPath,
    /// Recovery policy.
    pub recovery: RecoveryPolicy,
    /// CPUs for the queue threads: queue `q` runs on `cpus[q % cpus.len()]`.
    /// Empty means unpinned.
    pub cpus: Vec<usize>,
    /// Ask the driver for this device id (`None`: it picks one).
    pub dev_id: Option<u32>,
    /// Watchdogs and waits.
    pub timeouts: Timeouts,
}

impl UblkConfig {
    /// Defaults for `volume`: `min(8, cpus / 4)` queues, depth 128, 1 MiB buffers, 64 KiB
    /// chunks, copy path.
    pub fn new(volume: VolumeUuid) -> Self {
        Self {
            volume,
            queues: Self::default_queues(
                std::thread::available_parallelism().map_or(4, std::num::NonZeroUsize::get),
            ),
            depth: DEFAULT_DEPTH,
            max_io_buf_bytes: DEFAULT_IO_BUF_BYTES,
            chunk_bytes: 64 << 10,
            max_size_bytes: 1 << 50,
            read_only: false,
            data_path: DataPath::Copy,
            recovery: RecoveryPolicy::Reissue,
            cpus: Vec::new(),
            dev_id: None,
            timeouts: Timeouts::default(),
        }
    }

    /// The default queue count for a host: `min(8, cpus / 4)`, at least 1.
    pub fn default_queues(cpus: usize) -> u16 {
        u16::try_from((cpus / 4).clamp(1, 8)).unwrap_or(8)
    }

    /// Checks every setting.
    ///
    /// # Errors
    /// [`UblkError::Config`] naming the first bad setting.
    pub fn validate(&self) -> Result<()> {
        if !(1..=16).contains(&self.queues) {
            return Err(UblkError::Config("queues must be 1 to 16"));
        }
        if self.depth == 0 || self.depth > 4096 {
            return Err(UblkError::Config("depth must be 1 to 4096"));
        }
        let buf = self.max_io_buf_bytes;
        if !(MIN_IO_BUF_BYTES..=MAX_IO_BUF_BYTES).contains(&buf)
            || !u64::from(buf).is_multiple_of(BLOCK_BYTES)
        {
            return Err(UblkError::Config(
                "max_io_buf_bytes must be a multiple of 4096 from 256 KiB to 32 MiB",
            ));
        }
        let chunk = self.chunk_bytes;
        if chunk < 4096 || !chunk.is_power_of_two() {
            return Err(UblkError::Config(
                "chunk_bytes must be a power of two of at least 4096",
            ));
        }
        if self.max_size_bytes < BLOCK_BYTES || !self.max_size_bytes.is_multiple_of(BLOCK_BYTES) {
            return Err(UblkError::Config(
                "max_size_bytes must be a positive multiple of 4096",
            ));
        }
        Ok(())
    }
}

/// The `SET_PARAMS` payload of DESIGN §3.
///
/// `BASIC`: 4096-byte logical and physical blocks, `io_min` 4096, `io_opt` =
/// the chunk size, `max_sectors` = the tag buffer, `attrs = VOLATILE_CACHE |
/// FUA` (plus `READ_ONLY`), `dev_sectors` = the size. `DISCARD`: granularity
/// 4096, large `max_discard_sectors` and `max_write_zeroes_sectors`.
///
/// # Errors
/// A setting [`UblkConfig::validate`] refuses, or a size that is zero or not a
/// multiple of 4096.
pub fn build_params(cfg: &UblkConfig, size_bytes: u64) -> Result<sys::ublk_params> {
    cfg.validate()?;
    if size_bytes == 0 || !size_bytes.is_multiple_of(BLOCK_BYTES) {
        return Err(UblkError::Config(
            "device size must be a positive multiple of 4096",
        ));
    }
    let mut attrs = sys::UBLK_ATTR_VOLATILE_CACHE | sys::UBLK_ATTR_FUA;
    if cfg.read_only {
        attrs |= sys::UBLK_ATTR_READ_ONLY;
    }
    let shift = |n: u32| -> u8 { u8::try_from(n.trailing_zeros()).unwrap_or(u8::MAX) };
    Ok(sys::ublk_params {
        len: u32::try_from(std::mem::size_of::<sys::ublk_params>()).unwrap_or(u32::MAX),
        types: sys::UBLK_PARAM_TYPE_BASIC | sys::UBLK_PARAM_TYPE_DISCARD,
        basic: sys::ublk_param_basic {
            attrs,
            logical_bs_shift: 12,
            physical_bs_shift: 12,
            io_min_shift: 12,
            io_opt_shift: shift(cfg.chunk_bytes),
            max_sectors: cfg.max_io_buf_bytes >> 9,
            dev_sectors: size_bytes >> 9,
            ..Default::default()
        },
        discard: sys::ublk_param_discard {
            discard_alignment: 0,
            discard_granularity: 4096,
            max_discard_sectors: MAX_DISCARD_SECTORS,
            max_write_zeroes_sectors: MAX_DISCARD_SECTORS,
            max_discard_segments: 1,
            ..Default::default()
        },
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> UblkConfig {
        UblkConfig::new(VolumeUuid([7; 16]))
    }

    #[test]
    fn params_follow_design_section_3() {
        let mut c = cfg();
        c.chunk_bytes = 64 << 10;
        let p = build_params(&c, 16 << 30).unwrap();
        assert_eq!(
            p.types,
            sys::UBLK_PARAM_TYPE_BASIC | sys::UBLK_PARAM_TYPE_DISCARD
        );
        assert_eq!(p.basic.logical_bs_shift, 12);
        assert_eq!(p.basic.physical_bs_shift, 12);
        assert_eq!(p.basic.io_min_shift, 12);
        assert_eq!(p.basic.io_opt_shift, 16);
        assert_eq!(p.basic.max_sectors, 2048);
        assert_eq!(p.basic.dev_sectors, (16u64 << 30) >> 9);
        assert_eq!(
            p.basic.attrs,
            sys::UBLK_ATTR_VOLATILE_CACHE | sys::UBLK_ATTR_FUA
        );
        assert_eq!(p.discard.discard_granularity, 4096);
        assert_eq!(
            p.discard.max_discard_sectors,
            p.discard.max_write_zeroes_sectors
        );
        assert!(u64::from(p.discard.max_discard_sectors) * 512 <= i32::MAX as u64);
        assert_eq!(p.discard.max_discard_sectors % 8, 0);
        assert_eq!(p.len as usize, std::mem::size_of::<sys::ublk_params>());
    }

    #[test]
    fn read_only_adds_the_attribute() {
        let mut c = cfg();
        c.read_only = true;
        let p = build_params(&c, 1 << 20).unwrap();
        assert_ne!(p.basic.attrs & sys::UBLK_ATTR_READ_ONLY, 0);
        assert_ne!(p.basic.attrs & sys::UBLK_ATTR_FUA, 0);
    }

    #[test]
    fn max_sectors_follows_the_buffer() {
        let mut c = cfg();
        c.max_io_buf_bytes = 256 << 10;
        assert_eq!(build_params(&c, 1 << 20).unwrap().basic.max_sectors, 512);
        c.max_io_buf_bytes = 4 << 20;
        assert_eq!(build_params(&c, 1 << 20).unwrap().basic.max_sectors, 8192);
    }

    #[test]
    fn sizes_must_be_positive_and_aligned() {
        let c = cfg();
        assert!(build_params(&c, 0).is_err());
        assert!(build_params(&c, 4096 + 512).is_err());
        assert!(build_params(&c, 4096).is_ok());
    }

    #[test]
    fn validation_rejects_bad_settings() {
        let mut c = cfg();
        assert!(c.validate().is_ok());
        c.queues = 0;
        assert!(c.validate().is_err());
        c.queues = 17;
        assert!(c.validate().is_err());
        c.queues = 16;
        assert!(c.validate().is_ok());
        c.depth = 0;
        assert!(c.validate().is_err());
        c.depth = 128;
        c.max_io_buf_bytes = 128 << 10;
        assert!(c.validate().is_err());
        c.max_io_buf_bytes = (256 << 10) + 512;
        assert!(c.validate().is_err());
        c.max_io_buf_bytes = 1 << 20;
        c.chunk_bytes = 3 << 12;
        assert!(c.validate().is_err());
        c.chunk_bytes = 2048;
        assert!(c.validate().is_err());
        c.chunk_bytes = 4096;
        assert!(c.validate().is_ok());
        c.max_size_bytes = 100;
        assert!(c.validate().is_err());
    }

    #[test]
    fn default_queue_count_is_min_8_cpus_over_4() {
        assert_eq!(UblkConfig::default_queues(1), 1);
        assert_eq!(UblkConfig::default_queues(4), 1);
        assert_eq!(UblkConfig::default_queues(16), 4);
        assert_eq!(UblkConfig::default_queues(32), 8);
        assert_eq!(UblkConfig::default_queues(256), 8);
    }
}
