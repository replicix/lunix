//! [`PlainCodec`]: the three crypto traits without encryption.
//!
//! Chunk ids are `BLAKE3(plaintext)` (DESIGN, D11). Extents carry the D11 tag:
//! BLAKE3-128 over every byte before it, keyed by a key derived from the test
//! integrity key in integrity mode and unkeyed in plain mode. Metadata and
//! commits use the real form 0 envelope with a 32-byte MAC: keyed BLAKE3 in
//! integrity mode (the real codec uses HMAC-SHA-256 there; the testkit has no
//! `hmac`/`sha2`) and unkeyed BLAKE3 in plain mode, as in D16. So extents and
//! plain-mode objects carry the real bytes; only the integrity-mode metadata
//! MAC differs from `lunix-crypto`.
//!
//! Codecs: raw, LZ4 (a raw block, no size prefix) and zstd are supported by
//! `open` for [`OpenOutput::Plaintext`] and `verify_id`, with decompression
//! capped at the volume chunk size (64 KiB for nodes). The dictionary codec is
//! `Unsupported`, as in phase 1.

use lunix_format::commit::{decode_object, Commit};
use lunix_format::envelope::{self, Envelope, EnvelopeBody};
use lunix_format::extent::{tag_input, Cch1Header, DecodeParams, Extent, ExtentLayout, TAG_LEN};
use lunix_format::{ChunkId, Codec, CommitSeq, CryptoMode, ExtentKind, Form, ObjectKind};
use lunix_iface::crypto::{
    CommitCodec, EncodingDesc, ExtentCodec, MetaCodec, OpenExpect, OpenOutput, OpenedMeta,
    SealedCommit,
};
use lunix_iface::{Error, Result};

/// The fixed integrity key of [`PlainCodec::new`].
pub const TEST_INTEGRITY_KEY: [u8; 16] = *b"lunix-test-key-1";

/// The fixed volume id bound into metadata MACs.
pub const TEST_VOLUME_ID: [u8; 16] = *b"lunix-test-vol-1";

const NO_KEY: [u8; 16] = [0; 16];

/// `ExtentCodec + MetaCodec + CommitCodec` with no encryption.
#[derive(Clone, Copy, Debug)]
pub struct PlainCodec {
    mode: CryptoMode,
    /// The integrity key; all zero in plain mode (unused there).
    key: [u8; 16],
}

impl Default for PlainCodec {
    fn default() -> Self {
        Self::new()
    }
}

impl PlainCodec {
    /// Integrity mode with the fixed test key [`TEST_INTEGRITY_KEY`].
    pub fn new() -> Self {
        Self::with_key(TEST_INTEGRITY_KEY)
    }

    /// Integrity mode with another key (two volumes that must not open each
    /// other's objects).
    pub fn with_key(key: [u8; 16]) -> Self {
        Self {
            mode: CryptoMode::Integrity,
            key,
        }
    }

    /// Plain mode: the tag and the MAC are unkeyed checksums.
    pub fn unkeyed() -> Self {
        Self {
            mode: CryptoMode::Plain,
            key: NO_KEY,
        }
    }

    /// A hasher for `purpose`: keyed under a key derived from the integrity
    /// key in integrity mode, unkeyed in plain mode.
    fn hasher(&self, purpose: &str) -> blake3::Hasher {
        match self.mode {
            CryptoMode::Plain => blake3::Hasher::new(),
            _ => blake3::Hasher::new_keyed(&blake3::derive_key(purpose, &self.key)),
        }
    }

    fn tag_of(&self, input: &[u8]) -> [u8; TAG_LEN] {
        let mut h = self.hasher("lunix testkit extent tag");
        h.update(input);
        h.finalize().as_bytes()[..TAG_LEN]
            .try_into()
            .expect("16 bytes")
    }

    fn mac_of(&self, aad: &[u8], signed: &[u8]) -> [u8; 32] {
        let mut h = self.hasher("lunix testkit metadata mac");
        h.update(aad).update(signed);
        *h.finalize().as_bytes()
    }

