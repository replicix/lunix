//! The random source. Production code draws from the operating system through
//! aws-lc-rs on every call: there is no buffered pool and no seed (ADR-11).
//! With the `test-rng` feature a test may inject a source to reproduce the
//! known-answer vectors.

use lunix_iface::{Error, Result};

#[cfg(feature = "test-rng")]
use std::sync::Arc;

/// A replaceable random source (feature `test-rng` only).
#[cfg(feature = "test-rng")]
pub trait Rng: Send + Sync + 'static {
    /// Fills `dst`.
    fn fill(&self, dst: &mut [u8]);
}

/// Where a codec gets its salts and nonces.
#[derive(Clone, Default)]
pub(crate) enum Source {
    /// The operating system CSPRNG.
    #[default]
    Os,
    /// A test source.
    #[cfg(feature = "test-rng")]
    Custom(Arc<dyn Rng>),
}

impl Source {
    /// Fills `dst` with fresh random bytes.
    pub(crate) fn fill(&self, dst: &mut [u8]) -> Result<()> {
        match self {
            Self::Os => fill_os(dst),
            #[cfg(feature = "test-rng")]
            Self::Custom(r) => {
                r.fill(dst);
                Ok(())
            }
        }
    }
}

/// Fills `dst` from the operating system.
pub(crate) fn fill_os(dst: &mut [u8]) -> Result<()> {
    aws_lc_rs::rand::fill(dst).map_err(|_| {
        Error::Io(std::io::Error::other(
            "operating system random source failed",
        ))
    })
}
