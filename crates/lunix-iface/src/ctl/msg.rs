//! Request and response types of the control protocol.

use lunix_format::local::{FreeFloor, OnFull, SpaceSettings};
use serde::{Deserialize, Serialize};

use super::secret::Secret;
use super::PROTOCOL_VERSION;
use crate::gc::{GcPassSummary, GcStatusSnapshot};
use crate::space::SpaceSnapshot;
use crate::stats::EngineStats;

/// Longest volume name (same rule as `volume.json`).
pub const MAX_NAME: usize = 64;
/// Longest path in a request.
pub const MAX_PATH: usize = 4096;
/// Longest CPU list.
pub const MAX_CPUS: usize = 256;
/// Largest `max_io_buf_bytes` an attach may ask for (32 MiB).
pub const MAX_IO_BUF: u32 = 32 << 20;

/// Why a request failed validation.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid request: {0}")]
pub struct RequestInvalid(pub &'static str);

/// A request on the wire: version, correlation id, and the call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RequestFrame {
    /// Protocol version.
    pub v: u32,
    /// Echoed in the response.
    pub id: u64,
    /// The call: `"method"` and `"params"` sit beside `v` and `id`.
    #[serde(flatten)]
    pub call: Request,
}

impl RequestFrame {
    /// A frame at the current protocol version.
    pub fn new(id: u64, call: Request) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            id,
            call,
        }
    }
}

/// The calls. Unit variants carry no `params`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "method", content = "params", rename_all = "snake_case")]
pub enum Request {
    /// Liveness and version.
    Ping,
    /// Create a volume.
    VolumeCreate(VolumeCreate),
    /// List volumes.
    VolumeList,
    /// One volume's status.
    VolumeStatus(NameParam),
    /// Attach (create or recover the device).
    Attach(Attach),
    /// Detach.
    Detach(NameParam),
    /// Grow.
    Resize(Resize),
    /// Read or change the space policy.
    Space(SpaceRequest),
    /// Quiesce every device and exit (also `SIGTERM`).
    QuiesceAll,
    /// Run a GC pass (or a dry run).
    Gc(GcRequest),
    /// GC state.
    GcStatus(NameParam),
}

/// A request that names a volume.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NameParam {
    /// The volume.
    pub name: String,
}

/// The three volume modes (D16).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VolumeMode {
    /// AEAD extents, sealed metadata.
    Encrypted,
    /// Keyed tags and HMAC; the default.
    Integrity,
    /// Unkeyed checksums: corruption detection only.
    Plain,
}

impl From<lunix_format::CryptoMode> for VolumeMode {
    fn from(m: lunix_format::CryptoMode) -> Self {
        match m {
            lunix_format::CryptoMode::Plain => Self::Plain,
            lunix_format::CryptoMode::Integrity => Self::Integrity,
            lunix_format::CryptoMode::Encrypted => Self::Encrypted,
        }
    }
}

impl From<VolumeMode> for lunix_format::CryptoMode {
    fn from(m: VolumeMode) -> Self {
        match m {
            VolumeMode::Plain => Self::Plain,
            VolumeMode::Integrity => Self::Integrity,
            VolumeMode::Encrypted => Self::Encrypted,
        }
    }
}

/// The compression policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "policy", rename_all = "snake_case")]
pub enum Compress {
    /// LZ4 probe then zstd 1.
    Auto,
    /// LZ4.
    Lz4,
    /// zstd at a level.
    Zstd {
        /// The level, -7 ..= 22.
        level: i8,
    },
    /// Store raw.
    None,
}

fn default_chunk() -> u32 {
    64 * 1024
}
fn default_mode() -> VolumeMode {
    VolumeMode::Integrity
}
fn default_compress() -> Compress {
    Compress::Auto
}
fn default_id_len() -> u8 {
    16
}

