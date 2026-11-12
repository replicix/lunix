//! Extent sealing and opening (ADR-11, on-disk-formats.md section 6).
//!
//! * [`AesGcmExtents`] is the `encrypted` volume: a keyed chunk id, a
//!   per-extent key `keyed BLAKE3(DEK_gen, info)` over the canonical `info`
//!   with a **fresh random salt drawn inside every `seal`**, AES-256-GCM with
//!   the all-zero nonce through aws-lc-rs, and the canonical AAD. There is no
//!   salt setter and no API that takes a salt or a nonce.
//! * [`PlainExtents`] is the `integrity` and `plain` volume: no cipher, the
//!   16-byte tag is BLAKE3-128 over every byte of the extent before it, keyed
//!   with the extent tag key in `integrity` mode and unkeyed in `plain` mode
//!   (D11).
//!
//! # Open order
//!
//! Encrypted: header sanity (magic, version, known codec, structure, the
//! `chunk_id` the caller expects, a `dek_gen` the keyring can derive), the
//! AEAD tag, `ulen` against the volume's chunk size (never trusted from the
//! header), bounded decompression, and last the chunk-id recomputation (the
//! key commitment) when `verify_id` is set. Unencrypted: the tag first, then
//! the rest. A failed tag, AEAD or id check is the same
//! [`Error::Auth`](lunix_iface::Error::Auth), so the order is not observable
//! from the error.

use std::sync::Arc;

use aws_lc_rs::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM};
use lunix_format::extent::{
    self, tag_input, Cch1Header, CryptoRegion, DecodeParams, Extent, ExtentLayout, InfoParts,
    ENCRYPTED_PREFIX_LEN, MAX_NODE_ULEN, MAX_ULEN, TAG_LEN,
};
use lunix_format::{ChunkId, Codec, CryptoMode, ExtentKind, Profile};
use lunix_iface::crypto::{EncodingDesc, ExtentCodec, OpenExpect, OpenedMeta};
use lunix_iface::{Error, Result};

use crate::codec::{finish, plaintext_cap, rollback, Opened};
use crate::keys::{Material, VolumeKeys};
use crate::rng::Source;
use crate::tripwire::Tripwire;
use crate::{ct, kdf, mac};

const WHAT: &str = "extent";
const ZERO_NONCE: [u8; 12] = [0; 12];

fn auth() -> Error {
    Error::Auth { what: WHAT }
}

fn check_desc(kind: ExtentKind, desc: &EncodingDesc) -> Result<()> {
    desc.level.validate(desc.codec)?;
    if desc.dict_id.is_some() && desc.codec != Codec::ZstdDict {
        return Err(Error::invalid("dict_id only with codec 3"));
    }
    let cap = match kind {
        ExtentKind::Data => MAX_ULEN,
        ExtentKind::Node => MAX_NODE_ULEN,
    };
    if desc.ulen == 0 || u64::from(desc.ulen) > cap {
        return Err(Error::invalid("ulen outside the extent kind's range"));
    }
    Ok(())
}

fn check_expected(kind: ExtentKind, expect: &OpenExpect<'_>, id: &ChunkId) -> Result<()> {
    if let Some(want) = expect.chunk_id {
        if !ct::eq(&want.0, &id.0) {
            return Err(auth());
        }
    }
    if let Some(prefix) = expect.id_prefix {
        if !prefix.matches(id) {
            return Err(auth());
        }
    }
    // A node's cap is fixed; only a data extent depends on the volume's size.
    if kind == ExtentKind::Data
        && (expect.chunk_size == 0 || expect.chunk_size > lunix_format::volume::MAX_CHUNK_SIZE)
    {
        return Err(Error::invalid("chunk_size outside 16 KiB ..= 1 MiB"));
    }
    Ok(())
}

fn meta_of(h: &Cch1Header, id: ChunkId) -> Result<OpenedMeta> {
    Ok(OpenedMeta {
        desc: EncodingDesc {
            codec: h.codec,
            level: h.level,
            ulen: u32::try_from(h.ulen).map_err(|_| Error::invalid("ulen"))?,
            dict_id: None,
        },
        chunk_id: id,
    })
}

