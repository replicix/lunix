//! Metadata sealing, commits and the lease (ADR-11, on-disk-formats.md
//! sections 9, 10 and 13).
//!
//! One [`MetaSuite`] serves all three volume modes:
//!
//! | mode | envelope | protection |
//! |---|---|---|
//! | `encrypted` | form 1 | XChaCha20-Poly1305, random 192-bit nonce, one key per object kind, AAD `meta_aad(volume, kind, name)` |
//! | `integrity` | form 0 | HMAC-SHA-256 over `meta_aad ‖ envelope[0 .. 12 + body_len]` under the integrity HMAC key |
//! | `plain` | form 0 | the same input under unkeyed BLAKE3-256 (a checksum, corruption detection only) |
//!
//! `name` is always the **bucket-relative** object name (D9), so the bytes of
//! a local object upload verbatim. A commit's name is `commits/<seq:016x>`; the
//! envelope kind is 1 for FULL, EPOCH, CONFIG and TRANSFER and 3 for DELTA.

use std::sync::Arc;

use chacha20poly1305::aead::{Aead, Payload};
use chacha20poly1305::{KeyInit, XChaCha20Poly1305, XNonce};
use lunix_format::commit::{self, Commit};
use lunix_format::envelope::{self, Envelope, EnvelopeBody, MAC_LEN, NONCE_LEN, TAG_LEN};
use lunix_format::lease::Lease;
use lunix_format::{paths, CommitSeq, CryptoMode, Epoch, Form, ObjectKind, Profile};
use lunix_iface::crypto::{CommitCodec, MetaCodec, SealedCommit};
use lunix_iface::{Error, Result};

use crate::keys::{Material, VolumeKeys};
use crate::rng::Source;
use crate::{ct, mac};

const WHAT: &str = "metadata object";

fn auth() -> Error {
    Error::Auth { what: WHAT }
}

fn xnonce(nonce: &[u8; NONCE_LEN]) -> Result<XNonce> {
    XNonce::try_from(&nonce[..]).map_err(|_| Error::invalid("nonce length"))
}

/// Seals, opens and authenticates metadata objects and commits.
pub struct MetaSuite {
    keys: Arc<VolumeKeys>,
    rng: Source,
}

impl MetaSuite {
    /// A suite for the volume `keys` belong to.
    pub fn new(keys: Arc<VolumeKeys>) -> Self {
        Self {
            keys,
            rng: Source::Os,
        }
    }

    /// Like [`Self::new`] with an injected nonce source (feature `test-rng`).
    #[cfg(feature = "test-rng")]
    pub fn with_rng(keys: Arc<VolumeKeys>, rng: Arc<dyn crate::rng::Rng>) -> Self {
        Self {
            keys,
            rng: Source::Custom(rng),
        }
    }

    fn form(&self) -> Form {
        self.keys.mode().envelope_form()
    }

    fn check_profile(&self) -> Result<()> {
        // The FIPS AES-GCM form of metadata sealing is reserved (form 2).
        if self.keys.profile() == Profile::Fips {
            return Err(Error::Unsupported {
                what: "metadata sealing under the FIPS profile",
            });
        }
        Ok(())
    }

    /// The value form 0 stores: HMAC-SHA-256 (`integrity`) or BLAKE3-256
    /// (`plain`) of `aad ‖ signed`.
    fn form0_mac(&self, aad: &[u8], signed: &[u8]) -> [u8; MAC_LEN] {
        match self.keys.material() {
            Material::Integrity { hmac, .. } => mac::hmac_sha256(hmac.expose(), &[aad, signed]),
            _ => {
                let mut h = blake3::Hasher::new();
                h.update(aad);
                h.update(signed);
                *h.finalize().as_bytes()
            }
        }
    }

    fn seal_form0(&self, kind: ObjectKind, name: &str, body: &[u8]) -> Result<Vec<u8>> {
        let aad = envelope::meta_aad(&self.keys.volume().0, kind, name)?;
        let mut object = envelope::form0_unsigned(kind, body)?;
        let mac = self.form0_mac(&aad, &object);
        object.extend_from_slice(&mac);
        Ok(object)
    }

    fn seal_form1(&self, kind: ObjectKind, name: &str, body: &[u8]) -> Result<Vec<u8>> {
        let aad = envelope::meta_aad(&self.keys.volume().0, kind, name)?;
        let key = self.keys.meta_key(kind)?;
        let mut nonce = [0u8; NONCE_LEN];
        self.rng.fill(&mut nonce)?;
        let cipher = XChaCha20Poly1305::new(key.expose().into());
        let sealed = cipher
            .encrypt(
                &xnonce(&nonce)?,
                Payload {
                    msg: body,
                    aad: &aad,
                },
            )
            .map_err(|_| Error::invalid("metadata body cannot be sealed"))?;
        let (ct_bytes, tag) = sealed.split_at(sealed.len().saturating_sub(TAG_LEN));
        let tag: [u8; TAG_LEN] = tag
            .try_into()
            .map_err(|_| Error::invalid("metadata tag length"))?;
        Ok(envelope::encode_form1(kind, &nonce, ct_bytes, &tag)?)
    }

