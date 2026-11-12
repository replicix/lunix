//! The descriptor MAC and the attach downgrade check (ADR-11 F7,
//! on-disk-formats.md section 11, `docs/reference/attach-matrix.md`).
//!
//! The attach request, not the bucket, decides whether a volume must be
//! encrypted: key material means "encrypted", an integrity credential means
//! "integrity", and nothing means the caller expects a `plain` volume. The
//! rules are applied in the order the matrix gives (its "check order"); the
//! matrix file is the expected table of the tests, this module does not derive
//! it from anything else.

use lunix_format::local::Pin;
use lunix_format::purpose::Purpose;
use lunix_format::volume::VolumeDescriptor;
use lunix_format::{CryptoMode, DekGen, FormatError};
use lunix_iface::{Error, Result};

use crate::keyring::{self, KeyringError, Unlock};
use crate::secret::Root;
use crate::volume::VolumeCrypto;
use crate::{ct, kdf, mac};

/// Why an attach was refused. The variants are the matrix's results plus the
/// failures it leaves outside (`WrongCredential`, `KeyringRollback`).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AttachError {
    /// The request or the pin expects more protection than the descriptor has.
    #[error("downgrade: the volume is less protected than the request expects")]
    Downgrade,
    /// The volume's root differs from the supplied fingerprint or the pin.
    #[error("the volume's key is not the expected one")]
    WrongKmk,
    /// The descriptor MAC (or checksum) does not verify.
    #[error("the volume descriptor does not authenticate")]
    BadMac,
    /// The keyring is older than one already seen.
    #[error("the keyring is older than one already seen")]
    KeyringRollback,
    /// The descriptor is keyed and no credential was supplied.
    #[error("the volume needs a credential")]
    NeedCredential,
    /// An integrity credential was supplied for an encrypted volume.
    #[error("the credential declares another volume mode")]
    ModeMismatch,
    /// The credential unwraps no slot (also: a missing key file, a damaged
    /// keyring). This is the "WrongKmk class" of a key that does not fit.
    #[error("the credential does not unlock the volume")]
    WrongCredential,
    /// A feature phase 1 does not implement (KMS slots).
    #[error("unsupported: {0}")]
    Unsupported(&'static str),
    /// The keyring or the descriptor is not a valid structure.
    #[error("invalid structure: {0}")]
    Format(#[from] FormatError),
    /// An internal failure (for example the random source).
    #[error("internal error: {0}")]
    Internal(String),
}

impl AttachError {
    /// True for the errors that say "this is not the key of this volume":
    /// `WrongKmk` and `WrongCredential`.
    pub fn is_wrong_key_class(&self) -> bool {
        matches!(self, Self::WrongKmk | Self::WrongCredential)
    }
}

impl From<AttachError> for Error {
    fn from(e: AttachError) -> Self {
        match e {
            AttachError::Downgrade => Error::Refused {
                why: "downgrade: the volume is less protected than the request expects",
            },
            AttachError::NeedCredential => Error::Refused {
                why: "the volume needs a credential",
            },
            AttachError::ModeMismatch => Error::Refused {
                why: "the credential declares another volume mode",
            },
            AttachError::KeyringRollback => Error::Refused {
                why: "the keyring is older than one already seen",
            },
            AttachError::WrongKmk | AttachError::WrongCredential => {
                Error::Auth { what: "volume key" }
            }
            AttachError::BadMac => Error::Auth {
                what: "volume descriptor",
            },
            AttachError::Unsupported(what) => Error::Unsupported { what },
            AttachError::Format(f) => Error::Format(f),
            AttachError::Internal(_) => Error::Invalid {
                why: "internal error",
            },
        }
    }
}

fn from_keyring(e: KeyringError) -> AttachError {
    match e {
        KeyringError::Rollback => AttachError::KeyringRollback,
        KeyringError::Format(f) => AttachError::Format(f),
        KeyringError::Unsupported(w) => AttachError::Unsupported(w),
        KeyringError::Io(e) => AttachError::Internal(e.kind().to_string()),
        KeyringError::Invalid(w) => AttachError::Internal(w.to_owned()),
        KeyringError::WrongCredential | KeyringError::BelowFloor => AttachError::WrongCredential,
    }
}

fn internal(e: Error) -> AttachError {
    match e {
        Error::Format(f) => AttachError::Format(f),
        other => AttachError::Internal(format!("{:?}", other.kind())),
    }
}

/// The descriptor MAC of `desc`: HMAC-SHA-256 under the descriptor MAC key
/// (`integrity`, `encrypted`) or the unkeyed BLAKE3-256 checksum (`plain`),
/// over `"lunix volume v1\0" ‖ every byte before mac`.
///
/// # Errors
/// `Invalid` when `root` does not fit the descriptor's mode.
pub fn descriptor_mac(desc: &VolumeDescriptor, root: Option<&Root>) -> Result<[u8; 32]> {
    let input = desc.mac_input()?;
    match (desc.crypto_mode, root) {
        (CryptoMode::Plain, None) => Ok(*blake3::hash(&input).as_bytes()),
        (mode, Some(root)) if mode != CryptoMode::Plain && root.mode() == mode => {
            let key =
                kdf::purpose_key(desc.profile, root, &desc.family_id, &Purpose::DescriptorMac)?;
            Ok(mac::hmac_sha256(&key, &[&input]))
        }
        _ => Err(Error::invalid("root does not fit the volume mode")),
    }
}

/// The KMK (or integrity-key) fingerprint the descriptor stores.
pub fn kmk_fingerprint(desc: &VolumeDescriptor, root: &Root) -> Result<[u8; 16]> {
    kdf::fingerprint(desc.profile, root, &desc.family_id)
}

/// Fills in `kmk_fingerprint` and `mac` of a new descriptor and returns its
/// encoding, the exact bytes stored as `volume`.
pub fn seal_descriptor(desc: &mut VolumeDescriptor, root: Option<&Root>) -> Result<Vec<u8>> {
    desc.kmk_fingerprint = match root {
        Some(r) => kmk_fingerprint(desc, r)?,
        None => [0; 16],
    };
    desc.mac = descriptor_mac(desc, root)?;
    let mut out = Vec::new();
    desc.encode(&mut out)?;
    Ok(out)
}

/// True if the descriptor's stored MAC (or checksum) is right under `root`.
pub fn verify_descriptor_mac(desc: &VolumeDescriptor, root: Option<&Root>) -> bool {
    descriptor_mac(desc, root).is_ok_and(|m| ct::eq(&m, &desc.mac))
}

/// What the attach request supplies.
pub enum Credential<'a> {
    /// Key material (passphrase, key file, KMS reference): the volume must be
    /// `encrypted`.
    Key(Unlock<'a>),
    /// The credential of an integrity-only volume: the volume must be
    /// `integrity` and its MAC must verify under the integrity key.
    Integrity(Unlock<'a>),
}

/// The inputs of the attach check besides the descriptor and the keyring.
#[derive(Default)]
pub struct AttachRequest<'a> {
    /// The credential, if any.
    pub credential: Option<Credential<'a>>,
    /// The fingerprint the orchestrator supplies (StorageClass,
    /// `LunixBackendSet`, Secret).
    pub fingerprint: Option<[u8; 16]>,
    /// The local trust-on-first-use pin (interactive CLI).
    pub pin: Option<Pin>,
    /// The highest `keyring_seq` seen (from `state/pins`); 0 when unknown.
    pub keyring_seq_seen: u64,
}

/// A successful attach.
pub struct AttachOk {
    /// The unlocked codecs.
    pub crypto: VolumeCrypto,
    /// `Some` when no pin existed: the pin a caller with a pin store records
    /// (the matrix's `Ok+pin`); `None` for plain `Ok`.
    pub record_pin: Option<Pin>,
    /// The `keyring_seq` to record as seen (`None` for a `plain` volume).
    pub keyring_seq: Option<u64>,
}

/// Runs the attach check. `keyring` is the stored keyring object (absent for
/// a `plain` volume).
///
/// # Errors
/// An [`AttachError`] as `docs/reference/attach-matrix.md` prescribes.
pub fn check_attach(
    desc: &VolumeDescriptor,
    keyring: Option<&[u8]>,
    req: &AttachRequest<'_>,
) -> std::result::Result<AttachOk, AttachError> {
    let mode = desc.crypto_mode;

    // Rule 1: the request decides the mode.
    let unlock = match (&req.credential, mode) {
        (Some(Credential::Key(u)), CryptoMode::Encrypted)
        | (Some(Credential::Integrity(u)), CryptoMode::Integrity) => Some(u),
        (Some(Credential::Key(_)), _) | (Some(Credential::Integrity(_)), CryptoMode::Plain) => {
            return Err(AttachError::Downgrade)
        }
        (Some(Credential::Integrity(_)), CryptoMode::Encrypted) => {
            return Err(AttachError::ModeMismatch)
        }
        (None, CryptoMode::Encrypted | CryptoMode::Integrity) => {
            return Err(AttachError::NeedCredential)
        }
        (None, CryptoMode::Plain) => None,
    };

    // Rule 2: a descriptor weaker than expected.
    if mode == CryptoMode::Plain && req.fingerprint.is_some() {
        return Err(AttachError::Downgrade);
    }
    if req.pin.is_some_and(|p| p.mode > mode) {
        return Err(AttachError::Downgrade);
    }

    // Rule 3: unwrap a slot and verify the keyring.
    let (root, dek_gen, keyring_seq) = match unlock {
        None => (None, DekGen(0), None),
        Some(u) => {
            let bytes = keyring.ok_or(AttachError::WrongCredential)?;
            let unlocked = keyring::unlock(
                bytes,
                desc.profile,
                &desc.family_id,
                u,
                req.keyring_seq_seen,
                None,
            )
            .map_err(from_keyring)?;
            if unlocked.root.mode() != mode {
                return Err(AttachError::WrongCredential);
            }
            (
                Some(unlocked.root),
                unlocked.keyring.dek_gen,
                Some(unlocked.keyring.keyring_seq),
            )
        }
    };

    // Rule 4: the descriptor MAC (the checksum, for plain) and the stored
    // fingerprint it covers.
    let derived_fp = match &root {
        Some(r) => kmk_fingerprint(desc, r).map_err(internal)?,
        None => [0; 16],
    };
    if !verify_descriptor_mac(desc, root.as_ref()) || !ct::eq(&desc.kmk_fingerprint, &derived_fp) {
        return Err(AttachError::BadMac);
    }

    // Rules 5 and 6 only apply to keyed modes: a plain volume has no
    // fingerprint (rule 2a already refused a supplied one).
    if mode != CryptoMode::Plain {
        if req.fingerprint.is_some_and(|fp| !ct::eq(&fp, &derived_fp)) {
            return Err(AttachError::WrongKmk);
        }
        if req
            .pin
            .is_some_and(|p| p.mode != mode || !ct::eq(&p.fingerprint, &derived_fp))
        {
            return Err(AttachError::WrongKmk);
        }
    }

    // Rule 7.
    let record_pin = req.pin.is_none().then_some(Pin {
        mode,
        fingerprint: derived_fp,
    });
    let crypto = VolumeCrypto::new(desc, root, dek_gen).map_err(internal)?;
    Ok(AttachOk {
        crypto,
        record_pin,
        keyring_seq,
    })
}
