//! The in-process volume builder for the stack tests (M12b), fsck (M14), GC
//! on the real stack (M17.B) and the phase 1 exit tests (M15): a volume made
//! by [`crate::stack`] on a scratch directory, opened as a [`VolumeStack`]
//! without a block device, driven through the engine's queue handlers, and
//! crashed the way the plan's stack model test defines a crash.
//!
//! Test code only: the module exists with the `testsupport` cargo feature.
//!
//! # Stable API (M17.B, M15)
//!
//! | item | what |
//! |---|---|
//! | [`VolumeBuilder`] | the settings of a new volume (mode, chunk size, size, queues, `volume.json` and engine tweaks) and [`VolumeBuilder::create`] |
//! | [`TestVolume`] | a created volume ([`at`](TestVolume::at) for an existing directory): [`open`](TestVolume::open), [`open_with`](TestVolume::open_with), [`params`](TestVolume::params), [`crash`](TestVolume::crash), [`crash_driven`](TestVolume::crash_driven), [`crash_with`](TestVolume::crash_with), [`check`](TestVolume::check), [`fsck_args`](TestVolume::fsck_args), [`copy_as`](TestVolume::copy_as); and [`close`] |
//! | [`Driver`] | the queue driver: requests go to the engine's [`QueueHandler`]s as a ublk queue would hand them over, one at a time, so the order of the writes is the order of their `write_seq` |
//! | [`DeviceModel`], [`ModelOp`] | the flat-device oracle with the crash question: after a crash the device equals the oracle after some prefix of the mutating requests, at least as long as the durable prefix |
//! | [`payload`] | deterministic write data, incompressible, half or highly compressible by seed |
//! | [`damage_journal`], [`damage_unsynced`] | the two halves of a crash, usable on a volume directory nothing has open |
//! | [`TestRng`] | a seeded generator (no `rand` dependency) |
//! | [`Scratch`], [`fsck`], [`fsck_binary`] | a scratch directory on a real file system, and `lunix-fsck` on a closed volume |
//!
//! Additions keep these signatures; a change that breaks them is listed in
//! the milestone report that makes it.
//!
//! # What a crash is (plan M12, stack model test)
//!
//! [`TestVolume::crash_with`] drops the engine without a final commit, then:
//!
//! 1. **Journal:** every lane's records past its last flush point are damaged
//!    at byte level ([`damage_journal`]). The flush point is the larger of
//!    the caller's bound (what a completed FLUSH or FUA write covered, which
//!    the [`Driver`] tracks) and the flushed watermark the surviving records
//!    and marks carry on disk (ADR-4): a crash cannot touch a record a
//!    completed group flush made durable, so nothing at or below it changes.
//!    Each record above it is kept, zeroed (never written), torn from a
//!    sector boundary, or has one byte flipped, at random, independently in
//!    every lane and in the mark ring.
//! 2. **Files never synced:** `*.tmp` and `*.open` files (the store's and the
//!    index's unsealed objects; the only files whose directory entry the
//!    store never syncs) are deleted, truncated or kept at random
//!    ([`damage_unsynced`]).
//!
//! The store's `FsOps` test hook (renames and creates not yet covered by a
//! directory `fsync`) cannot be installed through
//! [`crate::stack::OpenParams`] today; the M12b report proposes the field.
//! With every store object made visible by rename and directory `fsync`
//! before anything names it, and the drop crash happening between calls,
//! the unsynced set is the one above.
//!
//! # Example
//!
//! ```no_run
//! use lunixd::testsupport::{close, Driver, VolumeBuilder};
//! use lunix_format::CryptoMode;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let vol = VolumeBuilder::new("/scratch/vols")
//!     .manual()
//!     .mode(CryptoMode::Encrypted)
//!     .chunk_size(16 << 10)
//!     .create()?;
//! let stack = vol.open()?;
//! let mut d = Driver::new(&stack)?;
//! assert!(d.write(0, 0, &[7u8; 4096], false).is_ok());
//! assert!(d.flush(0).is_ok());
//! let damage = vol.crash_driven(d, stack, 1);
//! assert!(damage.durable_floor.0 >= 1);
//! let stack = vol.open()?;
//! close(stack)?;
//! # Ok(())
//! # }
//! ```

use std::collections::HashMap;
use std::fs::OpenOptions;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use lunix_crypto::keyring::{Argon2Cost, KeyFile};
use lunix_engine::{EngineConfig, OnJournalDamage};
use lunix_format::journal::{scan_file, RecordKind, RECORD_HEADER_LEN};
use lunix_format::local::VolumeJson;
use lunix_format::volume::{CodecPolicy, VolumeDescriptor};
use lunix_format::{CryptoMode, IdLen, VolumeUuid, WriteSeq};
use lunix_iface::block::{
    BlockEngine, Completer, IoOp, IoRequest, IoStatus, QueueConfig, QueueHandler, QueueId, Tag,
};
use lunix_iface::ctl::Secret;
use lunix_iface::journal::Journal;
use lunix_iface::Error;
use lunix_journal::FileJournal;
use lunix_store::read_volume_file;

use crate::check::{check_volume, CheckReport};
use crate::stack::{
    create_volume, open_volume, random_bytes, AttachCredential, CreateParams, EngineTweak,
    OpenParams, StackError, VolumeStack,
};

/// A change to the `volume.json` defaults.
pub type JsonTweak = Box<dyn Fn(&mut VolumeJson) + Send + Sync>;

/// The Argon2id cost of a test passphrase slot (the smallest the keyring
/// takes in tests).
pub const TEST_ARGON2: Argon2Cost = Argon2Cost {
    m_kib: 19_456,
    t: 2,
    p: 1,
};

