//! Local-only files (freeze item 14, on-disk-formats.md sections 18-21):
//! `state/checkpoint`, `state/device`, `state/pins`, `volume.json` and the
//! host registry `volumes.json`. (`volume` is item 8's descriptor.)

use serde::{Deserialize, Serialize};

use crate::bytes::{crc32c, Put, Reader};
use crate::error::{FormatError, Result};
use crate::ids::{CommitSeq, FamilyId, VolumeUuid};
use crate::mode::CryptoMode;

// ---------------------------------------------------------------------------
// Checkpoint

const CKP: &str = "checkpoint";
/// Checkpoint slot magic.
pub const CKP_MAGIC: &[u8; 8] = b"LUNIXCKP";
/// Size of one checkpoint slot.
pub const CKP_SLOT_LEN: usize = 4096;
/// Size of the checkpoint file (two slots).
pub const CKP_FILE_LEN: usize = 2 * CKP_SLOT_LEN;
/// Most lanes.
pub const CKP_MAX_LANES: usize = 256;
/// Most counters.
pub const CKP_MAX_STATS: usize = 192;
const CKP_CRC_AT: usize = 4092;

/// One checkpoint slot: a hint, never authoritative.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct CheckpointSlot {
    /// Rises with every write, at least 1.
    pub slot_seq: u64,
    /// The volume.
    pub volume_uuid: VolumeUuid,
    /// The tip when written.
    pub last_commit_seq: u64,
    /// `ObjId` counter high water.
    pub id_high: u64,
    /// Per lane: files with a lower ordinal may be skipped (1 ..= 256 lanes).
    pub lane_tail_ordinals: Vec<u32>,
    /// Engine-defined counters (at most 192).
    pub stats: Vec<u64>,
}

impl CheckpointSlot {
    /// Appends the 4096-byte slot.
    pub fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        let lanes = self.lane_tail_ordinals.len();
        if lanes == 0 || lanes > CKP_MAX_LANES {
            return Err(FormatError::count(
                CKP,
                "lanes",
                lanes as u64,
                CKP_MAX_LANES as u64,
            ));
        }
        if self.stats.len() > CKP_MAX_STATS {
            return Err(FormatError::count(
                CKP,
                "n_stats",
                self.stats.len() as u64,
                CKP_MAX_STATS as u64,
            ));
        }
        if self.slot_seq == 0 {
            return Err(FormatError::range(CKP, "slot_seq", 0));
        }
        let start = out.len();
        out.extend_from_slice(CKP_MAGIC);
        out.put_u16(1);
        out.put_u16(lanes as u16);
        out.put_u16(self.stats.len() as u16);
        out.put_zeros(2);
        out.put_u64(self.slot_seq);
        out.extend_from_slice(&self.volume_uuid.0);
        out.put_u64(self.last_commit_seq);
        out.put_u64(self.id_high);
        out.put_zeros(8);
        for &o in &self.lane_tail_ordinals {
            out.put_u32(o);
            out.put_u32(0);
        }
        for &s in &self.stats {
            out.put_u64(s);
        }
        out.resize(start + CKP_CRC_AT, 0);
        let crc = crc32c(&out[start..start + CKP_CRC_AT]);
        out.put_u32(crc);
        Ok(())
    }

    /// Decodes exactly one 4096-byte slot.
    pub fn decode(buf: &[u8]) -> Result<Self> {
        if buf.len() != CKP_SLOT_LEN {
            return Err(FormatError::Truncated {
                what: CKP,
                need: CKP_SLOT_LEN,
                have: buf.len(),
            });
        }
        let stored = u32::from_le_bytes([buf[4092], buf[4093], buf[4094], buf[4095]]);
        if stored != crc32c(&buf[..CKP_CRC_AT]) {
            return Err(FormatError::Checksum { what: CKP });
        }
        let mut r = Reader::new(&buf[..CKP_CRC_AT], CKP);
        r.magic(CKP_MAGIC)?;
        r.version_u16(1)?;
        let lanes = usize::from(r.u16()?);
        let n_stats = usize::from(r.u16()?);
        r.reserved(2)?;
        if lanes == 0 || lanes > CKP_MAX_LANES {
            return Err(FormatError::count(
                CKP,
                "lanes",
                lanes as u64,
                CKP_MAX_LANES as u64,
            ));
        }
        if n_stats > CKP_MAX_STATS {
            return Err(FormatError::count(
                CKP,
                "n_stats",
                n_stats as u64,
                CKP_MAX_STATS as u64,
            ));
        }
        let slot_seq = r.u64()?;
        if slot_seq == 0 {
            return Err(FormatError::range(CKP, "slot_seq", 0));
        }
        let volume_uuid = VolumeUuid(r.array()?);
        let last_commit_seq = r.u64()?;
        let id_high = r.u64()?;
        r.reserved(8)?;
        r.check_count("lanes", lanes as u64, CKP_MAX_LANES as u64, 8)?;
        let mut lane_tail_ordinals = Vec::with_capacity(lanes);
        for _ in 0..lanes {
            lane_tail_ordinals.push(r.u32()?);
            r.reserved(4)?;
        }
        r.check_count("n_stats", n_stats as u64, CKP_MAX_STATS as u64, 8)?;
        let mut stats = Vec::with_capacity(n_stats);
        for _ in 0..n_stats {
            stats.push(r.u64()?);
        }
        let rest = r.remaining();
        r.reserved(rest)?;
        Ok(Self {
            slot_seq,
            volume_uuid,
            last_commit_seq,
            id_high,
            lane_tail_ordinals,
            stats,
        })
    }
}

