//! Hooks for the known-answer and discipline tests (feature `test-rng`, which
//! `tools/check-features.sh` keeps off every normal and build edge).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use chacha20poly1305::aead::{Aead, Payload};
use chacha20poly1305::{KeyInit, XChaCha20Poly1305, XNonce};
use lunix_format::purpose::Purpose;
use lunix_format::{FamilyId, Profile};
use lunix_iface::{Error, Result};

pub use crate::rng::Rng;
use crate::secret::Root;

/// A source that hands out the listed byte strings in turn, cycling when it
/// runs out. Replaying one string models a restored VM snapshot or a forked
/// RNG: the tripwire must fire.
pub struct ReplayRng {
    items: Vec<Vec<u8>>,
    next: AtomicUsize,
}

impl ReplayRng {
    /// Every `fill` copies the first `dst.len()` bytes of the next item.
    ///
    /// # Panics
    /// If `items` is empty.
    pub fn new(items: Vec<Vec<u8>>) -> Arc<Self> {
        assert!(!items.is_empty(), "ReplayRng needs at least one item");
        Arc::new(Self {
            items,
            next: AtomicUsize::new(0),
        })
    }
}

impl Rng for ReplayRng {
    fn fill(&self, dst: &mut [u8]) {
        let i = self.next.fetch_add(1, Ordering::Relaxed) % self.items.len();
        let item = &self.items[i];
        dst.copy_from_slice(&item[..dst.len()]);
    }
}

/// The per-extent key of the vectors: `PRF(dek, info)` of the profile.
pub fn extent_key(profile: Profile, dek: &[u8; 32], info: &[u8]) -> Result<[u8; 32]> {
    Ok(*crate::kdf::extent_key(profile, dek, info)?)
}

/// XChaCha20-Poly1305 with a caller-chosen nonce, for the vector that pins the
/// primitive. Production sealing draws its nonce internally.
pub fn xchacha_seal(key: &[u8; 32], nonce: &[u8; 24], aad: &[u8], msg: &[u8]) -> Result<Vec<u8>> {
    XChaCha20Poly1305::new(key.into())
        .encrypt(
            &XNonce::try_from(&nonce[..]).map_err(|_| Error::invalid("nonce"))?,
            Payload { msg, aad },
        )
        .map_err(|_| Error::invalid("xchacha seal"))
}

/// The key of `purpose` under `root`, for the known-answer vectors.
pub fn purpose_key(
    profile: Profile,
    root: &Root,
    family: &FamilyId,
    purpose: &Purpose,
) -> Result<[u8; 32]> {
    Ok(*crate::kdf::purpose_key(profile, root, family, purpose)?)
}
