//! The wiring of the real crates into one volume (plan M12a steps 4 and 5):
//! creating a volume directory with its genesis commit, and opening it as an
//! [`Engine`] over the store, journal, overlay, map, index, space, destage and
//! crypto crates.
//!
//! This module knows nothing about ublk or the control socket: the daemon
//! puts a device in front of [`VolumeStack::engine`], and the in-process
//! stack tests (`lunixd::testsupport`, M12b) drive the engine's queue
//! handlers directly.
//!
//! # Attach order (DESIGN §4.4, §8.4)
//!
//! 1. Read `volume`, the family keyring and `state/pins` (unlocked reads),
//!    run `check_attach` (downgrade, wrong key, descriptor MAC).
//! 2. Open the volume directory with the unlocked `MetaCodec` (takes `LOCK`).
//! 3. The ring size against the queue count (`journal_max >= queues x 2
//!    ring files`, 128 MiB per queue with the production 64 MiB files).
//! 4. Open the journal (lanes = queues), overlay, index, space gate, map
//!    opener and the M9 destage adapter, then `Engine::open`, which verifies
//!    the chain, applies R4, sweeps orphans, catches the index up, recovers
//!    the journal and raises the floor.
//! 5. Register GC's urgent pass as the space ladder's compact step
//!    ([`CompactStep`]).
//! 6. Only then record the trust-on-first-use pin and the keyring sequence in
//!    `state/pins` (R4 precedes every write).

mod destage;
mod floor;
mod ladder;

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicU64;
use std::sync::Arc;

use lunix_crypto::keyring::{self, Argon2Cost, KeyFile, NewSlot, Unlock};
use lunix_crypto::{
    check_attach, seal_descriptor, AttachError, AttachRequest, Credential, UnlockOptions,
    VolumeCrypto,
};
use lunix_destage::{DestageConfig, DestageParams};
use lunix_engine::{
    Engine, EngineConfig, EngineDeps, MapOpener, NoHooks, OnJournalDamage, OpenError,
};
use lunix_format::commit::{Commit, Full, MapRoot};
use lunix_format::keyring::RootKind;
use lunix_format::local::{
    LocalPins, LocalPinsFile, Pin, RollbackFloor, SpaceSettings, VolumeJson,
};
use lunix_format::paths::local as lpath;
use lunix_format::volume::{CodecPolicy, VolumeDescriptor, BLOCK_SIZE};
use lunix_format::{CommitSeq, CryptoMode, DekGen, FamilyId, IdLen, Profile, VolumeUuid};
use lunix_iface::block::BlockEngine;
use lunix_iface::clock::{Clock, SystemClock};
use lunix_iface::crypto::{CommitCodec, ExtentCodec, MetaCodec};
use lunix_iface::map::{BlockMap, NodeSource};
use lunix_iface::space::{Ladder, LadderStepKind, SpaceGate};
use lunix_iface::store::{CommitStore, PackRead, VolumeDir};
use lunix_iface::Error;
use lunix_index::LsmIndex;
use lunix_journal::{FileJournal, JournalConfig};
use lunix_map::{MapConfig, RadixMap};
use lunix_overlay::ShardedOverlay;
use lunix_space::{LunixSpace, SpaceConfig, StatvfsProbe};
use lunix_store::{read_volume_file, FsOps, LocalVolume, StoreOptions};

pub use destage::{map_error, map_outcome, retry_of, PipelineDestager, PressureThresholds};
pub use floor::DirFloor;
pub use ladder::{Borrow, Borrows, CompactStep};

use crate::fsio;

/// The largest keyring the format allows (32 slots, 192 KiB).
const MAX_KEYRING: u64 = 192 << 10;

/// Why a volume could not be created or opened. Messages never carry a
/// secret.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum StackError {
    /// The attach check refused (downgrade, wrong key, bad MAC, ...).
    #[error("attach refused: {0}")]
    Attach(#[from] AttachError),
    /// R4: the tip is below the floor this host recorded.
    #[error("rollback: the tip {tip} is below the floor {floor} this host recorded")]
    Rollback {
        /// The floor's `max_commit_seq`.
        floor: CommitSeq,
        /// The verified tip.
        tip: CommitSeq,
    },
    /// R4: the commit at the floor's height is not the one recorded.
    #[error("fork: the commit at height {at} is not the one this host recorded")]
    Fork {
        /// The height compared.
        at: CommitSeq,
    },
    /// R4 (ADR-31): the chain from the tip back to the floor cannot be walked.
    #[error("fork unverifiable: the chain from tip {tip} back to the floor {floor} cannot be walked ({detail}); an operator may attach with allow_unverifiable_fork")]
    ForkUnverifiable {
        /// The floor's `max_commit_seq`.
        floor: CommitSeq,
        /// The tip.
        tip: CommitSeq,
        /// Why the walk failed.
        detail: String,
    },
    /// Journal damage with `on_journal_damage = refuse`; nothing was wiped.
    #[error(
        "journal damage found and on_journal_damage = refuse; the journal is untouched for fsck"
    )]
    JournalDamage,
    /// A request the volume or this build cannot take.
    #[error("{0}")]
    Invalid(String),
    /// A store, journal, index, map or crypto failure.
    #[error(transparent)]
    Store(#[from] Error),
    /// Another engine open failure (tip, chain, configuration).
    #[error(transparent)]
    Open(OpenError),
}