/// The 8192-byte checkpoint file: two slots, each valid or not.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct CheckpointFile {
    /// Slot 0 and slot 1 as found.
    pub slots: [Result<CheckpointSlot>; 2],
}

impl CheckpointFile {
    /// Reads both slots; an invalid slot is recorded, not fatal. Only a file
    /// of the wrong length is an error.
    pub fn decode(buf: &[u8]) -> Result<Self> {
        if buf.len() != CKP_FILE_LEN {
            return Err(FormatError::Truncated {
                what: CKP,
                need: CKP_FILE_LEN,
                have: buf.len(),
            });
        }
        Ok(Self {
            slots: [
                CheckpointSlot::decode(&buf[..CKP_SLOT_LEN]),
                CheckpointSlot::decode(&buf[CKP_SLOT_LEN..]),
            ],
        })
    }

    /// The valid slot with the higher `slot_seq` (slot 0 on a tie), or `None`
    /// if neither is valid: the file is then ignored.
    pub fn newest_valid(&self) -> Option<(usize, &CheckpointSlot)> {
        match (&self.slots[0], &self.slots[1]) {
            (Ok(a), Ok(b)) => Some(if b.slot_seq > a.slot_seq {
                (1, b)
            } else {
                (0, a)
            }),
            (Ok(a), Err(_)) => Some((0, a)),
            (Err(_), Ok(b)) => Some((1, b)),
            (Err(_), Err(_)) => None,
        }
    }

    /// The slot a writer overwrites next: the one not holding the newest
    /// valid copy (slot 0 when neither is valid).
    pub fn next_slot_to_write(&self) -> usize {
        match self.newest_valid() {
            Some((i, _)) => 1 - i,
            None => 0,
        }
    }

    /// Encodes a file from two valid slots.
    pub fn encode_pair(a: &CheckpointSlot, b: &CheckpointSlot, out: &mut Vec<u8>) -> Result<()> {
        a.encode(out)?;
        b.encode(out)
    }
}

// ---------------------------------------------------------------------------
// Fixed 96-byte records

fn check_crc96(buf: &[u8], what: &'static str) -> Result<()> {
    if buf.len() != 96 {
        return Err(FormatError::Truncated {
            what,
            need: 96,
            have: buf.len(),
        });
    }
    let stored = u32::from_le_bytes([buf[92], buf[93], buf[94], buf[95]]);
    if stored != crc32c(&buf[..92]) {
        return Err(FormatError::Checksum { what });
    }
    Ok(())
}

fn finish96(out: &mut Vec<u8>, start: usize) {
    let crc = crc32c(&out[start..start + 92]);
    out.put_u32(crc);
}