const BLOCK: u64 = 4096;

/// Builds a new volume under a scratch directory.
///
/// The fields are public; the `#[must_use]` setters are shorthands.
pub struct VolumeBuilder {
    /// The directory that holds the volume directory (and, for an encrypted
    /// volume without a passphrase, its generated key file).
    pub parent: PathBuf,
    /// Chunk size (16 KiB or 64 KiB in the stack tests).
    pub chunk_size: u32,
    /// Size in bytes (a multiple of the chunk size).
    pub size_bytes: u64,
    /// Crypto mode.
    pub mode: CryptoMode,
    /// A passphrase slot. Without one an encrypted volume gets a generated key
    /// file next to the volume directory ([`TestVolume::key_file`]) and an
    /// integrity volume its own `integrity.key` (D16).
    pub passphrase: Option<String>,
    /// Journal lanes (queues).
    pub queues: u16,
    /// `volume.json` changes, applied after the builder's own default
    /// ([`default_journal_max`](Self::default_journal_max)).
    pub json: Option<JsonTweak>,
    /// Engine settings (background coordinator off, manual ticks).
    pub engine: Option<EngineTweak>,
    /// Compression policy (descriptor).
    pub policy: CodecPolicy,
    /// Compression level for `policy`.
    pub level: i8,
    /// Map id-prefix length (16 or 32).
    pub id_len: IdLen,
    /// Map fanout (256 or 512).
    pub map_fanout: u16,
    /// The destage pool's worker threads (`None`: M9's default).
    pub destage_workers: Option<usize>,
    /// What an open does about journal damage.
    pub on_journal_damage: OnJournalDamage,
}

impl VolumeBuilder {
    /// Defaults: 64 KiB chunks, 1 GiB, `integrity`, one queue, `Auto`
    /// compression at level 1, 16-byte id prefixes, fanout 512, the daemon's
    /// engine settings (background coordinator on).
    pub fn new(parent: impl Into<PathBuf>) -> Self {
        Self {
            parent: parent.into(),
            chunk_size: 64 << 10,
            size_bytes: 1 << 30,
            mode: CryptoMode::Integrity,
            passphrase: None,
            queues: 1,
            json: None,
            engine: None,
            policy: CodecPolicy::Auto,
            level: 1,
            id_len: IdLen::Sixteen,
            map_fanout: 512,
            destage_workers: None,
            on_journal_damage: OnJournalDamage::Wipe,
        }
    }

    /// Sets the crypto mode.
    #[must_use]
    pub fn mode(mut self, mode: CryptoMode) -> Self {
        self.mode = mode;
        self
    }

    /// Sets the chunk size.
    #[must_use]
    pub fn chunk_size(mut self, bytes: u32) -> Self {
        self.chunk_size = bytes;
        self
    }

    /// Sets the size.
    #[must_use]
    pub fn size_bytes(mut self, bytes: u64) -> Self {
        self.size_bytes = bytes;
        self
    }

    /// Sets the queue (lane) count.
    #[must_use]
    pub fn queues(mut self, queues: u16) -> Self {
        self.queues = queues;
        self
    }

    /// Sets a passphrase slot.
    #[must_use]
    pub fn passphrase(mut self, passphrase: &str) -> Self {
        self.passphrase = Some(passphrase.to_owned());
        self
    }

    /// Deterministic engine for tests: no background coordinator (the test
    /// calls `commit_now` and `tick`), two read helpers, two destage workers.
    /// Replaces any earlier engine tweak.
    #[must_use]
    pub fn manual(mut self) -> Self {
        self.engine = Some(Arc::new(|c: &mut EngineConfig| {
            c.background = false;
            c.read_helpers = 2;
        }));
        self.destage_workers = Some(2);
        self
    }

    /// The `journal_max` the builder writes before the caller's tweak: the
    /// smallest ring the queues fit (`queues` x 128 MiB), at least 256 MiB.
    pub fn default_journal_max(&self) -> u64 {
        (u64::from(self.queues) * (128 << 20)).max(256 << 20)
    }

    /// Creates the volume (descriptor, keys, genesis) without opening it.
    ///
    /// # Errors
    /// What `stack::create_volume` reports, or the key file write.
    pub fn create(self) -> Result<TestVolume, StackError> {
        std::fs::create_dir_all(&self.parent)?;
        let key_file = if self.mode == CryptoMode::Encrypted && self.passphrase.is_none() {
            let r: [u8; 8] = random_bytes()?;
            let path = self.parent.join(format!("key-{}.bin", hex(&r)));
            let kf = KeyFile::generate().map_err(Error::from)?;
            kf.write_new(&path).map_err(Error::from)?;
            Some(path)
        } else {
            None
        };
        let journal_max = self.default_journal_max();
        let tweak = |j: &mut VolumeJson| {
            j.journal_max = journal_max;
            if let Some(t) = &self.json {
                t(j);
            }
        };
        let created = create_volume(&CreateParams {
            name: "test",
            parent: &self.parent,
            size_bytes: self.size_bytes,
            chunk_size: self.chunk_size,
            mode: self.mode,
            policy: self.policy,
            level: self.level,
            id_len: self.id_len,
            map_fanout: self.map_fanout,
            key_file: key_file.as_deref(),
            passphrase: self.passphrase.as_deref().map(str::as_bytes),
            argon2: TEST_ARGON2,
            json: Some(&tweak),
        })?;
        Ok(TestVolume {
            voldir: created.voldir,
            builder: self,
            uuid: created.uuid,
            key_file,
        })
    }
}