/// `volume create`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VolumeCreate {
    /// The volume name.
    pub name: String,
    /// The directory that will hold the volume (absolute).
    pub dir: String,
    /// Size in bytes (a multiple of 4096).
    pub size_bytes: u64,
    /// Chunk size, a power of two from 16 KiB to 1 MiB.
    #[serde(default = "default_chunk")]
    pub chunk_size: u32,
    /// Volume mode.
    #[serde(default = "default_mode")]
    pub mode: VolumeMode,
    /// Compression policy.
    #[serde(default = "default_compress")]
    pub compress: Compress,
    /// Map id-prefix length: 0, 16 or 32.
    #[serde(default = "default_id_len")]
    pub id_len: u8,
    /// Map fanout, 256 or 512 (the daemon's default when absent).
    #[serde(default)]
    pub map_fanout: Option<u16>,
    /// An existing key file for the first slot instead of a generated one.
    #[serde(default)]
    pub key_file: Option<String>,
    /// A passphrase slot (for volumes that move).
    #[serde(default)]
    pub passphrase: Option<Secret>,
}

/// What the client supplies to unlock a volume.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Credential {
    /// A passphrase.
    #[serde(default)]
    pub passphrase: Option<Secret>,
    /// A key file path (read by the daemon).
    #[serde(default)]
    pub key_file: Option<String>,
    /// The KMK fingerprint the client expects, 32 lowercase hex digits.
    #[serde(default)]
    pub fingerprint: Option<String>,
    /// The mode the caller expects the volume to have (ADR-11 F7: the request
    /// decides the mode). Absent with key material means `encrypted`;
    /// `integrity` declares an integrity credential. `plain` with key material
    /// is refused by `validate`.
    #[serde(default)]
    pub mode: Option<VolumeMode>,
}

/// What an attach does about journal damage found at recovery (DESIGN §4.4
/// step 7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JournalDamagePolicy {
    /// Log the damage and let the first lane open wipe it.
    Wipe,
    /// Refuse to attach (code `journal_damage`), leaving the journal for `fsck`.
    Refuse,
}

/// `attach`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Attach {
    /// The volume.
    pub name: String,
    /// Queues, 1 ..= 16 (the host default when absent).
    #[serde(default)]
    pub queues: Option<u16>,
    /// Queue depth, 1 ..= 4096.
    #[serde(default)]
    pub depth: Option<u16>,
    /// CPU list for the queue threads, such as `2-5,8`.
    #[serde(default)]
    pub cpus: Option<String>,
    /// `max_io_buf_bytes` (D14), at least 256 KiB.
    #[serde(default)]
    pub max_io_buf_bytes: Option<u32>,
    /// Unlock material.
    #[serde(default)]
    pub credential: Option<Credential>,
    /// Journal damage policy of this attach; the daemon's configured default
    /// when absent.
    #[serde(default)]
    pub on_journal_damage: Option<JournalDamagePolicy>,
    /// Operator override of `fork_unverifiable` (ADR-31): attach although the
    /// chain from the tip back to the floor cannot be walked. Audited.
    #[serde(default)]
    pub allow_unverifiable_fork: bool,
}

/// `resize`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Resize {
    /// The volume.
    pub name: String,
    /// The new size in bytes (grow only).
    pub size_bytes: u64,
}

/// `space`: absent fields are left alone; all absent is a query.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpaceRequest {
    /// The volume.
    pub name: String,
    /// New free-space floor.
    #[serde(default)]
    pub free_floor: Option<FreeFloor>,
    /// New `on_full`.
    #[serde(default)]
    pub on_full: Option<OnFull>,
    /// New `full_timeout` in milliseconds.
    #[serde(default)]
    pub full_timeout_ms: Option<u64>,
    /// New cache cap in bytes.
    #[serde(default)]
    pub cache_max: Option<u64>,
}

/// `gc`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GcRequest {
    /// The volume.
    pub name: String,
    /// Change nothing; report what a pass would do.
    #[serde(default)]
    pub dry_run: bool,
}