const DEV: &str = "device record";

/// `state/device`: the ublk device identity for recovery.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct DeviceRecord {
    /// Queues, 1 ..= 256.
    pub nr_queues: u16,
    /// Queue depth, 1 ..= 4096.
    pub queue_depth: u16,
    /// The ublk device id.
    pub dev_id: u32,
    /// At least 262 144 (D14).
    pub max_io_buf_bytes: u32,
    /// `/proc/sys/kernel/random/boot_id` as 16 bytes.
    pub boot_id: [u8; 16],
    /// The volume.
    pub volume_uuid: VolumeUuid,
    /// Size in 512-byte sectors.
    pub dev_sectors: u64,
    /// The ublk device flags.
    pub flags: u64,
}

impl DeviceRecord {
    /// Encoded length.
    pub const LEN: usize = 96;

    /// Appends the 96-byte record.
    pub fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        self.validate()?;
        let start = out.len();
        out.extend_from_slice(b"LUNIXDEV");
        out.put_u16(1);
        out.put_u16(self.nr_queues);
        out.put_u16(self.queue_depth);
        out.put_zeros(2);
        out.put_u32(self.dev_id);
        out.put_u32(self.max_io_buf_bytes);
        out.extend_from_slice(&self.boot_id);
        out.extend_from_slice(&self.volume_uuid.0);
        out.put_u64(self.dev_sectors);
        out.put_u64(self.flags);
        out.put_zeros(20);
        finish96(out, start);
        Ok(())
    }

    fn validate(&self) -> Result<()> {
        if !(1..=256).contains(&self.nr_queues) {
            return Err(FormatError::range(
                DEV,
                "nr_queues",
                u64::from(self.nr_queues),
            ));
        }
        if !(1..=4096).contains(&self.queue_depth) {
            return Err(FormatError::range(
                DEV,
                "queue_depth",
                u64::from(self.queue_depth),
            ));
        }
        if self.max_io_buf_bytes < 262_144 {
            return Err(FormatError::range(
                DEV,
                "max_io_buf_bytes",
                u64::from(self.max_io_buf_bytes),
            ));
        }
        Ok(())
    }

    /// Decodes exactly one record.
    pub fn decode(buf: &[u8]) -> Result<Self> {
        check_crc96(buf, DEV)?;
        let mut r = Reader::new(&buf[..92], DEV);
        r.magic(b"LUNIXDEV")?;
        r.version_u16(1)?;
        let nr_queues = r.u16()?;
        let queue_depth = r.u16()?;
        r.reserved(2)?;
        let d = Self {
            nr_queues,
            queue_depth,
            dev_id: r.u32()?,
            max_io_buf_bytes: r.u32()?,
            boot_id: r.array()?,
            volume_uuid: VolumeUuid(r.array()?),
            dev_sectors: r.u64()?,
            flags: r.u64()?,
        };
        r.reserved(20)?;
        r.finish()?;
        d.validate()?;
        Ok(d)
    }
}

const PIN: &str = "local pins";
const PIN_V2_LEN: usize = 136;

/// The trust-on-first-use pin of a volume's mode and fingerprint.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Pin {
    /// The pinned mode.
    pub mode: CryptoMode,
    /// The pinned fingerprint; zero unless the mode is keyed.
    pub fingerprint: [u8; 16],
}

/// The local rollback anchor of `state/pins` version 2 (on-disk-formats.md
/// section 20, amendment 2026-11-12): the highest verified tip this host has
/// recorded.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct RollbackFloor {
    /// The `commit_seq` of the recorded tip. Raised, never lowered, except by
    /// `fsck --repair --truncate-tip`.
    pub max_commit_seq: CommitSeq,
    /// BLAKE3-256 of that commit object's bytes as stored (the `parent_hash`
    /// its child would carry).
    pub tip_hash: [u8; 32],
}