/// Extents of an `encrypted` volume.
pub struct AesGcmExtents {
    keys: Arc<VolumeKeys>,
    rng: Source,
    ledger: Tripwire,
    /// Test vectors of the FIPS derivation seal with the zero nonce; a real
    /// FIPS seal is `Unsupported` in phase 1.
    #[cfg(feature = "test-rng")]
    fips_vectors: bool,
}

impl AesGcmExtents {
    /// Extents for the volume `keys` belong to.
    ///
    /// # Errors
    /// `Invalid` unless the volume is `encrypted`.
    pub fn new(keys: Arc<VolumeKeys>) -> Result<Self> {
        if keys.mode() != CryptoMode::Encrypted {
            return Err(Error::invalid("AES-GCM extents need an encrypted volume"));
        }
        Ok(Self {
            keys,
            rng: Source::Os,
            ledger: Tripwire::default(),
            #[cfg(feature = "test-rng")]
            fips_vectors: false,
        })
    }

    /// Like [`Self::new`] with an injected random source, to reproduce the
    /// known-answer vectors (feature `test-rng`). With `fips_zero_nonce` the
    /// FIPS profile may seal with the zero nonce, which only the vectors use.
    #[cfg(feature = "test-rng")]
    pub fn with_rng(
        keys: Arc<VolumeKeys>,
        rng: Arc<dyn crate::rng::Rng>,
        fips_zero_nonce: bool,
    ) -> Result<Self> {
        let mut s = Self::new(keys)?;
        s.rng = Source::Custom(rng);
        s.fips_vectors = fips_zero_nonce;
        Ok(s)
    }

    /// How many `(key, nonce)` pairs the tripwire has recorded (0 when it is
    /// compiled out).
    pub fn tripwire_len(&self) -> usize {
        self.ledger.len()
    }

    fn fips_allowed(&self) -> bool {
        #[cfg(feature = "test-rng")]
        {
            self.fips_vectors
        }
        #[cfg(not(feature = "test-rng"))]
        {
            false
        }
    }

    fn id_hash(&self, kind: ExtentKind, data: &[u8]) -> [u8; 32] {
        match self.keys.material() {
            Material::Encrypted {
                addressing, node, ..
            } => {
                let key = match kind {
                    ExtentKind::Data => addressing,
                    ExtentKind::Node => node,
                };
                mac::addressing_hash(self.keys.profile(), key.expose(), data)
            }
            // `new` refuses every other mode.
            _ => [0; 32],
        }
    }

    fn open_inner(
        &self,
        kind: ExtentKind,
        bytes: &[u8],
        expect: &OpenExpect<'_>,
        out: &mut Vec<u8>,
    ) -> Result<OpenedMeta> {
        let k = &*self.keys;
        // 1. Header sanity: magic, version, known codec, level, ulen range,
        // structure, nonce rule. The chunk-size rule waits for the tag.
        let header = Cch1Header::parse(bytes)?;
        let ulen32 = u32::try_from(header.ulen).map_err(|_| Error::invalid("ulen"))?;
        let params = DecodeParams {
            layout: ExtentLayout::Encrypted,
            kind,
            chunk_size: plaintext_cap(kind, ulen32),
            allow_nonzero_nonce: false,
        };
        let ext = Extent::decode(bytes, params)?;
        let CryptoRegion {
            dek_gen,
            salt,
            nonce,
        } = ext.crypto.ok_or(Error::invalid("encrypted extent"))?;
        check_expected(kind, expect, &ext.chunk_id)?;
        let dek = k.dek(dek_gen)?;
        let clen = u32::try_from(ext.payload.len()).map_err(|_| Error::invalid("clen"))?;
        let info = extent::info(&InfoParts {
            family: k.family(),
            kind,
            dek_gen,
            codec: header.codec,
            level: header.level,
            clen,
            id_len: k.id_len(),
            chunk_id: &ext.chunk_id,
            salt: &salt,
        })?;
        let key = kdf::extent_key(k.profile(), &dek, &info)?;
        let aad = extent::aad(
            &header,
            kind,
            dek_gen,
            k.family(),
            k.id_len(),
            &ext.chunk_id,
        )?;

        // 2. The AEAD tag. Ciphertext and tag are contiguous in the extent.
        let start = out.len();
        out.extend_from_slice(ext.payload);
        out.extend_from_slice(&ext.tag);
        let gcm = LessSafeKey::new(UnboundKey::new(&AES_256_GCM, &key[..]).map_err(|_| auth())?);
        gcm.open_in_place(
            Nonce::assume_unique_for_key(nonce),
            Aad::from(&aad[..]),
            &mut out[start..],
        )
        .map_err(|_| auth())?;
        out.truncate(start + ext.payload.len());

        // 3. ulen against the volume, never against the header alone.
        if kind == ExtentKind::Data && ulen32 != expect.chunk_size {
            return Err(Error::Corrupt {
                what: WHAT,
                detail: "ulen differs from the volume chunk size",
            });
        }
        // 4 and 5. Bounded decompression, then the key commitment.
        finish(
            &Opened {
                codec: header.codec,
                ulen: ulen32 as usize,
                kind,
            },
            start,
            out,
            expect,
            |plain| ct::eq(&self.id_hash(kind, plain), &ext.chunk_id.0),
        )?;
        meta_of(&header, ext.chunk_id)
    }
}

