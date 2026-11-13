//! Device identity (DESIGN §3): ublk has no owner tag on a device and ids are
//! reused after a reboot, so a device is recovered only if it is the one the
//! daemon recorded in `state/device`.

use std::fmt;
use std::io;

use lunix_format::local::DeviceRecord;
use lunix_format::VolumeUuid;

use crate::ctl::{DevInfo, STATE_LIVE, STATE_QUIESCED};

/// What the kernel says about the recorded device.
#[derive(Clone, Copy, Debug)]
pub struct KernelView {
    /// `GET_DEV_INFO2`.
    pub info: DevInfo,
    /// `basic.dev_sectors` of `GET_PARAMS`.
    pub dev_sectors: u64,
}

/// The first identity check that failed. A failed check leaves the device
/// untouched.
#[derive(Clone, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum IdentityMismatch {
    /// The record was made in another boot (ids are reused after a reboot).
    BootId,
    /// The record belongs to another volume.
    Volume,
    /// `GET_DEV_INFO` found no such device.
    NotFound,
    /// The kernel device carries another volume's tag in `ublksrv_flags`: it
    /// is some other Lunix device (or not ours at all).
    VolumeTag {
        /// The tag of the volume being recovered.
        expected: u64,
        /// The tag the device carries.
        actual: u64,
    },
    /// The device is `LIVE` and its server process is still running.
    LiveServer {
        /// The server's pid, as the kernel recorded it.
        pid: i32,
    },
    /// The device is not `QUIESCED` and not `LIVE` (a `LIVE` device whose
    /// daemon died is quiesced by the kernel moments later).
    State {
        /// `UBLK_S_DEV_*`.
        state: u16,
    },
    /// Different queue count.
    QueueCount {
        /// Recorded.
        recorded: u16,
        /// Found.
        actual: u16,
    },
    /// Different queue depth.
    QueueDepth {
        /// Recorded.
        recorded: u16,
        /// Found.
        actual: u16,
    },
    /// Different buffer size.
    MaxIoBufBytes {
        /// Recorded.
        recorded: u32,
        /// Found.
        actual: u32,
    },
    /// Different driver flags.
    Flags {
        /// Recorded.
        recorded: u64,
        /// Found.
        actual: u64,
    },
    /// Different size in sectors.
    DevSectors {
        /// Recorded.
        recorded: u64,
        /// Found.
        actual: u64,
    },
    /// The engine is smaller than the device (a shrink never happens online).
    EngineSmaller {
        /// The engine's size in 512-byte sectors.
        engine_sectors: u64,
        /// The device's size in 512-byte sectors.
        device_sectors: u64,
    },
}

impl fmt::Display for IdentityMismatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BootId => f.write_str("the record is from another boot"),
            Self::Volume => f.write_str("the record belongs to another volume"),
            Self::NotFound => f.write_str("no such device"),
            Self::VolumeTag { expected, actual } => write!(
                f,
                "the device carries volume tag {actual:#x}, not {expected:#x}: it is not this volume's device"
            ),
            Self::LiveServer { pid } => {
                write!(f, "the device is live and its server (pid {pid}) is still running")
            }
            Self::State { state } => write!(f, "device state {state} is not recoverable"),
            Self::QueueCount { recorded, actual } => {
                write!(f, "queue count {actual} differs from the recorded {recorded}")
            }
            Self::QueueDepth { recorded, actual } => {
                write!(f, "queue depth {actual} differs from the recorded {recorded}")
            }
            Self::MaxIoBufBytes { recorded, actual } => {
                write!(f, "buffer size {actual} differs from the recorded {recorded}")
            }
            Self::Flags { recorded, actual } => {
                write!(f, "flags {actual:#x} differ from the recorded {recorded:#x}")
            }
            Self::DevSectors { recorded, actual } => write!(
                f,
                "size {actual} sectors is below the recorded {recorded}: a device never shrinks"
            ),
            Self::EngineSmaller {
                engine_sectors,
                device_sectors,
            } => write!(
                f,
                "the engine ({engine_sectors} sectors) is smaller than the device ({device_sectors})"
            ),
        }
    }
}

