//! The `(key, nonce)` tripwire (ADR-11, F3).
//!
//! In debug and fuzz builds (`debug_assertions` or the `tripwire` feature)
//! every AES-GCM seal records the pair it used and **panics on a repeat**. A
//! repeat cannot happen with a fresh random salt per extent key; the tripwire
//! exists for the day a change reintroduces a carried-over salt, a stale RNG
//! or a deterministic path. Only a digest of the pair is kept, never the key.
//! The ledger grows with every seal, which is why it is not in release builds.

#[cfg(any(debug_assertions, feature = "tripwire"))]
mod on {
    use std::collections::HashSet;
    use std::sync::{Mutex, PoisonError};

    /// The ledger of one codec.
    #[derive(Default)]
    pub(crate) struct Tripwire {
        seen: Mutex<HashSet<[u8; 16]>>,
    }

    impl Tripwire {
        /// Records the pair.
        ///
        /// # Panics
        /// If the pair was recorded before: the nonce discipline is broken.
        pub(crate) fn record(&self, key: &[u8; 32], nonce: &[u8; 12]) {
            let mut h = blake3::Hasher::new();
            h.update(key);
            h.update(nonce);
            let mut digest = [0u8; 16];
            digest.copy_from_slice(&h.finalize().as_bytes()[..16]);
            let fresh = self
                .seen
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(digest);
            assert!(
                fresh,
                "AES-GCM (key, nonce) pair repeated: the salt discipline is broken (ADR-11)"
            );
        }

        /// Pairs recorded so far.
        pub(crate) fn len(&self) -> usize {
            self.seen
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .len()
        }
    }
}

#[cfg(not(any(debug_assertions, feature = "tripwire")))]
mod off {
    /// The disabled ledger: records nothing.
    #[derive(Default)]
    pub(crate) struct Tripwire;

    impl Tripwire {
        #[inline]
        pub(crate) fn record(&self, _key: &[u8; 32], _nonce: &[u8; 12]) {}

        pub(crate) fn len(&self) -> usize {
            0
        }
    }
}

#[cfg(not(any(debug_assertions, feature = "tripwire")))]
pub(crate) use off::Tripwire;
#[cfg(any(debug_assertions, feature = "tripwire"))]
pub(crate) use on::Tripwire;

/// True when this build runs the tripwire.
pub const ENABLED: bool = cfg!(any(debug_assertions, feature = "tripwire"));
