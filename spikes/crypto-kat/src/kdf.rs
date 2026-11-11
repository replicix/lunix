//! Key derivation: the ADR-11 extent key and the purpose keys under the KMK,
//! plus the two proposed variants (`Hardened`, `Fips`).

use aws_lc_rs::hkdf::{KeyType, Prk, HKDF_SHA256};
use zeroize::Zeroizing;

pub const CTX_EXTENT_V0: &str = "lunix extent v0"; // first draft (chunk_id only), kept to show the hole
pub const CTX_EXTENT_V1: &str = "lunix extent v1"; // ADR-11 as amended 2026-11-11
pub const LABEL_EXTENT_V2: &[u8] = b"lunix extent v2\0"; // proposed
pub const LABEL_KDF_V1: &[u8] = b"lunix kdf v1\0"; // proposed purpose-key message prefix

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Profile {
    /// First draft: `derive_key("lunix extent v0", DEK || chunk_id)`, constant nonce.
    V0Draft,
    /// ADR-11 as written: `derive_key("lunix extent v1", DEK || chunk_id || salt)`.
    Adr,
    /// Proposal: keyed PRF form with family, kind, DEK epoch and encoding descriptor.
    Hardened,
    /// FIPS profile: HKDF-SHA-256 Expand with the same `info` as `Hardened`;
    /// AES-GCM nonce comes from the module (see `extent`).
    Fips,
}

/// What the extent writer knows about the encoding when it derives the key.
#[derive(Clone, Copy, Debug)]
pub struct EncDesc {
    pub codec: u8,
    pub level: i8,
    pub clen: u32,
}

/// Extent kinds: data chunk vs map node vs anything else sharing the layout.
pub const KIND_DATA: u8 = 0;
pub const KIND_MAP_NODE: u8 = 1;

#[derive(Clone, Copy)]
pub struct KeyCtx<'a> {
    pub dek: &'a [u8; 32],
    pub family: &'a [u8; 16],
    pub dek_epoch: u32,
    pub kind: u8,
}

struct Len(usize);
impl KeyType for Len {
    fn len(&self) -> usize {
        self.0
    }
}

/// The `info`/message used by the proposed profiles. Fixed-length fields first;
/// `chunk_id` is the only variable-length field and `id_len` is explicit, so the
/// encoding is injective even if a family ever mixes id lengths.
pub fn info_v2(ctx: &KeyCtx, id: &[u8], salt: &[u8; 24], enc: &EncDesc) -> Vec<u8> {
    let mut m = Vec::with_capacity(96 + id.len());
    m.extend_from_slice(LABEL_EXTENT_V2);
    m.extend_from_slice(ctx.family);
    m.push(ctx.kind);
    m.extend_from_slice(&ctx.dek_epoch.to_le_bytes());
    m.push(enc.codec);
    m.push(enc.level as u8);
    m.extend_from_slice(&enc.clen.to_le_bytes());
    m.push(id.len() as u8);
    m.extend_from_slice(id);
    m.extend_from_slice(salt);
    m
}

/// Precomputed context state for the ADR derivation (saves the context hash).
#[derive(Clone)]
pub struct AdrKdf {
    base: blake3::Hasher,
}

impl Default for AdrKdf {
    fn default() -> Self {
        Self::new()
    }
}

impl AdrKdf {
    pub fn new() -> Self {
        Self {
            base: blake3::Hasher::new_derive_key(CTX_EXTENT_V1),
        }
    }
    pub fn key(&self, dek: &[u8; 32], id: &[u8], salt: &[u8; 24]) -> Zeroizing<[u8; 32]> {
        let mut h = self.base.clone();
        h.update(dek);
        h.update(id);
        h.update(salt);
        Zeroizing::new(*h.finalize().as_bytes())
    }
}

pub fn extent_key(
    profile: Profile,
    ctx: &KeyCtx,
    id: &[u8],
    salt: &[u8; 24],
    enc: &EncDesc,
) -> Zeroizing<[u8; 32]> {
    match profile {
        Profile::V0Draft => {
            let mut m = Vec::with_capacity(32 + id.len());
            m.extend_from_slice(ctx.dek);
            m.extend_from_slice(id);
            Zeroizing::new(blake3::derive_key(CTX_EXTENT_V0, &m))
        }
        Profile::Adr => {
            let mut m = Vec::with_capacity(32 + id.len() + 24);
            m.extend_from_slice(ctx.dek);
            m.extend_from_slice(id);
            m.extend_from_slice(salt);
            Zeroizing::new(blake3::derive_key(CTX_EXTENT_V1, &m))
        }
        Profile::Hardened => {
            let info = info_v2(ctx, id, salt, enc);
            Zeroizing::new(*blake3::keyed_hash(ctx.dek, &info).as_bytes())
        }
        Profile::Fips => {
            let info = info_v2(ctx, id, salt, enc);
            hkdf_expand(ctx.dek, &info)
        }
    }
}

pub fn hkdf_expand(prk: &[u8; 32], info: &[u8]) -> Zeroizing<[u8; 32]> {
    let prk = Prk::new_less_safe(HKDF_SHA256, prk);
    let infos = [info];
    let okm = prk.expand(&infos, Len(32)).expect("hkdf expand");
    let mut out = Zeroizing::new([0u8; 32]);
    okm.fill(&mut out[..]).expect("hkdf fill");
    out
}

/// Keys derived from the KMK by purpose (ADR-11: "by purpose, using keyed BLAKE3").
#[derive(Clone, Copy, Debug)]
pub enum Purpose {
    Addressing,
    Dek {
        epoch: u32,
    },
    /// Per-object-kind AEAD key for metadata (commit, lease, map pack, heat, ...).
    Meta {
        kind: u8,
    },
    VolumeMac,
    Pin,
}

pub const META_COMMIT: u8 = 1;
pub const META_LEASE: u8 = 2;
pub const META_DELTA: u8 = 3;
pub const META_MAPPACK: u8 = 4;
pub const META_HEAT: u8 = 5;

fn purpose_msg(family: &[u8; 16], p: Purpose) -> Vec<u8> {
    let mut m = Vec::with_capacity(40);
    m.extend_from_slice(LABEL_KDF_V1);
    m.extend_from_slice(family);
    match p {
        Purpose::Addressing => m.push(0x01),
        Purpose::Dek { epoch } => {
            m.push(0x02);
            m.extend_from_slice(&epoch.to_le_bytes());
        }
        Purpose::Meta { kind } => {
            m.push(0x03);
            m.push(kind);
        }
        Purpose::VolumeMac => m.push(0x04),
        Purpose::Pin => m.push(0x05),
    }
    m
}

pub fn purpose_key(kmk: &[u8; 32], family: &[u8; 16], p: Purpose) -> Zeroizing<[u8; 32]> {
    Zeroizing::new(*blake3::keyed_hash(kmk, &purpose_msg(family, p)).as_bytes())
}

pub fn purpose_key_fips(kmk: &[u8; 32], family: &[u8; 16], p: Purpose) -> Zeroizing<[u8; 32]> {
    hkdf_expand(kmk, &purpose_msg(family, p))
}
