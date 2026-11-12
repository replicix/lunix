//! The `volume` descriptor (freeze item 8, on-disk-formats.md section 11).
//!
//! The descriptor is create-only and plaintext. The local `volume` file holds
//! exactly these bytes. The MAC (or, in plain mode, the BLAKE3-256 checksum)
//! is carried as given; computing and verifying it is `lunix-crypto`'s, over
//! [`VolumeDescriptor::mac_input`].

use crate::bytes::Put;
use crate::bytes::Reader;
use crate::error::{FormatError, Result};
use crate::ids::{CommitSeq, FamilyId, IdLen, TimeMs, VolumeUuid};
use crate::mode::{CryptoMode, Profile};

const WHAT: &str = "volume descriptor";

/// Descriptor magic.
pub const MAGIC: &[u8; 8] = b"LUNIXVOL";
/// Descriptor `format_ver`.
pub const FORMAT_VER: u16 = 1;
/// The only legal block size.
pub const BLOCK_SIZE: u32 = 4096;
/// Smallest chunk size (16 KiB).
pub const MIN_CHUNK_SIZE: u32 = 16 * 1024;
/// Largest chunk size (1 MiB).
pub const MAX_CHUNK_SIZE: u32 = 1024 * 1024;
/// Fixed part before `lineage[]`.
pub const FIXED_LEN: usize = 88;
/// MAC length.
pub const MAC_LEN: usize = 32;
/// Length of a descriptor with no lineage.
pub const BASE_LEN: usize = FIXED_LEN + MAC_LEN;
/// Longest lineage `backend_hint`.
pub const MAX_BACKEND_HINT: usize = 1024;
/// Most lineage entries.
pub const MAX_LINEAGE: usize = 255;
/// `"lunix volume v1\0"`, the label of the descriptor MAC input.
pub const MAC_LABEL: &[u8; 16] = b"lunix volume v1\0";
const LINEAGE_MIN: usize = 16 + 8 + 2;

/// How chunks are compressed (ADR-12).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum CodecPolicy {
    /// Store raw.
    None = 0,
    /// Probe, then zstd at `codec_level`.
    Auto = 1,
    /// LZ4.
    Lz4 = 2,
    /// zstd at `codec_level`.
    Zstd = 3,
}

impl CodecPolicy {
    /// Parses the descriptor byte.
    pub fn from_u8(v: u8) -> Result<Self> {
        match v {
            0 => Ok(Self::None),
            1 => Ok(Self::Auto),
            2 => Ok(Self::Lz4),
            3 => Ok(Self::Zstd),
            _ => Err(FormatError::range(WHAT, "codec_policy", u64::from(v))),
        }
    }

    /// True if `level` is legal for this policy.
    pub fn level_ok(self, level: i8) -> bool {
        match self {
            Self::None | Self::Lz4 => level == 0,
            Self::Auto => (1..=22).contains(&level),
            Self::Zstd => (-7..=22).contains(&level) && level != 0,
        }
    }
}

/// One `lineage[]` entry (ADR-25); empty in v1.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct LineageEntry {
    /// The base volume.
    pub volume_uuid: VolumeUuid,
    /// The base commit.
    pub base_seq: CommitSeq,
    /// Where to find the base (UTF-8, at most 1024 bytes).
    pub backend_hint: String,
}

/// The decoded `volume` object.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct VolumeDescriptor {
    /// plain, integrity or encrypted.
    pub crypto_mode: CryptoMode,
    /// default or fips (fips only with encrypted).
    pub profile: Profile,
    /// Always 4096.
    pub block_size: u32,
    /// Power of two, 16 KiB ..= 1 MiB.
    pub chunk_size: u32,
    /// 256 or 512.
    pub map_fanout: u16,
    /// Map id-prefix length.
    pub id_len: IdLen,
    /// Compression policy.
    pub codec_policy: CodecPolicy,
    /// Policy level (see [`CodecPolicy::level_ok`]).
    pub codec_level: i8,
    /// This volume.
    pub volume_uuid: VolumeUuid,
    /// Its key family.
    pub family_id: FamilyId,
    /// First 16 bytes of the root's fingerprint purpose; zero in plain mode.
    pub kmk_fingerprint: [u8; 16],
    /// Creation time.
    pub created_at: TimeMs,
    /// Always empty in v1.
    pub lineage: Vec<LineageEntry>,
    /// HMAC-SHA-256 (keyed modes) or BLAKE3-256 checksum (plain).
    pub mac: [u8; 32],
}

