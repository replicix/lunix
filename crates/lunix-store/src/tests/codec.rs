//! A small fake of the crypto seams, enough for the store tests (M2's real
//! codecs and the testkit's `PlainCodec` are not available to wave 1a).
//!
//! The layouts are the real ones (extent: header, id, clen, crypto region,
//! payload, 16-byte tag; metadata: envelope form 0 or 1); only the primitives
//! are stand-ins: BLAKE3 for the tag and the MAC, and a BLAKE3 keystream for
//! "encryption". It authenticates every byte before the tag, which is what the
//! store tests rely on.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use lunix_format::envelope::{self, meta_aad, Envelope, EnvelopeBody};
use lunix_format::{ChunkId, Codec, CryptoMode, ExtentKind, ObjectKind};
use lunix_iface::crypto::{
    EncodingDesc, ExtentCodec, MetaCodec, OpenExpect, OpenOutput, OpenedMeta,
};
use lunix_iface::{Error, Result};

const KEY: [u8; 32] = *b"lunix-store test key, not secret";
static NONCE: AtomicU64 = AtomicU64::new(1);

pub(crate) struct TestCodec {
    mode: CryptoMode,
    uuid: [u8; 16],
}

impl TestCodec {
    pub(crate) fn new(mode: CryptoMode, uuid: [u8; 16]) -> Arc<Self> {
        Arc::new(Self { mode, uuid })
    }
}

fn keystream(salt: &[u8], len: usize) -> Vec<u8> {
    let mut h = blake3::Hasher::new_keyed(&KEY);
    h.update(salt);
    let mut out = vec![0u8; len];
    h.finalize_xof().fill(&mut out);
    out
}

fn xor(data: &[u8], salt: &[u8]) -> Vec<u8> {
    data.iter()
        .zip(keystream(salt, data.len()))
        .map(|(a, b)| a ^ b)
        .collect()
}

fn tag(mode: CryptoMode, prefix: &[u8]) -> [u8; 16] {
    let h = match mode {
        CryptoMode::Plain => blake3::hash(prefix),
        _ => blake3::keyed_hash(&KEY, prefix),
    };
    let mut t = [0u8; 16];
    t.copy_from_slice(&h.as_bytes()[..16]);
    t
}

/// Builds a complete extent of `mode` around `payload` (already "compressed").
pub(crate) fn build_extent(
    mode: CryptoMode,
    codec: Codec,
    level: i8,
    ulen: u32,
    id: &ChunkId,
    payload: &[u8],
) -> Vec<u8> {
    let mut v = Vec::with_capacity(payload.len() + 110);
    v.extend_from_slice(b"CCH1");
    v.push(1);
    v.extend_from_slice(&codec.id().to_le_bytes());
    v.extend_from_slice(&level.to_le_bytes());
    v.extend_from_slice(&u64::from(ulen).to_le_bytes());
    v.extend_from_slice(&id.0);
    v.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    if mode == CryptoMode::Encrypted {
        let mut salt = [0u8; 24];
        salt[..8].copy_from_slice(&NONCE.fetch_add(1, Ordering::Relaxed).to_le_bytes());
        v.extend_from_slice(&0u32.to_le_bytes());
        v.extend_from_slice(&salt);
        v.extend_from_slice(&[0u8; 12]);
        v.extend_from_slice(&xor(payload, &salt));
    } else {
        v.extend_from_slice(payload);
    }
    let t = tag(mode, &v);
    v.extend_from_slice(&t);
    v
}

/// The stored payload of a verified extent.
pub(crate) fn open_extent(mode: CryptoMode, bytes: &[u8]) -> Result<Vec<u8>> {
    let ovh = mode.extent_ovh() as usize;
    if bytes.len() <= ovh {
        return Err(Error::Auth { what: "extent" });
    }
    let (prefix, got) = bytes.split_at(bytes.len() - 16);
    if tag(mode, prefix)[..] != *got {
        return Err(Error::Auth { what: "extent" });
    }
    let payload = &prefix[ovh - 16..];
    if mode == CryptoMode::Encrypted {
        Ok(xor(payload, &prefix[56..80]))
    } else {
        Ok(payload.to_vec())
    }
}