fn valid_name(s: &str) -> bool {
    !s.is_empty()
        && s != "."
        && s != ".."
        && s.len() <= MAX_NAME
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

fn valid_path(s: &str) -> bool {
    !s.is_empty() && s.len() <= MAX_PATH && s.starts_with('/') && !s.contains('\0')
}

fn valid_cpus(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= MAX_CPUS
        && s.chars()
            .all(|c| c.is_ascii_digit() || c == ',' || c == '-')
}

fn name_ok(n: &str) -> Result<(), RequestInvalid> {
    valid_name(n)
        .then_some(())
        .ok_or(RequestInvalid("volume name"))
}

impl Request {
    /// Checks every bound a server must enforce before acting: name charset
    /// and length, path shape and length, id and chunk geometry, queue ranges,
    /// CPU list, secret and fingerprint shape. Fails closed.
    ///
    /// # Errors
    /// [`RequestInvalid`] naming the first offending field (never its value).
    pub fn validate(&self) -> Result<(), RequestInvalid> {
        match self {
            Self::Ping | Self::VolumeList | Self::QuiesceAll => Ok(()),
            Self::VolumeStatus(p) | Self::Detach(p) | Self::GcStatus(p) => name_ok(&p.name),
            Self::Resize(r) => {
                name_ok(&r.name)?;
                if r.size_bytes == 0 || !r.size_bytes.is_multiple_of(4096) {
                    return Err(RequestInvalid("size"));
                }
                Ok(())
            }
            Self::Space(s) => {
                name_ok(&s.name)?;
                if let Some(FreeFloor::Percent(p)) = &s.free_floor {
                    let ok = p
                        .strip_suffix('%')
                        .and_then(|n| n.parse::<u8>().ok())
                        .is_some_and(|n| n <= 100);
                    if !ok {
                        return Err(RequestInvalid("free_floor"));
                    }
                }
                if s.full_timeout_ms == Some(0) {
                    return Err(RequestInvalid("full_timeout_ms"));
                }
                Ok(())
            }
            Self::Gc(g) => name_ok(&g.name),
            Self::Attach(a) => {
                name_ok(&a.name)?;
                if a.queues.is_some_and(|q| !(1..=16).contains(&q)) {
                    return Err(RequestInvalid("queues"));
                }
                if a.depth.is_some_and(|d| !(1..=4096).contains(&d)) {
                    return Err(RequestInvalid("depth"));
                }
                if a.cpus.as_deref().is_some_and(|c| !valid_cpus(c)) {
                    return Err(RequestInvalid("cpus"));
                }
                if a.max_io_buf_bytes
                    .is_some_and(|b| !(262_144..=MAX_IO_BUF).contains(&b))
                {
                    return Err(RequestInvalid("max_io_buf_bytes"));
                }
                if let Some(c) = &a.credential {
                    c.validate()?;
                }
                Ok(())
            }
            Self::VolumeCreate(c) => {
                name_ok(&c.name)?;
                if !valid_path(&c.dir) {
                    return Err(RequestInvalid("dir"));
                }
                if c.size_bytes == 0 || !c.size_bytes.is_multiple_of(4096) {
                    return Err(RequestInvalid("size"));
                }
                if !c.chunk_size.is_power_of_two()
                    || !(16 * 1024..=1024 * 1024).contains(&c.chunk_size)
                {
                    return Err(RequestInvalid("chunk_size"));
                }
                if !matches!(c.id_len, 0 | 16 | 32) {
                    return Err(RequestInvalid("id_len"));
                }
                if c.map_fanout.is_some_and(|f| f != 256 && f != 512) {
                    return Err(RequestInvalid("map_fanout"));
                }
                if let Compress::Zstd { level } = c.compress {
                    if !(-7..=22).contains(&level) {
                        return Err(RequestInvalid("compress level"));
                    }
                }
                if c.key_file.as_deref().is_some_and(|p| !valid_path(p)) {
                    return Err(RequestInvalid("key_file"));
                }
                if c.mode == VolumeMode::Plain && (c.key_file.is_some() || c.passphrase.is_some()) {
                    return Err(RequestInvalid("plain volumes have no key material"));
                }
                Ok(())
            }
        }
    }
}

impl Credential {
    fn validate(&self) -> Result<(), RequestInvalid> {
        if self.key_file.as_deref().is_some_and(|p| !valid_path(p)) {
            return Err(RequestInvalid("key_file"));
        }
        let key_material = self.passphrase.is_some() || self.key_file.is_some();
        if key_material && self.mode == Some(VolumeMode::Plain) {
            return Err(RequestInvalid("plain volumes take no key material"));
        }
        if let Some(f) = &self.fingerprint {
            let ok = f.len() == 32 && f.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
            if !ok {
                return Err(RequestInvalid("fingerprint"));
            }
        }
        Ok(())
    }
}

/// Error codes. The CLI maps them to exit codes (M13a).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// The volume or device is in use (exit 3).
    Busy,
    /// No such volume (exit 4).
    NotFound,
    /// A credential or pin does not match the volume's mode (exit 6).
    Downgrade,
    /// The unlock key does not match the volume (exit 6).
    WrongKmk,
    /// R4: the tip is below this host's rollback floor (exit 6).
    Rollback,
    /// R4: the chain from the tip does not pass through the commit the floor
    /// recorded (exit 6).
    Fork,
    /// R4: the chain from the tip back to the floor cannot be walked (commits
    /// missing); an operator override attaches anyway (exit 6).
    ForkUnverifiable,
    /// Journal damage found and the attach asked to refuse: run `fsck` (exit 7).
    JournalDamage,
    /// No space (exit 5).
    NoSpace,
    /// The request is invalid (exit 2).
    Invalid,
    /// A host I/O error (exit 1).
    Io,
    /// A bug or an unclassified failure (exit 1).
    Internal,
    /// A code a newer daemon sent that this client does not know (exit 1).
    #[serde(other)]
    Unknown,
}

