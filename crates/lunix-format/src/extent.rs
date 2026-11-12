//! Extent (freeze item 3, on-disk-formats.md section 6).
//!
//! One stored chunk or map node: a `CCH1` header, then Lunix's own fields,
//! the payload and a 16-byte tag. An extent is **not** a `CCH1` object; its
//! `CCH1` object is `extent[0..16] ‖ payload` (after opening, when
//! encrypted).
//!
//! This module frames extents and builds the AAD, the key-derivation `info`
//! and the tag input. Computing tags and AEAD is `lunix-crypto`'s.

use crate::bytes::{Put, Reader};
use crate::error::{FormatError, Result};
use crate::ids::{ChunkId, Codec, DekGen, ExtentKind, FamilyId, IdLen, Level};
use crate::mode::{OVH_ENCRYPTED, OVH_UNENCRYPTED};

const WHAT: &str = "extent";

/// `CCH1` magic.
pub const CCH1_MAGIC: &[u8; 4] = b"CCH1";
/// Length of the `CCH1` header.
pub const CCH1_HEADER_LEN: usize = 16;
/// Bytes before the payload of a plain or integrity extent.
pub const PLAIN_PREFIX_LEN: usize = 52;
/// Bytes before the payload of an encrypted extent.
pub const ENCRYPTED_PREFIX_LEN: usize = 92;
/// Tag length.
pub const TAG_LEN: usize = 16;
/// Largest `ulen` (the largest chunk size).
pub const MAX_ULEN: u64 = 1 << 20;
/// Largest `ulen` of a map node.
pub const MAX_NODE_ULEN: u64 = 65_536;
/// Length of the AAD with a 32-byte chunk id.
pub const AAD_LEN: usize = 61;
/// Length of the key-derivation `info`.
pub const INFO_LEN: usize = 100;
/// `"lunix extent v2\0"`.
pub const INFO_LABEL: &[u8; 16] = b"lunix extent v2\0";

/// Which extent layout a volume uses.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ExtentLayout {
    /// Plain and integrity volumes: no crypto region.
    Unencrypted,
    /// Encrypted volumes: `dek_gen`, `salt`, `nonce` after `clen`.
    Encrypted,
}

impl ExtentLayout {
    /// `OVH`: bytes an extent adds to its payload.
    pub fn ovh(self) -> u32 {
        match self {
            Self::Unencrypted => OVH_UNENCRYPTED,
            Self::Encrypted => OVH_ENCRYPTED,
        }
    }

    /// Bytes before the payload.
    pub fn prefix_len(self) -> usize {
        match self {
            Self::Unencrypted => PLAIN_PREFIX_LEN,
            Self::Encrypted => ENCRYPTED_PREFIX_LEN,
        }
    }
}

/// The 16-byte `CCH1` header as `chunkfmt` writes it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Cch1Header {
    /// Codec, 0 ..= 3.
    pub codec: Codec,
    /// Codec level as written.
    pub level: Level,
    /// Uncompressed length.
    pub ulen: u64,
}

impl Cch1Header {
    /// Appends the 16 bytes.
    pub fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        self.level.validate(self.codec)?;
        if self.ulen == 0 || self.ulen > MAX_ULEN {
            return Err(FormatError::range(WHAT, "ulen", self.ulen));
        }
        out.extend_from_slice(CCH1_MAGIC);
        out.put_u8(1);
        out.put_u16(self.codec.id());
        out.put_u8(self.level.0 as u8);
        out.put_u64(self.ulen);
        Ok(())
    }

    /// Reads the header from `r`.
    pub fn read(r: &mut Reader<'_>) -> Result<Self> {
        r.magic(CCH1_MAGIC)?;
        r.version_u8(1)?;
        let codec = Codec::from_u16(r.u16()?)?;
        let level = Level(r.i8()?);
        level.validate(codec)?;
        let ulen = r.u64()?;
        if ulen == 0 || ulen > MAX_ULEN {
            return Err(FormatError::range(WHAT, "ulen", ulen));
        }
        Ok(Self { codec, level, ulen })
    }

    /// Parses just the first 16 bytes of an extent.
    pub fn parse(extent: &[u8]) -> Result<Self> {
        Self::read(&mut Reader::new(extent, WHAT))
    }
}

