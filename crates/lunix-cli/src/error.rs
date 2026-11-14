//! Errors of the client and their exit codes.

use std::io;

use lunix_iface::ctl::{CtlError, ErrorCode};

/// Exit code: success.
pub const EXIT_OK: u8 = 0;
/// Exit code: any other failure.
pub const EXIT_GENERIC: u8 = 1;
/// Exit code: a usage error.
pub const EXIT_USAGE: u8 = 2;

/// Why a command failed. Never carries a secret: messages name fields, not
/// values.
#[derive(Debug, thiserror::Error)]
pub enum CliError {
    /// The command line or a request field is wrong (exit 2).
    #[error("{0}")]
    Usage(String),
    /// The control socket cannot be reached (exit 1).
    #[error("cannot connect to lunixd at {path}: {source}")]
    Connect {
        /// The socket path.
        path: String,
        /// The OS error.
        #[source]
        source: io::Error,
    },
    /// The daemon spoke wrongly, or the stream broke (exit 1).
    #[error("{0}")]
    Protocol(String),
    /// The daemon refused the request (exit by code).
    #[error("{}", sanitize(&.0.message))]
    Daemon(CtlError),
    /// Anything else (exit 1).
    #[error("{0}")]
    Other(String),
}

impl CliError {
    /// The process exit code.
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::Usage(_) => EXIT_USAGE,
            Self::Daemon(e) => e.code.exit_code(),
            Self::Connect { .. } | Self::Protocol(_) | Self::Other(_) => EXIT_GENERIC,
        }
    }

    /// The error class for the first line of the message, `error` when the
    /// error did not come from the daemon.
    pub fn class(&self) -> &'static str {
        match self {
            Self::Daemon(e) => code_name(e.code),
            Self::Usage(_) => "usage",
            _ => "error",
        }
    }
}

impl From<io::Error> for CliError {
    fn from(e: io::Error) -> Self {
        Self::Other(format!("i/o error: {e}"))
    }
}

/// The wire name of an error code.
pub fn code_name(c: ErrorCode) -> &'static str {
    match c {
        ErrorCode::Busy => "busy",
        ErrorCode::NotFound => "not_found",
        ErrorCode::Downgrade => "downgrade",
        ErrorCode::WrongKmk => "wrong_kmk",
        ErrorCode::Rollback => "rollback",
        ErrorCode::Fork => "fork",
        ErrorCode::ForkUnverifiable => "fork_unverifiable",
        ErrorCode::JournalDamage => "journal_damage",
        ErrorCode::NoSpace => "no_space",
        ErrorCode::Invalid => "invalid",
        ErrorCode::Io => "io",
        ErrorCode::Internal => "internal",
        ErrorCode::Unknown => "unknown",
    }
}

/// Replaces control characters (a daemon message must not drive the terminal).
pub fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { '?' } else { c })
        .collect()
}

/// A second line of advice for an attach failure, when there is one.
pub fn attach_hint(code: ErrorCode, name: &str) -> Option<String> {
    match code {
        ErrorCode::ForkUnverifiable => Some(
            "the chain back to this host's rollback floor cannot be walked; if you accept that risk, \
             attach again with --allow-unverifiable-fork (audited)"
                .to_owned(),
        ),
        ErrorCode::Rollback | ErrorCode::Fork => Some(format!(
            "this host recorded a newer or different history of the volume; restore the newer state, \
             or lower the floor offline with `lunix fsck {name} --repair --truncate-tip` (loses the commits above it)"
        )),
        ErrorCode::JournalDamage => Some(format!(
            "run `lunix fsck {name}` to inspect the journal, or attach with --on-journal-damage wipe to discard the damaged tail"
        )),
        ErrorCode::Downgrade | ErrorCode::WrongKmk => Some(
            "check the key file or passphrase, and --credential-mode (a passphrase for an integrity volume needs --credential-mode integrity)"
                .to_owned(),
        ),
        _ => None,
    }
}