    /// The plaintext of a verified payload, capped at `cap` bytes.
    fn decompress(codec: Codec, payload: &[u8], ulen: usize, cap: usize) -> Result<Vec<u8>> {
        if ulen > cap {
            return Err(Error::Corrupt {
                what: "extent",
                detail: "ulen above the cap",
            });
        }
        let plain = match codec {
            Codec::Raw => payload.to_vec(),
            Codec::Lz4 => {
                lz4_flex::block::decompress(payload, ulen).map_err(|_| Error::Corrupt {
                    what: "extent",
                    detail: "lz4 block",
                })?
            }
            Codec::Zstd => zstd::bulk::decompress(payload, ulen).map_err(|_| Error::Corrupt {
                what: "extent",
                detail: "zstd frame",
            })?,
            Codec::ZstdDict => {
                return Err(Error::Unsupported {
                    what: "dictionary codec",
                })
            }
        };
        if plain.len() != ulen {
            return Err(Error::Corrupt {
                what: "extent",
                detail: "decompressed length differs from ulen",
            });
        }
        Ok(plain)
    }

    fn aad(kind: ObjectKind, name: &str) -> Result<Vec<u8>> {
        Ok(envelope::meta_aad(&TEST_VOLUME_ID, kind, name)?)
    }
}

impl ExtentCodec for PlainCodec {
    fn mode(&self) -> CryptoMode {
        self.mode
    }

    fn chunk_id(&self, kind: ExtentKind, data: &[u8]) -> ChunkId {
        // Data in integrity mode: keyed BLAKE3 under an addressing key derived
        // from the test key (as the amendment of 2026-11-12). Otherwise, and
        // always for nodes (the node hash the map computes over `hash_input`
        // and passes as `data`), unkeyed BLAKE3. A node therefore never passes
        // as data under the integrity key, and the reverse.
        match (self.mode, kind) {
            (CryptoMode::Integrity, ExtentKind::Data) => {
                let key = blake3::derive_key("lunix testkit addressing", &self.key);
                ChunkId(*blake3::keyed_hash(&key, data).as_bytes())
            }
            _ => ChunkId(*blake3::hash(data).as_bytes()),
        }
    }

    fn seal(
        &self,
        _kind: ExtentKind,
        id: &ChunkId,
        desc: &EncodingDesc,
        compressed: &[u8],
        out: &mut Vec<u8>,
    ) -> Result<()> {
        if desc.dict_id.is_some() != (desc.codec == Codec::ZstdDict) {
            return Err(Error::invalid("dict_id is for codec 3 only"));
        }
        if desc.ulen == 0 {
            return Err(Error::invalid("extent ulen is zero"));
        }
        let extent = Extent {
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
        let mut buf = Vec::with_capacity(extent.len());
        extent
            .encode_untagged(&mut buf)
            .map_err(|_| Error::invalid("extent descriptor does not fit the payload"))?;
        let tag = self.tag_of(&buf);
        buf.extend_from_slice(&tag);
        out.extend_from_slice(&buf);
        Ok(())
    }

    fn open(
        &self,
        kind: ExtentKind,
        bytes: &[u8],
        expect: &OpenExpect<'_>,
        out: &mut Vec<u8>,
    ) -> Result<OpenedMeta> {
        let input = tag_input(bytes)?;
        if self.tag_of(input)[..] != bytes[input.len()..] {
            return Err(Error::Auth { what: "extent tag" });
        }
        let params = DecodeParams {
            layout: ExtentLayout::Unencrypted,
            kind,
            chunk_size: expect.chunk_size,
            allow_nonzero_nonce: false,
        };
        let (e, used) = Extent::decode_prefix(bytes, params)?;
        if used != bytes.len() {
            return Err(Error::Corrupt {
                what: "extent",
                detail: "trailing bytes",
            });
        }
        if expect.chunk_id.is_some_and(|id| *id != e.chunk_id)
            || expect.id_prefix.is_some_and(|p| !p.matches(&e.chunk_id))
        {
            return Err(Error::Auth { what: "chunk id" });
        }
        let cap = match kind {
            ExtentKind::Data => expect.chunk_size as usize,
            ExtentKind::Node => lunix_format::extent::MAX_NODE_ULEN as usize,
        };
        let ulen = usize::try_from(e.header.ulen).map_err(|_| Error::invalid("ulen"))?;
        let needs_plain = (expect.verify_id && kind == ExtentKind::Data)
            || expect.output == OpenOutput::Plaintext;
        let plain = if needs_plain {
            Some(Self::decompress(e.header.codec, e.payload, ulen, cap)?)
        } else {
            None
        };
        // A node's hash is checked by the map, not here (R2 of the amendment).
        if expect.verify_id && kind == ExtentKind::Data {
            let plain = plain.as_deref().expect("decompressed above");
            if self.chunk_id(kind, plain) != e.chunk_id {
                return Err(Error::Auth { what: "chunk id" });
            }
        }
        let desc = EncodingDesc {
            codec: e.header.codec,
            level: e.header.level,
            ulen: u32::try_from(e.header.ulen).map_err(|_| Error::invalid("ulen"))?,
            dict_id: None,
        };
        match (&plain, expect.output) {
            (Some(plain), OpenOutput::Plaintext) => out.extend_from_slice(plain),
            _ => out.extend_from_slice(e.payload),
        }
        Ok(OpenedMeta {
            desc,
            chunk_id: e.chunk_id,
        })
    }
}

impl MetaCodec for PlainCodec {
    fn mode(&self) -> CryptoMode {
        self.mode
    }