impl From<OpenError> for StackError {
    fn from(e: OpenError) -> Self {
        match e {
            OpenError::Rollback { floor, tip } => Self::Rollback { floor, tip },
            OpenError::Fork { tip } => Self::Fork { at: tip },
            OpenError::ForkUnverifiable { floor, tip, source } => Self::ForkUnverifiable {
                floor,
                tip,
                detail: source.to_string(),
            },
            OpenError::JournalDamage => Self::JournalDamage,
            OpenError::Store(e) => Self::Store(e),
            other => Self::Open(other),
        }
    }
}

impl From<std::io::Error> for StackError {
    fn from(e: std::io::Error) -> Self {
        Self::Store(Error::Io(e))
    }
}

fn invalid(s: impl Into<String>) -> StackError {
    StackError::Invalid(s.into())
}

/// Random bytes from the kernel (`getrandom(2)`, cross-cutting rule 3).
///
/// # Errors
/// The random source failed.
pub fn random_bytes<const N: usize>() -> std::io::Result<[u8; N]> {
    let mut b = [0u8; N];
    let mut done = 0;
    while done < N {
        match rustix::rand::getrandom(&mut b[done..], rustix::rand::GetRandomFlags::empty()) {
            Ok(0) => return Err(std::io::Error::other("random source returned nothing")),
            Ok(n) => done += n,
            Err(rustix::io::Errno::INTR) => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(b)
}

/// A fresh random (version 4) volume uuid.
///
/// # Errors
/// The random source failed.
pub fn new_uuid() -> std::io::Result<VolumeUuid> {
    let mut b: [u8; 16] = random_bytes()?;
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    Ok(VolumeUuid(b))
}

/// The keyring path of a family next to a volume directory
/// (`<dir>/families/<family-id>/keyring`).
pub fn keyring_path(voldir: &Path, family: &FamilyId) -> PathBuf {
    let parent = voldir.parent().unwrap_or_else(|| Path::new("/"));
    parent
        .join("families")
        .join(family.to_string())
        .join("keyring")
}

// ---------------------------------------------------------------- create

/// What `volume create` asks for.
pub struct CreateParams<'a> {
    /// The volume name (`volume.json`).
    pub name: &'a str,
    /// The directory that holds volume directories (`<dir>`).
    pub parent: &'a Path,
    /// Size in bytes, a multiple of the chunk size.
    pub size_bytes: u64,
    /// Chunk size.
    pub chunk_size: u32,
    /// Crypto mode (D16).
    pub mode: CryptoMode,
    /// Compression policy and level (descriptor fields).
    pub policy: CodecPolicy,
    /// Level for `policy`.
    pub level: i8,
    /// Map id-prefix length (16 or 32; 0 is refused in v1).
    pub id_len: IdLen,
    /// Map fanout, 256 or 512.
    pub map_fanout: u16,
    /// An existing key file for the first slot.
    pub key_file: Option<&'a Path>,
    /// A passphrase slot.
    pub passphrase: Option<&'a [u8]>,
    /// Argon2id cost of a passphrase slot.
    pub argon2: Argon2Cost,
    /// Changes to the `volume.json` defaults (journal size, cadence).
    pub json: Option<&'a dyn Fn(&mut VolumeJson)>,
}

/// What `create_volume` made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreatedVolume {
    /// The new uuid.
    pub uuid: VolumeUuid,
    /// `<dir>/<uuid>`.
    pub voldir: PathBuf,
    /// The key file generated for a default `integrity` volume.
    pub generated_key_file: Option<PathBuf>,
}

struct Keys {
    root: Option<lunix_crypto::Root>,
    keyring: Option<Vec<u8>>,
    keyring_seq: u64,
    generated: Option<PathBuf>,
}