/// A volume made by [`VolumeBuilder`].
pub struct TestVolume {
    /// `<parent>/<uuid>`.
    pub voldir: PathBuf,
    /// The builder's settings, for reopening.
    pub builder: VolumeBuilder,
    /// The volume uuid.
    pub uuid: VolumeUuid,
    /// The generated key file of an encrypted volume made without a
    /// passphrase.
    pub key_file: Option<PathBuf>,
}

impl std::fmt::Debug for TestVolume {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TestVolume")
            .field("voldir", &self.voldir)
            .field("mode", &self.builder.mode)
            .field("chunk_size", &self.builder.chunk_size)
            .finish_non_exhaustive()
    }
}

impl TestVolume {
    /// An existing volume directory made with `builder`'s settings (a child
    /// process of a crash test, a copy), with the key file of an encrypted
    /// volume made without a passphrase. Reads the uuid from the descriptor.
    ///
    /// # Errors
    /// The descriptor cannot be read or decoded.
    pub fn at(
        voldir: impl Into<PathBuf>,
        builder: VolumeBuilder,
        key_file: Option<PathBuf>,
    ) -> Result<TestVolume, StackError> {
        let voldir = voldir.into();
        let bytes = read_volume_file(&voldir)?;
        let desc = VolumeDescriptor::decode(&bytes).map_err(Error::from)?;
        Ok(TestVolume {
            voldir,
            builder,
            uuid: desc.volume_uuid,
            key_file,
        })
    }

    /// The credential an attach of this volume needs.
    pub fn credential(&self) -> AttachCredential {
        let b = &self.builder;
        let passphrase = b
            .passphrase
            .as_ref()
            .map(|p| Secret::new(p.as_bytes().to_vec()));
        match b.mode {
            CryptoMode::Plain => AttachCredential::default(),
            CryptoMode::Integrity => AttachCredential {
                mode: passphrase.is_some().then_some(CryptoMode::Integrity),
                passphrase,
                ..AttachCredential::default()
            },
            CryptoMode::Encrypted => AttachCredential {
                passphrase,
                key_file: self.key_file.clone(),
                ..AttachCredential::default()
            },
        }
    }

    /// The open parameters the builder implies (credential, queues, engine
    /// tweak, destage workers, journal-damage policy).
    pub fn params(&self) -> OpenParams {
        let b = &self.builder;
        let mut p = OpenParams::new(self.voldir.clone(), b.queues);
        p.credential = self.credential();
        p.tweak.clone_from(&b.engine);
        p.destage_workers = b.destage_workers;
        p.on_journal_damage = b.on_journal_damage;
        p
    }

    /// Opens the volume (recovery included) as the daemon would, without a
    /// device: build a [`Driver`] (or queue handlers) from the stack.
    ///
    /// # Errors
    /// What `stack::open_volume` reports (R4, attach check, recovery).
    pub fn open(&self) -> Result<VolumeStack, StackError> {
        open_volume(self.params())
    }

    /// Like [`open`](Self::open) with the parameters changed by `f` first.
    ///
    /// # Errors
    /// What `stack::open_volume` reports.
    pub fn open_with(&self, f: impl FnOnce(&mut OpenParams)) -> Result<VolumeStack, StackError> {
        let mut p = self.params();
        f(&mut p);
        open_volume(p)
    }

    /// A crash: drops `stack` without a final commit, then damages every
    /// journal lane past its last flush point and handles the files that were
    /// never synced (see the module docs).
    ///
    /// Without a driver's bound, the flush point is the watermark on disk:
    /// this waits three mark delays first, so the journal has witnessed every
    /// completed flush with a mark (ADR-4). A caller with a [`Driver`] uses
    /// [`crash_driven`](Self::crash_driven), which needs no wait.
    pub fn crash(&self, stack: VolumeStack, seed: u64) -> CrashDamage {
        std::thread::sleep(Duration::from_millis(150));
        self.crash_with(stack, WriteSeq(0), seed)
    }

    /// A crash of a volume driven by `driver`: the driver is dropped (nothing
    /// is in flight between its calls), then [`crash_with`](Self::crash_with)
    /// with the driver's durable bound.
    pub fn crash_driven(&self, driver: Driver, stack: VolumeStack, seed: u64) -> CrashDamage {
        let durable = driver.durable();
        drop(driver);
        self.crash_with(stack, durable, seed)
    }

    /// A crash with `durable` known to be durable (every record at or below
    /// it was covered by a completed FLUSH or FUA write).
    pub fn crash_with(&self, stack: VolumeStack, durable: WriteSeq, seed: u64) -> CrashDamage {
        drop(stack);
        let mut rng = TestRng::new(seed);
        let mut d = damage_journal(&self.voldir, durable, &mut rng);
        let (removed, truncated) = damage_unsynced(&self.voldir, &mut rng);
        d.files_removed = removed;
        d.files_truncated = truncated;
        d
    }

    /// The store-level self-check (`lunixd check`) of the closed volume.
    ///
    /// # Errors
    /// The check could not run (attach refused, `LOCK` held).
    pub fn check(&self) -> Result<CheckReport, StackError> {
        check_volume(&self.voldir, &self.credential())
    }

    /// The credential arguments of `lunix-fsck` for this volume, or `None`
    /// when it needs a passphrase (which `lunix-fsck` reads from a
    /// descriptor only).
    pub fn fsck_args(&self) -> Option<Vec<String>> {
        match (self.builder.mode, &self.builder.passphrase, &self.key_file) {
            (CryptoMode::Plain, _, _) | (CryptoMode::Integrity, None, _) => Some(Vec::new()),
            (CryptoMode::Encrypted, None, Some(k)) => {
                Some(vec!["--key-file".into(), k.to_string_lossy().into_owned()])
            }
            _ => None,
        }
    }