impl ExtentCodec for AesGcmExtents {
    fn mode(&self) -> CryptoMode {
        CryptoMode::Encrypted
    }

    fn chunk_id(&self, kind: ExtentKind, data: &[u8]) -> ChunkId {
        ChunkId(self.id_hash(kind, data))
    }

    fn seal(
        &self,
        kind: ExtentKind,
        id: &ChunkId,
        desc: &EncodingDesc,
        compressed: &[u8],
        out: &mut Vec<u8>,
    ) -> Result<()> {
        if self.keys.profile() == Profile::Fips && !self.fips_allowed() {
            return Err(Error::Unsupported {
                what: "sealing under the FIPS profile",
            });
        }
        check_desc(kind, desc)?;
        let k = &*self.keys;
        let dek_gen = k.dek_gen();
        // A fresh salt on every call, from the operating system, here and
        // nowhere else (ADR-11 F3).
        let mut salt = [0u8; 24];
        self.rng.fill(&mut salt)?;

        let header = Cch1Header {
            codec: desc.codec,
            level: desc.level,
            ulen: u64::from(desc.ulen),
        };
        let clen = u32::try_from(compressed.len()).map_err(|_| Error::invalid("clen"))?;
        let info = extent::info(&InfoParts {
            family: k.family(),
            kind,
            dek_gen,
            codec: desc.codec,
            level: desc.level,
            clen,
            id_len: k.id_len(),
            chunk_id: id,
            salt: &salt,
        })?;
        let dek = k.dek(dek_gen)?;
        let key = kdf::extent_key(k.profile(), &dek, &info)?;
        let aad = extent::aad(&header, kind, dek_gen, k.family(), k.id_len(), id)?;

        let start = out.len();
        let ext = Extent {
            header,
            chunk_id: *id,
            crypto: Some(CryptoRegion {
                dek_gen,
                salt,
                nonce: ZERO_NONCE,
            }),
            payload: compressed,
            tag: [0; TAG_LEN],
        };
        if let Err(e) = ext.encode_untagged(out) {
            out.truncate(start);
            return Err(e.into());
        }
        let sealed = (|| {
            let gcm =
                LessSafeKey::new(UnboundKey::new(&AES_256_GCM, &key[..]).map_err(|_| auth())?);
            gcm.seal_in_place_separate_tag(
                Nonce::assume_unique_for_key(ZERO_NONCE),
                Aad::from(&aad[..]),
                &mut out[start + ENCRYPTED_PREFIX_LEN..],
            )
            .map_err(|_| auth())
        })();
        let tag = match sealed {
            Ok(t) => t,
            Err(e) => {
                out.truncate(start);
                return Err(e);
            }
        };
        out.extend_from_slice(tag.as_ref());
        self.ledger.record(&key, &ZERO_NONCE);
        Ok(())
    }