fn make_keys(p: &CreateParams<'_>, family: FamilyId, voldir: &Path) -> Result<Keys, StackError> {
    let kind = match p.mode {
        CryptoMode::Plain => {
            if p.key_file.is_some() || p.passphrase.is_some() {
                return Err(invalid("plain volumes have no key material"));
            }
            return Ok(Keys {
                root: None,
                keyring: None,
                keyring_seq: 0,
                generated: None,
            });
        }
        CryptoMode::Integrity => RootKind::Ik,
        CryptoMode::Encrypted => RootKind::Kmk,
    };
    let kf = match p.key_file {
        Some(path) => Some(KeyFile::read(path).map_err(|_| {
            invalid("the key file cannot be read (a regular file of exactly 32 bytes)")
        })?),
        None => None,
    };
    let pass = p.passphrase.map(|pw| NewSlot::Passphrase {
        passphrase: pw,
        cost: p.argon2,
    });
    let created = match (&kf, &pass) {
        (Some(kf), _) => keyring::create(Profile::Default, family, kind, &NewSlot::KeyFile(kf)),
        (None, Some(slot)) => keyring::create(Profile::Default, family, kind, slot),
        (None, None) if kind == RootKind::Ik => {
            let path = voldir.join(lpath::integrity_key());
            let c = keyring::create_integrity_with_key_file(family, &path).map_err(Error::from)?;
            return Ok(Keys {
                root: Some(c.root),
                keyring: Some(c.bytes),
                keyring_seq: c.keyring.keyring_seq,
                generated: Some(path),
            });
        }
        (None, None) => {
            return Err(invalid(
                "an encrypted volume needs a passphrase or a key file",
            ))
        }
    }
    .map_err(Error::from)?;
    let (bytes, seq) = match (&kf, pass) {
        (Some(_), Some(slot)) => {
            let (kr, _) =
                keyring::add_slot(&created.keyring, Profile::Default, &created.root, &slot)
                    .map_err(Error::from)?;
            (keyring::encode(&kr).map_err(Error::from)?, kr.keyring_seq)
        }
        _ => (created.bytes, created.keyring.keyring_seq),
    };
    Ok(Keys {
        root: Some(created.root),
        keyring: Some(bytes),
        keyring_seq: seq,
        generated: None,
    })
}

/// Creates a volume directory: the layout, the key material (if any), the
/// MAC'd descriptor, `volume.json`, the genesis FULL commit with an empty map,
/// and `state/pins` with the trust-on-first-use pin. The caller adds the
/// registry entry.
///
/// # Errors
/// `Invalid` for parameters the format refuses; I/O and crypto failures.
/// On failure the half-made volume directory is removed again.
pub fn create_volume(p: &CreateParams<'_>) -> Result<CreatedVolume, StackError> {
    let cs = u64::from(p.chunk_size);
    if p.size_bytes == 0 || !p.size_bytes.is_multiple_of(cs) {
        return Err(invalid(
            "the size must be a non-zero multiple of the chunk size",
        ));
    }
    if p.id_len == IdLen::Zero {
        return Err(invalid("id_len 0 (lean map) is refused in v1"));
    }
    if !p.policy.level_ok(p.level) {
        return Err(invalid("compression level not allowed for the policy"));
    }
    let uuid = new_uuid()?;
    let family = FamilyId(random_bytes()?);
    let voldir = p.parent.join(uuid.to_string());
    fsio::create_dir_durable(p.parent)?;
    std::fs::create_dir(&voldir)?;
    match create_in(p, uuid, family, &voldir) {
        Ok(generated_key_file) => Ok(CreatedVolume {
            uuid,
            voldir,
            generated_key_file,
        }),
        Err(e) => {
            // Best effort: nothing names this directory yet.
            let _ = std::fs::remove_dir_all(&voldir);
            let _ =
                std::fs::remove_dir_all(keyring_path(&voldir, &family).parent().unwrap_or(&voldir));
            Err(e)
        }
    }
}

fn create_in(
    p: &CreateParams<'_>,
    uuid: VolumeUuid,
    family: FamilyId,
    voldir: &Path,
) -> Result<Option<PathBuf>, StackError> {
    let keys = make_keys(p, family, voldir)?;
    let mut desc = VolumeDescriptor {
        crypto_mode: p.mode,
        profile: Profile::Default,
        block_size: BLOCK_SIZE,
        chunk_size: p.chunk_size,
        map_fanout: p.map_fanout,
        id_len: p.id_len,
        codec_policy: p.policy,
        codec_level: p.level,
        volume_uuid: uuid,
        family_id: family,
        kmk_fingerprint: [0; 16],
        created_at: SystemClock.now(),
        lineage: Vec::new(),
        mac: [0; 32],
    };
    let volume_bytes = seal_descriptor(&mut desc, keys.root.as_ref())?;
    if let Some(kr) = &keys.keyring {
        let path = keyring_path(voldir, &family);
        if let Some(dir) = path.parent() {
            fsio::create_dir_durable(dir)?;
        }
        fsio::create_durable(&path, kr, 0o600)?;
    }
    let pin = Pin {
        mode: p.mode,
        fingerprint: desc.kmk_fingerprint,
    };
    let crypto = VolumeCrypto::new(&desc, keys.root, DekGen(0))?;
    let meta = Arc::clone(crypto.meta());
    let mut json = VolumeJson::new(p.name, &voldir.to_string_lossy());
    if let Some(f) = p.json {
        f(&mut json);
    }
    let meta_codec: Arc<dyn MetaCodec> = meta.clone();
    let store = LocalVolume::create(
        voldir,
        &volume_bytes,
        &json,
        meta_codec,
        StoreOptions::default(),
    )?;
    // Genesis (D5): commit_seq 0, the empty map of this size, parent_hash =
    // BLAKE3 of the descriptor bytes as stored.
    let source: Arc<dyn NodeSource> = Arc::new(store.clone());
    let map_cfg = MapConfig::new(p.map_fanout, p.id_len);
    let empty = RadixMap::new(
        Arc::clone(crypto.extents()),
        source,
        p.size_bytes / u64::from(p.chunk_size),
        map_cfg,
    )?
    .root();
    let genesis = Full::genesis(
        meta.object_hash(&volume_bytes),
        p.size_bytes,
        empty.hash,
        SystemClock.now(),
    );
    let sealed = meta.seal_commit(&Commit::Full(genesis))?;
    store.put(CommitSeq(0), &sealed.bytes)?;
    // The floor starts at the genesis commit (ADR-31): the first attach on
    // the host that made the volume is not a "first attach".
    store.raise_rollback_floor(&RollbackFloor {
        max_commit_seq: CommitSeq(0),
        tip_hash: sealed.hash,
    })?;
    store.store_local_pins(&LocalPins {
        volume_uuid: uuid,
        family_id: family,
        max_keyring_seq: keys.keyring_seq,
        max_epoch: 0,
        pin: Some(pin),
    })?;
    Ok(keys.generated)
}