/// `state/pins`: monotonic floors and the pin.
///
/// The rollback anchor is in [`LocalPinsFile`]; encoding a `LocalPins` writes
/// the file **without** a floor, and decoding one drops the floor. A caller
/// that keeps the anchor must read and write [`LocalPinsFile`] instead.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct LocalPins {
    /// The volume.
    pub volume_uuid: VolumeUuid,
    /// Its family.
    pub family_id: FamilyId,
    /// Highest `keyring_seq` seen.
    pub max_keyring_seq: u64,
    /// Highest epoch seen.
    pub max_epoch: u64,
    /// The pin, if one was recorded.
    pub pin: Option<Pin>,
}

impl LocalPins {
    /// Encoded length.
    pub const LEN: usize = PIN_V2_LEN;

    /// Appends the file with no floor.
    pub fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        LocalPinsFile {
            pins: self.clone(),
            floor: None,
        }
        .encode(out)
    }

    /// Decodes the file, dropping any floor.
    pub fn decode(buf: &[u8]) -> Result<Self> {
        Ok(LocalPinsFile::decode(buf)?.pins)
    }
}

/// `state/pins` as a whole: the pins of [`LocalPins`] and the rollback
/// anchor.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct LocalPinsFile {
    /// Floors and the pin.
    pub pins: LocalPins,
    /// The rollback anchor; `None` is `floor_present` = 0.
    pub floor: Option<RollbackFloor>,
}

fn check_crc(buf: &[u8], len: usize) -> Result<()> {
    let at = len - 4;
    let stored = u32::from_le_bytes([buf[at], buf[at + 1], buf[at + 2], buf[at + 3]]);
    if stored != crc32c(&buf[..at]) {
        return Err(FormatError::Checksum { what: PIN });
    }
    Ok(())
}

impl LocalPinsFile {
    /// Length of the file.
    pub const LEN: usize = PIN_V2_LEN;

    fn validate(&self) -> Result<()> {
        if let Some(p) = &self.pins.pin {
            if !p.mode.is_keyed() && p.fingerprint != [0u8; 16] {
                return Err(FormatError::invalid(PIN, "plain pin with a fingerprint"));
            }
        }
        Ok(())
    }

    /// Appends the 136-byte version 2 file.
    pub fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        self.validate()?;
        let start = out.len();
        let pins = &self.pins;
        out.extend_from_slice(b"LUNIXPIN");
        out.put_u16(2);
        out.put_u8(u8::from(pins.pin.is_some()));
        out.put_u8(pins.pin.map_or(0, |p| p.mode as u8));
        out.put_u8(u8::from(self.floor.is_some()));
        out.put_zeros(3);
        out.extend_from_slice(&pins.volume_uuid.0);
        out.extend_from_slice(&pins.family_id.0);
        out.put_u64(pins.max_keyring_seq);
        out.put_u64(pins.max_epoch);
        out.extend_from_slice(&pins.pin.map_or([0u8; 16], |p| p.fingerprint));
        out.put_u64(self.floor.map_or(0, |f| f.max_commit_seq.0));
        out.extend_from_slice(&self.floor.map_or([0u8; 32], |f| f.tip_hash));
        out.put_zeros(12);
        let crc = crc32c(&out[start..start + PIN_V2_LEN - 4]);
        out.put_u32(crc);
        Ok(())
    }

    /// Decodes the 136-byte version 2 file. Any other length or version is an
    /// error; the caller treats a rejected file as absent.
    pub fn decode(buf: &[u8]) -> Result<Self> {
        if buf.len() != PIN_V2_LEN {
            return Err(FormatError::Truncated {
                what: PIN,
                need: PIN_V2_LEN,
                have: buf.len(),
            });
        }
        check_crc(buf, buf.len())?;
        let mut r = Reader::new(&buf[..buf.len() - 4], PIN);
        r.magic(b"LUNIXPIN")?;
        r.version_u16(2)?;
        let present = r.u8()?;
        let mode = r.u8()?;
        let floor_present = r.u8()?;
        r.reserved(3)?;
        let volume_uuid = VolumeUuid(r.array()?);
        let family_id = FamilyId(r.array()?);
        let max_keyring_seq = r.u64()?;
        let max_epoch = r.u64()?;
        let fp: [u8; 16] = r.array()?;
        let seq = r.u64()?;
        let hash: [u8; 32] = r.array()?;
        let floor = match floor_present {
            0 => {
                if seq != 0 || hash != [0u8; 32] {
                    return Err(FormatError::invalid(
                        PIN,
                        "floor fields set without a floor",
                    ));
                }
                None
            }
            1 => Some(RollbackFloor {
                max_commit_seq: CommitSeq(seq),
                tip_hash: hash,
            }),
            v => return Err(FormatError::range(PIN, "floor_present", u64::from(v))),
        };
        r.reserved(12)?;
        r.finish()?;
        let pin = match present {
            0 => {
                if mode != 0 || fp != [0u8; 16] {
                    return Err(FormatError::invalid(PIN, "pin fields set without a pin"));
                }
                None
            }
            1 => {
                let mode = CryptoMode::from_u8(mode)?;
                if !mode.is_keyed() && fp != [0u8; 16] {
                    return Err(FormatError::invalid(PIN, "plain pin with a fingerprint"));
                }
                Some(Pin {
                    mode,
                    fingerprint: fp,
                })
            }
            v => return Err(FormatError::range(PIN, "pin_present", u64::from(v))),
        };
        Ok(Self {
            pins: LocalPins {
                volume_uuid,
                family_id,
                max_keyring_seq,
                max_epoch,
                pin,
            },
            floor,
        })
    }
}