impl ErrorCode {
    /// The `lunix` process exit code for this error: 3 busy, 4 not found, 5 no
    /// space, 6 downgrade, wrong key, rollback or fork, 7 journal damage, 2
    /// usage, 1 anything else.
    pub fn exit_code(self) -> u8 {
        match self {
            Self::Busy => 3,
            Self::NotFound => 4,
            Self::NoSpace => 5,
            Self::Downgrade
            | Self::WrongKmk
            | Self::Rollback
            | Self::Fork
            | Self::ForkUnverifiable => 6,
            Self::JournalDamage => 7,
            Self::Invalid => 2,
            Self::Io | Self::Internal | Self::Unknown => 1,
        }
    }
}

/// An error response body. `message` is for people, never contains a secret,
/// and is cut to [`CtlError::MAX_MESSAGE`] characters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CtlError {
    /// The class.
    pub code: ErrorCode,
    /// A human-readable explanation.
    pub message: String,
}

impl CtlError {
    /// Longest message, in characters.
    pub const MAX_MESSAGE: usize = 512;

    /// Builds an error, truncating the message.
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        let message: String = message.into().chars().take(Self::MAX_MESSAGE).collect();
        Self { code, message }
    }
}

/// A response on the wire.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResponseFrame {
    /// Protocol version.
    pub v: u32,
    /// The request's id (0 when the request could not be parsed).
    pub id: u64,
    /// `ok` or `err` beside `v` and `id`.
    #[serde(flatten)]
    pub result: Outcome,
}

impl ResponseFrame {
    /// A success frame at the current version.
    pub fn ok(id: u64, body: Response) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            id,
            result: Outcome::Ok(body),
        }
    }

    /// An error frame at the current version.
    pub fn err(id: u64, error: CtlError) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            id,
            result: Outcome::Err(error),
        }
    }
}

/// Success or failure.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// The call worked.
    Ok(Response),
    /// The call failed.
    Err(CtlError),
}

/// Success bodies, tagged by `"type"`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    /// Answer to `ping`.
    Pong {
        /// The protocol version.
        version: u32,
        /// The daemon's build version.
        daemon: String,
    },
    /// Answer to `volume_create`.
    VolumeCreated(VolumeCreated),
    /// Answer to `volume_list`.
    VolumeList {
        /// All volumes in the registry.
        volumes: Vec<VolumeSummary>,
    },
    /// Answer to `volume_status`.
    VolumeStatus(Box<VolumeStatusBody>),
    /// Answer to `attach`.
    Attached(Attached),
    /// Answer to `detach`.
    Detached,
    /// Answer to `resize`.
    Resized {
        /// The size now.
        size_bytes: u64,
    },
    /// Answer to `space`.
    Space(SpaceBody),
    /// Answer to `quiesce_all`.
    Quiesced {
        /// Devices quiesced.
        devices: u32,
    },
    /// Answer to `gc`.
    Gc(GcPassSummary),
    /// Answer to `gc_status`.
    GcStatus(GcStatusSnapshot),
}

/// `volume_created`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VolumeCreated {
    /// The name.
    pub name: String,
    /// The uuid, hyphenated.
    pub uuid: String,
    /// The volume directory.
    pub dir: String,
    /// The mode created.
    pub mode: VolumeMode,
    /// The key file the daemon generated, for the default integrity volume.
    #[serde(default)]
    pub key_file: Option<String>,
}