    fn open(
        &self,
        kind: ExtentKind,
        bytes: &[u8],
        expect: &OpenExpect<'_>,
        out: &mut Vec<u8>,
    ) -> Result<OpenedMeta> {
        let start = out.len();
        let r = self.open_inner(kind, bytes, expect, out);
        if r.is_err() {
            rollback(out, start);
        }
        r
    }
}

/// Extents of an `integrity` or `plain` volume: framing and a 16-byte tag, no
/// cipher.
pub struct PlainExtents {
    keys: Arc<VolumeKeys>,
}

impl PlainExtents {
    /// Extents for the volume `keys` belong to.
    ///
    /// # Errors
    /// `Invalid` for an `encrypted` volume.
    pub fn new(keys: Arc<VolumeKeys>) -> Result<Self> {
        if keys.mode() == CryptoMode::Encrypted {
            return Err(Error::invalid("plain extents need an unencrypted volume"));
        }
        Ok(Self { keys })
    }

    /// The BLAKE3-128 tag of `input`: keyed in `integrity` mode.
    fn tag(&self, input: &[u8]) -> [u8; TAG_LEN] {
        let full = match self.keys.material() {
            Material::Integrity { tag, .. } => *blake3::keyed_hash(tag.expose(), input).as_bytes(),
            _ => *blake3::hash(input).as_bytes(),
        };
        let mut t = [0u8; TAG_LEN];
        t.copy_from_slice(&full[..TAG_LEN]);
        t
    }

    fn open_inner(
        &self,
        kind: ExtentKind,
        bytes: &[u8],
        expect: &OpenExpect<'_>,
        out: &mut Vec<u8>,
    ) -> Result<OpenedMeta> {
        // The tag first: nothing else is trusted before it verifies.
        let input = tag_input(bytes)?;
        if !ct::eq(&self.tag(input), &bytes[input.len()..]) {
            return Err(auth());
        }
        let params = DecodeParams {
            layout: ExtentLayout::Unencrypted,
            kind,
            chunk_size: plaintext_cap(kind, expect.chunk_size),
            allow_nonzero_nonce: false,
        };
        let ext = Extent::decode(bytes, params)?;
        check_expected(kind, expect, &ext.chunk_id)?;
        let start = out.len();
        out.extend_from_slice(ext.payload);
        let ulen = u32::try_from(ext.header.ulen).map_err(|_| Error::invalid("ulen"))?;
        finish(
            &Opened {
                codec: ext.header.codec,
                ulen: ulen as usize,
                kind,
            },
            start,
            out,
            expect,
            |plain| ct::eq(blake3::hash(plain).as_bytes(), &ext.chunk_id.0),
        )?;
        meta_of(&ext.header, ext.chunk_id)
    }
}

impl ExtentCodec for PlainExtents {
    fn mode(&self) -> CryptoMode {
        self.keys.mode()
    }

    fn chunk_id(&self, _kind: ExtentKind, data: &[u8]) -> ChunkId {
        ChunkId(*blake3::hash(data).as_bytes())
    }

    fn seal(
        &self,
        kind: ExtentKind,
        id: &ChunkId,
        desc: &EncodingDesc,
        compressed: &[u8],
        out: &mut Vec<u8>,
    ) -> Result<()> {
        check_desc(kind, desc)?;
        let start = out.len();
        let ext = Extent {
            header: Cch1Header {
                codec: desc.codec,
                level: desc.level,
                ulen: u64::from(desc.ulen),
            },
            chunk_id: *id,
            crypto: None,
            payload: compressed,
            tag: [0; TAG_LEN],
        };
        if let Err(e) = ext.encode_untagged(out) {
            out.truncate(start);
            return Err(e.into());
        }
        let tag = self.tag(&out[start..]);
        out.extend_from_slice(&tag);
        Ok(())
    }

    fn open(
        &self,
        kind: ExtentKind,
        bytes: &[u8],
        expect: &OpenExpect<'_>,
        out: &mut Vec<u8>,
    ) -> Result<OpenedMeta> {
        let start = out.len();
        let r = self.open_inner(kind, bytes, expect, out);
        if r.is_err() {
            rollback(out, start);
        }
        r
    }
}
