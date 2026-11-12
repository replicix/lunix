//! Primitives (freeze item 1, on-disk-formats.md section 3) and their text
//! forms (section 22).

use core::fmt;
use core::str::FromStr;

use crate::bytes::{Put, Reader};
use crate::error::{FormatError, Result};

const WHAT: &str = "primitive";

/// Lowercase hex of `bytes`.
pub fn hex_lower(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push(char::from(DIGITS[usize::from(b >> 4)]));
        s.push(char::from(DIGITS[usize::from(b & 15)]));
    }
    s
}

/// Parses exactly `out.len() * 2` lowercase hex digits into `out`.
pub fn parse_hex_lower(s: &str, out: &mut [u8]) -> Result<()> {
    let b = s.as_bytes();
    if b.len() != out.len() * 2 {
        return Err(FormatError::invalid(WHAT, "wrong hex length"));
    }
    let nib = |c: u8| -> Result<u8> {
        match c {
            b'0'..=b'9' => Ok(c - b'0'),
            b'a'..=b'f' => Ok(c - b'a' + 10),
            _ => Err(FormatError::invalid(WHAT, "not lowercase hex")),
        }
    };
    for (i, o) in out.iter_mut().enumerate() {
        *o = nib(b[2 * i])? << 4 | nib(b[2 * i + 1])?;
    }
    Ok(())
}

/// A 32-byte chunk id (or map node hash in a map-node extent).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct ChunkId(pub [u8; 32]);

impl ChunkId {
    /// The bytes.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Debug for ChunkId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ChunkId({})", hex_lower(&self.0))
    }
}

impl fmt::Display for ChunkId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&hex_lower(&self.0))
    }
}

/// Map id-prefix length, from the descriptor: 0, 16 or 32.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum IdLen {
    /// Lean map: no prefix stored (decodes; `volume create` refuses it in v1).
    Zero,
    /// 16-byte prefix.
    Sixteen,
    /// 32-byte prefix.
    ThirtyTwo,
}

impl IdLen {
    /// Parses the descriptor byte.
    pub fn new(v: u8) -> Result<Self> {
        match v {
            0 => Ok(Self::Zero),
            16 => Ok(Self::Sixteen),
            32 => Ok(Self::ThirtyTwo),
            _ => Err(FormatError::range(WHAT, "id_len", u64::from(v))),
        }
    }

    /// The byte value, 0, 16 or 32.
    pub fn as_u8(self) -> u8 {
        match self {
            Self::Zero => 0,
            Self::Sixteen => 16,
            Self::ThirtyTwo => 32,
        }
    }

    /// The length as `usize`.
    pub fn len(self) -> usize {
        usize::from(self.as_u8())
    }

    /// True for the lean (0) length.
    pub fn is_empty(self) -> bool {
        self == Self::Zero
    }
}

/// The first `IdLen` bytes of a [`ChunkId`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct IdPrefix {
    len: IdLen,
    bytes: [u8; 32],
}

impl IdPrefix {
    /// Truncates `id` to `len` bytes.
    pub fn of(id: &ChunkId, len: IdLen) -> Self {
        let mut bytes = [0u8; 32];
        bytes[..len.len()].copy_from_slice(&id.0[..len.len()]);
        Self { len, bytes }
    }

    /// Builds a prefix from exactly `len` bytes.
    pub fn from_slice(len: IdLen, s: &[u8]) -> Result<Self> {
        if s.len() != len.len() {
            return Err(FormatError::invalid(WHAT, "id prefix length mismatch"));
        }
        let mut bytes = [0u8; 32];
        bytes[..s.len()].copy_from_slice(s);
        Ok(Self { len, bytes })
    }

    /// The stored length.
    pub fn id_len(&self) -> IdLen {
        self.len
    }

    /// The prefix bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len.len()]
    }

    /// True if `id` starts with this prefix.
    pub fn matches(&self, id: &ChunkId) -> bool {
        id.0[..self.len.len()] == self.bytes[..self.len.len()]
    }
}

/// An object id: `epoch << 40 | counter`. One id space per volume names
/// segments, map packs and index runs. Zero means "none" and is never an
/// `ObjId`; use `Option<ObjId>`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ObjId(u64);

/// A segment id.
pub type SegId = ObjId;
/// A map pack id.
pub type PackId = ObjId;
/// An index run id.
pub type RunId = ObjId;

