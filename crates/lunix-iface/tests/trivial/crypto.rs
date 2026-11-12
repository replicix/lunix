use lunix_format::commit::{decode_object, Commit};
use lunix_format::extent::{tag_input, Cch1Header, DecodeParams, Extent, ExtentLayout};
use lunix_format::{ChunkId, Codec, CommitSeq, CryptoMode, ExtentKind, Level, ObjectKind};
use lunix_iface::contract::crypto::CryptoFixture;
use lunix_iface::crypto::*;
use lunix_iface::store::CommitStore;
use lunix_iface::{Error, Result};

use super::hash32;
use super::store::TStore;

/// Plain-layout codec with a toy tag (first 16 bytes of `hash32`).
pub struct Toy;

fn tag_of(input: &[u8]) -> [u8; 16] {
    hash32(input)[..16].try_into().expect("16")
}

impl ExtentCodec for Toy {
    fn mode(&self) -> CryptoMode {
        CryptoMode::Plain
    }

    fn chunk_id(&self, _kind: ExtentKind, data: &[u8]) -> ChunkId {
        ChunkId(hash32(data))
    }

    fn seal(
        &self,
        _kind: ExtentKind,
        id: &ChunkId,
        desc: &EncodingDesc,
        compressed: &[u8],
        out: &mut Vec<u8>,
    ) -> Result<()> {
        let e = Extent {
            header: Cch1Header {
                codec: desc.codec,
                level: desc.level,
                ulen: u64::from(desc.ulen),
            },
            chunk_id: *id,
            crypto: None,
            payload: compressed,
            tag: [0; 16],
        };
        let start = out.len();
        e.encode_untagged(out)?;
        let tag = tag_of(&out[start..]);
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
        let input = tag_input(bytes)?;
        if tag_of(input)[..] != bytes[input.len()..] {
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
        if e.header.codec != Codec::Raw {
            return Err(Error::Unsupported { what: "toy codec" });
        }
        if expect.verify_id && self.chunk_id(kind, e.payload) != e.chunk_id {
            return Err(Error::Auth { what: "chunk id" });
        }
        out.extend_from_slice(e.payload);
        let desc = EncodingDesc {
            codec: e.header.codec,
            level: e.header.level,
            ulen: u32::try_from(e.header.ulen).map_err(|_| Error::invalid("ulen"))?,
            dict_id: None,
        };
        Ok(OpenedMeta {
            desc,
            chunk_id: e.chunk_id,
        })
    }
}

fn meta_tag(kind: ObjectKind, name: &str, body: &[u8]) -> [u8; 16] {
    let mut v = vec![kind.id()];
    v.extend_from_slice(name.as_bytes());
    v.push(0);
    v.extend_from_slice(body);
    tag_of(&v)
}

impl MetaCodec for Toy {
    fn mode(&self) -> CryptoMode {
        CryptoMode::Plain
    }

    fn seal(&self, kind: ObjectKind, name: &str, body: &[u8]) -> Result<Vec<u8>> {
        let mut out = meta_tag(kind, name, body).to_vec();
        out.extend_from_slice(body);
        Ok(out)
    }

    fn open(&self, kind: ObjectKind, name: &str, bytes: &[u8]) -> Result<Vec<u8>> {
        let (tag, body) = bytes
            .split_at_checked(16)
            .ok_or(Error::Auth { what: "object" })?;
        if tag != meta_tag(kind, name, body) {
            return Err(Error::Auth { what: "object" });
        }
        Ok(body.to_vec())
    }
}

fn commit_mac(seq: CommitSeq, signed: &[u8]) -> [u8; 32] {
    let mut v = lunix_format::paths::commit(seq).into_bytes();
    v.extend_from_slice(signed);
    hash32(&v)
}

impl CommitCodec for Toy {
    fn seal_commit(&self, commit: &Commit) -> Result<SealedCommit> {
        let seq = commit.common().commit_seq;
        let unsigned = commit.encode_object_form0_unsigned()?;
        let bytes = commit.encode_object_form0(&commit_mac(seq, &unsigned))?;
        let hash = self.object_hash(&bytes);
        Ok(SealedCommit { bytes, hash })
    }

    fn open_commit(&self, seq: CommitSeq, bytes: &[u8]) -> Result<Commit> {
        let obj = decode_object(bytes)?;
        if obj.mac != commit_mac(seq, obj.envelope.signed) {
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
        hash32(bytes)
    }
}

pub struct CFix {
    store: TStore,
}

impl CFix {
    pub fn new() -> Self {
        Self {
            store: TStore::new(),
        }
    }
}

impl CryptoFixture for CFix {
    fn extents(&self) -> &dyn ExtentCodec {
        &Toy
    }
    fn meta(&self) -> &dyn MetaCodec {
        &Toy
    }
    fn commits(&self) -> &dyn CommitCodec {
        &Toy
    }
    fn store(&self) -> &dyn CommitStore {
        &self.store
    }
    fn codecs(&self) -> Vec<(Codec, Level)> {
        vec![(Codec::Raw, Level(0))]
    }
    fn compress(&self, _codec: Codec, plain: &[u8]) -> Vec<u8> {
        plain.to_vec()
    }
}