// ---------------------------------------------------------------------------
// volume.json and volumes.json

const VJ: &str = "volume.json";
/// Largest accepted JSON document.
pub const MAX_JSON_LEN: usize = 1 << 20;

fn json_err(_e: serde_json::Error) -> FormatError {
    FormatError::invalid(VJ, "malformed JSON, unknown key or wrong type")
}

fn name_ok(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

fn abs_dir_ok(s: &str) -> bool {
    s.starts_with('/') && !s.contains('\0')
}

/// `space.free_floor`: bytes, or a percentage string such as `"10%"`.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum FreeFloor {
    /// Absolute bytes.
    Bytes(u64),
    /// `"N%"`, N in 0 ..= 100.
    Percent(String),
}

/// Behaviour when the space ladder is exhausted.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OnFull {
    /// Block writers until space returns.
    Wait,
    /// Fail the write.
    Error,
}

/// The `space` object of `volume.json`.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct SpaceSettings {
    /// Free-space floor.
    pub free_floor: FreeFloor,
    /// On exhaustion.
    pub on_full: OnFull,
    /// Wait timeout (at least 1) or none.
    pub full_timeout_ms: Option<u64>,
    /// Cache cap in bytes (an absolute cap), or none. At most one of
    /// `cache_max` and `cache_max_percent` is set.
    pub cache_max: Option<u64>,
    /// Cache cap in percent of the volume size (100 ..= 10 000), recomputed
    /// from the size at open and on every grow (FREEZE F2, ADR-32 N2), or
    /// none. The key is required in the file (null for none).
    pub cache_max_percent: Option<u32>,
    /// Headroom in bytes.
    pub headroom: u64,
}

impl Default for SpaceSettings {
    fn default() -> Self {
        Self {
            free_floor: FreeFloor::Percent("10%".into()),
            on_full: OnFull::Wait,
            full_timeout_ms: None,
            cache_max: None,
            cache_max_percent: None,
            headroom: 268_435_456,
        }
    }
}

/// The `commit` object of `volume.json`.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct CommitSettings {
    /// Commit interval `T` (at least 1000).
    pub interval_ms: u64,
    /// Dirty bytes `B` (at least 1 MiB).
    pub dirty_bytes: u64,
}

impl Default for CommitSettings {
    fn default() -> Self {
        Self {
            interval_ms: 30_000,
            dirty_bytes: 1 << 30,
        }
    }
}