    /// Copies the closed volume directory to `<parent>/<name>` (same parent,
    /// so the family keyring is shared) and returns it as a volume with the
    /// same settings, without the `volume.json` tweak (already applied). The
    /// copy has the same uuid: open one of the two at a time.
    ///
    /// # Errors
    /// A copy failure.
    pub fn copy_as(&self, name: &str) -> Result<TestVolume, StackError> {
        let to = self.builder.parent.join(name);
        copy_tree(&self.voldir, &to)?;
        let b = &self.builder;
        Ok(TestVolume {
            voldir: to,
            builder: VolumeBuilder {
                parent: b.parent.clone(),
                chunk_size: b.chunk_size,
                size_bytes: b.size_bytes,
                mode: b.mode,
                passphrase: b.passphrase.clone(),
                queues: b.queues,
                json: None,
                engine: b.engine.clone(),
                policy: b.policy,
                level: b.level,
                id_len: b.id_len,
                map_fanout: b.map_fanout,
                destage_workers: b.destage_workers,
                on_journal_damage: b.on_journal_damage,
            },
            uuid: self.uuid,
            key_file: self.key_file.clone(),
        })
    }
}

/// A clean close: the final commit (`quiesce`), then the drop. Drop every
/// [`Driver`] of the volume first.
///
/// # Errors
/// The final commit's error (the stack is dropped anyway).
pub fn close(stack: VolumeStack) -> Result<(), Error> {
    let r = stack.engine.quiesce();
    drop(stack);
    r
}

fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir(to)?;
    for e in std::fs::read_dir(from)? {
        let e = e?;
        let ty = e.file_type()?;
        let dst = to.join(e.file_name());
        if ty.is_dir() {
            copy_tree(&e.path(), &dst)?;
        } else if ty.is_file() {
            std::fs::copy(e.path(), &dst)?;
        }
    }
    Ok(())
}

fn hex(b: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(b.len() * 2);
    for x in b {
        let _ = write!(s, "{x:02x}");
    }
    s
}

// ------------------------------------------------------------------ rng

/// A seeded splitmix64 generator.
#[derive(Clone, Debug)]
pub struct TestRng(u64);

impl TestRng {
    /// A generator from `seed`.
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    /// The next 64 bits.
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// A value in `0..n` (`0` when `n` is 0).
    pub fn below(&mut self, n: u64) -> u64 {
        self.next_u64() % n.max(1)
    }
}

/// `len` deterministic bytes from `seed`. The low two bits of the seed pick
/// the kind: 0 and 3 incompressible, 1 half compressible (16 random bytes,
/// 16 zero bytes), 2 highly compressible (a repeated 64-byte pattern). Two
/// writes with one seed and one length carry the same bytes (dedup).
pub fn payload(seed: u64, len: usize) -> Vec<u8> {
    let mut r = TestRng::new(seed ^ 0x6c75_6e69_785f_7374);
    let mut v = Vec::with_capacity(len + 64);
    match seed & 3 {
        1 => {
            while v.len() < len {
                v.extend_from_slice(&r.next_u64().to_le_bytes());
                v.extend_from_slice(&r.next_u64().to_le_bytes());
                v.extend_from_slice(&[0u8; 16]);
            }
        }
        2 => {
            let mut pat = Vec::with_capacity(64);
            for _ in 0..8 {
                pat.extend_from_slice(&r.next_u64().to_le_bytes());
            }
            while v.len() < len {
                v.extend_from_slice(&pat);
            }
        }
        _ => {
            while v.len() < len {
                v.extend_from_slice(&r.next_u64().to_le_bytes());
            }
        }
    }
    v.truncate(len);
    v
}

// --------------------------------------------------------------- driver

/// The completions and tag buffers of a [`Driver`] (the ublk layer's side of
/// [`Completer`]).
#[derive(Debug)]
pub struct Completions {
    buf_len: usize,
    bufs: HashMap<Tag, Vec<u8>>,
    completed: Vec<(Tag, IoStatus)>,
}

impl Completions {
    fn new(buf_len: u32) -> Self {
        Self {
            buf_len: buf_len as usize,
            bufs: HashMap::new(),
            completed: Vec::new(),
        }
    }
}

impl Completer for Completions {
    fn complete(&mut self, tag: Tag, status: IoStatus) {
        assert!(
            !self.completed.iter().any(|(t, _)| *t == tag),
            "tag {tag:?} completed twice"
        );
        self.completed.push((tag, status));
    }

    fn tag_buf(&mut self, tag: Tag) -> &mut [u8] {
        let n = self.buf_len;
        self.bufs.entry(tag).or_insert_with(|| vec![0; n])
    }
}

/// Drives an engine's queue handlers directly, the way a ublk queue hands
/// requests over, one request at a time (each completes before the next is
/// submitted), so the order of the writes is the order of their
/// `write_seq`. Requests go to the queue the caller names (modulo the queue
/// count), so several lanes fill.
///
/// The driver tracks the durable bound a crash must respect
/// ([`durable`](Self::durable)). Drop it before closing or crashing the
/// stack; nothing is in flight between its calls.
pub struct Driver {
    queues: Vec<Box<dyn QueueHandler>>,
    done: Completions,
    journal: Arc<FileJournal>,
    depth: u16,
    next: u16,
    durable: WriteSeq,
    timeout: Duration,
    max_io: u32,
}

