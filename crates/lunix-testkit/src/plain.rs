//! [`PlainCodec`]: the three crypto traits without encryption.
//!
//! Extents carry the D11 tag (the first 16 bytes of a digest over every byte
//! before it, keyed in integrity mode and unkeyed in plain mode); metadata and
//! commits use the real form 0 envelope with a 32-byte MAC. The digest is the
//! testkit's own (see [`crate::digest`]), not BLAKE3 and not HMAC, so the bytes
//! are *not* those of `lunix-crypto`; every framing rule is.
//!
//! Limits, by design: payloads are opened only for [`Codec::Raw`] (the testkit
//! has no zstd or LZ4 dependency). Other codecs can be sealed and opened as
//! [`OpenOutput::Stored`], but asking for their plaintext, or `verify_id` on
//! them, is `Unsupported`.

use lunix_format::commit::{decode_object, Commit};
use lunix_format::envelope::{self, Envelope, EnvelopeBody};
use lunix_format::extent::{tag_input, Cch1Header, DecodeParams, Extent, ExtentLayout, TAG_LEN};
use lunix_format::{ChunkId, Codec, CommitSeq, CryptoMode, ExtentKind, Form, ObjectKind};
use lunix_iface::crypto::{
    CommitCodec, EncodingDesc, ExtentCodec, MetaCodec, OpenExpect, OpenOutput, OpenedMeta,
    SealedCommit,
};
use lunix_iface::{Error, Result};

use crate::digest::digest;

/// The fixed integrity key of [`PlainCodec::new`].
pub const TEST_INTEGRITY_KEY: [u8; 16] = *b"lunix-test-key-1";

/// The fixed volume id bound into metadata MACs.
pub const TEST_VOLUME_ID: [u8; 16] = *b"lunix-test-vol-1";

const NO_KEY: [u8; 16] = [0; 16];

const D_ID_DATA: u8 = 1;
const D_ID_NODE: u8 = 2;
const D_EXTENT_TAG: u8 = 3;
const D_META_MAC: u8 = 4;
const D_OBJECT_HASH: u8 = 5;

/// `ExtentCodec + MetaCodec + CommitCodec` with no encryption.
#[derive(Clone, Copy, Debug)]
pub struct PlainCodec {
    mode: CryptoMode,
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

    fn tag_of(&self, input: &[u8]) -> [u8; TAG_LEN] {
        let full = digest(D_EXTENT_TAG, &self.key, &[input]);
        full[..TAG_LEN].try_into().expect("16 bytes")
    }

    fn mac_of(&self, aad: &[u8], signed: &[u8]) -> [u8; 32] {
        digest(D_META_MAC, &self.key, &[aad, signed])
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
        let domain = match kind {
            ExtentKind::Data => D_ID_DATA,
            ExtentKind::Node => D_ID_NODE,
        };
        ChunkId(digest(domain, &NO_KEY, &[data]))
    }

    fn seal(
        &self,
        _kind: ExtentKind,
        id: &ChunkId,
        desc: &EncodingDesc,
        compressed: &[u8],
        out: &mut Vec<u8>,
    ) -> Result<()> {
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
        let raw = e.header.codec == Codec::Raw;
        if !raw && (expect.verify_id || expect.output == OpenOutput::Plaintext) {
            return Err(Error::Unsupported {
                what: "PlainCodec opens only the raw codec",
            });
        }
        if raw && expect.verify_id && self.chunk_id(kind, e.payload) != e.chunk_id {
            return Err(Error::Auth { what: "chunk id" });
        }
        let desc = EncodingDesc {
            codec: e.header.codec,
            level: e.header.level,
            ulen: u32::try_from(e.header.ulen).map_err(|_| Error::invalid("ulen"))?,
            dict_id: None,
        };
        out.extend_from_slice(e.payload);
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
        digest(D_OBJECT_HASH, &NO_KEY, &[bytes])
    }
}
