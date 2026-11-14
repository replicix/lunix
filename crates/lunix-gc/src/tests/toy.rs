//! A toy "encrypted" extent codec for the salt test: the encrypted layout
//! (`OVH` 108, a crypto region with a salt), a fresh salt per `seal`, the
//! payload XORed with a keystream derived from the salt and a checksum tag.
//! It is not cryptography; it only makes "re-sealed with a new salt"
//! observable, which `PlainCodec` (no salt) cannot.

use std::sync::atomic::{AtomicU64, Ordering};

use lunix_format::extent::{
    tag_input, Cch1Header, CryptoRegion, DecodeParams, Extent, ExtentLayout, TAG_LEN,
};
use lunix_format::{ChunkId, Codec, CryptoMode, DekGen, ExtentKind};
use lunix_iface::crypto::{EncodingDesc, ExtentCodec, OpenExpect, OpenOutput, OpenedMeta};
use lunix_iface::{Error, Result};
use lunix_testkit::PlainCodec;

#[derive(Debug, Default)]
pub(crate) struct ToyCodec {
    inner: PlainCodec,
    salts: AtomicU64,
}

fn splitmix(z: &mut u64) -> u64 {
    *z = z.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut x = *z;
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}

fn xor_stream(salt: &[u8; 24], data: &mut [u8]) {
    let mut z = u64::from_le_bytes(salt[..8].try_into().expect("8"))
        ^ u64::from_le_bytes(salt[8..16].try_into().expect("8"));
    for chunk in data.chunks_mut(8) {
        let k = splitmix(&mut z).to_le_bytes();
        for (b, k) in chunk.iter_mut().zip(k) {
            *b ^= k;
        }
    }
}

impl ToyCodec {
    fn tag(&self, input: &[u8]) -> [u8; TAG_LEN] {
        let h = self.inner.chunk_id(ExtentKind::Node, input);
        h.0[..TAG_LEN].try_into().expect("16")
    }

    fn fresh_salt(&self) -> [u8; 24] {
        let mut z = self.salts.fetch_add(1, Ordering::Relaxed) ^ 0x5eed_5a17;
        let mut s = [0u8; 24];
        for c in s.chunks_mut(8) {
            c.copy_from_slice(&splitmix(&mut z).to_le_bytes());
        }
        s
    }

    /// The salt of a stored extent.
    pub(crate) fn salt_of(bytes: &[u8]) -> Option<[u8; 24]> {
        bytes.get(56..80).and_then(|s| s.try_into().ok())
    }
}

impl ExtentCodec for ToyCodec {
    fn mode(&self) -> CryptoMode {
        CryptoMode::Encrypted
    }

    fn chunk_id(&self, kind: ExtentKind, data: &[u8]) -> ChunkId {
        self.inner.chunk_id(kind, data)
    }

    fn seal(
        &self,
        _kind: ExtentKind,
        id: &ChunkId,
        desc: &EncodingDesc,
        compressed: &[u8],
        out: &mut Vec<u8>,
    ) -> Result<()> {
        let salt = self.fresh_salt();
        let mut ct = compressed.to_vec();
        xor_stream(&salt, &mut ct);
        let e = Extent {
            header: Cch1Header {
                codec: desc.codec,
                level: desc.level,
                ulen: u64::from(desc.ulen),
            },
            chunk_id: *id,
            crypto: Some(CryptoRegion {
                dek_gen: DekGen(1),
                salt,
                nonce: [0; 12],
            }),
            payload: &ct,
            tag: [0; TAG_LEN],
        };
        let mut buf = Vec::new();
        e.encode_untagged(&mut buf)
            .map_err(|_| Error::invalid("extent descriptor does not fit the payload"))?;
        let tag = self.tag(&buf);
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
        if self.tag(input)[..] != bytes[input.len()..] {
            return Err(Error::Auth { what: "extent tag" });
        }
        let e = Extent::decode(
            bytes,
            DecodeParams {
                layout: ExtentLayout::Encrypted,
                kind,
                chunk_size: expect.chunk_size,
                allow_nonzero_nonce: false,
            },
        )?;
        if expect.chunk_id.is_some_and(|id| *id != e.chunk_id)
            || expect.id_prefix.is_some_and(|p| !p.matches(&e.chunk_id))
        {
            return Err(Error::Auth { what: "chunk id" });
        }
        let salt = e
            .crypto
            .map(|c| c.salt)
            .ok_or(Error::Auth { what: "salt" })?;
        let mut plain = e.payload.to_vec();
        xor_stream(&salt, &mut plain);
        if expect.output == OpenOutput::Plaintext && e.header.codec != Codec::Raw {
            return Err(Error::Unsupported {
                what: "toy codec decompression",
            });
        }
        if expect.verify_id && kind == ExtentKind::Data && self.chunk_id(kind, &plain) != e.chunk_id
        {
            return Err(Error::Auth { what: "chunk id" });
        }
        out.extend_from_slice(&plain);
        Ok(OpenedMeta {
            desc: EncodingDesc {
                codec: e.header.codec,
                level: e.header.level,
                ulen: u32::try_from(e.header.ulen).map_err(|_| Error::invalid("ulen"))?,
                dict_id: None,
            },
            chunk_id: e.chunk_id,
        })
    }
}