// ---------------------------------------------------------------- open

/// The unlock material of an attach.
#[derive(Default)]
pub struct AttachCredential {
    /// A passphrase.
    pub passphrase: Option<lunix_iface::ctl::Secret>,
    /// A key file path.
    pub key_file: Option<PathBuf>,
    /// The KMK (or integrity key) fingerprint the caller expects.
    pub fingerprint: Option<[u8; 16]>,
    /// The mode the caller expects (ADR-11 F7). Key material without one is an
    /// encryption credential; `integrity` declares an integrity credential.
    pub mode: Option<CryptoMode>,
}

impl std::fmt::Debug for AttachCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AttachCredential")
            .field(
                "passphrase",
                &self.passphrase.as_ref().map(|_| "<redacted>"),
            )
            .field("key_file", &self.key_file)
            .field("fingerprint", &self.fingerprint.is_some())
            .field("mode", &self.mode)
            .finish()
    }
}

/// A hook that adjusts the engine settings (tests turn the background
/// coordinator off and drive `tick`).
pub type EngineTweak = Arc<dyn Fn(&mut EngineConfig) + Send + Sync>;

/// A change to the journal's settings, applied after the production
/// geometry (tests: a small `file_len` so each open scans less).
pub type JournalTweak = Arc<dyn Fn(&mut JournalConfig) + Send + Sync>;

/// What opening a volume needs.
pub struct OpenParams {
    /// `<dir>/<uuid>`.
    pub voldir: PathBuf,
    /// The unlock material.
    pub credential: AttachCredential,
    /// Journal lanes, one per queue.
    pub queues: u16,
    /// `max_io_buf_bytes` of the attach (`None`: `volume.json`).
    pub max_io_buf_bytes: Option<u32>,
    /// What to do about journal damage.
    pub on_journal_damage: OnJournalDamage,
    /// Engine settings hook.
    pub tweak: Option<EngineTweak>,
    /// The destage pool's worker threads (`None`: M9's default).
    pub destage_workers: Option<usize>,
    /// Segment seals that may run beside the destage packer (`None`: M9's
    /// default; 0 seals on the packer thread, as before ADR-33 T8).
    pub destage_seals: Option<usize>,
    /// Operator override of `ForkUnverifiable` (ADR-31); the caller audits it.
    pub allow_unverifiable_fork: bool,
    /// Lock the journal's I/O memory (DESIGN §3, W7); the daemon sets it when
    /// it runs hardened. A failed lock fails the open.
    pub lock_memory: bool,
    /// The store's durability calls (`None`: [`lunix_store::RealFs`]). Tests
    /// install a recording [`FsOps`] to know what a crash may lose; the
    /// daemon never sets it.
    pub store_fs: Option<Arc<dyn FsOps>>,
    /// Cap on the resident journal window (the daemon's
    /// `journal_resident_max`; `None`: the volume's `journal_max`).
    pub journal_resident_max: Option<u64>,
    /// The engine's cut target (the daemon's `cut_target_ms`, ADR-33).
    pub cut_target: std::time::Duration,
    /// Journal settings hook, applied after [`JournalConfig::new`] and
    /// `lock_memory` (`None`: the production geometry). Tests only; the
    /// daemon never sets it.
    pub journal_tweak: Option<JournalTweak>,
    /// The collector's own settings (`None`: the daemon's,
    /// [`lunix_gc::GcConfig::daemon`]). The GC settings of `volume.json` reach it through the
    /// engine's `gc_params`; tests shorten the pass interval here.
    pub gc: Option<lunix_gc::GcConfig>,
    /// The wait at the limit when `space.full_timeout_ms` is null (the
    /// daemon's `full_timeout_ms`, ADR-32).
    pub full_timeout: std::time::Duration,
    /// The paced band (the daemon's `throttle*` keys, ADR-32); `r_mid` comes
    /// from the volume's `gc_rate` at open.
    pub throttle: lunix_space::ThrottleConfig,
    /// The compaction rate's multiple while writes are slowed (the daemon's
    /// `gc_pressure_rate_factor`).
    pub gc_pressure_rate_factor: u64,
    /// Recheck-only GC passes under pressure, with their delay (the daemon's
    /// `gc_fast_recheck` and `gc_fast_recheck_delay_ms`; `None`: whatever
    /// `gc` says).
    pub gc_fast_recheck: Option<std::time::Duration>,
    /// The engine's per-segment reference table (the daemon's
    /// `gc_condemn_on_cut`); `Shadow` also makes the engine switch the
    /// collector's counting of the tip root's entries on.
    pub gc_condemn_on_cut: lunix_engine::CondemnOnCut,
    /// The short grace of condemned segments under pressure (the daemon's
    /// `gc_pressure_grace_ms`; `None`: off; default 2 s).
    pub gc_pressure_grace: Option<std::time::Duration>,
    /// A hook that runs inside every segment `read_extent` (before the open
    /// and after it) and may block, so that a read part outlives the
    /// collector's grace and a delete falls between its lookup, its open and
    /// its read. Tests only (`crashpoints` build); the daemon never sets it.
    #[cfg(feature = "crashpoints")]
    pub segment_read_hook: Option<lunix_store::ReadHook>,
    /// The band's middle rate in bytes per second, instead of
    /// `max(gc_rate, 16 MiB/s)` (`None`). Tests only: a slow band lets a
    /// small volume hold writes for seconds; the daemon never sets it.
    pub throttle_mid_rate: Option<u64>,
}

