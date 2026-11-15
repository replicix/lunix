//! Key hierarchy, extent AEAD, metadata sealing, keyring and the attach
//! downgrade check (M2; ADR-11, on-disk-formats.md sections 4, 6, 9-13).
//!
//! | module | what |
//! |---|---|
//! | [`secret`] | key types: arena-backed, zeroized, no `Debug`/`Clone`/`Copy` |
//! | `kdf` (private) | the purpose key hierarchy (keyed BLAKE3; HKDF-SHA-256 for the FIPS profile) |
//! | [`keys`] | [`VolumeKeys`]: every long-lived key of an attached volume |
//! | [`extent`] | [`AesGcmExtents`] and [`PlainExtents`] (the `ExtentCodec` impls) |
//! | [`meta`] | [`MetaSuite`]: `MetaCodec` and `CommitCodec` for all three modes, the lease |
//! | [`keyring`] | create, unlock, add, remove, rewrap, rotate; Argon2id and key-file slots |
//! | [`attach`] | descriptor MAC, fingerprint, [`check_attach`] |
//! | [`volume`] | [`VolumeCrypto`]: the codecs of one volume |
//!
//! # Rules this crate enforces
//!
//! * **Salt rule.** Nothing here accepts a salt or a nonce. `seal` draws a
//!   fresh 192-bit salt from the kernel with `getrandom(2)` on every call
//!   (`AesGcmExtents::seal`), and metadata nonces, slot nonces, Argon2id salts
//!   and new roots likewise; never from a userspace DRBG. The `test-rng`
//!   feature (dev-dependency only) can swap the source for the vectors.
//! * **Keys.** Every key lives in the page-locked, dump-excluded arena
//!   ([`arena_stats`] reports how far `mlock` and `MADV_DONTDUMP` got) and is
//!   zeroized on drop. No key type is `Debug`, `Clone` or `Copy`.
//! * **Mode.** "Encrypted" is read from the authenticated descriptor, never
//!   from an extent. The FIPS profile derives keys but cannot seal in phase 1
//!   (`Unsupported`).
//! * **Constant time.** MACs, tags and ids are compared without early exit.
//!
//! # Limits of zeroization
//!
//! Key bytes the arena holds are wiped on drop. Copies inside the AES and
//! HMAC contexts of aws-lc-rs and inside a BLAKE3 hasher are the libraries'
//! to wipe; Argon2's block memory is wiped through the `argon2` crate's
//! `zeroize` feature. This crate keeps its own temporaries in
//! [`zeroize::Zeroizing`] and does not claim more. Decrypted or decompressed
//! extent data is local plaintext and outside ADR-11's key protection (F11).

#![deny(unsafe_code)]

mod arena;
mod codec;
mod ct;
mod mac;
mod rng;
mod tripwire;

pub mod attach;
pub mod extent;
mod kdf;
pub mod keyring;
pub mod keys;
pub mod meta;
pub mod secret;
pub mod volume;

#[cfg(feature = "test-rng")]
pub mod testing;

pub use attach::{
    check_attach, descriptor_mac, kmk_fingerprint, seal_descriptor, verify_descriptor_mac,
    AttachError, AttachOk, AttachRequest, Credential,
};
pub use extent::{AesGcmExtents, PlainExtents};
pub use keyring::{
    Argon2Cost, KeyFile, KeyringError, NewSlot, Unlock, UnlockOptions, UnlockPolicy,
};
pub use keys::VolumeKeys;
pub use meta::{lease_is_stale, MetaSuite};
pub use secret::{
    arena_stats, require_locked, AddressingKey, ArenaStats, Dek, Ik, Kmk, MacKey, MetaKey, Root,
    Secret, TagKey,
};
pub use tripwire::ENABLED as TRIPWIRE_ENABLED;
pub use volume::VolumeCrypto;