/// The plain prefix: `CCH1` header, `chunk_id` and `clen` (52 bytes).
pub fn parse_plain_prefix(extent: &[u8]) -> Result<(Cch1Header, ChunkId, u32)> {
    let mut r = Reader::new(extent, WHAT);
    let h = Cch1Header::read(&mut r)?;
    let id = ChunkId(r.array()?);
    let clen = r.u32()?;
    Ok((h, id, clen))
}

/// The bytes the tag covers: every byte before the 16-byte tag (D11).
pub fn tag_input(extent: &[u8]) -> Result<&[u8]> {
    extent
        .len()
        .checked_sub(TAG_LEN)
        .map(|n| &extent[..n])
        .ok_or(FormatError::Truncated {
            what: WHAT,
            need: TAG_LEN,
            have: extent.len(),
        })
}

/// The crypto region of an encrypted extent.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CryptoRegion {
    /// The DEK generation.
    pub dek_gen: DekGen,
    /// Random salt, drawn inside the seal function.
    pub salt: [u8; 24],
    /// Zero in the default profile; the module nonce in the FIPS profile.
    pub nonce: [u8; 12],
}

/// A parsed or to-be-written extent.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Extent<'a> {
    /// The `CCH1` header.
    pub header: Cch1Header,
    /// The chunk id (the node hash for a map node).
    pub chunk_id: ChunkId,
    /// Present exactly for the encrypted layout.
    pub crypto: Option<CryptoRegion>,
    /// Codec output (ciphertext when encrypted); its length is `clen`.
    pub payload: &'a [u8],
    /// BLAKE3-128 tag, keyed tag, or AEAD tag.
    pub tag: [u8; 16],
}

/// What a reader expects of an extent.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DecodeParams {
    /// Layout of the volume.
    pub layout: ExtentLayout,
    /// Data chunk or map node.
    pub kind: ExtentKind,
    /// The volume's chunk size; a data extent's `ulen` must equal it.
    pub chunk_size: u32,
    /// Accept a non-zero nonce (FIPS profile only).
    pub allow_nonzero_nonce: bool,
}

impl Extent<'_> {
    /// The layout this extent is written in.
    pub fn layout(&self) -> ExtentLayout {
        if self.crypto.is_some() {
            ExtentLayout::Encrypted
        } else {
            ExtentLayout::Unencrypted
        }
    }

    /// Total length, `OVH + clen`.
    pub fn len(&self) -> usize {
        self.layout().prefix_len() + self.payload.len() + TAG_LEN
    }

    /// Never empty; present for the usual `len`/`is_empty` pair.
    pub fn is_empty(&self) -> bool {
        false
    }

    fn check_clen(codec: Codec, ulen: u64, clen: u64) -> Result<()> {
        let ok = match codec {
            Codec::Raw => clen == ulen,
            _ => (1..=ulen).contains(&clen),
        };
        if ok {
            Ok(())
        } else {
            Err(FormatError::range(WHAT, "clen", clen))
        }
    }

    /// Appends everything but the tag; the caller hashes or seals these
    /// bytes (they are [`tag_input`]) and then appends the tag.
    pub fn encode_untagged(&self, out: &mut Vec<u8>) -> Result<()> {
        Self::check_clen(
            self.header.codec,
            self.header.ulen,
            self.payload.len() as u64,
        )?;
        let clen = u32::try_from(self.payload.len())
            .map_err(|_| FormatError::range(WHAT, "clen", self.payload.len() as u64))?;
        self.header.encode(out)?;
        out.extend_from_slice(&self.chunk_id.0);
        out.put_u32(clen);
        if let Some(c) = &self.crypto {
            out.put_u32(c.dek_gen.0);
            out.extend_from_slice(&c.salt);
            out.extend_from_slice(&c.nonce);
        }
        out.extend_from_slice(self.payload);
        Ok(())
    }

    /// Appends the whole extent.
    pub fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        self.encode_untagged(out)?;
        out.extend_from_slice(&self.tag);
        Ok(())
    }

    /// Parses the extent at the start of `buf`, returning it and its length.
    pub fn decode_prefix(buf: &[u8], p: DecodeParams) -> Result<(Extent<'_>, usize)> {
        let mut r = Reader::new(buf, WHAT);
        let header = Cch1Header::read(&mut r)?;
        match p.kind {
            ExtentKind::Data => {
                if header.ulen != u64::from(p.chunk_size) {
                    return Err(FormatError::range(WHAT, "ulen", header.ulen));
                }
            }
            ExtentKind::Node => {
                if header.ulen > MAX_NODE_ULEN {
                    return Err(FormatError::range(WHAT, "ulen", header.ulen));
                }
            }
        }
        let chunk_id = ChunkId(r.array()?);
        let clen = r.u32()?;
        Self::check_clen(header.codec, header.ulen, u64::from(clen))?;
        let crypto = match p.layout {
            ExtentLayout::Unencrypted => None,
            ExtentLayout::Encrypted => {
                let dek_gen = DekGen(r.u32()?);
                let salt = r.array()?;
                let nonce: [u8; 12] = r.array()?;
                if !p.allow_nonzero_nonce && nonce.iter().any(|&b| b != 0) {
                    return Err(FormatError::invalid(
                        WHAT,
                        "nonce must be zero in the default profile",
                    ));
                }
                Some(CryptoRegion {
                    dek_gen,
                    salt,
                    nonce,
                })
            }
        };
        let payload = r.take(clen as usize)?;
        let tag = r.array()?;
        let used = r.pos();
        Ok((
            Extent {
                header,
                chunk_id,
                crypto,
                payload,
                tag,
            },
            used,
        ))
    }

    /// Decodes exactly one extent.
    pub fn decode(buf: &[u8], p: DecodeParams) -> Result<Extent<'_>> {
        let (e, used) = Self::decode_prefix(buf, p)?;
        if used != buf.len() {
            return Err(FormatError::Trailing {
                what: WHAT,
                extra: buf.len() - used,
            });
        }
        Ok(e)
    }
}

