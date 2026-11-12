//! Segment and map-pack container (freeze item 4, on-disk-formats.md
//! section 7).
//!
//! ```text
//! [extent]*  [footer envelope]  [trailer (64 bytes)]
//! ```
//!
//! The footer is a metadata envelope of kind 6 around a single zstd frame
//! whose decoded bytes are the [`FooterBody`]. This crate frames and parses;
//! the MAC (form 0) or the AEAD (form 1) over the frame is `lunix-crypto`'s.

use crate::bytes::{crc32c, Put, Reader};
use crate::envelope::{self, Envelope};
use crate::error::{FormatError, Result};
use crate::ids::{ChunkId, Codec, ObjId, WriteSeq};
use crate::mode::{Form, ObjectKind};

const WHAT: &str = "container";

/// Trailer length.
pub const TRAILER_LEN: usize = 64;
/// Trailer magic.
pub const TRAILER_MAGIC: &[u8; 4] = b"LXST";
/// Container and footer version.
pub const CONTAINER_VER: u8 = 1;
/// Footer entry length.
pub const ENTRY_LEN: usize = 48;
/// Fixed part of the footer body.
pub const FOOTER_FIXED_LEN: usize = 72;
/// Shortest possible extent (`OVH` 68 plus one payload byte).
pub const MIN_EXTENT_LEN: u64 = 69;
/// Longest container (every `Hint.off` fits a `u32`).
pub const MAX_CONTAINER_LEN: u64 = u32::MAX as u64;
/// Smallest footer envelope (form 0 with an empty body).
pub const MIN_FOOTER_LEN: u32 = 44;
/// Largest `ulen` of a data extent.
pub const MAX_DATA_ULEN: u32 = 1 << 20;
/// Largest `ulen` of a map node extent.
pub const MAX_NODE_ULEN: u32 = 65_536;

/// Which container this is.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum ContainerKind {
    /// `segments/<seg>`: data extents.
    Segment = 0,
    /// `map/<pack>`: map node extents.
    MapPack = 1,
}

impl ContainerKind {
    /// Parses the stored byte.
    pub fn from_u8(v: u8) -> Result<Self> {
        match v {
            0 => Ok(Self::Segment),
            1 => Ok(Self::MapPack),
            _ => Err(FormatError::range(WHAT, "container_kind", u64::from(v))),
        }
    }
}

/// The last 64 bytes of a container. Not authenticated: a wrong value only
/// makes the footer fail its MAC or AEAD.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Trailer {
    /// Length of the extent region; the footer starts here.
    pub footer_off: u64,
    /// Length of the footer envelope.
    pub footer_len: u32,
    /// Number of extents.
    pub extent_count: u32,
    /// The id in the object's name.
    pub object_id: ObjId,
    /// Segment or map pack.
    pub container_kind: ContainerKind,
}