impl std::fmt::Debug for Driver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Driver")
            .field("queues", &self.queues.len())
            .field("durable", &self.durable)
            .finish_non_exhaustive()
    }
}

impl Driver {
    /// One handler per journal lane, tag buffers of the volume's
    /// `max_io_buf_bytes`.
    ///
    /// # Errors
    /// The engine refused a handler.
    pub fn new(stack: &VolumeStack) -> Result<Self, Error> {
        Self::with_buf(stack, stack.json.max_io_buf_bytes)
    }

    /// Like [`new`](Self::new) with tag buffers of `max_io_buf_bytes` (at
    /// most what the volume was opened with).
    ///
    /// # Errors
    /// The engine refused a handler.
    pub fn with_buf(stack: &VolumeStack, max_io_buf_bytes: u32) -> Result<Self, Error> {
        let n = stack.journal.lanes();
        let depth = 64;
        let mut queues = Vec::with_capacity(usize::from(n));
        for q in 0..n {
            queues.push(stack.engine.queue_handler(&QueueConfig {
                queue: QueueId(q),
                nr_queues: n,
                depth,
                max_io_buf_bytes,
            })?);
        }
        // The first lane open syncs the records recovery kept (journal docs,
        // on-disk-formats section 5.2): everything below the recovery's next
        // number is durable once the handlers exist.
        let recovered = stack.engine.recovery_report().next_seq.0.saturating_sub(1);
        Ok(Self {
            queues,
            done: Completions::new(max_io_buf_bytes),
            journal: Arc::clone(&stack.journal),
            depth,
            next: 0,
            durable: WriteSeq(recovered),
            timeout: Duration::from_secs(120),
            max_io: max_io_buf_bytes,
        })
    }

    /// The journal's highest issued `write_seq` now. Read right after a
    /// write completed, it is an upper bound of that write's number (the
    /// journal's own marks take numbers too): a commit whose cut is at or
    /// above it covers the write.
    pub fn issued(&self) -> WriteSeq {
        self.journal.last_issued()
    }

    /// The queue count.
    pub fn queues(&self) -> usize {
        self.queues.len()
    }

    /// The largest request the tag buffers take.
    pub fn max_io(&self) -> u32 {
        self.max_io
    }

    /// Every write at or below this `write_seq` is durable: recovery kept it
    /// (the first lane open synced it), or a FLUSH covering it or a FUA
    /// write at or above it completed successfully.
    pub fn durable(&self) -> WriteSeq {
        self.durable
    }

    /// Runs one request on queue `q` (modulo the queue count) and waits for
    /// its completion. Returns the status and, for a successful read, the
    /// data.
    ///
    /// # Panics
    /// When the request does not complete within two minutes, or the engine
    /// completes a tag twice or one that is not outstanding.
    pub fn run(
        &mut self,
        q: usize,
        op: IoOp,
        fua: bool,
        offset_bytes: u64,
        len: u32,
        data: &[u8],
    ) -> (IoStatus, Vec<u8>) {
        // Nothing is in flight: every write acknowledged so far has a number
        // at or below this, and a FLUSH's group covers at least this much.
        let before = self.journal.last_issued();
        let tag = Tag(self.next % self.depth);
        self.next = self.next.wrapping_add(1);
        let req = IoRequest {
            tag,
            op,
            fua,
            offset_bytes,
            len,
        };
        let mut buf = match op {
            IoOp::Write => data.to_vec(),
            IoOp::Read => vec![0u8; len as usize],
            _ => Vec::new(),
        };
        let qi = q % self.queues.len();
        self.queues[qi].on_request(req, &mut buf, &mut self.done);
        let start = Instant::now();
        let mut inline = true;
        let status = loop {
            if let Some(i) = self.done.completed.iter().position(|(t, _)| *t == tag) {
                break self.done.completed.swap_remove(i).1;
            }
            assert!(
                self.done.completed.is_empty(),
                "a tag completed that was not outstanding: {:?}",
                self.done.completed
            );
            inline = false;
            for h in &mut self.queues {
                h.poll(&mut self.done);
            }
            if !self.done.completed.iter().any(|(t, _)| *t == tag) {
                assert!(
                    start.elapsed() < self.timeout,
                    "{op:?} at {offset_bytes} (len {len}) on queue {qi} never completed"
                );
                std::thread::sleep(Duration::from_micros(20));
            }
        };
        let out = if op == IoOp::Read && status.is_ok() {
            if inline {
                buf
            } else {
                self.done.tag_buf(tag)[..len as usize].to_vec()
            }
        } else {
            Vec::new()
        };
        if status.is_ok() {
            match op {
                IoOp::Flush => self.durable = self.durable.max(before),
                // The FUA write's own number is above `before` (marks may
                // take numbers in between): `before + 1` is a lower bound.
                IoOp::Write if fua => self.durable = self.durable.max(WriteSeq(before.0 + 1)),
                _ => {}
            }
        }
        (status, out)
    }

    /// A write of `data` at `offset_bytes`.
    pub fn write(&mut self, q: usize, offset_bytes: u64, data: &[u8], fua: bool) -> IoStatus {
        let len = u32::try_from(data.len()).unwrap_or(u32::MAX);
        self.run(q, IoOp::Write, fua, offset_bytes, len, data).0
    }

    /// A read.
    ///
    /// # Errors
    /// The engine's status when it is not success.
    pub fn read(&mut self, q: usize, offset_bytes: u64, len: u32) -> Result<Vec<u8>, IoStatus> {
        let (s, d) = self.run(q, IoOp::Read, false, offset_bytes, len, &[]);
        if s.is_ok() {
            Ok(d)
        } else {
            Err(s)
        }
    }