/// The AEAD associated data (61 bytes with a 32-byte id), D2: canonical
/// 1/1/4 widths, not the stored header bytes.
///
/// `id_len` is the descriptor's value, not the length of the field.
pub fn aad(
    header: &Cch1Header,
    kind: ExtentKind,
    dek_gen: DekGen,
    family: &FamilyId,
    id_len: IdLen,
    chunk_id: &ChunkId,
) -> Result<[u8; AAD_LEN]> {
    let ulen = u32::try_from(header.ulen)
        .ok()
        .filter(|&u| u64::from(u) <= MAX_ULEN)
        .ok_or(FormatError::range(WHAT, "ulen", header.ulen))?;
    let codec = u8::try_from(header.codec.id())
        .map_err(|_| FormatError::range(WHAT, "codec", u64::from(header.codec.id())))?;
    let mut v = Vec::with_capacity(AAD_LEN);
    v.put_u8(1);
    v.put_u8(codec);
    v.put_u8(header.level.0 as u8);
    v.put_u32(ulen);
    v.put_u8(kind as u8);
    v.put_u32(dek_gen.0);
    v.extend_from_slice(&family.0);
    v.put_u8(id_len.as_u8());
    v.extend_from_slice(&chunk_id.0);
    v.try_into()
        .map_err(|_| FormatError::invalid(WHAT, "aad length"))
}

/// Everything `info` takes besides the header.
#[derive(Clone, Copy, Debug)]
pub struct InfoParts<'a> {
    /// Family id from the descriptor.
    pub family: &'a FamilyId,
    /// Data or node, from the container.
    pub kind: ExtentKind,
    /// The extent's `dek_gen`.
    pub dek_gen: DekGen,
    /// The extent's codec.
    pub codec: Codec,
    /// The extent's level.
    pub level: Level,
    /// Payload length.
    pub clen: u32,
    /// Descriptor `id_len`.
    pub id_len: IdLen,
    /// The extent's chunk id.
    pub chunk_id: &'a ChunkId,
    /// The extent's salt.
    pub salt: &'a [u8; 24],
}

/// The extent-key derivation `info` (100 bytes), D2.
pub fn info(p: &InfoParts<'_>) -> Result<[u8; INFO_LEN]> {
    let codec = u8::try_from(p.codec.id())
        .map_err(|_| FormatError::range(WHAT, "codec", u64::from(p.codec.id())))?;
    let mut v = Vec::with_capacity(INFO_LEN);
    v.extend_from_slice(INFO_LABEL);
    v.extend_from_slice(&p.family.0);
    v.put_u8(p.kind as u8);
    v.put_u32(p.dek_gen.0);
    v.put_u8(codec);
    v.put_u8(p.level.0 as u8);
    v.put_u32(p.clen);
    v.put_u8(p.id_len.as_u8());
    v.extend_from_slice(&p.chunk_id.0);
    v.extend_from_slice(p.salt);
    v.try_into()
        .map_err(|_| FormatError::invalid(WHAT, "info length"))
}
