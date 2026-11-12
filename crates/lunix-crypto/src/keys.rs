//! The keys of one attached volume.
//!
//! [`VolumeKeys`] derives, once at attach, every long-lived key the codecs
//! need from the family root and keeps them in the key arena. It is the
//! concrete type behind the opaque [`lunix_iface::crypto::Keys`] handle.

use std::any::Any;
use std::ops::Deref;
use std::sync::{Arc, Weak};

use lunix_format::purpose::Purpose;
use lunix_format::volume::VolumeDescriptor;
use lunix_format::{CryptoMode, DekGen, FamilyId, IdLen, ObjectKind, Profile, VolumeUuid};
use lunix_iface::crypto::{KeyMaterial, Keys};
use lunix_iface::{Error, Result};
use zeroize::Zeroizing;

use crate::kdf;
use crate::secret::{AddressingKey, Dek, Kmk, MacKey, MetaKey, Root, TagKey};

pub(crate) enum Material {
    /// No key material at all.
    Plain,
    Integrity {
        hmac: MacKey,
        tag: TagKey,
        /// Purpose 0x0a: the keyed chunk id of data chunks (amendment
        /// 2026-11-12). Node hashes stay unkeyed in this mode.
        addressing: AddressingKey,
    },
    Encrypted {
        /// Kept to derive the DEK of an older `dek_gen` on demand.
        kmk: Kmk,
        addressing: AddressingKey,
        node: AddressingKey,
        dek: Dek,
        meta: Vec<(ObjectKind, MetaKey)>,
    },
}

/// A borrowed or freshly derived DEK.
pub(crate) enum DekRef<'a> {
    Current(&'a Dek),
    Older(Zeroizing<[u8; 32]>),
}

impl DekRef<'_> {
    pub(crate) fn bytes(&self) -> &[u8; 32] {
        match self {
            Self::Current(d) => d.expose(),
            Self::Older(b) => b,
        }
    }
}

impl Deref for DekRef<'_> {
    type Target = [u8; 32];

    fn deref(&self) -> &[u8; 32] {
        self.bytes()
    }
}

/// Everything a volume's codecs need from its root. No `Debug`, `Clone` or
/// `Copy`; share it through the `Arc` it is created in.
pub struct VolumeKeys {
    me: Weak<VolumeKeys>,
    mode: CryptoMode,
    profile: Profile,
    volume: VolumeUuid,
    family: FamilyId,
    id_len: IdLen,
    dek_gen: DekGen,
    fingerprint: [u8; 16],
    material: Material,
}

impl VolumeKeys {
    /// Derives the keys of the volume `desc` describes from `root`.
    ///
    /// `root` is `None` exactly for a `plain` volume. `dek_gen` is the
    /// keyring's current generation (0 for `integrity`); extents of a higher
    /// generation are rejected by the codec.
    ///
    /// The descriptor MAC is **not** checked here: `check_attach` does that
    /// before it calls this.
    ///
    /// # Errors
    /// `Invalid` when the root does not fit the descriptor's mode.
    pub fn derive(
        desc: &VolumeDescriptor,
        root: Option<Root>,
        dek_gen: DekGen,
    ) -> Result<Arc<Self>> {
        let profile = desc.profile;
        let family = desc.family_id;
        let (material, fingerprint, dek_gen) = match (desc.crypto_mode, root) {
            (CryptoMode::Plain, None) => (Material::Plain, [0u8; 16], DekGen(0)),
            (CryptoMode::Integrity, Some(root @ Root::Ik(_))) => {
                let hmac = kdf::purpose_secret(profile, &root, &family, &Purpose::IntegrityHmac)?;
                let tag = kdf::purpose_secret(profile, &root, &family, &Purpose::ExtentTag)?;
                let addressing =
                    kdf::purpose_secret(profile, &root, &family, &Purpose::IntegrityAddressing)?;
                let fp = kdf::fingerprint(profile, &root, &family)?;
                (
                    Material::Integrity {
                        hmac: MacKey::from_secret(hmac),
                        tag: TagKey::from_secret(tag),
                        addressing: AddressingKey::from_secret(addressing),
                    },
                    fp,
                    DekGen(0),
                )
            }
            (CryptoMode::Encrypted, Some(root @ Root::Kmk(_))) => {
                let addressing =
                    kdf::purpose_secret(profile, &root, &family, &Purpose::DataAddressing)?;
                let node = kdf::purpose_secret(profile, &root, &family, &Purpose::NodeAddressing)?;
                let dek = kdf::purpose_secret(profile, &root, &family, &Purpose::Dek(dek_gen))?;
                let mut meta = Vec::with_capacity(ObjectKind::ALL.len());
                for kind in ObjectKind::ALL {
                    if kind == ObjectKind::Keyring {
                        continue;
                    }
                    let k = kdf::purpose_secret(profile, &root, &family, &Purpose::Metadata(kind))?;
                    meta.push((kind, MetaKey::from_secret(k)));
                }
                let fp = kdf::fingerprint(profile, &root, &family)?;
                let Root::Kmk(kmk) = root else {
                    return Err(Error::invalid("root does not fit the volume mode"));
                };
                (
                    Material::Encrypted {
                        kmk,
                        addressing: AddressingKey::from_secret(addressing),
                        node: AddressingKey::from_secret(node),
                        dek: Dek::from_secret(dek),
                        meta,
                    },
                    fp,
                    dek_gen,
                )
            }
            _ => return Err(Error::invalid("root does not fit the volume mode")),
        };
        Ok(Arc::new_cyclic(|me| Self {
            me: me.clone(),
            mode: desc.crypto_mode,
            profile,
            volume: desc.volume_uuid,
            family,
            id_len: desc.id_len,
            dek_gen,
            fingerprint,
            material,
        }))
    }