impl ObjId {
    /// Highest counter value.
    pub const MAX_COUNTER: u64 = (1 << 40) - 1;
    /// Epochs below this fit an id.
    pub const EPOCH_LIMIT: u64 = 1 << 24;

    /// Builds an id from its parts; `counter` is 1 ..= 2^40 - 1.
    pub fn new(epoch: u64, counter: u64) -> Result<Self> {
        if epoch >= Self::EPOCH_LIMIT {
            return Err(FormatError::range(WHAT, "epoch", epoch));
        }
        if counter == 0 || counter > Self::MAX_COUNTER {
            return Err(FormatError::range(WHAT, "counter", counter));
        }
        Ok(Self(epoch << 40 | counter))
    }

    /// Parses the raw `u64` as stored. Zero (and a zero counter) is refused.
    pub fn from_raw(raw: u64) -> Result<Self> {
        if raw & Self::MAX_COUNTER == 0 {
            return Err(FormatError::range(WHAT, "object id", raw));
        }
        Ok(Self(raw))
    }

    /// Parses a stored `u64` where 0 means "none".
    pub fn from_raw_opt(raw: u64) -> Result<Option<Self>> {
        if raw == 0 {
            Ok(None)
        } else {
            Self::from_raw(raw).map(Some)
        }
    }

    /// The stored `u64`.
    pub fn raw(self) -> u64 {
        self.0
    }

    /// The epoch part.
    pub fn epoch(self) -> u64 {
        self.0 >> 40
    }

    /// The counter part.
    pub fn counter(self) -> u64 {
        self.0 & Self::MAX_COUNTER
    }

    /// The next id of the same epoch, or an error at the counter limit.
    pub fn next(self) -> Result<Self> {
        Self::new(self.epoch(), self.counter() + 1)
    }
}

impl fmt::Debug for ObjId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ObjId({:016x})", self.0)
    }
}

impl fmt::Display for ObjId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:016x}", self.0)
    }
}

impl FromStr for ObjId {
    type Err = FormatError;
    fn from_str(s: &str) -> Result<Self> {
        let mut b = [0u8; 8];
        parse_hex_lower(s, &mut b)?;
        Self::from_raw(u64::from_be_bytes(b))
    }
}

macro_rules! seq_newtype {
    ($(#[$m:meta])* $name:ident, $field:literal) => {
        $(#[$m])*
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Debug)]
        pub struct $name(pub u64);

        impl $name {
            /// The raw value.
            pub fn get(self) -> u64 {
                self.0
            }

            /// The next value, or an error on overflow.
            pub fn next(self) -> Result<Self> {
                self.0
                    .checked_add(1)
                    .map(Self)
                    .ok_or(FormatError::Overflow { what: $field, why: "sequence number" })
            }

            /// Checked addition.
            pub fn checked_add(self, n: u64) -> Result<Self> {
                self.0
                    .checked_add(n)
                    .map(Self)
                    .ok_or(FormatError::Overflow { what: $field, why: "sequence number" })
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{:016x}", self.0)
            }
        }
    };
}

seq_newtype!(
    /// Global write sequence (DESIGN section 8.1); `cut_seq` is a `WriteSeq`.
    WriteSeq,
    "write_seq"
);
seq_newtype!(
    /// Position of a commit in the chain; 0 is genesis (D5).
    CommitSeq,
    "commit_seq"
);

/// Owner generation; 0 for every local record and commit in phase 1.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Debug)]
pub struct Epoch(pub u64);

/// DEK generation (the KAT's `dek_epoch`).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Debug)]
pub struct DekGen(pub u32);

/// Milliseconds since the Unix epoch, UTC.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Debug)]
pub struct TimeMs(pub u64);

/// Index into the descriptor's `lineage[]` (0 = this volume; 0 in v1).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Debug)]
pub struct LineageIdx(pub u8);

/// Random 16-byte family id; text form is 32 lowercase hex digits.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct FamilyId(pub [u8; 16]);

impl fmt::Debug for FamilyId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "FamilyId({})", hex_lower(&self.0))
    }
}

impl fmt::Display for FamilyId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&hex_lower(&self.0))
    }
}

impl FromStr for FamilyId {
    type Err = FormatError;
    fn from_str(s: &str) -> Result<Self> {
        let mut b = [0u8; 16];
        parse_hex_lower(s, &mut b)?;
        Ok(Self(b))
    }
}