impl Trailer {
    /// Appends the 64-byte trailer.
    pub fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        self.check()?;
        let start = out.len();
        out.put_u64(self.footer_off);
        out.put_u32(self.footer_len);
        out.put_u32(self.extent_count);
        out.put_u64(self.object_id.raw());
        out.put_u8(CONTAINER_VER);
        out.put_u8(self.container_kind as u8);
        out.put_zeros(30);
        let crc = crc32c(&out[start..]);
        out.put_u32(crc);
        out.extend_from_slice(TRAILER_MAGIC);
        Ok(())
    }

    fn check(&self) -> Result<()> {
        if self.footer_len < MIN_FOOTER_LEN {
            return Err(FormatError::range(
                WHAT,
                "footer_len",
                u64::from(self.footer_len),
            ));
        }
        if self.extent_count == 0
            || u64::from(self.extent_count).saturating_mul(MIN_EXTENT_LEN) > self.footer_off
        {
            return Err(FormatError::range(
                WHAT,
                "extent_count",
                u64::from(self.extent_count),
            ));
        }
        let total = self
            .footer_off
            .checked_add(u64::from(self.footer_len))
            .and_then(|v| v.checked_add(TRAILER_LEN as u64))
            .ok_or(FormatError::Overflow {
                what: WHAT,
                why: "container length",
            })?;
        if total > MAX_CONTAINER_LEN {
            return Err(FormatError::range(WHAT, "container length", total));
        }
        Ok(())
    }

    /// Decodes exactly 64 bytes.
    pub fn decode(b: &[u8]) -> Result<Self> {
        let mut r = Reader::new(b, "trailer");
        let footer_off = r.u64()?;
        let footer_len = r.u32()?;
        let extent_count = r.u32()?;
        let raw_id = r.u64()?;
        r.version_u8(CONTAINER_VER)?;
        let container_kind = ContainerKind::from_u8(r.u8()?)?;
        r.reserved(30)?;
        let crc = r.u32()?;
        r.magic(TRAILER_MAGIC)?;
        r.finish()?;
        if crc32c(&b[..56]) != crc {
            return Err(FormatError::Checksum { what: "trailer" });
        }
        let t = Self {
            footer_off,
            footer_len,
            extent_count,
            object_id: ObjId::from_raw(raw_id)?,
            container_kind,
        };
        t.check()?;
        Ok(t)
    }

    /// Reads the trailer at the end of a whole container and checks
    /// `footer_off + footer_len + 64 == container.len()`.
    pub fn decode_from_container(container: &[u8]) -> Result<Self> {
        let n = container.len();
        let start = n.checked_sub(TRAILER_LEN).ok_or(FormatError::Truncated {
            what: WHAT,
            need: TRAILER_LEN,
            have: n,
        })?;
        let t = Self::decode(&container[start..])?;
        if t.footer_off + u64::from(t.footer_len) + TRAILER_LEN as u64 != n as u64 {
            return Err(FormatError::invalid(
                WHAT,
                "trailer does not match the container length",
            ));
        }
        Ok(t)
    }
}

/// A container split at the trailer's offsets.
#[derive(Clone, Copy, Debug)]
pub struct ContainerParts<'a> {
    /// The trailer.
    pub trailer: Trailer,
    /// The extent region `[0, footer_off)`.
    pub extents: &'a [u8],
    /// The footer envelope.
    pub footer: &'a [u8],
}

/// Splits a whole container into extents, footer envelope and trailer.
pub fn split_container(container: &[u8]) -> Result<ContainerParts<'_>> {
    let trailer = Trailer::decode_from_container(container)?;
    let off = usize::try_from(trailer.footer_off)
        .map_err(|_| FormatError::range(WHAT, "footer_off", trailer.footer_off))?;
    let end = off + trailer.footer_len as usize;
    Ok(ContainerParts {
        trailer,
        extents: &container[..off],
        footer: &container[off..end],
    })
}

/// Assembles `extents ‖ footer envelope ‖ trailer`.
pub fn assemble_container(
    extents: &[u8],
    footer_envelope: &[u8],
    object_id: ObjId,
    container_kind: ContainerKind,
    extent_count: u32,
) -> Result<Vec<u8>> {
    let t = Trailer {
        footer_off: extents.len() as u64,
        footer_len: u32::try_from(footer_envelope.len())
            .map_err(|_| FormatError::range(WHAT, "footer_len", footer_envelope.len() as u64))?,
        extent_count,
        object_id,
        container_kind,
    };
    let mut v = Vec::with_capacity(extents.len() + footer_envelope.len() + TRAILER_LEN);
    v.extend_from_slice(extents);
    v.extend_from_slice(footer_envelope);
    t.encode(&mut v)?;
    Ok(v)
}

/// One footer entry: the index of one extent.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct FooterEntry {
    /// The extent's `chunk_id` (the node hash in a map pack).
    pub chunk_id: ChunkId,
    /// Offset of the extent in the container.
    pub off: u32,
    /// Payload length.
    pub clen: u32,
    /// Uncompressed length.
    pub ulen: u32,
    /// Codec.
    pub codec: Codec,
}

/// The decoded footer body (section 7.2).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct FooterBody {
    /// Segment or map pack.
    pub container_kind: ContainerKind,
    /// The id in the object's name.
    pub object_id: ObjId,
    /// Length of the extent region (= the trailer's `footer_off`).
    pub extents_len: u64,
    /// Lowest `write_seq` whose data is in this container; 0 when none.
    pub write_seq_lo: WriteSeq,
    /// Highest such `write_seq`; 0 when none.
    pub write_seq_hi: WriteSeq,
    /// BLAKE3-256 of the extent region.
    pub seg_hash: [u8; 32],
    /// Entries, sorted strictly by (`chunk_id`, `off`).
    pub entries: Vec<FooterEntry>,
    /// zstd dictionary ids used by codec-3 extents, sorted ascending, unique.
    pub dict_ids: Vec<u32>,
}