    /// Recovers the shared keys behind an opaque handle made by this crate.
    pub fn from_handle(keys: &Keys) -> Option<Arc<Self>> {
        keys.material()
            .as_any()
            .downcast_ref::<Self>()
            .and_then(|k| k.me.upgrade())
    }

    /// The opaque handle the engine passes around.
    pub fn handle(self: &Arc<Self>) -> Keys {
        Keys::new(Arc::clone(self) as Arc<dyn KeyMaterial>)
    }

    /// The volume mode.
    pub fn mode(&self) -> CryptoMode {
        self.mode
    }

    pub(crate) fn profile(&self) -> Profile {
        self.profile
    }

    pub(crate) fn volume(&self) -> &VolumeUuid {
        &self.volume
    }

    pub(crate) fn family(&self) -> &FamilyId {
        &self.family
    }

    pub(crate) fn id_len(&self) -> IdLen {
        self.id_len
    }

    /// The current DEK generation: new extents use it, older ones keep theirs.
    pub fn dek_gen(&self) -> DekGen {
        self.dek_gen
    }

    pub(crate) fn material(&self) -> &Material {
        &self.material
    }

    /// The DEK of `gen`: the cached current one, or derived on demand for an
    /// older generation. Never a generation above the keyring's.
    pub(crate) fn dek(&self, generation: DekGen) -> Result<DekRef<'_>> {
        let Material::Encrypted { kmk, dek, .. } = &self.material else {
            return Err(Error::invalid("volume has no DEK"));
        };
        if generation == self.dek_gen {
            return Ok(DekRef::Current(dek));
        }
        if generation > self.dek_gen {
            // Before authentication a forged header cannot be told from a
            // writer bug, so this is the same error as a failed tag.
            return Err(Error::Auth { what: "extent" });
        }
        let msg = Purpose::Dek(generation).message(&self.family)?;
        Ok(DekRef::Older(kdf::prf(self.profile, kmk.expose(), &msg)?))
    }

    /// The metadata key of `kind`.
    pub(crate) fn meta_key(&self, kind: ObjectKind) -> Result<&MetaKey> {
        let Material::Encrypted { meta, .. } = &self.material else {
            return Err(Error::invalid("volume has no metadata keys"));
        };
        meta.iter()
            .find(|(k, _)| *k == kind)
            .map(|(_, k)| k)
            .ok_or(Error::invalid("the keyring kind has no metadata key"))
    }

    /// For a test that checks the derived keys against the vectors.
    #[cfg(feature = "test-rng")]
    pub fn meta_key_for_tests(&self, kind: ObjectKind) -> Result<[u8; 32]> {
        Ok(self.meta_key(kind)?.expose_for_tests())
    }

    /// For a test that checks the derived keys against the vectors.
    #[cfg(feature = "test-rng")]
    pub fn hmac_key_for_tests(&self) -> Option<[u8; 32]> {
        match &self.material {
            Material::Integrity { hmac, .. } => Some(hmac.expose_for_tests()),
            _ => None,
        }
    }

    /// For a test that checks the derived keys against the vectors.
    #[cfg(feature = "test-rng")]
    pub fn tag_key_for_tests(&self) -> Option<[u8; 32]> {
        match &self.material {
            Material::Integrity { tag, .. } => Some(tag.expose_for_tests()),
            _ => None,
        }
    }

    /// For a test that checks the derived keys against the vectors.
    #[cfg(feature = "test-rng")]
    pub fn addressing_key_for_tests(&self, node: bool) -> Option<[u8; 32]> {
        match &self.material {
            Material::Integrity { addressing, .. } if !node => Some(addressing.expose_for_tests()),
            Material::Encrypted {
                addressing,
                node: n,
                ..
            } => Some(if node { n } else { addressing }.expose_for_tests()),
            _ => None,
        }
    }

    /// Keys for the known-answer vectors, whose DEK is given directly rather
    /// than derived from a KMK.
    #[cfg(feature = "test-rng")]
    pub fn for_vectors(
        profile: Profile,
        family: FamilyId,
        volume: VolumeUuid,
        id_len: IdLen,
        dek_gen: DekGen,
        dek: &[u8; 32],
    ) -> Arc<Self> {
        let filler = [0x5au8; 32];
        Arc::new_cyclic(|me| Self {
            me: me.clone(),
            mode: CryptoMode::Encrypted,
            profile,
            volume,
            family,
            id_len,
            dek_gen,
            fingerprint: [0; 16],
            material: Material::Encrypted {
                kmk: Kmk::from_bytes(&filler),
                addressing: AddressingKey::from_bytes(&filler),
                node: AddressingKey::from_bytes(&filler),
                dek: Dek::from_bytes(dek),
                meta: Vec::new(),
            },
        })
    }
}

impl KeyMaterial for VolumeKeys {
    fn mode(&self) -> CryptoMode {
        self.mode
    }

    fn fingerprint(&self) -> [u8; 16] {
        self.fingerprint
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