/// Volume uuid (RFC 9562 version 4); text form is the 36-character
/// lowercase hyphenated form.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct VolumeUuid(pub [u8; 16]);

impl fmt::Debug for VolumeUuid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "VolumeUuid({self})")
    }
}

impl fmt::Display for VolumeUuid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let h = hex_lower(&self.0);
        write!(
            f,
            "{}-{}-{}-{}-{}",
            &h[0..8],
            &h[8..12],
            &h[12..16],
            &h[16..20],
            &h[20..32]
        )
    }
}

impl FromStr for VolumeUuid {
    type Err = FormatError;
    fn from_str(s: &str) -> Result<Self> {
        let b = s.as_bytes();
        if b.len() != 36 || [8, 13, 18, 23].iter().any(|&i| b[i] != b'-') {
            return Err(FormatError::invalid(WHAT, "not a hyphenated uuid"));
        }
        let compact: String = s.chars().filter(|&c| c != '-').collect();
        let mut out = [0u8; 16];
        parse_hex_lower(&compact, &mut out)?;
        Ok(Self(out))
    }
}

/// Chunk codec ids of the `CCH1` registry.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum Codec {
    /// Stored as is.
    Raw = 0,
    /// One zstd frame.
    Zstd = 1,
    /// An LZ4 raw block with no size prefix.
    Lz4 = 2,
    /// zstd with a trained dictionary (framed in phase 1, decoded from phase 4).
    ZstdDict = 3,
}

impl Codec {
    /// Parses the stored `u16`; ids above 3 are rejected.
    pub fn from_u16(v: u16) -> Result<Self> {
        match v {
            0 => Ok(Self::Raw),
            1 => Ok(Self::Zstd),
            2 => Ok(Self::Lz4),
            3 => Ok(Self::ZstdDict),
            _ => Err(FormatError::range(WHAT, "codec", u64::from(v))),
        }
    }

    /// The stored id.
    pub fn id(self) -> u16 {
        self as u16
    }
}

/// Codec level as written (`i8`); not needed to decompress.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Debug)]
pub struct Level(pub i8);

impl Level {
    /// Checks the level against the codec: 0 for raw and LZ4, -7 ..= 22 for
    /// the zstd codecs.
    pub fn validate(self, codec: Codec) -> Result<()> {
        let ok = match codec {
            Codec::Raw | Codec::Lz4 => self.0 == 0,
            Codec::Zstd | Codec::ZstdDict => (-7..=22).contains(&self.0),
        };
        if ok {
            Ok(())
        } else {
            Err(FormatError::range(WHAT, "level", self.0 as u8 as u64))
        }
    }
}

/// What an extent holds. Never stored in the extent: it comes from the
/// container (`segments/` or `map/`).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum ExtentKind {
    /// A data chunk.
    Data = 0,
    /// A map node.
    Node = 1,
}

impl ExtentKind {
    /// Parses the byte used in the AAD and `info`.
    pub fn from_u8(v: u8) -> Result<Self> {
        match v {
            0 => Ok(Self::Data),
            1 => Ok(Self::Node),
            _ => Err(FormatError::range(WHAT, "extent kind", u64::from(v))),
        }
    }
}

/// Location of an extent: container id, offset and payload length.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Hint {
    /// The segment (or map pack).
    pub seg: ObjId,
    /// Offset of the extent in the container.
    pub off: u32,
    /// Payload length (`clen`); the extent occupies `OVH + clen` bytes.
    pub clen: u32,
}

impl Hint {
    /// Encoded length.
    pub const LEN: usize = 16;

    /// Appends the 16-byte form.
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.put_u64(self.seg.raw());
        out.put_u32(self.off);
        out.put_u32(self.clen);
    }

    /// Reads one hint from `r`.
    pub fn read(r: &mut Reader<'_>) -> Result<Self> {
        let seg = ObjId::from_raw(r.u64()?)?;
        let off = r.u32()?;
        let clen = r.u32()?;
        Ok(Self { seg, off, clen })
    }

    /// Decodes exactly one hint.
    pub fn decode(b: &[u8]) -> Result<Self> {
        let mut r = Reader::new(b, "hint");
        let h = Self::read(&mut r)?;
        r.finish()?;
        Ok(h)
    }
}