/// Cap on the decoded footer body for an extent region of `extents_len`:
/// `72 + 52 * floor(extents_len / 69)`.
pub fn footer_body_cap(extents_len: u64) -> Result<usize> {
    (extents_len / MIN_EXTENT_LEN)
        .checked_mul(52)
        .and_then(|v| v.checked_add(FOOTER_FIXED_LEN as u64))
        .and_then(|v| usize::try_from(v).ok())
        .ok_or(FormatError::Overflow {
            what: "footer",
            why: "body cap",
        })
}

fn ulen_limit(kind: ContainerKind) -> u32 {
    match kind {
        ContainerKind::Segment => MAX_DATA_ULEN,
        ContainerKind::MapPack => MAX_NODE_ULEN,
    }
}

impl FooterBody {
    fn check(&self) -> Result<()> {
        const W: &str = "footer";
        let n = self.entries.len() as u64;
        if n == 0 || n > self.extents_len / MIN_EXTENT_LEN {
            return Err(FormatError::count(
                W,
                "extent_count",
                n,
                self.extents_len / MIN_EXTENT_LEN,
            ));
        }
        if self.write_seq_hi < self.write_seq_lo {
            return Err(FormatError::invalid(W, "write_seq_hi below write_seq_lo"));
        }
        let limit = ulen_limit(self.container_kind);
        let mut dict_users = 0u64;
        for w in self.entries.windows(2) {
            if (w[0].chunk_id, w[0].off) >= (w[1].chunk_id, w[1].off) {
                return Err(FormatError::not_canonical(W, "entries not strictly sorted"));
            }
        }
        for e in &self.entries {
            if e.ulen == 0 || e.ulen > limit {
                return Err(FormatError::range(W, "ulen", u64::from(e.ulen)));
            }
            let ok = if e.codec == Codec::Raw {
                e.clen == e.ulen
            } else {
                (1..=e.ulen).contains(&e.clen)
            };
            if !ok {
                return Err(FormatError::range(W, "clen", u64::from(e.clen)));
            }
            if e.codec == Codec::ZstdDict {
                dict_users += 1;
            }
        }
        let m = self.dict_ids.len() as u64;
        if self.container_kind == ContainerKind::MapPack && m != 0 {
            return Err(FormatError::invalid(W, "map pack with dictionary ids"));
        }
        if (m == 0) != (dict_users == 0) || m > dict_users || m > u64::from(u16::MAX) {
            return Err(FormatError::invalid(
                W,
                "n_dict_ids does not match codec-3 extents",
            ));
        }
        if self.dict_ids.windows(2).any(|w| w[0] >= w[1]) {
            return Err(FormatError::not_canonical(
                W,
                "dict_ids not sorted and unique",
            ));
        }
        Ok(())
    }

