//! The one error type of every decoder and fallible encoder in this crate.
//!
//! Variants carry a `what` (the structure being read) and, where useful, the
//! field, so a failure names its place without echoing attacker-chosen bytes.

/// Why a byte string is not a valid frozen structure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum FormatError {
    /// The input ends before the structure does.
    #[error("{what}: truncated (need {need} bytes, have {have})")]
    Truncated {
        what: &'static str,
        need: usize,
        have: usize,
    },
    /// A magic differs from the one the structure requires.
    #[error("{what}: bad magic")]
    BadMagic { what: &'static str },
    /// A version byte this reader does not know.
    #[error("{what}: unsupported version {got}")]
    BadVersion { what: &'static str, got: u64 },
    /// A reserved field or bit is not zero.
    #[error("{what}: reserved bits or bytes are not zero")]
    Reserved { what: &'static str },
    /// A field holds a value outside its legal range.
    #[error("{what}: {field} out of range ({value})")]
    Range {
        what: &'static str,
        field: &'static str,
        value: u64,
    },
    /// A count exceeds its stated maximum or the remaining input.
    #[error("{what}: {field} count {count} exceeds the limit {max}")]
    Count {
        what: &'static str,
        field: &'static str,
        count: u64,
        max: u64,
    },
    /// A CRC or checksum stored in the structure does not match.
    #[error("{what}: checksum mismatch")]
    Checksum { what: &'static str },
    /// The structure decodes but is not the canonical encoding of its content.
    #[error("{what}: not canonical ({why})")]
    NotCanonical {
        what: &'static str,
        why: &'static str,
    },
    /// Bytes remain after the structure ended.
    #[error("{what}: {extra} trailing bytes")]
    Trailing { what: &'static str, extra: usize },
    /// Two fields contradict each other or a structural rule fails.
    #[error("{what}: invalid ({why})")]
    Invalid {
        what: &'static str,
        why: &'static str,
    },
    /// A string field is not UTF-8 or uses a forbidden character.
    #[error("{what}: {field} is not a valid string")]
    BadString {
        what: &'static str,
        field: &'static str,
    },
    /// Checked arithmetic on sizes, offsets or sequence numbers overflowed.
    #[error("{what}: arithmetic overflow ({why})")]
    Overflow {
        what: &'static str,
        why: &'static str,
    },
    /// A compressed frame could not be decoded within its cap.
    #[error("{what}: compression error ({why})")]
    Compression {
        what: &'static str,
        why: &'static str,
    },
}

/// Result alias used throughout the crate.
pub type Result<T, E = FormatError> = core::result::Result<T, E>;

impl FormatError {
    /// A field outside its legal range.
    pub fn range(what: &'static str, field: &'static str, value: u64) -> Self {
        Self::Range { what, field, value }
    }

    /// A count over its limit.
    pub fn count(what: &'static str, field: &'static str, count: u64, max: u64) -> Self {
        Self::Count {
            what,
            field,
            count,
            max,
        }
    }

    /// A structural rule that fails.
    pub fn invalid(what: &'static str, why: &'static str) -> Self {
        Self::Invalid { what, why }
    }

    /// A non-canonical encoding.
    pub fn not_canonical(what: &'static str, why: &'static str) -> Self {
        Self::NotCanonical { what, why }
    }
}