/// `volume.json`: local settings only (never the backend set).
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct VolumeJson {
    /// Always 1.
    pub version: u32,
    /// 1 ..= 64 of `[A-Za-z0-9._-]`.
    pub name: String,
    /// Absolute path.
    pub directory: String,
    /// Space policy.
    pub space: SpaceSettings,
    /// Commit cadence.
    pub commit: CommitSettings,
    /// FULL commits kept, 1 ..= 1000.
    pub retain_last: u32,
    /// GC grace, at least `10_000 + 2 * read_timeout_ms`.
    pub gc_grace_ms: u64,
    /// Read timeout, at least 1000.
    pub read_timeout_ms: u64,
    /// GC rate in bytes per second.
    pub gc_rate: u64,
    /// Live fraction below which a segment is compacted, 0 < x < 1.
    pub gc_live_threshold: f64,
    /// GC step budget, at least 1.
    pub gc_step_budget_ms: u64,
    /// Journal cap, at least 256 MiB.
    pub journal_max: u64,
    /// Index bytes per chunk.
    pub index_ram_per_chunk: u64,
    /// Dirty map leaves cap, at least 100.
    pub map_dirty_leaves_max: u64,
    /// Map cache bytes.
    pub map_cache_max: u64,
    /// Read cache bytes.
    pub read_cache_max: u64,
    /// Recompute chunk ids on reads.
    pub verify_reads: bool,
    /// Attach default, at least 262 144 (D14).
    pub max_io_buf_bytes: u32,
}

impl Default for VolumeJson {
    fn default() -> Self {
        Self {
            version: 1,
            name: String::new(),
            directory: String::new(),
            space: SpaceSettings::default(),
            commit: CommitSettings::default(),
            retain_last: 2,
            gc_grace_ms: 60_000,
            read_timeout_ms: 10_000,
            gc_rate: 67_108_864,
            gc_live_threshold: 0.5,
            gc_step_budget_ms: 50,
            journal_max: 2_147_483_648,
            index_ram_per_chunk: 16,
            map_dirty_leaves_max: 20_000,
            map_cache_max: 268_435_456,
            read_cache_max: 134_217_728,
            verify_reads: false,
            max_io_buf_bytes: 1_048_576,
        }
    }
}

impl VolumeJson {
    /// Defaults with a name and directory.
    pub fn new(name: &str, directory: &str) -> Self {
        Self {
            name: name.into(),
            directory: directory.into(),
            ..Self::default()
        }
    }

    /// Checks every range of section 21.
    pub fn validate(&self) -> Result<()> {
        let bad = |field: &'static str| FormatError::range(VJ, field, 0);
        if self.version != 1 {
            return Err(FormatError::BadVersion {
                what: VJ,
                got: u64::from(self.version),
            });
        }
        if !name_ok(&self.name) {
            return Err(FormatError::BadString {
                what: VJ,
                field: "name",
            });
        }
        if !abs_dir_ok(&self.directory) {
            return Err(FormatError::BadString {
                what: VJ,
                field: "directory",
            });
        }
        if let FreeFloor::Percent(p) = &self.space.free_floor {
            let ok = p
                .strip_suffix('%')
                .filter(|n| !n.is_empty() && n.len() <= 3 && n.bytes().all(|b| b.is_ascii_digit()))
                .and_then(|n| n.parse::<u32>().ok())
                .is_some_and(|n| n <= 100);
            if !ok {
                return Err(bad("space.free_floor"));
            }
        }
        if self.space.full_timeout_ms == Some(0) {
            return Err(bad("space.full_timeout_ms"));
        }
        if let Some(p) = self.space.cache_max_percent {
            if !(100..=10_000).contains(&p) || self.space.cache_max.is_some() {
                return Err(bad("space.cache_max_percent"));
            }
        }
        if self.commit.interval_ms < 1000 {
            return Err(bad("commit.interval_ms"));
        }
        if self.commit.dirty_bytes < 1 << 20 {
            return Err(bad("commit.dirty_bytes"));
        }
        if !(1..=1000).contains(&self.retain_last) {
            return Err(bad("retain_last"));
        }
        if self.read_timeout_ms < 1000 {
            return Err(bad("read_timeout_ms"));
        }
        let floor = self
            .read_timeout_ms
            .checked_mul(2)
            .and_then(|v| v.checked_add(10_000));
        if floor.is_none_or(|f| self.gc_grace_ms < f) {
            return Err(bad("gc_grace_ms"));
        }
        if !(self.gc_live_threshold > 0.0 && self.gc_live_threshold < 1.0) {
            return Err(bad("gc_live_threshold"));
        }
        if self.gc_step_budget_ms < 1 {
            return Err(bad("gc_step_budget_ms"));
        }
        if self.journal_max < 268_435_456 {
            return Err(bad("journal_max"));
        }
        if self.map_dirty_leaves_max < 100 {
            return Err(bad("map_dirty_leaves_max"));
        }
        if self.max_io_buf_bytes < 262_144 {
            return Err(bad("max_io_buf_bytes"));
        }
        Ok(())
    }