impl VolumeDescriptor {
    fn validate(&self) -> Result<()> {
        if self.profile == Profile::Fips && self.crypto_mode != CryptoMode::Encrypted {
            return Err(FormatError::invalid(WHAT, "fips needs an encrypted volume"));
        }
        if self.block_size != BLOCK_SIZE {
            return Err(FormatError::range(
                WHAT,
                "block_size",
                u64::from(self.block_size),
            ));
        }
        if !self.chunk_size.is_power_of_two()
            || !(MIN_CHUNK_SIZE..=MAX_CHUNK_SIZE).contains(&self.chunk_size)
        {
            return Err(FormatError::range(
                WHAT,
                "chunk_size",
                u64::from(self.chunk_size),
            ));
        }
        if self.map_fanout != 256 && self.map_fanout != 512 {
            return Err(FormatError::range(
                WHAT,
                "map_fanout",
                u64::from(self.map_fanout),
            ));
        }
        if !self.codec_policy.level_ok(self.codec_level) {
            return Err(FormatError::range(
                WHAT,
                "codec_level",
                u64::from(self.codec_level as u8),
            ));
        }
        if self.crypto_mode == CryptoMode::Plain && self.kmk_fingerprint != [0u8; 16] {
            return Err(FormatError::invalid(WHAT, "plain volume has a fingerprint"));
        }
        if self.lineage.len() > MAX_LINEAGE {
            return Err(FormatError::count(
                WHAT,
                "lineage",
                self.lineage.len() as u64,
                MAX_LINEAGE as u64,
            ));
        }
        for e in &self.lineage {
            if e.backend_hint.len() > MAX_BACKEND_HINT || e.backend_hint.contains('\0') {
                return Err(FormatError::BadString {
                    what: WHAT,
                    field: "backend_hint",
                });
            }
        }
        Ok(())
    }

    /// Appends the descriptor, `mac` included.
    pub fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        self.encode_body(out)?;
        out.extend_from_slice(&self.mac);
        Ok(())
    }

    /// Appends every byte before the `mac`.
    pub fn encode_body(&self, out: &mut Vec<u8>) -> Result<()> {
        self.validate()?;
        out.extend_from_slice(MAGIC);
        out.put_u16(FORMAT_VER);
        out.put_u8(self.crypto_mode as u8);
        out.put_u8(self.profile as u8);
        out.put_u32(self.block_size);
        out.put_u32(self.chunk_size);
        out.put_u16(self.map_fanout);
        out.put_u8(self.id_len.as_u8());
        out.put_u8(self.codec_policy as u8);
        out.put_u8(self.codec_level as u8);
        out.put_u8(self.lineage.len() as u8);
        out.put_zeros(6);
        out.extend_from_slice(&self.volume_uuid.0);
        out.extend_from_slice(&self.family_id.0);
        out.extend_from_slice(&self.kmk_fingerprint);
        out.put_u64(self.created_at.0);
        for e in &self.lineage {
            out.extend_from_slice(&e.volume_uuid.0);
            out.put_u64(e.base_seq.0);
            out.put_u16(e.backend_hint.len() as u16);
            out.extend_from_slice(e.backend_hint.as_bytes());
        }
        Ok(())
    }

    /// The exact bytes the descriptor MAC covers: `"lunix volume v1\0"` and
    /// every descriptor byte before `mac`. In plain mode `mac` is the
    /// BLAKE3-256 of the same input.
    pub fn mac_input(&self) -> Result<Vec<u8>> {
        let mut v = MAC_LABEL.to_vec();
        self.encode_body(&mut v)?;
        Ok(v)
    }

    /// Decodes a whole descriptor.
    pub fn decode(buf: &[u8]) -> Result<Self> {
        if buf.len() < BASE_LEN {
            return Err(FormatError::Truncated {
                what: WHAT,
                need: BASE_LEN,
                have: buf.len(),
            });
        }
        let (body, mac_bytes) = buf.split_at(buf.len() - MAC_LEN);
        let mut mac = [0u8; 32];
        mac.copy_from_slice(mac_bytes);
        let mut r = Reader::new(body, WHAT);
        r.magic(MAGIC)?;
        r.version_u16(FORMAT_VER)?;
        let crypto_mode = CryptoMode::from_u8(r.u8()?)?;
        let profile = Profile::from_u8(r.u8()?)?;
        let block_size = r.u32()?;
        let chunk_size = r.u32()?;
        let map_fanout = r.u16()?;
        let id_len = IdLen::new(r.u8()?)?;
        let codec_policy = CodecPolicy::from_u8(r.u8()?)?;
        let codec_level = r.i8()?;
        let lineage_count = r.u8()?;
        r.reserved(6)?;
        let volume_uuid = VolumeUuid(r.array()?);
        let family_id = FamilyId(r.array()?);
        let kmk_fingerprint = r.array()?;
        let created_at = TimeMs(r.u64()?);
        let n = r.check_count(
            "lineage",
            u64::from(lineage_count),
            MAX_LINEAGE as u64,
            LINEAGE_MIN,
        )?;
        let mut lineage = Vec::with_capacity(n);
        for _ in 0..n {
            let volume_uuid = VolumeUuid(r.array()?);
            let base_seq = CommitSeq(r.u64()?);
            let hint_len = usize::from(r.u16()?);
            if hint_len > MAX_BACKEND_HINT {
                return Err(FormatError::count(
                    WHAT,
                    "backend_hint",
                    hint_len as u64,
                    MAX_BACKEND_HINT as u64,
                ));
            }
            let backend_hint = r.string("backend_hint", hint_len)?.to_owned();
            lineage.push(LineageEntry {
                volume_uuid,
                base_seq,
                backend_hint,
            });
        }
        r.finish()?;
        let d = Self {
            crypto_mode,
            profile,
            block_size,
            chunk_size,
            map_fanout,
            id_len,
            codec_policy,
            codec_level,
            volume_uuid,
            family_id,
            kmk_fingerprint,
            created_at,
            lineage,
            mac,
        };
        d.validate()?;
        Ok(d)
    }
}