    fn open_env(&self, env: &Envelope<'_>, kind: ObjectKind, name: &str) -> Result<Vec<u8>> {
        let aad = envelope::meta_aad(&self.keys.volume().0, kind, name)?;
        match &env.body {
            EnvelopeBody::Mac { body, mac } => {
                if !ct::eq(&self.form0_mac(&aad, env.signed), mac) {
                    return Err(auth());
                }
                Ok(body.to_vec())
            }
            EnvelopeBody::Sealed {
                nonce,
                ciphertext,
                tag,
            } => {
                let key = self.keys.meta_key(kind)?;
                let cipher = XChaCha20Poly1305::new(key.expose().into());
                let mut joined = Vec::with_capacity(ciphertext.len() + TAG_LEN);
                joined.extend_from_slice(ciphertext);
                joined.extend_from_slice(tag);
                cipher
                    .decrypt(
                        &xnonce(nonce)?,
                        Payload {
                            msg: &joined,
                            aad: &aad,
                        },
                    )
                    .map_err(|_| auth())
            }
        }
    }

    /// Seals a lease (`name` is `lease`, kind 2). The body carries its `epoch`
    /// inside the AEAD or MAC (ADR-11 F12).
    ///
    /// # Errors
    /// `Format` when the lease does not encode.
    pub fn seal_lease(&self, lease: &Lease) -> Result<Vec<u8>> {
        let mut body = Vec::new();
        lease.encode(&mut body)?;
        self.seal(ObjectKind::Lease, &paths::lease(), &body)
    }

    /// Opens a lease. Use [`Self::open_lease_fresh`] unless the caller checks
    /// the epoch itself.
    ///
    /// # Errors
    /// `Auth` for a bad MAC or AEAD, `Format` for a bad body.
    pub fn open_lease(&self, bytes: &[u8]) -> Result<Lease> {
        let body = self.open(ObjectKind::Lease, &paths::lease(), bytes)?;
        Ok(Lease::decode(&body)?)
    }

    /// Opens a lease and rejects one whose epoch is below the newest verified
    /// epoch: an older sealed version of the same mutable object would
    /// otherwise pass the AEAD (ADR-11 F12).
    ///
    /// # Errors
    /// As [`Self::open_lease`], and `Refused` for a stale lease.
    pub fn open_lease_fresh(&self, bytes: &[u8], max_epoch_seen: Epoch) -> Result<Lease> {
        let lease = self.open_lease(bytes)?;
        if lease_is_stale(&lease, max_epoch_seen) {
            return Err(Error::Refused {
                why: "lease epoch is below the newest verified epoch",
            });
        }
        Ok(lease)
    }
}

/// True if `lease` is a replay: its epoch is below the newest epoch seen in
/// the verified chain or in local state, whatever its AEAD or MAC says.
pub fn lease_is_stale(lease: &Lease, max_epoch_seen: Epoch) -> bool {
    lease.epoch < max_epoch_seen
}

impl MetaCodec for MetaSuite {
    fn mode(&self) -> CryptoMode {
        self.keys.mode()
    }

    fn seal(&self, kind: ObjectKind, name: &str, body: &[u8]) -> Result<Vec<u8>> {
        if kind == ObjectKind::Keyring {
            return Err(Error::invalid("the keyring is never an envelope"));
        }
        self.check_profile()?;
        match self.form() {
            Form::Mac => self.seal_form0(kind, name, body),
            Form::Sealed => self.seal_form1(kind, name, body),
        }
    }

    fn open(&self, kind: ObjectKind, name: &str, bytes: &[u8]) -> Result<Vec<u8>> {
        if kind == ObjectKind::Keyring {
            return Err(Error::invalid("the keyring is never an envelope"));
        }
        self.check_profile()?;
        let env = Envelope::decode(bytes, &[kind], self.form(), kind.max_body_len())?;
        self.open_env(&env, kind, name)
    }
}

impl CommitCodec for MetaSuite {
    fn seal_commit(&self, commit: &Commit) -> Result<SealedCommit> {
        let name = paths::commit(commit.common().commit_seq);
        let kind = commit.object_kind();
        let mut body = Vec::new();
        commit.encode_body(&mut body)?;
        let bytes = self.seal(kind, &name, &body)?;
        let hash = self.object_hash(&bytes);
        Ok(SealedCommit { bytes, hash })
    }

    fn open_commit(&self, seq: CommitSeq, bytes: &[u8]) -> Result<Commit> {
        self.check_profile()?;
        let env = commit::decode_envelope(bytes, self.form())?;
        // Authenticate the exact bytes first; only then parse the body.
        let body = self.open_env(&env, env.kind, &paths::commit(seq))?;
        let commit = Commit::decode_body(env.kind, &body)?;
        if commit.common().commit_seq != seq {
            return Err(Error::Corrupt {
                what: "commit",
                detail: "wrong commit_seq",
            });
        }
        Ok(commit)
    }

    fn object_hash(&self, bytes: &[u8]) -> [u8; 32] {
        *blake3::hash(bytes).as_bytes()
    }
}
