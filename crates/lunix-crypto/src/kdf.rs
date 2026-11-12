//! The key hierarchy (ADR-11, on-disk-formats.md section 4.2).
//!
//! Every key is derived from the family root R by purpose:
//!
//! ```text
//! purpose_key(R, purpose, args) = keyed BLAKE3(R, "lunix kdf v1\0" ‖ family_id ‖ purpose ‖ args)   default profile
//!                               = HKDF-SHA-256-Expand(PRK = R, info = msg, L = 32)                  fips profile
//! ```
//!
//! The message is built by `lunix_format::purpose`; this module only runs the
//! function. A purpose is never derived from a root of a mode its table row
//! does not list (an `integrity` family has no data addressing key, and so on).
//! The per-extent key is `keyed BLAKE3(DEK_gen, info)` over the `info` of
//! `lunix_format::extent::info`, or HKDF-Expand over the same bytes in the
//! FIPS profile.

use aws_lc_rs::hkdf::{KeyType, Prk, HKDF_SHA256};
use lunix_format::purpose::Purpose;
use lunix_format::{FamilyId, Profile};
use lunix_iface::{Error, Result};
use zeroize::Zeroizing;

use crate::secret::{Root, Secret};

struct Len32;

impl KeyType for Len32 {
    fn len(&self) -> usize {
        32
    }
}

/// HKDF-SHA-256 Expand with `prk` used directly as the pseudo-random key.
pub(crate) fn hkdf_expand(prk: &[u8; 32], info: &[u8]) -> Result<Zeroizing<[u8; 32]>> {
    let prk = Prk::new_less_safe(HKDF_SHA256, prk);
    let infos = [info];
    let okm = prk
        .expand(&infos, Len32)
        .map_err(|_| Error::invalid("hkdf expand"))?;
    let mut out = Zeroizing::new([0u8; 32]);
    okm.fill(&mut out[..])
        .map_err(|_| Error::invalid("hkdf expand"))?;
    Ok(out)
}

/// `PRF(key, msg)` of the profile: keyed BLAKE3 or HKDF-Expand.
pub(crate) fn prf(profile: Profile, key: &[u8; 32], msg: &[u8]) -> Result<Zeroizing<[u8; 32]>> {
    match profile {
        Profile::Default => Ok(Zeroizing::new(*blake3::keyed_hash(key, msg).as_bytes())),
        Profile::Fips => hkdf_expand(key, msg),
    }
}

/// Derives the key of `purpose` from `root`.
///
/// # Errors
/// `Invalid` when the purpose is not used in the root's mode, or has no key
/// (the keyring kind).
pub(crate) fn purpose_key(
    profile: Profile,
    root: &Root,
    family: &FamilyId,
    purpose: &Purpose,
) -> Result<Zeroizing<[u8; 32]>> {
    if !purpose.id().allowed_in(root.mode()) {
        return Err(Error::invalid("purpose is not used in this volume mode"));
    }
    let msg = purpose.message(family)?;
    prf(profile, root.expose(), &msg)
}

/// Like [`purpose_key`], stored in the key arena.
pub(crate) fn purpose_secret(
    profile: Profile,
    root: &Root,
    family: &FamilyId,
    purpose: &Purpose,
) -> Result<Secret> {
    let k = purpose_key(profile, root, family, purpose)?;
    Ok(Secret::new(&k))
}

/// The fingerprint of a root: the first 16 bytes of purpose 0x05.
pub fn fingerprint(profile: Profile, root: &Root, family: &FamilyId) -> Result<[u8; 16]> {
    let k = purpose_key(profile, root, family, &Purpose::Fingerprint)?;
    let mut fp = [0u8; 16];
    fp.copy_from_slice(&k[..16]);
    Ok(fp)
}

/// The per-extent key from a DEK and the canonical `info`.
pub(crate) fn extent_key(
    profile: Profile,
    dek: &[u8; 32],
    info: &[u8],
) -> Result<Zeroizing<[u8; 32]>> {
    prf(profile, dek, info)
}