impl OpenParams {
    /// Defaults for `voldir` with `queues` lanes.
    pub fn new(voldir: PathBuf, queues: u16) -> Self {
        Self {
            voldir,
            credential: AttachCredential::default(),
            queues,
            max_io_buf_bytes: None,
            on_journal_damage: OnJournalDamage::Wipe,
            tweak: None,
            destage_workers: None,
            destage_seals: None,
            allow_unverifiable_fork: false,
            lock_memory: false,
            store_fs: None,
            journal_resident_max: None,
            cut_target: std::time::Duration::from_secs(1),
            journal_tweak: None,
            gc: None,
            full_timeout: lunix_space::DEFAULT_FULL_TIMEOUT,
            throttle: lunix_space::ThrottleConfig::default(),
            gc_pressure_rate_factor: 4,
            gc_fast_recheck: None,
            gc_condemn_on_cut: lunix_engine::CondemnOnCut::Off,
            gc_pressure_grace: Some(std::time::Duration::from_millis(2_000)),
            #[cfg(feature = "crashpoints")]
            segment_read_hook: None,
            throttle_mid_rate: None,
        }
    }
}

/// An open volume: the engine and the components the daemon reports on.
/// Dropping it stops the engine's threads and releases `LOCK` (the store is
/// shared by every component; the lock goes with the last handle).
pub struct VolumeStack {
    /// The engine (drop it first: it holds every other component).
    pub engine: Arc<Engine>,
    /// The volume directory and stores.
    pub store: LocalVolume,
    /// The journal.
    pub journal: Arc<FileJournal>,
    /// The space gate.
    pub space: Arc<LunixSpace>,
    /// The chunk index.
    pub index: Arc<LsmIndex>,
    /// The unlocked codecs.
    pub crypto: Arc<VolumeCrypto>,
    /// The verified descriptor.
    pub desc: VolumeDescriptor,
    /// `volume.json`.
    pub json: VolumeJson,
    /// The volume directory.
    pub voldir: PathBuf,
    /// The rollback floor found before the engine opened (`None`: first
    /// attach on this host, which cannot detect an earlier rollback).
    pub floor_before: Option<RollbackFloor>,
    /// Runs of the space ladder's compact step that ended without an error
    /// (with or without bytes freed), for tests and diagnostics; a failed
    /// run is in `LunixSpace::step_errors`.
    pub compact_runs: Arc<AtomicU64>,
    /// Calls that use the engine besides its owner (a `Gc` call, the compact
    /// step); a detach closes it and waits for them.
    pub borrows: Arc<Borrows>,
}

impl std::fmt::Debug for VolumeStack {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VolumeStack")
            .field("voldir", &self.voldir)
            .field("uuid", &self.desc.volume_uuid)
            .finish_non_exhaustive()
    }
}

/// The map constructor the engine calls at open: `lunix-map` over the pack
/// store, with the descriptor's fanout and `volume.json`'s cache settings.
pub struct RadixOpener {
    /// Node hashing and sealing.
    pub codec: Arc<dyn ExtentCodec>,
    /// Where nodes are read.
    pub packs: Arc<dyn PackRead>,
    /// Fanout, id length and caches.
    pub cfg: MapConfig,
}

impl MapOpener for RadixOpener {
    fn open(&self, root: &MapRoot, chunk_count: u64) -> lunix_iface::Result<Arc<dyn BlockMap>> {
        let source: Arc<dyn NodeSource> = Arc::new(PackSource(Arc::clone(&self.packs)));
        Ok(Arc::new(RadixMap::open(
            Arc::clone(&self.codec),
            source,
            root,
            chunk_count,
            self.cfg,
        )?))
    }
}

struct PackSource(Arc<dyn PackRead>);

