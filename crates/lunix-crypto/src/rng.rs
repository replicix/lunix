//! The random source. Production code calls `getrandom(2)` on every draw: the
//! kernel CRNG, with no userspace buffer, pool or seed (ADR-11 F3). The AWS-LC
//! `RAND_bytes` DRBG is deliberately **not** used: it is a per-thread CTR-DRBG
//! reseeded from the OS only every few thousand calls, so a restored VM
//! snapshot could replay salts, which is the case the salt rule exists for.
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

/// Fills `dst` from the operating system with `getrandom(2)`, retrying on
/// `EINTR` and on short reads.
pub(crate) fn fill_os(dst: &mut [u8]) -> Result<()> {
    let mut done = 0;
    while done < dst.len() {
        match rustix::rand::getrandom(&mut dst[done..], rustix::rand::GetRandomFlags::empty()) {
            Ok(0) => return Err(random_failed()),
            Ok(n) => done += n,
            Err(rustix::io::Errno::INTR) => {}
            Err(_) => return Err(random_failed()),
        }
    }
    Ok(())
}

fn random_failed() -> Error {
    Error::Io(std::io::Error::other(
        "operating system random source failed",
    ))
}
