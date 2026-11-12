//! Key types.
//!
//! Every key is a [`Secret`] in the locked key arena and is zeroized on drop.
//! None of them implements `Debug`, `Clone`, `Copy` or `Display`; the
//! compile-fail doctests below and `tests/types.rs` keep it that way.
//!
//! ```compile_fail,E0277
//! fn needs_debug<T: std::fmt::Debug>() {}
//! needs_debug::<lunix_crypto::Kmk>();
//! ```
//!
//! ```compile_fail,E0277
//! fn needs_clone<T: Clone>() {}
//! needs_clone::<lunix_crypto::Dek>();
//! ```
//!
//! ```compile_fail,E0277
//! fn needs_copy<T: Copy>() {}
//! needs_copy::<lunix_crypto::MetaKey>();
//! ```

pub use crate::arena::{arena_stats, ArenaStats, Secret};

use lunix_format::keyring::RootKind;
use lunix_format::CryptoMode;
use lunix_iface::Result;

use crate::arena::KEY_LEN;
use crate::rng;

macro_rules! key_type {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        pub struct $name(Secret);

        impl $name {
            /// Copies the key from `bytes`. The caller zeroizes its own copy.
            pub fn from_bytes(bytes: &[u8; KEY_LEN]) -> Self {
                Self(Secret::new(bytes))
            }

            #[allow(dead_code)] // only the derived key types are built from an arena secret
            pub(crate) fn from_secret(s: Secret) -> Self {
                Self(s)
            }

            /// The key bytes, for the code that uses them.
            pub(crate) fn expose(&self) -> &[u8; KEY_LEN] {
                self.0.expose()
            }

            /// The arena slot, for the zeroization test.
            #[cfg(feature = "test-rng")]
            pub fn secret(&self) -> &Secret {
                &self.0
            }

            /// The bytes, for known-answer tests only.
            #[cfg(feature = "test-rng")]
            pub fn expose_for_tests(&self) -> [u8; KEY_LEN] {
                *self.0.expose()
            }
        }
    };
}

key_type!(
    /// The key master key of an `encrypted` family.
    Kmk
);
key_type!(
    /// The integrity key of an `integrity` family.
    Ik
);
key_type!(
    /// The data-encryption key of one `dek_gen`: the PRF key of the per-extent key.
    Dek
);
key_type!(
    /// The XChaCha20-Poly1305 key of one metadata object kind.
    MetaKey
);
key_type!(
    /// The keyed-BLAKE3 (or HMAC) key of chunk ids or node hashes.
    AddressingKey
);
key_type!(
    /// An HMAC-SHA-256 key: descriptor MAC, keyring MAC or integrity HMAC.
    MacKey
);
key_type!(
    /// The key of the keyed BLAKE3-128 extent tag of an `integrity` volume.
    TagKey
);

/// The root secret of a family, as a keyring slot wraps it.
pub enum Root {
    /// An `encrypted` family's KMK.
    Kmk(Kmk),
    /// An `integrity` family's IK.
    Ik(Ik),
}

impl Root {
    /// A fresh random root of the given kind.
    ///
    /// # Errors
    /// `Io` when the operating system random source fails.
    pub fn generate(kind: RootKind) -> Result<Self> {
        let mut b = zeroize::Zeroizing::new([0u8; KEY_LEN]);
        rng::fill_os(&mut b[..])?;
        Ok(Self::from_bytes(kind, &b))
    }

    /// A root from known bytes (tests, migration tools).
    pub fn from_bytes(kind: RootKind, bytes: &[u8; KEY_LEN]) -> Self {
        match kind {
            RootKind::Kmk => Self::Kmk(Kmk::from_bytes(bytes)),
            RootKind::Ik => Self::Ik(Ik::from_bytes(bytes)),
        }
    }

    /// What the keyring calls this root.
    pub fn kind(&self) -> RootKind {
        match self {
            Self::Kmk(_) => RootKind::Kmk,
            Self::Ik(_) => RootKind::Ik,
        }
    }

    /// The volume mode this root serves.
    pub fn mode(&self) -> CryptoMode {
        match self {
            Self::Kmk(_) => CryptoMode::Encrypted,
            Self::Ik(_) => CryptoMode::Integrity,
        }
    }

    pub(crate) fn expose(&self) -> &[u8; KEY_LEN] {
        match self {
            Self::Kmk(k) => k.expose(),
            Self::Ik(k) => k.expose(),
        }
    }

    /// The bytes, for known-answer tests only.
    #[cfg(feature = "test-rng")]
    pub fn expose_for_tests(&self) -> [u8; KEY_LEN] {
        *self.expose()
    }
}
