//! The `(key, nonce)` tripwire (ADR-11, F3).
//!
//! In test and fuzz builds (`cfg(test)` or the `tripwire` feature; **not**
//! plain debug builds of a daemon) every AES-GCM seal records the pair it used and **panics on a repeat**. A
//! repeat cannot happen with a fresh random salt per extent key; the tripwire
//! exists for the day a change reintroduces a carried-over salt, a stale RNG
//! or a deterministic path. Only a digest of the pair is kept, never the key.
//! The ledger is capped at [`CAP`] pairs (64 MiB); past it nothing more is
//! recorded and [`Tripwire::overflowed`] says so, so a long test run cannot
//! grow without bound.

#[cfg(any(test, feature = "tripwire"))]
mod on {
    use std::collections::HashSet;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Mutex, PoisonError};

    /// Most pairs one ledger keeps.
    pub(crate) const CAP: usize = 4 * 1024 * 1024;

    /// The ledger of one codec.
    #[derive(Default)]
    pub(crate) struct Tripwire {
        seen: Mutex<HashSet<[u8; 16]>>,
        overflowed: AtomicBool,
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
            let mut seen = self.seen.lock().unwrap_or_else(PoisonError::into_inner);
            if seen.len() >= CAP {
                self.overflowed.store(true, Ordering::Relaxed);
                return;
            }
            let fresh = seen.insert(digest);
            drop(seen);
            assert!(
                fresh,
                "AES-GCM (key, nonce) pair repeated: the salt discipline is broken (ADR-11)"
            );
        }

        /// True once the ledger stopped recording at its cap.
        #[allow(dead_code)]
        pub(crate) fn overflowed(&self) -> bool {
            self.overflowed.load(Ordering::Relaxed)
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

#[cfg(not(any(test, feature = "tripwire")))]
mod off {
    /// The disabled ledger: records nothing.
    #[derive(Default)]
    pub(crate) struct Tripwire(());

    impl Tripwire {
        #[inline]
        pub(crate) fn record(&self, _key: &[u8; 32], _nonce: &[u8; 12]) {}

        pub(crate) fn len(&self) -> usize {
            0
        }

        #[allow(dead_code)]
        pub(crate) fn overflowed(&self) -> bool {
            false
        }
    }
}

#[cfg(not(any(test, feature = "tripwire")))]
pub(crate) use off::Tripwire;
#[cfg(any(test, feature = "tripwire"))]
pub(crate) use on::Tripwire;

/// True when this build runs the tripwire.
pub const ENABLED: bool = cfg!(any(test, feature = "tripwire"));