/// Parses the text of `/proc/sys/kernel/random/boot_id` (a hyphenated UUID).
pub fn parse_boot_id(text: &str) -> Option<[u8; 16]> {
    let hex: Vec<u8> = text.trim().bytes().filter(|b| *b != b'-').collect();
    if hex.len() != 32 {
        return None;
    }
    let nibble = |b: u8| -> Option<u8> {
        match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'a'..=b'f' => Some(b - b'a' + 10),
            b'A'..=b'F' => Some(b - b'A' + 10),
            _ => None,
        }
    };
    let mut out = [0u8; 16];
    for (slot, pair) in out.iter_mut().zip(hex.chunks(2)) {
        *slot = nibble(pair[0])? << 4 | nibble(pair[1])?;
    }
    Some(out)
}

/// The current boot's id.
///
/// # Errors
/// The read error, or `InvalidData` for text that is not a UUID.
pub fn current_boot_id() -> io::Result<[u8; 16]> {
    let text = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")?;
    parse_boot_id(&text)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "boot_id is not a UUID"))
}

/// The record of a freshly added or recovered device.
pub fn make_record(
    info: &DevInfo,
    volume: VolumeUuid,
    boot_id: [u8; 16],
    dev_sectors: u64,
) -> DeviceRecord {
    DeviceRecord {
        nr_queues: info.nr_hw_queues,
        queue_depth: info.queue_depth,
        dev_id: info.dev_id,
        max_io_buf_bytes: info.max_io_buf_bytes,
        boot_id,
        volume_uuid: volume,
        dev_sectors,
        flags: info.flags,
    }
}

/// The tag stored in `ublksrv_flags` of every device of `volume`. The driver
/// keeps that field as given at `ADD_DEV` and never changes it, so it is the
/// one thing that ties a kernel device to a volume (ublk has no owner
/// concept). It is never zero.
pub fn volume_tag(volume: &VolumeUuid) -> u64 {
    let (lo, hi) = volume.0.split_at(8);
    let a = u64::from_le_bytes(lo.try_into().unwrap_or([0; 8]));
    let b = u64::from_le_bytes(hi.try_into().unwrap_or([0; 8]));
    (a ^ b.rotate_left(29)) | (1 << 63)
}

/// Whether a process is running (not a zombie). Used for the server pid of a
/// `LIVE` device.
pub fn process_alive(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return false;
    };
    // "pid (comm) S ...": the state follows the last ')'.
    let state = stat
        .rsplit_once(')')
        .and_then(|(_, rest)| rest.trim_start().chars().next());
    !matches!(state, Some('Z' | 'X') | None)
}