    /// Appends the footer body bytes (before zstd framing).
    pub fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        self.check()?;
        out.put_u8(CONTAINER_VER);
        out.put_u8(self.container_kind as u8);
        out.put_u16(self.dict_ids.len() as u16);
        out.put_u32(self.entries.len() as u32);
        out.put_u64(self.object_id.raw());
        out.put_u64(self.extents_len);
        out.put_u64(self.write_seq_lo.0);
        out.put_u64(self.write_seq_hi.0);
        out.extend_from_slice(&self.seg_hash);
        for e in &self.entries {
            out.extend_from_slice(&e.chunk_id.0);
            out.put_u32(e.off);
            out.put_u32(e.clen);
            out.put_u32(e.ulen);
            out.put_u8(e.codec as u8);
            out.put_zeros(3);
        }
        for d in &self.dict_ids {
            out.put_u32(*d);
        }
        Ok(())
    }

    /// Decodes a footer body; the whole input must be one body.
    pub fn decode(buf: &[u8]) -> Result<Self> {
        let mut r = Reader::new(buf, "footer");
        r.version_u8(CONTAINER_VER)?;
        let container_kind = ContainerKind::from_u8(r.u8()?)?;
        let n_dict = usize::from(r.u16()?);
        let extent_count = u64::from(r.u32()?);
        let object_id = ObjId::from_raw(r.u64()?)?;
        let extents_len = r.u64()?;
        let lo = WriteSeq(r.u64()?);
        let hi = WriteSeq(r.u64()?);
        let seg_hash = r.array::<32>()?;
        let n = r.check_count(
            "extent_count",
            extent_count,
            extents_len / MIN_EXTENT_LEN,
            ENTRY_LEN,
        )?;
        if n == 0 {
            return Err(FormatError::count("footer", "extent_count", 0, 0));
        }
        let mut entries = Vec::with_capacity(n);
        for _ in 0..n {
            let chunk_id = ChunkId(r.array::<32>()?);
            let off = r.u32()?;
            let clen = r.u32()?;
            let ulen = r.u32()?;
            let codec = Codec::from_u16(u16::from(r.u8()?))?;
            r.reserved(3)?;
            entries.push(FooterEntry {
                chunk_id,
                off,
                clen,
                ulen,
                codec,
            });
        }
        r.check_count("n_dict_ids", n_dict as u64, n as u64, 4)?;
        let mut dict_ids = Vec::with_capacity(n_dict);
        for _ in 0..n_dict {
            dict_ids.push(r.u32()?);
        }
        r.finish()?;
        let b = Self {
            container_kind,
            object_id,
            extents_len,
            write_seq_lo: lo,
            write_seq_hi: hi,
            seg_hash,
            entries,
            dict_ids,
        };
        b.check()?;
        Ok(b)
    }

    /// Checks the footer against the trailer.
    pub fn check_trailer(&self, t: &Trailer) -> Result<()> {
        if self.container_kind != t.container_kind
            || self.object_id != t.object_id
            || self.extents_len != t.footer_off
            || self.entries.len() as u64 != u64::from(t.extent_count)
        {
            return Err(FormatError::invalid(
                "footer",
                "footer and trailer disagree",
            ));
        }
        Ok(())
    }

    /// The tiling rule: the extents (`ovh + clen` bytes each, `ovh` from
    /// [`crate::mode::CryptoMode::extent_ovh`]) tile `[0, extents_len)`
    /// exactly, with no gap and no overlap.
    pub fn check_tiling(&self, ovh: u32) -> Result<()> {
        let mut spans: Vec<(u32, u64)> = self
            .entries
            .iter()
            .map(|e| (e.off, u64::from(ovh) + u64::from(e.clen)))
            .collect();
        spans.sort_unstable();
        let mut cursor = 0u64;
        for (off, len) in spans {
            if u64::from(off) != cursor {
                return Err(FormatError::invalid(
                    "footer",
                    "extents do not tile the region",
                ));
            }
            cursor += len;
        }
        if cursor != self.extents_len {
            return Err(FormatError::invalid(
                "footer",
                "extents do not tile the region",
            ));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// The zstd frame around the footer body.

/// How [`footer_frame`] writes the zstd frame.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FrameCompression {
    /// A hand-written single raw block (bodies under 128 KiB).
    Raw,
    /// A zstd frame at the given level, with the content size recorded.
    Zstd(i32),
}

const MAX_RAW_BLOCK: usize = 128 * 1024;

fn raw_frame(body: &[u8]) -> Result<Vec<u8>> {
    if body.len() >= MAX_RAW_BLOCK {
        return Err(FormatError::range(
            "footer frame",
            "raw body length",
            body.len() as u64,
        ));
    }
    let n = body.len() as u64;
    let mut v = Vec::with_capacity(body.len() + 16);
    v.extend_from_slice(&[0x28, 0xb5, 0x2f, 0xfd]);
    // Frame_Header_Descriptor: single segment, FCS field size from the length.
    if n <= 255 {
        v.put_u8(0x20);
        v.put_u8(n as u8);
    } else if n <= 256 + 65_535 {
        v.put_u8(0x60);
        v.put_u16((n - 256) as u16);
    } else {
        v.put_u8(0xa0);
        v.put_u32(n as u32);
    }
    // Block header: last block, raw, size.
    v.put_u24((n as u32) << 3 | 1);
    v.extend_from_slice(body);
    Ok(v)
}

/// Encodes the footer body into the zstd frame that the envelope carries
/// (the bytes the MAC covers or the AEAD seals).
pub fn footer_frame(body: &FooterBody, how: FrameCompression) -> Result<Vec<u8>> {
    let mut raw = Vec::with_capacity(FOOTER_FIXED_LEN + ENTRY_LEN * body.entries.len());
    body.encode(&mut raw)?;
    match how {
        FrameCompression::Raw => raw_frame(&raw),
        FrameCompression::Zstd(level) => {
            zstd::bulk::compress(&raw, level).map_err(|_| FormatError::Compression {
                what: "footer frame",
                why: "zstd compress failed",
            })
        }
    }
}

/// The declared content size of a single-frame zstd stream without a
/// dictionary, or an error for anything else.
fn frame_content_size(frame: &[u8]) -> Result<u64> {
    const W: &str = "footer frame";
    let mut r = Reader::new(frame, W);
    r.magic(&[0x28, 0xb5, 0x2f, 0xfd])?;
    let d = r.u8()?;
    if d & 0b0001_1000 != 0 || d & 0b11 != 0 {
        return Err(FormatError::invalid(W, "reserved bit or dictionary id set"));
    }
    let fcs_flag = d >> 6;
    let single = d & 0x20 != 0;
    if !single {
        r.u8()?; // window descriptor
    }
    Ok(match (fcs_flag, single) {
        (0, true) => u64::from(r.u8()?),
        (0, false) => {
            return Err(FormatError::invalid(W, "content size not recorded"));
        }
        (1, _) => u64::from(r.u16()?) + 256,
        (2, _) => u64::from(r.u32()?),
        _ => r.u64()?,
    })
}

/// Decodes a footer frame (the opened bytes of the envelope).
///
/// `extents_len` is the trailer's `footer_off`; it sets the cap on the
/// decoded size, which is checked against the frame's declared content size
/// before anything is decompressed. The frame must be exactly one frame.
pub fn decode_footer_frame(frame: &[u8], extents_len: u64) -> Result<FooterBody> {
    const W: &str = "footer frame";
    let cap = footer_body_cap(extents_len)?;
    let size = frame_content_size(frame)?;
    if size > cap as u64 || size < FOOTER_FIXED_LEN as u64 {
        return Err(FormatError::count(W, "content size", size, cap as u64));
    }
    let used = zstd::zstd_safe::find_frame_compressed_size(frame).map_err(|_| {
        FormatError::Compression {
            what: W,
            why: "not a complete frame",
        }
    })?;
    if used != frame.len() {
        return Err(FormatError::Trailing {
            what: W,
            extra: frame.len() - used,
        });
    }
    let body =
        zstd::bulk::decompress(frame, size as usize).map_err(|_| FormatError::Compression {
            what: W,
            why: "zstd decode failed",
        })?;
    if body.len() as u64 != size {
        return Err(FormatError::invalid(
            W,
            "decoded size differs from the declared size",
        ));
    }
    let b = FooterBody::decode(&body)?;
    if b.extents_len != extents_len {
        return Err(FormatError::invalid(
            W,
            "footer extents_len differs from the trailer",
        ));
    }
    Ok(b)
}

/// Wraps a footer frame in a form 0 envelope (plain and integrity modes).
pub fn encode_footer_form0(frame: &[u8], mac: &[u8; 32]) -> Result<Vec<u8>> {
    envelope::encode_form0(ObjectKind::Footer, frame, mac)
}

/// The form 0 bytes the MAC covers after the AAD.
pub fn footer_form0_signed(frame: &[u8]) -> Result<Vec<u8>> {
    envelope::form0_unsigned(ObjectKind::Footer, frame)
}

/// Wraps a sealed footer frame in a form 1 envelope (encrypted mode).
pub fn encode_footer_form1(nonce: &[u8; 24], ciphertext: &[u8], tag: &[u8; 16]) -> Result<Vec<u8>> {
    envelope::encode_form1(ObjectKind::Footer, nonce, ciphertext, tag)
}

/// Parses a footer envelope of the given form. `footer_len` bounds the body.
pub fn decode_footer_envelope(buf: &[u8], form: Form) -> Result<Envelope<'_>> {
    let max = u32::try_from(buf.len()).unwrap_or(u32::MAX);
    Envelope::decode(buf, &[ObjectKind::Footer], form, max)
}
