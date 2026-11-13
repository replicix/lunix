//! Errors of the ublk layer.

use std::fmt;
use std::io;
use std::time::Duration;

use crate::ctl::CtlOp;
use crate::features::MissingFeatures;
use crate::identity::IdentityMismatch;

/// A positive `errno` with its `strerror` text.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Errno(pub i32);

impl fmt::Display for Errno {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // `io::Error` already prints "... (os error N)".
        write!(f, "{}", io::Error::from_raw_os_error(self.0))
    }
}

/// Why a requested device size was refused. The kernel accepts all of these
/// (ADR-20), so the daemon refuses them before any command is sent.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum SizeError {
    /// Zero bytes.
    Zero,
    /// Not a multiple of the 4096-byte logical block.
    Unaligned,
    /// Smaller than the current size (shrink is not an online operation).
    Shrink {
        /// The current size in bytes.
        current: u64,
    },
    /// Above the configured maximum.
    AboveMax {
        /// The configured maximum in bytes.
        max: u64,
    },
}

impl fmt::Display for SizeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Zero => f.write_str("size is zero"),
            Self::Unaligned => f.write_str("size is not a multiple of 4096"),
            Self::Shrink { current } => {
                write!(f, "shrinking is refused (current size is {current})")
            }
            Self::AboveMax { max } => write!(f, "above the configured maximum of {max}"),
        }
    }
}

/// A failure of the ublk layer.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum UblkError {
    /// A system call failed.
    #[error("i/o error: {0}")]
    Io(#[from] io::Error),
    /// The kernel rejected a control command.
    #[error("{op} failed: {errno}")]
    Ctl {
        /// The command.
        op: CtlOp,
        /// The kernel's answer.
        errno: Errno,
    },
    /// A control command did not return in time (rule W4). The command may
    /// still be pending in the kernel: the caller reports, stops its driver
    /// chain and does not retry or signal anything.
    #[error(
        "{op} did not return within {after:?}; the command may still be pending in the kernel"
    )]
    Timeout {
        /// The command (or helper) that did not return.
        op: &'static str,
        /// The watchdog.
        after: Duration,
    },
    /// `TRY_STOP_DEV` found openers (a mounted file system): the device stays.
    #[error("{what} is busy (still mounted or open); the device stays")]
    Busy {
        /// What refused.
        what: &'static str,
    },
    /// `QUIESCE_DEV` did not see the device idle in time. The driver does not
    /// undo its canceling mark: the device refuses new I/O until the quiesce
    /// is retried or the daemon dies.
    #[error(
        "QUIESCE_DEV timed out: the device is draining and refuses new I/O; retry the quiesce or crash for recovery"
    )]
    QuiesceTimeout,
    /// The driver lacks a mandatory feature flag.
    #[error("{0}")]
    MissingFeatures(MissingFeatures),
    /// The kernel or the build cannot do what was asked.
    #[error("unsupported: {0}")]
    Unsupported(&'static str),
    /// The recorded device is not the device found; it was left alone.
    #[error("refusing to recover device {dev_id}: {why}")]
    Identity {
        /// The recorded id.
        dev_id: u32,
        /// The first check that failed.
        why: IdentityMismatch,
    },
    /// A resize request was refused before it touched the kernel.
    #[error("invalid device size {requested}: {why}")]
    InvalidSize {
        /// The requested size in bytes.
        requested: u64,
        /// The rule.
        why: SizeError,
    },
    /// The operation is not allowed in the device's current lifecycle state
    /// (stopping, quiescing, deleted) or in the kernel's device state.
    #[error("{0}")]
    State(&'static str),
    /// An error that happened after the kernel device `dev_id` existed. The
    /// id is here so that a caller that dies on the error can record it and
    /// have the device deleted by another process.
    #[error("device {dev_id}: {source}")]
    Device {
        /// The kernel's device id.
        dev_id: u32,
        /// What failed.
        source: Box<UblkError>,
    },
    /// The configuration is not acceptable.
    #[error("invalid configuration: {0}")]
    Config(&'static str),
    /// The engine failed.
    #[error("engine: {0}")]
    Engine(#[from] lunix_iface::Error),
    /// A queue thread could not start.
    #[error("queue {queue} failed to start: {why}")]
    QueueStart {
        /// The queue.
        queue: u16,
        /// What failed.
        why: String,
    },
    /// libublk reported an error while attaching.
    #[error("libublk: {0}")]
    Libublk(String),
    /// The queue threads did not end in time after the device was stopped.
    #[error("queue threads did not end within {after:?}; nothing was killed")]
    DrainTimeout {
        /// The wait.
        after: Duration,
    },
}

impl UblkError {
    /// The kernel errno of a [`UblkError::Ctl`], if this is one.
    pub fn errno(&self) -> Option<i32> {
        match self {
            Self::Ctl { errno, .. } => Some(errno.0),
            Self::Device { source, .. } => source.errno(),
            _ => None,
        }
    }

    /// The device id of a [`UblkError::Device`], if this is one.
    pub fn dev_id(&self) -> Option<u32> {
        match self {
            Self::Device { dev_id, .. } => Some(*dev_id),
            _ => None,
        }
    }

    /// The error without its [`UblkError::Device`] wrapper.
    pub fn root(&self) -> &UblkError {
        match self {
            Self::Device { source, .. } => source.root(),
            other => other,
        }
    }
}

/// Result alias of this crate.
pub type Result<T, E = UblkError> = std::result::Result<T, E>;