impl ExtentCodec for TestCodec {
    fn mode(&self) -> CryptoMode {
        self.mode
    }

    fn chunk_id(&self, _kind: ExtentKind, data: &[u8]) -> ChunkId {
        ChunkId(*blake3::hash(data).as_bytes())
    }

    fn seal(
        &self,
        _kind: ExtentKind,
        id: &ChunkId,
        desc: &EncodingDesc,
        compressed: &[u8],
        out: &mut Vec<u8>,
    ) -> Result<()> {
        out.extend_from_slice(&build_extent(
            self.mode,
            desc.codec,
            desc.level.0,
            desc.ulen,
            id,
            compressed,
        ));
        Ok(())
    }

    fn open(
        &self,
        _kind: ExtentKind,
        bytes: &[u8],
        expect: &OpenExpect<'_>,
        out: &mut Vec<u8>,
    ) -> Result<OpenedMeta> {
        let payload = open_extent(self.mode, bytes)?;
        let id = ChunkId(
            bytes[16..48]
                .try_into()
                .map_err(|_| Error::Auth { what: "extent" })?,
        );
        if expect.chunk_id.is_some_and(|e| *e != id) {
            return Err(Error::Auth { what: "extent" });
        }
        if expect.output == OpenOutput::Plaintext {
            return Err(Error::Unsupported {
                what: "plaintext output in the store test codec",
            });
        }
        out.extend_from_slice(&payload);
        let codec = Codec::from_u16(u16::from_le_bytes([bytes[5], bytes[6]]))?;
        let ulen = u64::from_le_bytes(bytes[8..16].try_into().unwrap_or([0; 8]));
        Ok(OpenedMeta {
            desc: EncodingDesc {
                codec,
                level: lunix_format::Level(i8::from_le_bytes([bytes[7]])),
                ulen: u32::try_from(ulen).unwrap_or(0),
                dict_id: None,
            },
            chunk_id: id,
        })
    }
}

impl MetaCodec for TestCodec {
    fn mode(&self) -> CryptoMode {
        self.mode
    }

    fn seal(&self, kind: ObjectKind, name: &str, body: &[u8]) -> Result<Vec<u8>> {
        let aad = meta_aad(&self.uuid, kind, name)?;
        if self.mode == CryptoMode::Encrypted {
            let mut nonce = [0u8; 24];
            nonce[..8].copy_from_slice(&NONCE.fetch_add(1, Ordering::Relaxed).to_le_bytes());
            let ct = xor(body, &nonce);
            let mut h = blake3::Hasher::new_keyed(&KEY);
            h.update(&aad);
            h.update(&nonce);
            h.update(&ct);
            let mut t = [0u8; 16];
            t.copy_from_slice(&h.finalize().as_bytes()[..16]);
            Ok(envelope::encode_form1(kind, &nonce, &ct, &t)?)
        } else {
            let signed = envelope::form0_unsigned(kind, body)?;
            let mac = *blake3::hash(&envelope::mac_input(&aad, &signed)).as_bytes();
            Ok(envelope::encode_form0(kind, body, &mac)?)
        }
    }

    fn open(&self, kind: ObjectKind, name: &str, bytes: &[u8]) -> Result<Vec<u8>> {
        let aad = meta_aad(&self.uuid, kind, name)?;
        let env = Envelope::decode(
            bytes,
            &[kind],
            self.mode.envelope_form(),
            kind.max_body_len(),
        )?;
        let auth = Error::Auth {
            what: "metadata object",
        };
        match env.body {
            EnvelopeBody::Mac { body, mac } => {
                let want = *blake3::hash(&envelope::mac_input(&aad, env.signed)).as_bytes();
                if want != mac {
                    return Err(auth);
                }
                Ok(body.to_vec())
            }
            EnvelopeBody::Sealed {
                nonce,
                ciphertext,
                tag,
            } => {
                let mut h = blake3::Hasher::new_keyed(&KEY);
                h.update(&aad);
                h.update(&nonce);
                h.update(ciphertext);
                if h.finalize().as_bytes()[..16] != tag {
                    return Err(auth);
                }
                Ok(xor(ciphertext, &nonce))
            }
        }
    }
}