impl NodeSource for PackSource {
    fn load_node(
        &self,
        loc: lunix_format::node::NodeLoc,
        out: &mut Vec<u8>,
    ) -> lunix_iface::Result<()> {
        self.0.read_node(loc, out)
    }
}

/// `state/pins` read without the volume lock, before the store can be opened
/// (the attach check needs the pin to build the `MetaCodec` the store needs).
/// A missing file is "no pins"; a rejected one is an error (on-disk-formats
/// §20), as the store treats it: reading it as absent would drop the floor.
///
/// # Errors
/// `Corrupt` for a rejected file, or the read's error.
pub fn read_pins_unlocked(voldir: &Path) -> Result<Option<LocalPinsFile>, StackError> {
    match fsio::read_capped(&voldir.join(lpath::state_pins()), 4096)? {
        None => Ok(None),
        Some(bytes) => LocalPinsFile::decode(&bytes).map(Some).map_err(|_| {
            StackError::Store(Error::Corrupt {
                what: "state/pins",
                detail: "rejected (length, version or checksum); fsck --repair rewrites it",
            })
        }),
    }
}

/// Runs the attach check of ADR-11 F7 for `voldir` with `cred`.
///
/// The request decides the mode (ADR-11 F7): a supplied passphrase or key
/// file is an **encryption** credential unless the request declares
/// `integrity` ([`AttachCredential::mode`]). Without a credential an
/// `integrity` volume is unlocked with its own `integrity.key` (D16).
///
/// # Errors
/// [`StackError::Attach`] with the matrix's result, or a read failure.
pub fn unlock(
    voldir: &Path,
    desc: &VolumeDescriptor,
    cred: &AttachCredential,
) -> Result<lunix_crypto::AttachOk, StackError> {
    let keyring = if desc.crypto_mode == CryptoMode::Plain {
        None
    } else {
        fsio::read_capped(&keyring_path(voldir, &desc.family_id), MAX_KEYRING)?
    };
    let pins = read_pins_unlocked(voldir)?;
    let pin = pins.as_ref().and_then(|p| p.pins.pin);
    let seen = pins.as_ref().map_or(0, |p| p.pins.max_keyring_seq);
    let declares_integrity = cred.mode == Some(CryptoMode::Integrity);
    if cred.mode == Some(CryptoMode::Plain)
        && (cred.passphrase.is_some() || cred.key_file.is_some())
    {
        return Err(invalid("a plain volume takes no key material"));
    }
    let own_key = voldir.join(lpath::integrity_key());
    let supplied = match (&cred.passphrase, &cred.key_file) {
        (Some(_), Some(_)) => {
            return Err(invalid("give a passphrase or a key file, not both"));
        }
        (Some(pw), None) => Some(Unlock::Passphrase(pw.expose())),
        (None, Some(path)) => Some(Unlock::KeyFilePath(path)),
        (None, None) => None,
    };
    let credential = match supplied {
        Some(u) if declares_integrity => Some(Credential::Integrity(u)),
        Some(u) => Some(Credential::Key(u)),
        None if desc.crypto_mode == CryptoMode::Integrity && own_key.exists() => {
            Some(Credential::Integrity(Unlock::KeyFilePath(&own_key)))
        }
        None => None,
    };
    let req = AttachRequest {
        credential,
        fingerprint: cred.fingerprint,
        pin,
        keyring_seq_seen: seen,
        unlock: UnlockOptions::default(),
    };
    Ok(check_attach(desc, keyring.as_deref(), &req)?)
}

/// Takes a volume's `LOCK` after the attach check passed, without opening the
/// engine: for housekeeping that must not race another daemon on the same
/// directory (clearing a stale `state/device`, deleting a DEAD device).
///
/// # Errors
/// The attach check's refusal, `Busy` while another process holds `LOCK`, or
/// a read failure.
pub fn lock_volume(voldir: &Path, cred: &AttachCredential) -> Result<LocalVolume, StackError> {
    let bytes = read_volume_file(voldir)?;
    let desc = VolumeDescriptor::decode(&bytes).map_err(Error::from)?;
    let ok = unlock(voldir, &desc, cred)?;
    let meta: Arc<dyn MetaCodec> = ok.crypto.meta().clone();
    Ok(LocalVolume::open(voldir, meta, StoreOptions::default())?)
}

/// Records the pin (first use) and the highest keyring sequence seen, keeping
/// the rollback floor. Called after the engine opened (R4 first).
fn record_pins(
    store: &LocalVolume,
    desc: &VolumeDescriptor,
    record_pin: Option<Pin>,
    keyring_seq: Option<u64>,
) -> Result<(), StackError> {
    let cur = store.local_pins()?;
    let mut next = cur.clone().unwrap_or(LocalPins {
        volume_uuid: desc.volume_uuid,
        family_id: desc.family_id,
        max_keyring_seq: 0,
        max_epoch: 0,
        pin: None,
    });
    if next.pin.is_none() {
        next.pin = record_pin;
    }
    if let Some(s) = keyring_seq {
        next.max_keyring_seq = next.max_keyring_seq.max(s);
    }
    if cur.as_ref() != Some(&next) {
        store.store_local_pins(&next)?;
    }
    Ok(())
}