    /// Reads `[0, size_bytes)` in requests of at most the buffer size,
    /// spread over the queues.
    ///
    /// # Errors
    /// The first failed read's status.
    pub fn read_all(&mut self, size_bytes: u64) -> Result<Vec<u8>, IoStatus> {
        let mut out = Vec::with_capacity(usize::try_from(size_bytes).unwrap_or(0));
        let step = u64::from(self.max_io).max(BLOCK);
        let mut off = 0;
        let mut q = 0;
        while off < size_bytes {
            let len = step.min(size_bytes - off);
            out.extend(self.read(q, off, u32::try_from(len).unwrap_or(u32::MAX))?);
            off += len;
            q += 1;
        }
        Ok(out)
    }

    /// A FLUSH on queue `q`.
    pub fn flush(&mut self, q: usize) -> IoStatus {
        self.run(q, IoOp::Flush, false, 0, 0, &[]).0
    }

    /// A DISCARD.
    pub fn discard(&mut self, q: usize, offset_bytes: u64, len: u32) -> IoStatus {
        self.run(q, IoOp::Discard, false, offset_bytes, len, &[]).0
    }

    /// A WRITE_ZEROES.
    pub fn write_zeroes(&mut self, q: usize, offset_bytes: u64, len: u32) -> IoStatus {
        self.run(q, IoOp::WriteZeroes, false, offset_bytes, len, &[])
            .0
    }
}

// ---------------------------------------------------------------- oracle

/// One mutating request (or a grow) in the order the engine accepted it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ModelOp {
    /// A write of [`payload`]`(seed, len)`.
    Write {
        /// Byte offset.
        offset: u64,
        /// Length in bytes.
        len: u32,
        /// The payload seed.
        seed: u64,
    },
    /// A DISCARD or WRITE_ZEROES (reads as zeros).
    Zero {
        /// Byte offset.
        offset: u64,
        /// Length in bytes.
        len: u32,
    },
    /// The device grew to this size.
    Grow(u64),
}

/// The flat-device oracle with an operation log.
///
/// Requests are applied in the order the engine accepted them (with the
/// [`Driver`], the submission order). After a crash the device must equal
/// the oracle after some prefix of the log at least [`durable`](Self::durable)
/// long (every request a completed FLUSH, FUA write or durable commit
/// covered); [`first_match`](Self::first_match) finds it and
/// [`truncate`](Self::truncate) makes it the new state.
#[derive(Clone, Debug)]
pub struct DeviceModel {
    initial: u64,
    ops: Vec<ModelOp>,
    cur: Vec<u8>,
    durable: usize,
}

fn to_usize(v: u64) -> usize {
    usize::try_from(v).expect("a test device fits in memory")
}

impl DeviceModel {
    /// A zeroed device of `size_bytes`.
    ///
    /// # Panics
    /// When the size does not fit in memory.
    pub fn new(size_bytes: u64) -> Self {
        Self {
            initial: size_bytes,
            ops: Vec::new(),
            cur: vec![0; to_usize(size_bytes)],
            durable: 0,
        }
    }

    /// The current size.
    pub fn size(&self) -> u64 {
        self.cur.len() as u64
    }

    /// The current contents.
    pub fn bytes(&self) -> &[u8] {
        &self.cur
    }

    /// The log.
    pub fn ops(&self) -> &[ModelOp] {
        &self.ops
    }

    /// Log length.
    pub fn len(&self) -> usize {
        self.ops.len()
    }

    /// True when nothing is logged.
    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }

    /// The durable prefix length.
    pub fn durable(&self) -> usize {
        self.durable
    }

    /// Everything logged so far is durable (a completed FLUSH, a durable
    /// commit, a grow).
    pub fn all_durable(&mut self) {
        self.durable = self.ops.len();
    }

    /// The first `n` entries are durable (never lowers the bound).
    pub fn durable_upto(&mut self, n: usize) {
        self.durable = self.durable.max(n.min(self.ops.len()));
    }

    fn apply_to(state: &mut Vec<u8>, op: &ModelOp) {
        match *op {
            ModelOp::Write { offset, len, seed } => {
                let o = to_usize(offset);
                state[o..o + len as usize].copy_from_slice(&payload(seed, len as usize));
            }
            ModelOp::Zero { offset, len } => {
                let o = to_usize(offset);
                state[o..o + len as usize].fill(0);
            }
            ModelOp::Grow(size) => state.resize(to_usize(size), 0),
        }
    }

    /// Logs and applies `op`; returns the log length.
    ///
    /// # Panics
    /// A range past the end, or a grow that shrinks.
    pub fn apply(&mut self, op: ModelOp) -> usize {
        match op {
            ModelOp::Write { offset, len, .. } | ModelOp::Zero { offset, len } => {
                assert!(
                    offset + u64::from(len) <= self.size(),
                    "{op:?} past the end"
                );
            }
            ModelOp::Grow(size) => assert!(size >= self.size(), "{op:?} shrinks"),
        }
        Self::apply_to(&mut self.cur, &op);
        self.ops.push(op);
        self.ops.len()
    }

    /// `len` bytes at `offset` of the current state.
    ///
    /// # Panics
    /// A range past the end.
    pub fn read(&self, offset: u64, len: u64) -> &[u8] {
        &self.cur[to_usize(offset)..to_usize(offset + len)]
    }

    /// The device after the first `n` entries.
    ///
    /// # Panics
    /// If `n` is past the end of the log.
    pub fn state_after(&self, n: usize) -> Vec<u8> {
        let mut s = vec![0u8; to_usize(self.initial)];
        for op in &self.ops[..n] {
            Self::apply_to(&mut s, op);
        }
        s
    }

    /// The smallest `n >= durable` such that `actual` equals the device
    /// after the first `n` entries, size included.
    pub fn first_match(&self, actual: &[u8]) -> Option<usize> {
        let mut cur = self.state_after(self.durable);
        let mut bad = count_bad(&cur, actual);
        if bad == 0 {
            return Some(self.durable);
        }
        for (i, op) in self.ops.iter().enumerate().skip(self.durable) {
            match *op {
                ModelOp::Write { offset, len, .. } | ModelOp::Zero { offset, len } => {
                    let first = to_usize(offset / BLOCK);
                    let last = to_usize((offset + u64::from(len)) / BLOCK);
                    let before = (first..last)
                        .filter(|&b| !block_eq(&cur, actual, b))
                        .count();
                    Self::apply_to(&mut cur, op);
                    let after = (first..last)
                        .filter(|&b| !block_eq(&cur, actual, b))
                        .count();
                    bad = bad + after - before;
                }
                ModelOp::Grow(_) => {
                    Self::apply_to(&mut cur, op);
                    bad = count_bad(&cur, actual);
                }
            }
            if bad == 0 {
                return Some(i + 1);
            }
        }
        None
    }

    /// Keeps the first `n` entries (what a crash left) and makes them the
    /// durable prefix.
    ///
    /// # Panics
    /// If `n` is past the end of the log.
    pub fn truncate(&mut self, n: usize) {
        self.cur = self.state_after(n);
        self.ops.truncate(n);
        self.durable = n;
    }
}