/// One row of `volume_list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VolumeSummary {
    /// The name.
    pub name: String,
    /// The uuid, hyphenated.
    pub uuid: String,
    /// The volume directory.
    pub dir: String,
    /// Size in bytes; 0 when unknown (a detached encrypted volume's size is
    /// sealed in its commits).
    pub size_bytes: u64,
    /// The mode.
    pub mode: VolumeMode,
    /// The device path when attached.
    #[serde(default)]
    pub device: Option<String>,
}

/// Where a volume is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VolumeState {
    /// Not attached.
    Detached,
    /// Serving I/O.
    Attached,
    /// Recovering after a daemon restart.
    Recovering,
    /// Read-only after a fatal host I/O error.
    ReadOnly,
}

/// The ledger as the socket reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SpaceFigures {
    /// Bytes charged against the limit.
    pub used: u64,
    /// The limit.
    pub limit: u64,
    /// Free bytes on the host file system.
    pub fs_free: u64,
    /// Journal bytes reserved.
    pub journal: u64,
    /// Segment bytes.
    pub segments: u64,
    /// Cache bytes.
    pub cache: u64,
    /// Map pack bytes.
    pub map: u64,
    /// Index run bytes.
    pub index: u64,
    /// Commit bytes.
    pub commits: u64,
    /// Headroom held.
    pub headroom_held: u64,
    /// Requests parked for space.
    pub waiters: u64,
}

impl From<&SpaceSnapshot> for SpaceFigures {
    fn from(s: &SpaceSnapshot) -> Self {
        use crate::space::SpaceKind as K;
        let k = |kind: K| s.by_kind[kind.index()];
        Self {
            used: s.used(),
            limit: s.limit,
            fs_free: s.fs_free,
            journal: k(K::JournalReserved),
            segments: k(K::Segments),
            cache: k(K::Cache),
            map: k(K::Map),
            index: k(K::Index),
            commits: k(K::Commits),
            headroom_held: k(K::Headroom),
            waiters: s.waiters,
        }
    }
}

/// `space` response: the policy and the figures.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpaceBody {
    /// The policy now in force.
    pub settings: SpaceSettings,
    /// The ledger.
    pub figures: SpaceFigures,
}

/// `attached`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attached {
    /// `/dev/lunix/<name>`.
    pub device: String,
    /// The ublk device id.
    pub dev_id: u32,
    /// Queues in use.
    pub queues: u16,
    /// Queue depth.
    pub depth: u16,
    /// True when an existing device was recovered instead of added.
    pub recovered: bool,
    /// No rollback floor existed on this host: a rollback of history before
    /// this attach cannot be detected (ADR-31). The floor is recorded now.
    #[serde(default)]
    pub first_attach: bool,
}

/// The rollback floor of `state/pins` (ADR-31) as the socket reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RollbackFloorView {
    /// The highest verified tip this host recorded.
    pub max_commit_seq: u64,
    /// BLAKE3-256 of that commit's stored bytes, 64 lowercase hex digits.
    pub tip_hash: String,
}

/// `volume_status`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VolumeStatusBody {
    /// The name.
    pub name: String,
    /// The uuid, hyphenated.
    pub uuid: String,
    /// State.
    pub state: VolumeState,
    /// Size in bytes; 0 when unknown (a detached encrypted volume's size is
    /// sealed in its commits).
    pub size_bytes: u64,
    /// Chunk size.
    pub chunk_size: u32,
    /// The mode.
    pub mode: VolumeMode,
    /// The device path when attached.
    #[serde(default)]
    pub device: Option<String>,
    /// Counters.
    pub stats: EngineStats,
    /// The ledger.
    pub space: SpaceFigures,
    /// Journal bytes in use.
    pub journal_used: u64,
    /// Journal capacity.
    pub journal_capacity: u64,
    /// The tip's `commit_seq`.
    pub last_commit_seq: u64,
    /// The tip's `created_at`, milliseconds since the epoch.
    #[serde(default)]
    pub last_commit_ms: Option<u64>,
    /// Logical bytes over stored bytes of unique chunks (finite) or absent.
    #[serde(default)]
    pub dedup_ratio: Option<f64>,
    /// Chunk bytes over stored bytes (finite) or absent.
    #[serde(default)]
    pub compression_ratio: Option<f64>,
    /// The rollback floor this host recorded, attached or not; absent when
    /// none was ever recorded.
    #[serde(default)]
    pub rollback_floor: Option<RollbackFloorView>,
}
