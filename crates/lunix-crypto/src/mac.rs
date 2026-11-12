//! HMAC-SHA-256 and the keyed addressing hash, through aws-lc-rs.

use aws_lc_rs::hmac;
use lunix_format::Profile;

/// HMAC-SHA-256 over the concatenation of `parts`.
pub(crate) fn hmac_sha256(key: &[u8; 32], parts: &[&[u8]]) -> [u8; 32] {
    let k = hmac::Key::new(hmac::HMAC_SHA256, key);
    let mut ctx = hmac::Context::with_key(&k);
    for p in parts {
        ctx.update(p);
    }
    let tag = ctx.sign();
    let mut out = [0u8; 32];
    out.copy_from_slice(tag.as_ref());
    out
}

/// The keyed chunk-id or node-hash function: keyed BLAKE3 in the default
/// profile, HMAC-SHA-256 in the FIPS profile.
pub(crate) fn addressing_hash(profile: Profile, key: &[u8; 32], data: &[u8]) -> [u8; 32] {
    match profile {
        Profile::Default => *blake3::keyed_hash(key, data).as_bytes(),
        Profile::Fips => hmac_sha256(key, &[data]),
    }
}