fn block_eq(a: &[u8], b: &[u8], block: usize) -> bool {
    let bs = BLOCK as usize;
    let r = block * bs..(block + 1) * bs;
    match (a.get(r.clone()), b.get(r)) {
        (Some(x), Some(y)) => x == y,
        (None, None) => true,
        _ => false,
    }
}

fn count_bad(a: &[u8], b: &[u8]) -> usize {
    let blocks = a.len().max(b.len()).div_ceil(BLOCK as usize);
    (0..blocks).filter(|&i| !block_eq(a, b, i)).count()
}

// ----------------------------------------------------------------- crash

/// What a crash did to the volume directory.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CrashDamage {
    /// The flush point used: the larger of the caller's bound and the
    /// watermark the journal carries on disk. Nothing at or below it changed.
    pub durable_floor: WriteSeq,
    /// The highest `write_seq` found in the journal before the damage.
    pub highest: WriteSeq,
    /// Records above the flush point left as they were.
    pub records_kept: usize,
    /// Records above the flush point zeroed, torn or flipped.
    pub records_damaged: usize,
    /// Unsynced files deleted.
    pub files_removed: usize,
    /// Unsynced files truncated.
    pub files_truncated: usize,
}

fn journal_files(voldir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(voldir.join("journal")) else {
        return Vec::new();
    };
    let mut v: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "j"))
        .collect();
    v.sort();
    v
}

/// Damages, at byte level, every journal record (lane records and marks)
/// whose `write_seq` is above the flush point: the larger of `durable` and
/// the flushed watermark any record or mark on disk carries. Each such record
/// is kept, zeroed, torn from a 512-byte boundary, or has one byte flipped,
/// independently. Nothing at or below the flush point is touched.
///
/// The volume must not be open.
///
/// # Panics
/// On an I/O error on the journal files (a test cannot go on).
pub fn damage_journal(voldir: &Path, durable: WriteSeq, rng: &mut TestRng) -> CrashDamage {
    struct Rec {
        file: usize,
        off: u64,
        len: usize,
        seq: u64,
    }
    let files = journal_files(voldir);
    let mut recs = Vec::new();
    let mut watermark = 0u64;
    let mut highest = 0u64;
    for (fi, path) in files.iter().enumerate() {
        let bytes = std::fs::read(path)
            .unwrap_or_else(|e| panic!("read journal file {}: {e}", path.display()));
        let Ok(scan) = scan_file(&bytes, WriteSeq(0)) else {
            continue;
        };
        for r in &scan.records {
            let h = &r.header;
            watermark = watermark.max(h.flushed.0);
            highest = highest.max(h.write_seq.0);
            let data = if h.kind == RecordKind::Write {
                r.data.len()
            } else {
                0
            };
            recs.push(Rec {
                file: fi,
                off: h.off,
                len: RECORD_HEADER_LEN + data,
                seq: h.write_seq.0,
            });
        }
    }
    let floor = watermark.max(durable.0);
    let mut out = CrashDamage {
        durable_floor: WriteSeq(floor),
        highest: WriteSeq(highest),
        ..CrashDamage::default()
    };
    for r in recs.iter().filter(|r| r.seq > floor) {
        let path = &files[r.file];
        let f = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .unwrap_or_else(|e| panic!("open journal file {}: {e}", path.display()));
        let mut rec = vec![0u8; r.len];
        f.read_exact_at(&mut rec, r.off)
            .unwrap_or_else(|e| panic!("read record: {e}"));
        match rng.below(8) {
            0..=2 => {
                out.records_kept += 1;
                continue;
            }
            3 | 4 => rec.fill(0),
            5 | 6 => {
                // Torn: the sectors from a 512-byte boundary inside the
                // record on never reached the disk.
                let end = r.off + r.len as u64;
                let first = (r.off / 512 + 1) * 512;
                if first >= end {
                    rec.fill(0);
                } else {
                    let n = (end - 1 - first) / 512 + 1;
                    let cut = first + 512 * rng.below(n) - r.off;
                    rec[to_usize(cut)..].fill(0);
                }
            }
            _ => {
                let i = to_usize(rng.below(r.len as u64));
                rec[i] ^= 1 << rng.below(8);
            }
        }
        f.write_all_at(&rec, r.off)
            .unwrap_or_else(|e| panic!("write record: {e}"));
        f.sync_data().unwrap_or_else(|e| panic!("sync: {e}"));
        out.records_damaged += 1;
    }
    out
}

