//! The one error type shared by every storage-facing trait.
//!
//! Variants name a class of failure, not a source: implementations map their
//! own errors onto them so that callers (the engine, GC, fsck) can decide by
//! [`ErrorKind`] without knowing which implementation they hold. Messages never
//! echo bytes read from disk or from the socket.

use std::io;

use lunix_format::FormatError;

/// Result alias used by every trait in this crate.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// A failure of a storage, index, map, space or crypto operation.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The host file system or a device failed. A host `EIO` is fatal for the
    /// volume (see [`crate::space::SpaceEvents::fatal_io`]).
    #[error("i/o error: {0}")]
    Io(#[from] io::Error),
    /// The named object does not exist.
    #[error("not found: {what}")]
    NotFound {
        /// The kind of object.
        what: &'static str,
    },
    /// A create-only object already exists.
    #[error("already exists: {what}")]
    AlreadyExists {
        /// The kind of object.
        what: &'static str,
    },
    /// Stored bytes are structurally wrong (checksum, framing, cross-field rule).
    #[error("corrupt {what}: {detail}")]
    Corrupt {
        /// The kind of object.
        what: &'static str,
        /// A fixed description written by the detecting code.
        detail: &'static str,
    },
    /// A frozen-format decoder rejected the bytes.
    #[error("invalid stored format: {0}")]
    Format(#[from] FormatError),
    /// A tag, AEAD or MAC did not verify, or a key does not fit the object.
    /// Wrong credential and tampered object give the same error on purpose.
    #[error("authentication failed: {what}")]
    Auth {
        /// What failed to authenticate.
        what: &'static str,
    },
    /// The space gate or the host file system has no room.
    #[error("no space")]
    NoSpace,
    /// The volume is read-only after a fatal host I/O error.
    #[error("volume is read-only")]
    ReadOnly,
    /// The map reached its dirty-leaf cap: seal before applying more updates.
    #[error("map is full: seal before more updates")]
    MapFull,
    /// A feature that phase 1 accepts in the format but does not implement.
    #[error("unsupported: {what}")]
    Unsupported {
        /// The feature.
        what: &'static str,
    },
    /// A safety rule refused the operation (for example deleting at or above
    /// `chain_floor`). Nothing changed.
    #[error("refused: {why}")]
    Refused {
        /// The rule.
        why: &'static str,
    },
    /// Another holder has the resource (volume lock, open update).
    #[error("busy: {what}")]
    Busy {
        /// The resource.
        what: &'static str,
    },
    /// The caller broke a documented precondition (a bug, not a data error).
    #[error("invalid argument: {why}")]
    Invalid {
        /// The precondition.
        why: &'static str,
    },
    /// The component was shut down.
    #[error("closed")]
    Closed,
}

/// The class of an [`Error`], for matching without destructuring.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[non_exhaustive]
pub enum ErrorKind {
    /// [`Error::Io`].
    Io,
    /// [`Error::NotFound`].
    NotFound,
    /// [`Error::AlreadyExists`].
    AlreadyExists,
    /// [`Error::Corrupt`] and [`Error::Format`].
    Corrupt,
    /// [`Error::Auth`].
    Auth,
    /// [`Error::NoSpace`].
    NoSpace,
    /// [`Error::ReadOnly`].
    ReadOnly,
    /// [`Error::MapFull`].
    MapFull,
    /// [`Error::Unsupported`].
    Unsupported,
    /// [`Error::Refused`].
    Refused,
    /// [`Error::Busy`].
    Busy,
    /// [`Error::Invalid`].
    Invalid,
    /// [`Error::Closed`].
    Closed,
}

impl Error {
    /// The class of this error.
    pub fn kind(&self) -> ErrorKind {
        match self {
            Self::Io(_) => ErrorKind::Io,
            Self::NotFound { .. } => ErrorKind::NotFound,
            Self::AlreadyExists { .. } => ErrorKind::AlreadyExists,
            Self::Corrupt { .. } | Self::Format(_) => ErrorKind::Corrupt,
            Self::Auth { .. } => ErrorKind::Auth,
            Self::NoSpace => ErrorKind::NoSpace,
            Self::ReadOnly => ErrorKind::ReadOnly,
            Self::MapFull => ErrorKind::MapFull,
            Self::Unsupported { .. } => ErrorKind::Unsupported,
            Self::Refused { .. } => ErrorKind::Refused,
            Self::Busy { .. } => ErrorKind::Busy,
            Self::Invalid { .. } => ErrorKind::Invalid,
            Self::Closed => ErrorKind::Closed,
        }
    }

    /// The positive errno a block-device completion reports for this error.
    ///
    /// Read-only maps to `EIO` (the ublk layer has no `EROFS` for a live
    /// device); a corrupt or unauthenticated extent is `EIO`, never data.
    pub fn errno(&self) -> i32 {
        use rustix::io::Errno;
        match self {
            Self::Io(e) => e.raw_os_error().unwrap_or(Errno::IO.raw_os_error()),
            Self::NoSpace => Errno::NOSPC.raw_os_error(),
            Self::Invalid { .. } => Errno::INVAL.raw_os_error(),
            Self::Busy { .. } => Errno::BUSY.raw_os_error(),
            _ => Errno::IO.raw_os_error(),
        }
    }

    /// Shorthand for [`Error::NotFound`].
    pub fn not_found(what: &'static str) -> Self {
        Self::NotFound { what }
    }

    /// Shorthand for [`Error::Invalid`].
    pub fn invalid(why: &'static str) -> Self {
        Self::Invalid { why }
    }
}
