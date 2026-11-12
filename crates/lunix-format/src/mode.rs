//! Crypto modes, profiles and the sealed-object kind table
//! (on-disk-formats.md sections 4.1 and 4.3).

use crate::error::{FormatError, Result};

const WHAT: &str = "mode";

/// Extent overhead (header, `chunk_id`, `clen`, tag) of plain and integrity
/// volumes: 52 + 16.
pub const OVH_UNENCRYPTED: u32 = 68;
/// Extent overhead of encrypted volumes: 92 + 16.
pub const OVH_ENCRYPTED: u32 = 108;

/// The volume's crypto mode, from the descriptor.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum CryptoMode {
    /// Unkeyed BLAKE3 checksums: corruption detection only.
    Plain = 0,
    /// Keyed tags and HMAC over metadata.
    Integrity = 1,
    /// AEAD extents and sealed metadata.
    Encrypted = 2,
}

impl CryptoMode {
    /// Parses the descriptor byte.
    pub fn from_u8(v: u8) -> Result<Self> {
        match v {
            0 => Ok(Self::Plain),
            1 => Ok(Self::Integrity),
            2 => Ok(Self::Encrypted),
            _ => Err(FormatError::range(WHAT, "crypto_mode", u64::from(v))),
        }
    }

    /// True when the family has a root secret (a keyring).
    pub fn is_keyed(self) -> bool {
        self != Self::Plain
    }

    /// The extent overhead `OVH` of this mode.
    pub fn extent_ovh(self) -> u32 {
        match self {
            Self::Encrypted => OVH_ENCRYPTED,
            _ => OVH_UNENCRYPTED,
        }
    }

    /// The metadata envelope form this mode requires.
    pub fn envelope_form(self) -> Form {
        match self {
            Self::Encrypted => Form::Sealed,
            _ => Form::Mac,
        }
    }
}

/// The key-derivation profile.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum Profile {
    /// BLAKE3 derivation, AES-256-GCM with a zero nonce.
    Default = 0,
    /// HMAC/HKDF-SHA-256 derivation (legal only with `Encrypted`).
    Fips = 1,
}

impl Profile {
    /// Parses the descriptor byte.
    pub fn from_u8(v: u8) -> Result<Self> {
        match v {
            0 => Ok(Self::Default),
            1 => Ok(Self::Fips),
            _ => Err(FormatError::range(WHAT, "profile", u64::from(v))),
        }
    }
}

/// Envelope `form` byte.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum Form {
    /// Plain body followed by a 32-byte `mac`.
    Mac = 0,
    /// XChaCha20-Poly1305 sealed body.
    Sealed = 1,
}

impl Form {
    /// Parses the envelope byte; form 2 is reserved and rejected in v1.
    pub fn from_u8(v: u8) -> Result<Self> {
        match v {
            0 => Ok(Self::Mac),
            1 => Ok(Self::Sealed),
            _ => Err(FormatError::range(WHAT, "form", u64::from(v))),
        }
    }
}

/// Metadata object kind: the `kind` byte of the envelope, the AAD and the
/// metadata-key purpose argument.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum ObjectKind {
    /// `commits/<n>` of kind FULL, EPOCH, CONFIG, TRANSFER.
    Commit = 1,
    /// `lease`.
    Lease = 2,
    /// `commits/<n>` of kind DELTA.
    Delta = 3,
    /// `heat/<n>`.
    Heat = 5,
    /// The footer of a segment or map pack.
    Footer = 6,
    /// `gc/<pass>`.
    Gc = 7,
    /// `readers/<host>-<id>`.
    Readers = 8,
    /// `holds/<child-uuid>`.
    Holds = 9,
    /// `snapshots/<name>`.
    Snapshots = 10,
    /// `dicts/<id>`.
    Dicts = 11,
    /// `gc/space/<n>`.
    Space = 12,
    /// `families/<id>/keyring` (only in the keyring MAC input).
    Keyring = 13,
}

impl ObjectKind {
    /// Every defined kind, in id order.
    pub const ALL: [ObjectKind; 12] = [
        Self::Commit,
        Self::Lease,
        Self::Delta,
        Self::Heat,
        Self::Footer,
        Self::Gc,
        Self::Readers,
        Self::Holds,
        Self::Snapshots,
        Self::Dicts,
        Self::Space,
        Self::Keyring,
    ];

    /// Parses a kind byte. 0, 4 and 14 ..= 255 are reserved and rejected.
    pub fn from_u8(v: u8) -> Result<Self> {
        Self::ALL
            .into_iter()
            .find(|k| k.id() == v)
            .ok_or(FormatError::range(WHAT, "object kind", u64::from(v)))
    }

    /// The kind byte.
    pub fn id(self) -> u8 {
        self as u8
    }

    /// The table's short name.
    pub fn name(self) -> &'static str {
        match self {
            Self::Commit => "commit",
            Self::Lease => "lease",
            Self::Delta => "delta",
            Self::Heat => "heat",
            Self::Footer => "footer",
            Self::Gc => "gc",
            Self::Readers => "readers",
            Self::Holds => "holds",
            Self::Snapshots => "snapshots",
            Self::Dicts => "dicts",
            Self::Space => "space",
            Self::Keyring => "keyring",
        }
    }

    /// Maximum envelope `body_len` for this kind (section 9.1). The footer
    /// bound depends on the object and is checked by the segment decoder;
    /// here it is the loosest value, the opaque-body maximum.
    pub fn max_body_len(self) -> u32 {
        const MIB: u32 = 1 << 20;
        match self {
            Self::Commit => 4 * MIB,
            Self::Delta => 68 * MIB,
            Self::Lease | Self::Readers | Self::Holds | Self::Snapshots => 4096,
            Self::Footer | Self::Heat | Self::Gc | Self::Dicts | Self::Space => 64 * MIB + 8,
            Self::Keyring => 0,
        }
    }
}