    fn seal(&self, kind: ObjectKind, name: &str, body: &[u8]) -> Result<Vec<u8>> {
        let aad = Self::aad(kind, name)?;
        let unsigned = envelope::form0_unsigned(kind, body)?;
        let mac = self.mac_of(&aad, &unsigned);
        Ok(envelope::encode_form0(kind, body, &mac)?)
    }

    fn open(&self, kind: ObjectKind, name: &str, bytes: &[u8]) -> Result<Vec<u8>> {
        // A moved object is an authentication failure, not a framing one.
        if bytes.get(5).is_some_and(|&k| k != kind.id()) {
            return Err(Error::Auth {
                what: "object kind",
            });
        }
        let aad = Self::aad(kind, name)?;
        let env = Envelope::decode(bytes, &[kind], Form::Mac, kind.max_body_len())?;
        let EnvelopeBody::Mac { body, mac } = env.body else {
            return Err(Error::Auth {
                what: "object form",
            });
        };
        if mac != self.mac_of(&aad, env.signed) {
            return Err(Error::Auth { what: "object" });
        }
        Ok(body.to_vec())
    }
}

impl CommitCodec for PlainCodec {
    fn seal_commit(&self, commit: &Commit) -> Result<SealedCommit> {
        let seq = commit.common().commit_seq;
        let aad = Self::aad(commit.object_kind(), &lunix_format::paths::commit(seq))?;
        let unsigned = commit.encode_object_form0_unsigned()?;
        let bytes = commit.encode_object_form0(&self.mac_of(&aad, &unsigned))?;
        let hash = self.object_hash(&bytes);
        Ok(SealedCommit { bytes, hash })
    }

    fn open_commit(&self, seq: CommitSeq, bytes: &[u8]) -> Result<Commit> {
        let obj = decode_object(bytes)?;
        let aad = Self::aad(obj.envelope.kind, &lunix_format::paths::commit(seq))?;
        if obj.mac != self.mac_of(&aad, obj.envelope.signed) {
            return Err(Error::Auth { what: "commit" });
        }
        if obj.commit.common().commit_seq != seq {
            return Err(Error::Corrupt {
                what: "commit",
                detail: "wrong commit_seq",
            });
        }
        Ok(obj.commit)
    }

    fn object_hash(&self, bytes: &[u8]) -> [u8; 32] {
        *blake3::hash(bytes).as_bytes()
    }
}