/// The cap a percentage `percent` of a volume of `size` bytes gives (ADR-32
/// M2, FREEZE F2): `percent` of the size, never below the size plus twice
/// `dirty_bytes` (until a cut the journal holds up to `commit.dirty_bytes`,
/// counted twice during the cut, so a small volume's first fill must not
/// reach the cap).
pub(crate) fn cache_cap(size: u64, percent: u32, dirty_bytes: u64) -> u64 {
    let cap = u64::try_from(u128::from(size) * u128::from(percent) / 100).unwrap_or(u64::MAX);
    cap.max(size.saturating_add(dirty_bytes.saturating_mul(2)))
}

/// The space settings the gate gets for a volume of `size` bytes: a
/// percentage cap (`space.cache_max_percent`) resolved to `cache_max` bytes
/// (FREEZE F2). The gate knows no size and refuses an unresolved one.
pub fn effective_space(json: &VolumeJson, size: u64) -> SpaceSettings {
    let mut s = json.space.clone();
    if let Some(p) = s.cache_max_percent.take() {
        s.cache_max = Some(cache_cap(size, p, json.commit.dirty_bytes));
    }
    s
}

/// Opens a volume: the attach check, the store, every component, the
/// engine (recovery), then the pins. See the module docs for the order.
///
/// # Errors
/// [`StackError`]; nothing is written before R4 passed (the engine's own
/// recovery writes only the sweep and the floor).
pub fn open_volume(p: OpenParams) -> Result<VolumeStack, StackError> {
    let voldir = p.voldir;
    let volume_bytes = read_volume_file(&voldir)?;
    let desc = VolumeDescriptor::decode(&volume_bytes).map_err(Error::from)?;
    let ok = unlock(&voldir, &desc, &p.credential)?;
    let crypto = Arc::new(ok.crypto);
    let meta = Arc::clone(crypto.meta());
    let meta_codec: Arc<dyn MetaCodec> = meta.clone();
    let mut sopts = StoreOptions::default();
    if let Some(fs) = &p.store_fs {
        sopts.fs = Arc::clone(fs);
    }
    let store = LocalVolume::open(&voldir, meta_codec, sopts)?;
    for w in store.take_warnings() {
        tracing::warn!(volume = %desc.volume_uuid, warning = %w, "store");
    }
    let json = store.volume_json()?;
    let floor_before = store.rollback_floor()?;

    let uuid = desc.volume_uuid;
    let cs = desc.chunk_size;
    let mut jcfg = JournalConfig::new(voldir.join("journal"), uuid, p.queues, json.journal_max);
    jcfg.lock_memory = p.lock_memory;
    jcfg.resident_max = p.journal_resident_max;
    if let Some(t) = &p.journal_tweak {
        t(&mut jcfg);
    }
    // A lane needs two ring files (lunix-journal): refuse a ring that cannot
    // hold the queues before anything is created.
    let need = u64::from(p.queues) * 2 * jcfg.file_len;
    if jcfg.journal_max < need {
        return Err(invalid(format!(
            "journal_max {} MiB is below queues x 2 ring files of {} MiB ({} queues need {} MiB); attach with fewer queues or raise journal_max",
            jcfg.journal_max >> 20,
            jcfg.file_len >> 20,
            p.queues,
            need >> 20
        )));
    }
    let journal = Arc::new(FileJournal::open(jcfg)?);
    let overlay = Arc::new(ShardedOverlay::new(u64::from(cs) / 4096));
    let index = Arc::new(LsmIndex::open(&voldir, store.ids(), desc.id_len)?);
    let report = index.open_report();
    if report.fell_back || !report.discarded_runs.is_empty() {
        tracing::warn!(volume = %uuid, fell_back = report.fell_back, discarded = report.discarded_runs.len(), "index manifest fell back; catch-up rebuilds it");
    }
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    // A percentage cap needs the volume size, which the engine knows only
    // once it opened: no cap until then, the resolved one right after. Safe
    // because nothing can reserve in between: recovery only charges, and no
    // queue handler exists before `open_volume` returns (a change that
    // attached queues earlier would have to resolve the cap first).
    let mut early = json.space.clone();
    early.cache_max_percent = None;
    let mut space_cfg = SpaceConfig::new(early);
    space_cfg.full_timeout = p.full_timeout;
    space_cfg.throttle = p.throttle.with_gc_rate(json.gc_rate);
    if let Some(r) = p.throttle_mid_rate {
        space_cfg.throttle.mid_rate = r;
    }
    let space = LunixSpace::new(
        space_cfg,
        Arc::new(StatvfsProbe::new(&voldir)),
        Arc::clone(&clock),
    )?;
    let extents = Arc::clone(crypto.extents());
    let map_cfg = MapConfig::new(desc.map_fanout, desc.id_len)
        .with_cache_max(usize::try_from(json.map_cache_max).unwrap_or(usize::MAX))
        .with_dirty_leaf_cap(json.map_dirty_leaves_max);
    let mut dcfg = DestageConfig::default();
    if let Some(w) = p.destage_workers {
        dcfg.workers = w.max(1);
        dcfg.result_window = dcfg.result_window.max(dcfg.workers);
    }
    if let Some(n) = p.destage_seals {
        dcfg.max_seals_in_flight = n;
    }
    let destager = PipelineDestager::new(DestageParams::from_descriptor(&desc), dcfg);

    let mut cfg = EngineConfig::from_volume_json(&json, cs, desc.id_len);
    // M11 re-review minor 8: the fanout comes from the descriptor.
    cfg.map_fanout = u32::from(desc.map_fanout);
    cfg.on_journal_damage = p.on_journal_damage;
    cfg.allow_unverifiable_fork = p.allow_unverifiable_fork;
    cfg.full_timeout = space.full_timeout();
    cfg.gc_pressure_rate_factor = p.gc_pressure_rate_factor;
    cfg.gc_condemn_on_cut = p.gc_condemn_on_cut;
    cfg.gc_pressure_grace = p.gc_pressure_grace;
    cfg.cut_target = p.cut_target;
    // GC (plan M17, the M12 wiring): one budgeted step at every C10 and on
    // idle ticks, with the settings of volume.json. A step may compact up to
    // one second of `gc_rate`: its time budget bounds it, and the collector's
    // own rate cap keeps the mean at `gc_rate`. A cap of one budget's worth
    // (`gc_rate` x 50 ms) held compaction to a few MiB per step, far below
    // `gc_rate` while the cuts leave the coordinator few steps per second
    // (milestone gcrate, the M15 root cell).
    let budget = std::time::Duration::from_millis(json.gc_step_budget_ms);
    cfg.gc_budget = Some(lunix_iface::gc::GcBudget {
        max_duration: budget,
        max_bytes: json.gc_rate.max(1 << 20),
    });
    cfg.gc_params = Some(lunix_iface::gc::GcParams {
        retain_last: json.retain_last,
        grace: std::time::Duration::from_millis(json.gc_grace_ms),
        read_timeout: std::time::Duration::from_millis(json.read_timeout_ms),
        rate_bytes_per_s: json.gc_rate,
        live_threshold: json.gc_live_threshold,
        chunk_size: cs,
        id_len: desc.id_len,
    });
    if let Some(b) = p.max_io_buf_bytes {
        cfg.max_io_buf_bytes = b;
    }
    if let Some(t) = &p.tweak {
        t(&mut cfg);
    }
    #[cfg(feature = "crashpoints")]
    if let Some(h) = &p.segment_read_hook {
        store.set_read_hook(Some(Arc::clone(h)));
    }
    let dir: Arc<dyn VolumeDir> = Arc::new(store.clone());
    let mut gc_config = p.gc.unwrap_or_else(lunix_gc::GcConfig::daemon);
    if p.gc_fast_recheck.is_some() {
        gc_config.fast_recheck = p.gc_fast_recheck;
    }
    let deps = EngineDeps {
        dir: Arc::clone(&dir),
        segments: Arc::new(store.clone()),
        packs: Arc::new(store.clone()),
        commits: Arc::new(store.clone()),
        checkpoint: Arc::new(store.clone()),
        pins: Arc::new(store.clone()),
        gc_state: Arc::new(store.clone()),
        floor: Arc::new(DirFloor(dir)),
        journal: journal.clone(),
        overlay,
        index: index.clone(),
        extents: Arc::clone(&extents),
        commit_codec: meta.clone(),
        space: space.clone(),
        clock,
        maps: Arc::new(RadixOpener {
            codec: extents,
            packs: Arc::new(store.clone()),
            cfg: map_cfg,
        }),
        destager: Arc::new(destager),
        // Built once and kept as long as the engine: unnamed compaction
        // outputs and undelivered moves are memory only (gc.md "What the
        // wiring must do"). The rollback floor comes from the volume
        // directory.
        collector: Some(Box::new(lunix_gc::Gc::new(
            Arc::new(store.clone()) as Arc<dyn lunix_gc::FloorSource>,
            gc_config,
        ))),
        hooks: Arc::new(NoHooks),
    };
    let engine = Arc::new(Engine::open(cfg, deps)?);
    if json.space.cache_max_percent.is_some() {
        space.reconfigure(&effective_space(&json, engine.size_bytes()))?;
    }
    // The ladder's compact step is GC's urgent pass (plan M12 step 12).
    let compact_runs = Arc::new(AtomicU64::new(0));
    let borrows = Arc::new(Borrows::default());
    space.register(
        LadderStepKind::Compact,
        Box::new(CompactStep {
            engine: Arc::downgrade(&engine),
            runs: Arc::clone(&compact_runs),
            borrows: Arc::clone(&borrows),
        }),
    );
    record_pins(&store, &desc, ok.record_pin, ok.keyring_seq)?;
    Ok(VolumeStack {
        engine,
        store,
        journal,
        space,
        index,
        crypto,
        desc,
        json,
        voldir,
        floor_before,
        compact_runs,
        borrows,
    })
}