/// Checks that the device the kernel reports is the recorded one:
///
/// * same boot and same volume;
/// * the state is `QUIESCED`, or `LIVE` with a server process that is gone
///   (`server_alive` says which pids run; the kernel quiesces such a device
///   moments after its server died);
/// * `ublksrv_flags` carries this volume's tag ([`volume_tag`]);
/// * queue count, depth, buffer size and driver flags equal the record's;
/// * `record.dev_sectors <= dev_sectors <=` the engine's size. The size is not
///   identity: a grow makes the engine durable, then the kernel, then the
///   record, and a crash between any two leaves them in this order. Only a
///   device smaller than its record, or larger than the engine, is refused.
///
/// The record's `dev_id` is what was looked up; it is not compared again.
///
/// # Errors
/// The first [`IdentityMismatch`], in the order above.
pub fn check_identity(
    record: &DeviceRecord,
    volume: &VolumeUuid,
    kernel: &KernelView,
    boot_id_now: [u8; 16],
    engine_bytes: u64,
    server_alive: &dyn Fn(i32) -> bool,
) -> Result<(), IdentityMismatch> {
    if record.boot_id != boot_id_now {
        return Err(IdentityMismatch::BootId);
    }
    if record.volume_uuid != *volume {
        return Err(IdentityMismatch::Volume);
    }
    let info = &kernel.info;
    if info.state != STATE_QUIESCED && info.state != STATE_LIVE {
        return Err(IdentityMismatch::State { state: info.state });
    }
    let expected = volume_tag(volume);
    if info.ublksrv_flags != expected {
        return Err(IdentityMismatch::VolumeTag {
            expected,
            actual: info.ublksrv_flags,
        });
    }
    if info.state == STATE_LIVE && server_alive(info.ublksrv_pid) {
        return Err(IdentityMismatch::LiveServer {
            pid: info.ublksrv_pid,
        });
    }
    if info.nr_hw_queues != record.nr_queues {
        return Err(IdentityMismatch::QueueCount {
            recorded: record.nr_queues,
            actual: info.nr_hw_queues,
        });
    }
    if info.queue_depth != record.queue_depth {
        return Err(IdentityMismatch::QueueDepth {
            recorded: record.queue_depth,
            actual: info.queue_depth,
        });
    }
    if info.max_io_buf_bytes != record.max_io_buf_bytes {
        return Err(IdentityMismatch::MaxIoBufBytes {
            recorded: record.max_io_buf_bytes,
            actual: info.max_io_buf_bytes,
        });
    }
    if info.flags != record.flags {
        return Err(IdentityMismatch::Flags {
            recorded: record.flags,
            actual: info.flags,
        });
    }
    if kernel.dev_sectors < record.dev_sectors {
        return Err(IdentityMismatch::DevSectors {
            recorded: record.dev_sectors,
            actual: kernel.dev_sectors,
        });
    }
    let engine_sectors = engine_bytes >> 9;
    if engine_sectors < kernel.dev_sectors {
        return Err(IdentityMismatch::EngineSmaller {
            engine_sectors,
            device_sectors: kernel.dev_sectors,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOOT: [u8; 16] = [1; 16];
    const VOL: VolumeUuid = VolumeUuid([9; 16]);
    const NOBODY: &dyn Fn(i32) -> bool = &|_| false;

    fn view() -> (DeviceRecord, KernelView) {
        let info = DevInfo {
            nr_hw_queues: 4,
            queue_depth: 128,
            state: STATE_QUIESCED,
            max_io_buf_bytes: 1 << 20,
            dev_id: 3,
            flags: 0x6745a,
            ublksrv_flags: volume_tag(&VOL),
            ublksrv_pid: 4242,
            ..Default::default()
        };
        let dev_sectors = (16u64 << 30) >> 9;
        let rec = make_record(&info, VOL, BOOT, dev_sectors);
        (rec, KernelView { info, dev_sectors })
    }

    fn check(rec: &DeviceRecord, k: &KernelView, engine: u64) -> Result<(), IdentityMismatch> {
        check_identity(rec, &VOL, k, BOOT, engine, NOBODY)
    }

    #[test]
    fn the_recorded_device_passes() {
        let (rec, k) = view();
        assert_eq!(check(&rec, &k, 16 << 30), Ok(()));
        // A grown engine is fine: the layer catches the device up.
        assert_eq!(check(&rec, &k, 32 << 30), Ok(()));
    }

    #[test]
    fn a_live_device_whose_daemon_died_passes_too() {
        let (rec, mut k) = view();
        k.info.state = STATE_LIVE;
        assert_eq!(check(&rec, &k, 16 << 30), Ok(()));
    }

    #[test]
    fn a_live_device_with_a_running_server_is_refused() {
        let (rec, mut k) = view();
        k.info.state = STATE_LIVE;
        let alive: &dyn Fn(i32) -> bool = &|pid| pid == 4242;
        assert_eq!(
            check_identity(&rec, &VOL, &k, BOOT, 16 << 30, alive),
            Err(IdentityMismatch::LiveServer { pid: 4242 })
        );
        // A quiesced device never asks about the pid.
        k.info.state = STATE_QUIESCED;
        assert_eq!(
            check_identity(&rec, &VOL, &k, BOOT, 16 << 30, alive),
            Ok(())
        );
    }

    #[test]
    fn a_stale_record_from_another_boot_is_refused() {
        let (rec, k) = view();
        assert_eq!(
            check_identity(&rec, &VOL, &k, [2; 16], 16 << 30, NOBODY),
            Err(IdentityMismatch::BootId)
        );
    }

    #[test]
    fn another_volume_is_refused() {
        let (rec, k) = view();
        assert_eq!(
            check_identity(&rec, &VolumeUuid([8; 16]), &k, BOOT, 16 << 30, NOBODY),
            Err(IdentityMismatch::Volume)
        );
    }

    #[test]
    fn a_record_of_equal_geometry_pointed_at_another_volumes_device_is_refused() {
        // Two volumes, same queues, depth, buffer, flags and size: only the
        // volume tag in `ublksrv_flags` tells the devices apart.
        let (rec, mut k) = view();
        k.info.ublksrv_flags = volume_tag(&VolumeUuid([7; 16]));
        let got = check(&rec, &k, 16 << 30);
        assert!(
            matches!(got, Err(IdentityMismatch::VolumeTag { .. })),
            "{got:?}"
        );
        // A device with no tag (not added by Lunix) is refused as well.
        k.info.ublksrv_flags = 0;
        assert!(matches!(
            check(&rec, &k, 16 << 30),
            Err(IdentityMismatch::VolumeTag { .. })
        ));
    }

    #[test]
    fn the_volume_tag_differs_per_volume_and_is_never_zero() {
        let a = volume_tag(&VolumeUuid([1; 16]));
        let b = volume_tag(&VolumeUuid([2; 16]));
        let mut c = [1u8; 16];
        c[3] = 9;
        assert_ne!(a, b);
        assert_ne!(a, volume_tag(&VolumeUuid(c)));
        assert_ne!(volume_tag(&VolumeUuid([0; 16])), 0);
        assert_ne!(a, 0);
    }

    #[test]
    fn states_that_cannot_be_recovered_are_refused() {
        for state in [0, 3, 9] {
            let (rec, mut k) = view();
            k.info.state = state;
            assert_eq!(
                check(&rec, &k, 16 << 30),
                Err(IdentityMismatch::State { state })
            );
        }
    }

    #[test]
    fn a_wrong_queue_count_depth_buffer_or_flags_is_refused() {
        let (rec, k) = view();
        let mut a = k;
        a.info.nr_hw_queues = 8;
        assert!(matches!(
            check(&rec, &a, 16 << 30),
            Err(IdentityMismatch::QueueCount {
                recorded: 4,
                actual: 8
            })
        ));
        let mut b = k;
        b.info.queue_depth = 64;
        assert!(matches!(
            check(&rec, &b, 16 << 30),
            Err(IdentityMismatch::QueueDepth { .. })
        ));
        let mut c = k;
        c.info.max_io_buf_bytes = 1 << 19;
        assert!(matches!(
            check(&rec, &c, 16 << 30),
            Err(IdentityMismatch::MaxIoBufBytes { .. })
        ));
        let mut d = k;
        d.info.flags |= 1 << 7;
        assert!(matches!(
            check(&rec, &d, 16 << 30),
            Err(IdentityMismatch::Flags { .. })
        ));
    }

    #[test]
    fn the_size_order_record_le_device_le_engine_is_accepted() {
        let (rec, k) = view();
        // Crash after UPDATE_SIZE, before the record was rewritten: the
        // device is ahead of the record and the engine is at least as large.
        let mut grown = k;
        grown.dev_sectors += (16u64 << 30) >> 9;
        assert_eq!(check(&rec, &grown, 32 << 30), Ok(()));
        // Crash after the record was rewritten, before UPDATE_SIZE: the
        // record is ahead of the device. That is not a device this record
        // describes.
        let mut ahead = rec.clone();
        ahead.dev_sectors += 8;
        assert!(matches!(
            check(&ahead, &k, 32 << 30),
            Err(IdentityMismatch::DevSectors { .. })
        ));
    }

    #[test]
    fn an_engine_smaller_than_the_device_is_refused() {
        let (rec, k) = view();
        assert!(matches!(
            check(&rec, &k, 8 << 30),
            Err(IdentityMismatch::EngineSmaller { .. })
        ));
    }

    #[test]
    fn a_zombie_or_missing_process_is_not_alive() {
        assert!(process_alive(i32::try_from(std::process::id()).unwrap()));
        assert!(!process_alive(0));
        assert!(!process_alive(-5));
        assert!(!process_alive(i32::MAX));
    }

    #[test]
    fn the_boot_id_text_is_parsed() {
        let id = parse_boot_id("0123abcd-4567-89ef-0123-456789abcdef\n").unwrap();
        assert_eq!(id[0], 0x01);
        assert_eq!(id[3], 0xcd);
        assert_eq!(id[15], 0xef);
        assert!(parse_boot_id("not a uuid").is_none());
        assert!(parse_boot_id("0123abcd-4567-89ef-0123-456789abcdeg").is_none());
        assert!(parse_boot_id("").is_none());
    }

    #[test]
    fn the_record_round_trips_through_the_frozen_format() {
        let (rec, _) = view();
        let mut bytes = Vec::new();
        rec.encode(&mut bytes).unwrap();
        assert_eq!(bytes.len(), DeviceRecord::LEN);
        assert_eq!(DeviceRecord::decode(&bytes).unwrap(), rec);
    }
}
