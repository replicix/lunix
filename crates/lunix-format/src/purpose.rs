//! Key-purpose table (freeze item 11, on-disk-formats.md section 4.2).
//!
//! This crate builds the derivation messages; the derivation itself
//! (keyed BLAKE3, or HKDF-SHA-256-Expand in the FIPS profile) is
//! `lunix-crypto`'s.

use crate::error::{FormatError, Result};
use crate::ids::{DekGen, FamilyId};
use crate::mode::{CryptoMode, ObjectKind};

/// `"lunix kdf v1\0"`.
pub const KDF_LABEL: &[u8; 13] = b"lunix kdf v1\0";

/// A key purpose and its id byte.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum PurposeId {
    /// Keyed `chunk_id` of data chunks.
    DataAddressing = 0x01,
    /// The DEK of a generation.
    Dek = 0x02,
    /// The metadata key of an object kind.
    Metadata = 0x03,
    /// HMAC key of the `volume` descriptor.
    DescriptorMac = 0x04,
    /// KMK or IK fingerprint (first 16 bytes).
    Fingerprint = 0x05,
    /// Keyed node hash.
    NodeAddressing = 0x06,
    /// HMAC key of the keyring `mac`.
    KeyringMac = 0x07,
    /// HMAC key of envelope form 0 `mac`.
    IntegrityHmac = 0x08,
    /// Key of the keyed BLAKE3-128 extent tag.
    ExtentTag = 0x09,
}

impl PurposeId {
    /// Every purpose, in id order.
    pub const ALL: [PurposeId; 9] = [
        Self::DataAddressing,
        Self::Dek,
        Self::Metadata,
        Self::DescriptorMac,
        Self::Fingerprint,
        Self::NodeAddressing,
        Self::KeyringMac,
        Self::IntegrityHmac,
        Self::ExtentTag,
    ];

    /// Parses an id byte; 0x00 and 0x0a ..= 0xff are reserved.
    pub fn from_u8(v: u8) -> Result<Self> {
        Self::ALL
            .into_iter()
            .find(|p| p.id() == v)
            .ok_or(FormatError::range("purpose", "id", u64::from(v)))
    }

    /// The id byte.
    pub fn id(self) -> u8 {
        self as u8
    }

    /// True if the purpose may be used in `mode` (a purpose is never used in
    /// a mode its row does not list; `Plain` has no key at all).
    pub fn allowed_in(self, mode: CryptoMode) -> bool {
        use CryptoMode::{Encrypted, Integrity};
        match self {
            Self::DataAddressing | Self::Dek | Self::Metadata | Self::NodeAddressing => {
                mode == Encrypted
            }
            Self::DescriptorMac | Self::Fingerprint | Self::KeyringMac => {
                mode == Encrypted || mode == Integrity
            }
            Self::IntegrityHmac | Self::ExtentTag => mode == Integrity,
        }
    }

    /// The table's name for the purpose.
    pub fn name(self) -> &'static str {
        match self {
            Self::DataAddressing => "data addressing key",
            Self::Dek => "DEK",
            Self::Metadata => "metadata key",
            Self::DescriptorMac => "descriptor MAC key",
            Self::Fingerprint => "KMK fingerprint",
            Self::NodeAddressing => "map-node addressing key",
            Self::KeyringMac => "keyring MAC key",
            Self::IntegrityHmac => "integrity HMAC key",
            Self::ExtentTag => "extent tag key",
        }
    }
}

/// A purpose with its arguments.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Purpose {
    /// Keyed `chunk_id` of data chunks.
    DataAddressing,
    /// `DEK_gen` for a generation.
    Dek(DekGen),
    /// Metadata key for an object kind (not `Keyring`).
    Metadata(ObjectKind),
    /// Descriptor MAC key.
    DescriptorMac,
    /// Fingerprint.
    Fingerprint,
    /// Node addressing key.
    NodeAddressing,
    /// Keyring MAC key.
    KeyringMac,
    /// Integrity HMAC key.
    IntegrityHmac,
    /// Extent tag key.
    ExtentTag,
}

impl Purpose {
    /// The purpose id.
    pub fn id(&self) -> PurposeId {
        match self {
            Self::DataAddressing => PurposeId::DataAddressing,
            Self::Dek(_) => PurposeId::Dek,
            Self::Metadata(_) => PurposeId::Metadata,
            Self::DescriptorMac => PurposeId::DescriptorMac,
            Self::Fingerprint => PurposeId::Fingerprint,
            Self::NodeAddressing => PurposeId::NodeAddressing,
            Self::KeyringMac => PurposeId::KeyringMac,
            Self::IntegrityHmac => PurposeId::IntegrityHmac,
            Self::ExtentTag => PurposeId::ExtentTag,
        }
    }

    /// The derivation message `"lunix kdf v1\0" ‖ family_id ‖ purpose ‖ args`.
    ///
    /// The keyring kind has no metadata key (it is never sealed) and is
    /// rejected.
    pub fn message(&self, family: &FamilyId) -> Result<Vec<u8>> {
        let mut m = Vec::with_capacity(13 + 16 + 1 + 4);
        m.extend_from_slice(KDF_LABEL);
        m.extend_from_slice(&family.0);
        m.push(self.id().id());
        match self {
            Self::Dek(g) => m.extend_from_slice(&g.0.to_le_bytes()),
            Self::Metadata(k) => {
                if *k == ObjectKind::Keyring {
                    return Err(FormatError::invalid(
                        "purpose",
                        "the keyring is never sealed",
                    ));
                }
                m.push(k.id());
            }
            _ => {}
        }
        Ok(m)
    }
}