    /// Parses and validates.
    pub fn from_json(s: &str) -> Result<Self> {
        if s.len() > MAX_JSON_LEN {
            return Err(FormatError::range(VJ, "length", s.len() as u64));
        }
        // The struct-level `default` would read a missing `version` as 1;
        // like volumes.json, the file must carry it.
        let raw: serde_json::Value = serde_json::from_str(s).map_err(json_err)?;
        if raw.get("version").is_none() {
            return Err(FormatError::invalid(VJ, "missing version"));
        }
        // How the cap was set is required too (FREEZE F2): no default, no
        // reading of a file written before the key existed.
        let has_cap_keys = raw
            .get("space")
            .is_some_and(|s| s.get("cache_max").is_some() && s.get("cache_max_percent").is_some());
        if !has_cap_keys {
            return Err(FormatError::invalid(
                VJ,
                "missing space.cache_max or space.cache_max_percent (both keys are required; null for none)",
            ));
        }
        let v: Self = serde_json::from_value(raw).map_err(json_err)?;
        v.validate()?;
        Ok(v)
    }

    /// Canonical output: two-space indent, fixed key order, trailing newline.
    pub fn to_json_pretty(&self) -> Result<String> {
        self.validate()?;
        let mut s = serde_json::to_string_pretty(self).map_err(json_err)?;
        s.push('\n');
        Ok(s)
    }
}

const VS: &str = "volumes.json";

/// One entry of the host registry.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryEntry {
    /// Unique name.
    pub name: String,
    /// Hyphenated lowercase uuid.
    pub uuid: String,
    /// Absolute path of the volume directory.
    pub directory: String,
}

/// `<state_dir>/volumes.json`, rebuildable by scanning volume directories.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VolumesJson {
    /// Always 1.
    pub version: u32,
    /// The volumes.
    pub volumes: Vec<RegistryEntry>,
}

impl VolumesJson {
    /// Checks names, uuids and directories.
    pub fn validate(&self) -> Result<()> {
        if self.version != 1 {
            return Err(FormatError::BadVersion {
                what: VS,
                got: u64::from(self.version),
            });
        }
        let mut names = std::collections::HashSet::new();
        for e in &self.volumes {
            if !name_ok(&e.name) {
                return Err(FormatError::BadString {
                    what: VS,
                    field: "name",
                });
            }
            if !names.insert(e.name.as_str()) {
                return Err(FormatError::invalid(VS, "duplicate name"));
            }
            if e.uuid.parse::<VolumeUuid>().is_err() {
                return Err(FormatError::BadString {
                    what: VS,
                    field: "uuid",
                });
            }
            if !abs_dir_ok(&e.directory) {
                return Err(FormatError::BadString {
                    what: VS,
                    field: "directory",
                });
            }
        }
        Ok(())
    }

    /// Parses and validates.
    pub fn from_json(s: &str) -> Result<Self> {
        if s.len() > MAX_JSON_LEN {
            return Err(FormatError::range(VS, "length", s.len() as u64));
        }
        let v: Self = serde_json::from_str(s)
            .map_err(|_| FormatError::invalid(VS, "malformed JSON, unknown key or wrong type"))?;
        v.validate()?;
        Ok(v)
    }

    /// Canonical output, as for [`VolumeJson::to_json_pretty`].
    pub fn to_json_pretty(&self) -> Result<String> {
        self.validate()?;
        let mut s = serde_json::to_string_pretty(self)
            .map_err(|_| FormatError::invalid(VS, "cannot serialise"))?;
        s.push('\n');
        Ok(s)
    }
}