/// Deletes, truncates or keeps (at random) every `*.tmp` and `*.open` file
/// below `voldir` outside `journal/` and `cache/`: the objects whose
/// directory entry or data was never synced. Returns (removed, truncated).
///
/// # Panics
/// On an I/O error.
pub fn damage_unsynced(voldir: &Path, rng: &mut TestRng) -> (usize, usize) {
    fn walk(dir: &Path, top: bool, out: &mut Vec<PathBuf>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        let mut v: Vec<_> = rd.flatten().collect();
        v.sort_by_key(std::fs::DirEntry::file_name);
        for e in v {
            let p = e.path();
            let name = e.file_name();
            let name = name.to_string_lossy();
            let skip = top && (name == "journal" || name == "cache");
            let unsynced = name.ends_with(".tmp") || name.ends_with(".open");
            match e.file_type() {
                Ok(t) if t.is_dir() && !skip => walk(&p, false, out),
                Ok(t) if t.is_file() && unsynced => out.push(p),
                _ => {}
            }
        }
    }
    let mut files = Vec::new();
    walk(voldir, true, &mut files);
    let (mut removed, mut truncated) = (0, 0);
    for p in files {
        match rng.below(3) {
            0 => {
                std::fs::remove_file(&p).unwrap_or_else(|e| panic!("remove {}: {e}", p.display()));
                removed += 1;
            }
            1 => {
                let len = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
                let f = OpenOptions::new()
                    .write(true)
                    .open(&p)
                    .unwrap_or_else(|e| panic!("open {}: {e}", p.display()));
                f.set_len(rng.below(len.max(1)))
                    .unwrap_or_else(|e| panic!("truncate {}: {e}", p.display()));
                truncated += 1;
            }
            _ => {}
        }
    }
    (removed, truncated)
}

// ------------------------------------------------------- scratch, fsck

/// A scratch directory on a real file system, removed on drop: under
/// `LUNIX_SPIKE_DIR` (the test host's scratch) or the system temporary
/// directory.
#[derive(Debug)]
pub struct Scratch {
    path: PathBuf,
}

impl Scratch {
    /// Makes `<base>/<prefix>-<random>`.
    ///
    /// # Panics
    /// When the directory cannot be made, or lies on tmpfs (the journal needs
    /// `O_DIRECT`; a test that cannot run must not pass).
    pub fn new(prefix: &str) -> Self {
        let base =
            std::env::var_os("LUNIX_SPIKE_DIR").map_or_else(std::env::temp_dir, PathBuf::from);
        std::fs::create_dir_all(&base).expect("scratch base");
        let r: [u8; 8] = random_bytes().expect("random");
        let path = base.join(format!("{prefix}-{}", hex(&r)));
        std::fs::create_dir(&path).expect("scratch dir");
        let st = rustix::fs::statfs(&path).expect("statfs");
        #[allow(clippy::unnecessary_cast)]
        let tmpfs = st.f_type as u64 == 0x0102_1994;
        assert!(
            !tmpfs,
            "{} is tmpfs: the journal needs O_DIRECT; set LUNIX_SPIKE_DIR to a real file system",
            path.display()
        );
        Self { path }
    }

    /// The directory.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// What `lunix-fsck` said about a volume.
#[derive(Clone, Debug)]
pub struct FsckOutcome {
    /// 0 clean, 1 warnings only, 2 errors.
    pub exit: i32,
    /// The findings (one per finding line), severity first:
    /// `("warning", "journal_file")`.
    pub findings: Vec<(String, String)>,
    /// The whole text report.
    pub text: String,
}

impl FsckOutcome {
    /// No finding of severity `error`.
    pub fn no_errors(&self) -> bool {
        self.exit < 2 && !self.findings.iter().any(|(s, _)| s == "error")
    }
}

/// The `lunix-fsck` binary, if it is built: `LUNIX_FSCK`, else
/// `lunix-fsck` in the target directory of the running test binary (build it
/// with `cargo build -p lunix-fsck` first).
pub fn fsck_binary() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("LUNIX_FSCK") {
        return Some(PathBuf::from(p));
    }
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    [dir.join("lunix-fsck"), dir.parent()?.join("lunix-fsck")]
        .into_iter()
        .find(|p| p.exists())
}

/// Runs `lunix-fsck` (read-only) on a closed volume. `None` when the binary is
/// not built or the volume needs a passphrase.
///
/// # Panics
/// When the binary cannot be run.
pub fn fsck(vol: &TestVolume) -> Option<FsckOutcome> {
    let bin = fsck_binary()?;
    let args = vol.fsck_args()?;
    let out = std::process::Command::new(&bin)
        .args(args)
        .arg(&vol.voldir)
        .output()
        .unwrap_or_else(|e| panic!("run {}: {e}", bin.display()));
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let findings = text
        .lines()
        .filter_map(|l| {
            let mut w = l.split_whitespace();
            let sev = w.next()?;
            matches!(sev, "warning" | "error" | "note")
                .then(|| (sev.to_owned(), w.next().unwrap_or("").to_owned()))
        })
        .collect();
    Some(FsckOutcome {
        exit: out.status.code().unwrap_or(-1),
        findings,
        text,
    })
}
