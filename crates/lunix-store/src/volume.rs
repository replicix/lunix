//! [`LocalVolume`]: the volume directory, its process lock, the object-id
//! allocator and the open-time orphan sweep.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use lunix_format::ids::hex_lower;
use lunix_format::local::{DeviceRecord, LocalPins, LocalPinsFile, RollbackFloor, VolumeJson};
use lunix_format::paths::local as lpath;
use lunix_format::segment::ContainerKind;
use lunix_format::volume::VolumeDescriptor;
use lunix_format::{ObjId, VolumeUuid};
use lunix_iface::crypto::MetaCodec;
use lunix_iface::store::{
    CheckpointStore, FloorReset, IdAllocator, Leftover, SweepReport, SweepRequest, VolumeDir,
};
use lunix_iface::{Error, Result};
use replicix_platform_lite::{open_lock_file, FileLock, FlockLock, LockGuard};

use crate::container::Container;
use crate::fsops::{
    io_err, io_or_missing, list_hex_ids, publish, read_capped, read_dir_names, unlink_durable,
    FsOps, Publish, RealFs, STATE_POINTS,
};

/// The durable record of every floor reset, relative to the volume directory.
pub const FLOOR_AUDIT: &str = "state/floor-resets.log";

/// Target size of a segment: the writer reports full from here on (DESIGN §6).
pub const SEGMENT_TARGET: u64 = 16 << 20;

/// `statfs` magic numbers.
const TMPFS_MAGIC: u64 = 0x0102_1994;
const RAMFS_MAGIC: u64 = 0x8584_58f6;
const BTRFS_MAGIC: u64 = 0x9123_683e;

/// The subdirectories of a volume directory, created by `create` (the keyring
/// and `integrity.key` live elsewhere). `journal/` is owned by M3, `index/` by
/// M6 and `cache/` by the engine; they are created here so every milestone
/// finds the layout in place.
const SUBDIRS: &[&str] = &[
    "journal",
    "segments",
    "map",
    "index",
    "cache",
    "state",
    "state/commits",
    "state/gc",
    "snapshots",
    "readers",
    "holds",
    "gc",
];

/// Options of [`LocalVolume::create`] and [`LocalVolume::open`].
#[derive(Clone, Debug)]
pub struct StoreOptions {
    /// The durability calls; tests wrap [`RealFs`] to record their order.
    pub fs: Arc<dyn FsOps>,
    /// Allow a volume on tmpfs/ramfs. Off unless `LUNIX_ALLOW_TMPFS_FOR_TESTS`
    /// is set: a volume whose "durable" files vanish at reboot is a trap.
    pub allow_tmpfs: bool,
    /// Segment target size in bytes.
    pub segment_target: u64,
    /// How many sealed containers the cache keeps open. A bound on the cache,
    /// not on descriptors: an `ExtentLocation` keeps its descriptor alive past
    /// eviction and delete.
    pub max_open_files: usize,
    /// Allow [`VolumeDir::reset_rollback_floor`], which lowers the rollback
    /// floor. Off. Only `lunix-fsck` sets it (offline repair); `tools/ci.sh`
    /// fails when any other crate does, so a daemon cannot lower its own floor.
    pub allow_floor_reset: bool,
}

impl Default for StoreOptions {
    fn default() -> Self {
        Self {
            fs: Arc::new(RealFs),
            allow_tmpfs: std::env::var_os("LUNIX_ALLOW_TMPFS_FOR_TESTS").is_some(),
            segment_target: SEGMENT_TARGET,
            max_open_files: 256,
            allow_floor_reset: false,
        }
    }
}

/// The shared object-id counter (segments, map packs and index runs).
#[derive(Debug, Default)]
pub struct IdCounter(AtomicU64);

impl IdAllocator for IdCounter {
    fn allocate(&self) -> Result<ObjId> {
        let mut cur = self.0.load(Ordering::SeqCst);
        loop {
            let next = if cur == 0 {
                ObjId::new(0, 1)
            } else {
                ObjId::from_raw(cur)
                    .map_err(|_| Error::Refused {
                        why: "an observed object id is not a valid id",
                    })?
                    .next()
            }
            .map_err(|_| Error::Refused {
                why: "object id counter exhausted",
            })?;
            match self
                .0
                .compare_exchange(cur, next.raw(), Ordering::SeqCst, Ordering::SeqCst)
            {
                Ok(_) => return Ok(next),
                Err(seen) => cur = seen,
            }
        }
    }

    fn high_water(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }

    fn observe(&self, raw: u64) {
        self.0.fetch_max(raw, Ordering::SeqCst);
    }
}

/// Open sealed containers, oldest evicted first. The `Arc` in every entry keeps
/// a descriptor valid for a reader that still holds it after eviction or a
/// delete.
#[derive(Default)]
pub(crate) struct ContainerCache {
    map: HashMap<ObjId, Arc<Container>>,
    order: VecDeque<ObjId>,
}

impl ContainerCache {
    pub(crate) fn get(&self, id: ObjId) -> Option<Arc<Container>> {
        self.map.get(&id).cloned()
    }

    pub(crate) fn insert(&mut self, c: Arc<Container>, cap: usize) {
        let id = c.id;
        if self.map.insert(id, c).is_none() {
            self.order.push_back(id);
        }
        while self.map.len() > cap.max(1) {
            match self.order.pop_front() {
                Some(old) => {
                    self.map.remove(&old);
                }
                None => break,
            }
        }
    }

    pub(crate) fn remove(&mut self, id: ObjId) {
        if self.map.remove(&id).is_some() {
            self.order.retain(|o| *o != id);
        }
    }
}

/// What every handle shares.
pub(crate) struct Inner {
    pub(crate) dir: PathBuf,
    pub(crate) uuid: VolumeUuid,
    pub(crate) meta: Arc<dyn MetaCodec>,
    pub(crate) ovh: u32,
    pub(crate) fs: Arc<dyn FsOps>,
    pub(crate) ids: Arc<IdCounter>,
    pub(crate) segment_target: u64,
    pub(crate) max_open_files: usize,
    pub(crate) allow_floor_reset: bool,
    pub(crate) cache: Mutex<ContainerCache>,
    warnings: Mutex<Warnings>,
    /// Serialises replace-style writes (one shared temp name per file).
    pub(crate) replace_lock: Mutex<()>,
    /// Serialises read-modify-write of `state/pins` (the floor and the pins
    /// share one file).
    pins_lock: Mutex<()>,
    /// Raw ids of containers being written; their `.open` files are not leftovers.
    pub(crate) open_writers: Mutex<HashSet<u64>>,
    // Held for the life of the volume; dropping the last handle releases it.
    _lock: LockGuard,
}

impl Inner {
    pub(crate) fn path(&self, rel: &str) -> PathBuf {
        self.dir.join(rel)
    }

    pub(crate) fn warn(&self, msg: impl Into<String>) {
        let mut w = lock(&self.warnings);
        if w.items.len() >= MAX_WARNINGS {
            w.items.pop_front();
            w.dropped += 1;
        }
        w.items.push_back(msg.into());
    }

    /// True for `<id>.open` files that a live writer of this process owns.
    pub(crate) fn is_live_writer_file(&self, rel: &str) -> bool {
        let Some((sub, name)) = rel.split_once('/') else {
            return false;
        };
        if sub != "segments" && sub != "map" {
            return false;
        }
        name.strip_suffix(".open")
            .and_then(crate::fsops::parse_hex16)
            .is_some_and(|raw| lock(&self.open_writers).contains(&raw))
    }

    pub(crate) fn cache(&self) -> MutexGuard<'_, ContainerCache> {
        lock(&self.cache)
    }
}

/// Warnings kept for the daemon; older ones are counted, not kept.
const MAX_WARNINGS: usize = 64;

#[derive(Default)]
struct Warnings {
    items: VecDeque<String>,
    dropped: u64,
}

/// A poisoned mutex only means a panic elsewhere; the data is plain state.
pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// One volume directory, locked by this process. Cheap to clone; the lock is
/// released when the last clone and the last open writer are dropped.
///
/// It implements every store trait of `lunix_iface::store`:
/// [`VolumeDir`], [`SegmentStore`](lunix_iface::store::SegmentStore),
/// [`PackStore`](lunix_iface::store::PackStore),
/// [`CommitStore`](lunix_iface::store::CommitStore),
/// [`CheckpointStore`](lunix_iface::store::CheckpointStore),
/// [`PinSource`](lunix_iface::store::PinSource) and
/// [`GcStateStore`](lunix_iface::store::GcStateStore).
#[derive(Clone)]
pub struct LocalVolume {
    pub(crate) inner: Arc<Inner>,
}

impl fmt::Debug for LocalVolume {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LocalVolume")
            .field("dir", &self.inner.dir)
            .field("uuid", &self.inner.uuid)
            .finish_non_exhaustive()
    }
}

/// Reads the `volume` descriptor bytes of a volume directory **without**
/// locking it, so the caller can derive keys (and build the [`MetaCodec`])
/// before [`LocalVolume::open`].
///
/// # Errors
/// `NotFound` when the directory has no `volume`; `Io`.
pub fn read_volume_file(dir: &Path) -> Result<Vec<u8>> {
    read_capped(&dir.join(lpath::volume()), MAX_VOLUME_FILE, "volume")
}

/// A descriptor is 120 bytes plus at most 255 lineage entries of about 1 KiB.
const MAX_VOLUME_FILE: u64 = 1 << 20;

/// The pid the holder of `dir`'s `LOCK` wrote into it, for an operator who got
/// `Busy`. `None` when unknown (the file is empty or unreadable). The pid may
/// be stale if the holder died; the flock, not this text, is the lock.
pub fn lock_holder_pid(dir: &Path) -> Option<u32> {
    let text = read_capped(&dir.join(lpath::lock()), 64, "LOCK").ok()?;
    std::str::from_utf8(&text).ok()?.trim().parse().ok()
}

fn statfs_magic(dir: &Path) -> io::Result<u64> {
    let st = rustix::fs::statfs(dir)?;
    #[allow(clippy::unnecessary_cast, clippy::cast_sign_loss)]
    Ok(st.f_type as u64)
}

fn check_fs(dir: &Path, allow_tmpfs: bool) -> Result<u64> {
    let magic = statfs_magic(dir).map_err(io_err)?;
    if !allow_tmpfs && (magic == TMPFS_MAGIC || magic == RAMFS_MAGIC) {
        return Err(Error::Refused {
            why: "volume directory is on tmpfs",
        });
    }
    Ok(magic)
}

/// `chattr +C` on a directory: files created in it afterwards are not
/// copy-on-write (the journal on btrfs). Best effort.
fn set_nocow(dir: &Path) -> io::Result<()> {
    let f = fs::File::open(dir)?;
    let flags = rustix::fs::ioctl_getflags(&f)?;
    rustix::fs::ioctl_setflags(&f, flags | rustix::fs::IFlags::NOCOW)?;
    Ok(())
}

fn take_lock(dir: &Path) -> Result<LockGuard> {
    let file = open_lock_file(&dir.join(lpath::lock()))
        .map_err(|e| io_or_missing(e, "volume directory"))?;
    // flock is released by the kernel when the holder dies, so a lock left by
    // a killed process is simply free: that is the takeover.
    let guard = FlockLock::new()
        .try_lock(file)
        .map_err(io_err)?
        .ok_or(Error::Busy {
            what: "volume lock",
        })?;
    // The pid is for a human reading the file; the lock is the flock.
    let mut f = guard.file();
    if f.set_len(0).is_ok() {
        let _ = writeln!(f, "{}", std::process::id());
    }
    Ok(guard)
}

impl LocalVolume {
    /// Creates a new volume directory: the layout, `volume.json`, then the
    /// `volume` descriptor last, so a crash leaves either no volume or a whole
    /// one. `volume_bytes` are the exact descriptor bytes (stored verbatim; the
    /// descriptor MAC is the caller's to have checked).
    ///
    /// # Errors
    /// `AlreadyExists` when `dir` already holds a volume, `Busy` when another
    /// process holds its lock, `Refused` on tmpfs (unless allowed), `Invalid`
    /// when the descriptor's crypto mode is not `meta`'s, `Format` for a bad
    /// descriptor or `volume.json`, `Io`.
    pub fn create(
        dir: &Path,
        volume_bytes: &[u8],
        json: &VolumeJson,
        meta: Arc<dyn MetaCodec>,
        opts: StoreOptions,
    ) -> Result<Self> {
        let desc = VolumeDescriptor::decode(volume_bytes)?;
        if desc.crypto_mode != meta.mode() {
            return Err(Error::invalid("descriptor mode differs from the codec's"));
        }
        let json_text = json.to_json_pretty()?;
        // Refuse tmpfs before creating anything: judge the nearest directory
        // that exists, then re-read the type of the new one for btrfs.
        let existing = dir
            .ancestors()
            .find(|p| p.exists())
            .unwrap_or_else(|| Path::new("."));
        check_fs(existing, opts.allow_tmpfs)?;
        fs::create_dir_all(dir).map_err(io_err)?;
        let magic = check_fs(dir, true)?;
        let guard = take_lock(dir)?;
        if dir.join(lpath::volume()).exists() {
            return Err(Error::AlreadyExists { what: "volume" });
        }
        for sub in SUBDIRS {
            fs::create_dir_all(dir.join(sub)).map_err(io_err)?;
        }
        let this = Self::assemble(dir, desc.volume_uuid, meta, &opts, guard);
        if magic == BTRFS_MAGIC {
            if let Err(e) = set_nocow(&dir.join("journal")) {
                this.inner
                    .warn(format!("could not set NOCOW on journal/: {e}"));
            }
        }
        for sub in SUBDIRS {
            this.inner.fs.fsync_dir(&dir.join(sub)).map_err(io_err)?;
        }
        publish(
            &this.inner,
            dir,
            &lpath::volume_json(),
            json_text.as_bytes(),
            Publish::Replace,
            &STATE_POINTS,
        )?;
        this.inner.fs.fsync_dir(dir).map_err(io_err)?;
        // The volume directory's own entry must be durable before `volume`.
        if let Some(parent) = dir.parent() {
            let parent = if parent.as_os_str().is_empty() {
                Path::new(".")
            } else {
                parent
            };
            this.inner.fs.fsync_dir(parent).map_err(io_err)?;
        }
        publish(
            &this.inner,
            dir,
            &lpath::volume(),
            volume_bytes,
            Publish::CreateOnly("volume"),
            &STATE_POINTS,
        )?;
        Ok(this)
    }

    /// Opens an existing volume directory and takes its lock. The object-id
    /// allocator starts above every segment, pack and index run file on disk
    /// (including unsealed ones), before anything is swept.
    ///
    /// # Errors
    /// `NotFound` without a `volume`, `Busy` when another process holds the
    /// lock, `Refused` on tmpfs (unless allowed), `Invalid` when the
    /// descriptor's crypto mode is not `meta`'s, `Format`, `Io`.
    pub fn open(dir: &Path, meta: Arc<dyn MetaCodec>, opts: StoreOptions) -> Result<Self> {
        check_fs(dir, opts.allow_tmpfs).map_err(|e| match e {
            Error::Io(ref io) if io.kind() == io::ErrorKind::NotFound => {
                Error::not_found("volume directory")
            }
            other => other,
        })?;
        let guard = take_lock(dir)?;
        let bytes = read_volume_file(dir)?;
        let desc = VolumeDescriptor::decode(&bytes)?;
        if desc.crypto_mode != meta.mode() {
            return Err(Error::invalid("descriptor mode differs from the codec's"));
        }
        let this = Self::assemble(dir, desc.volume_uuid, meta, &opts, guard);
        // Inputs of the id rule that live here: the files on disk and the
        // checkpoint hint. The caller adds the highest id its commits name.
        let highest = this.highest_id_on_disk()?;
        this.inner.ids.observe(highest);
        if let Ok(Some(slot)) = this.load() {
            if ObjId::from_raw(slot.id_high).is_ok() {
                this.inner.ids.observe(slot.id_high);
            }
        }
        Ok(this)
    }

    fn assemble(
        dir: &Path,
        uuid: VolumeUuid,
        meta: Arc<dyn MetaCodec>,
        opts: &StoreOptions,
        guard: LockGuard,
    ) -> Self {
        let ovh = meta.mode().extent_ovh();
        Self {
            inner: Arc::new(Inner {
                dir: dir.to_path_buf(),
                uuid,
                meta,
                ovh,
                fs: opts.fs.clone(),
                ids: Arc::new(IdCounter::default()),
                segment_target: opts.segment_target,
                max_open_files: opts.max_open_files,
                allow_floor_reset: opts.allow_floor_reset,
                cache: Mutex::new(ContainerCache::default()),
                warnings: Mutex::new(Warnings::default()),
                replace_lock: Mutex::new(()),
                pins_lock: Mutex::new(()),
                open_writers: Mutex::new(HashSet::new()),
                _lock: guard,
            }),
        }
    }

    /// Warnings the store raised since the last call (an ignored checkpoint, a
    /// NOCOW failure). The daemon logs them; the store has no logger of its own.
    pub fn take_warnings(&self) -> Vec<String> {
        let mut w = lock(&self.inner.warnings);
        let mut out = Vec::with_capacity(w.items.len() + 1);
        if w.dropped > 0 {
            out.push(format!("{} earlier warnings were dropped", w.dropped));
            w.dropped = 0;
        }
        out.extend(w.items.drain(..));
        out
    }

    /// The highest object id of any segment, pack or index-run file present,
    /// sealed or not (`.seg`, `.open`, `.tmp` all count: an id is never reused).
    /// Only the file names the layout defines are read; anything else is
    /// reported as a warning and ignored, so a stray name cannot poison the
    /// allocator.
    fn highest_id_on_disk(&self) -> Result<u64> {
        let mut highest = 0u64;
        for (sub, suffixes, extra) in [
            ("segments", &[".seg", ".open", ".tmp"][..], &[][..]),
            ("map", &["", ".open", ".tmp"][..], &[][..]),
            (
                "index",
                &["", ".tmp"][..],
                &["manifest", "manifest.tmp", "manifest.prev"][..],
            ),
        ] {
            for (name, _) in read_dir_names(&self.inner.path(sub)).map_err(io_err)? {
                if extra.contains(&name.as_str()) {
                    continue;
                }
                let id = suffixes
                    .iter()
                    .filter_map(|sfx| name.strip_suffix(sfx))
                    .find_map(crate::fsops::parse_hex16)
                    .filter(|raw| ObjId::from_raw(*raw).is_ok());
                match id {
                    Some(raw) => highest = highest.max(raw),
                    None => self.inner.warn(format!(
                        "unexpected file {sub}/{name} ignored by the id scan"
                    )),
                }
            }
        }
        Ok(highest)
    }

    fn read_state_file(&self, rel: &str, max: u64) -> Result<Option<Vec<u8>>> {
        match read_capped(&self.inner.path(rel), max, "state file") {
            Ok(b) => Ok(Some(b)),
            Err(e) if e.kind() == lunix_iface::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// `state/pins` as a whole (version 2); `None` when absent.
    fn read_pins_file(&self) -> Result<Option<LocalPinsFile>> {
        match self.read_state_file(&lpath::state_pins(), 4096)? {
            Some(b) => Ok(Some(LocalPinsFile::decode(&b)?)),
            None => Ok(None),
        }
    }

    /// Replaces `state/pins` with a version 2 file. The caller holds `pins_lock`.
    fn write_pins_file(&self, file: &LocalPinsFile) -> Result<()> {
        let mut b = Vec::with_capacity(LocalPinsFile::LEN);
        file.encode(&mut b)?;
        self.replace_state_file(&lpath::state_pins(), &b)
    }

    /// The state of an absent `state/pins` (zero floors, no pin), with the
    /// identity the file needs.
    fn fresh_pins_file(&self) -> Result<LocalPinsFile> {
        let desc = VolumeDescriptor::decode(&read_volume_file(&self.inner.dir)?)?;
        Ok(LocalPinsFile {
            pins: LocalPins {
                volume_uuid: self.inner.uuid,
                family_id: desc.family_id,
                max_keyring_seq: 0,
                max_epoch: 0,
                pin: None,
            },
            floor: None,
        })
    }

    /// Appends one line to `state/floor-resets.log` and makes it durable.
    fn audit_floor_reset(
        &self,
        why: FloorReset,
        old: Option<RollbackFloor>,
        new: &RollbackFloor,
    ) -> Result<()> {
        use std::io::Write as _;
        let show = |f: Option<RollbackFloor>| {
            f.map_or_else(
                || "none".to_owned(),
                |f| format!("{}:{}", f.max_commit_seq.0, hex_lower(&f.tip_hash)),
            )
        };
        let ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis());
        let line = format!(
            "{ms} {} old={} new={}:{}\n",
            why.word(),
            show(old),
            new.max_commit_seq.0,
            hex_lower(&new.tip_hash)
        );
        let path = self.inner.path(FLOOR_AUDIT);
        let existed = path.exists();
        let mut f = fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(&path)
            .map_err(io_err)?;
        f.write_all(line.as_bytes()).map_err(io_err)?;
        self.inner.fs.fdatasync(&f).map_err(io_err)?;
        if !existed {
            self.inner
                .fs
                .fsync_dir(&self.inner.path("state"))
                .map_err(io_err)?;
        }
        Ok(())
    }

    fn replace_state_file(&self, rel: &str, bytes: &[u8]) -> Result<()> {
        let path = self.inner.path(rel);
        let (dir, name) = split_rel(&path)?;
        publish(
            &self.inner,
            dir,
            name,
            bytes,
            Publish::Replace,
            &STATE_POINTS,
        )
    }

    /// Every `*.tmp` and `*.open` file below the volume directory (`cache/`
    /// excluded), relative to it.
    fn collect_leftovers(&self) -> Result<Vec<Leftover>> {
        let mut out = Vec::new();
        let mut stack = vec![(self.inner.dir.clone(), String::new())];
        while let Some((dir, prefix)) = stack.pop() {
            for entry in fs::read_dir(&dir).map_err(io_err)? {
                let entry = entry.map_err(io_err)?;
                let Ok(name) = entry.file_name().into_string() else {
                    continue;
                };
                let rel = format!("{prefix}{name}");
                let Ok(meta) = entry.metadata() else { continue };
                if meta.is_dir() {
                    if rel != "cache" {
                        stack.push((entry.path(), format!("{rel}/")));
                    }
                } else if (name.ends_with(".tmp") || name.ends_with(".open"))
                    && !self.inner.is_live_writer_file(&rel)
                {
                    out.push(Leftover {
                        name: rel,
                        size_bytes: meta.len(),
                    });
                }
            }
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }
}

fn split_rel(path: &Path) -> Result<(&Path, &str)> {
    let dir = path
        .parent()
        .ok_or_else(|| Error::invalid("path has no parent"))?;
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| Error::invalid("path has no file name"))?;
    Ok((dir, name))
}

impl VolumeDir for LocalVolume {
    fn uuid(&self) -> VolumeUuid {
        self.inner.uuid
    }

    fn dir(&self) -> &Path {
        &self.inner.dir
    }

    fn volume_bytes(&self) -> Result<Vec<u8>> {
        read_volume_file(&self.inner.dir)
    }

    fn volume_json(&self) -> Result<VolumeJson> {
        let bytes = self
            .read_state_file(
                &lpath::volume_json(),
                lunix_format::local::MAX_JSON_LEN as u64,
            )?
            .ok_or_else(|| Error::not_found("volume.json"))?;
        let text = std::str::from_utf8(&bytes).map_err(|_| Error::Corrupt {
            what: "volume.json",
            detail: "not UTF-8",
        })?;
        Ok(VolumeJson::from_json(text)?)
    }

    fn store_volume_json(&self, json: &VolumeJson) -> Result<()> {
        let text = json.to_json_pretty()?;
        self.replace_state_file(&lpath::volume_json(), text.as_bytes())
    }

    fn device_record(&self) -> Result<Option<DeviceRecord>> {
        match self.read_state_file(&lpath::state_device(), 4096)? {
            Some(b) => Ok(Some(DeviceRecord::decode(&b)?)),
            None => Ok(None),
        }
    }

    fn store_device_record(&self, rec: &DeviceRecord) -> Result<()> {
        let mut b = Vec::with_capacity(DeviceRecord::LEN);
        rec.encode(&mut b)?;
        self.replace_state_file(&lpath::state_device(), &b)
    }

    fn clear_device_record(&self) -> Result<()> {
        unlink_durable(&self.inner, &self.inner.path(&lpath::state_device())).map(|_| ())
    }

    fn local_pins(&self) -> Result<Option<LocalPins>> {
        Ok(self.read_pins_file()?.map(|f| f.pins))
    }

    fn store_local_pins(&self, pins: &LocalPins) -> Result<()> {
        let _serial = lock(&self.inner.pins_lock);
        // An unreadable file is an error, not an empty one: replacing it would
        // silently drop the rollback floor it may hold.
        let floor = self.read_pins_file()?.and_then(|f| f.floor);
        self.write_pins_file(&LocalPinsFile {
            pins: pins.clone(),
            floor,
        })
    }

    fn rollback_floor(&self) -> Result<Option<RollbackFloor>> {
        Ok(self.read_pins_file()?.and_then(|f| f.floor))
    }

    fn raise_rollback_floor(&self, floor: &RollbackFloor) -> Result<()> {
        let _serial = lock(&self.inner.pins_lock);
        let mut file = match self.read_pins_file()? {
            Some(f) => f,
            None => self.fresh_pins_file()?,
        };
        if let Some(cur) = file.floor {
            if floor.max_commit_seq < cur.max_commit_seq {
                return Err(Error::Refused {
                    why: "the rollback floor never decreases",
                });
            }
            if floor.max_commit_seq == cur.max_commit_seq {
                if floor.tip_hash != cur.tip_hash {
                    return Err(Error::Refused {
                        why: "the rollback floor is already recorded at this height with another hash",
                    });
                }
                return Ok(());
            }
        }
        file.floor = Some(*floor);
        self.write_pins_file(&file)
    }

    fn reset_rollback_floor(
        &self,
        floor: &RollbackFloor,
        why: FloorReset,
    ) -> Result<Option<RollbackFloor>> {
        if !self.inner.allow_floor_reset {
            return Err(Error::Refused {
                why: "the rollback floor is reset only by an offline repair",
            });
        }
        // The commit at the new height must exist and be the one named.
        let bytes = match lunix_iface::store::CommitStore::get(self, floor.max_commit_seq) {
            Ok(b) => b,
            Err(e) if e.kind() == lunix_iface::ErrorKind::NotFound => {
                return Err(Error::Refused {
                    why: "no commit exists at the new floor's height",
                })
            }
            Err(e) => return Err(e),
        };
        if *blake3::hash(&bytes).as_bytes() != floor.tip_hash {
            return Err(Error::Refused {
                why: "the commit at the new floor's height does not hash to the floor's tip_hash",
            });
        }
        let _serial = lock(&self.inner.pins_lock);
        let current = self.read_pins_file();
        let (mut file, old) = match (why, current) {
            (FloorReset::RepairPins, Ok(_)) => {
                return Err(Error::Refused {
                    why: "state/pins is not rejected; nothing to repair",
                })
            }
            (FloorReset::RepairPins, Err(e)) if e.kind() == lunix_iface::ErrorKind::Corrupt => {
                (self.fresh_pins_file()?, None)
            }
            (_, Err(e)) => return Err(e),
            (_, Ok(Some(f))) => {
                let old = f.floor;
                (f, old)
            }
            (_, Ok(None)) => (self.fresh_pins_file()?, None),
        };
        if why == FloorReset::TruncateTip
            && !old.is_some_and(|o| floor.max_commit_seq < o.max_commit_seq)
        {
            return Err(Error::Refused {
                why: "a truncate-tip reset must lower the recorded floor",
            });
        }
        self.audit_floor_reset(why, old, floor)?;
        file.floor = Some(*floor);
        self.write_pins_file(&file)?;
        self.inner.warn(format!(
            "rollback floor reset ({}): now commit {}",
            why.word(),
            floor.max_commit_seq.0
        ));
        Ok(old)
    }

    fn ids(&self) -> Arc<dyn IdAllocator> {
        self.inner.ids.clone()
    }

    fn leftovers(&self) -> Result<Vec<Leftover>> {
        self.collect_leftovers()
    }

    fn remove_leftovers(&self) -> Result<u32> {
        let found = self.collect_leftovers()?;
        let mut removed = 0u32;
        let mut dirs = Vec::new();
        for l in &found {
            let path = self.inner.path(&l.name);
            lunix_iface::crashpoint!("store.leftover.before_unlink");
            match self.inner.fs.unlink(&path) {
                Ok(()) => removed += 1,
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(io_err(e)),
            }
            lunix_iface::crashpoint!("store.leftover.after_unlink");
            if let Some(p) = path.parent() {
                if !dirs.iter().any(|d: &PathBuf| d == p) {
                    dirs.push(p.to_path_buf());
                }
            }
        }
        for d in &dirs {
            self.inner.fs.fsync_dir(d).map_err(io_err)?;
        }
        lunix_iface::crashpoint!("store.leftover.after_dirsync");
        Ok(removed)
    }

    fn sweep_orphans(&self, req: &SweepRequest<'_>) -> Result<SweepReport> {
        let bad = |id: &ObjId| id.raw() > req.high_water_mark;
        if req.named_segments.iter().any(bad) || req.named_packs.iter().any(bad) {
            return Err(Error::invalid(
                "a named id is above high_water_mark: the mark was computed wrongly",
            ));
        }
        let mut report = SweepReport {
            highest_id_before: self.highest_id_on_disk()?,
            ..SweepReport::default()
        };
        self.inner.ids.observe(report.highest_id_before);
        report.deleted_temp = self.remove_leftovers()?;
        for (raw, _) in list_hex_ids(&self.inner.path("segments"), ".seg").map_err(io_err)? {
            let Ok(id) = ObjId::from_raw(raw) else {
                continue;
            };
            if req.named_segments.contains(&id) {
                continue;
            }
            if raw > req.high_water_mark {
                self.inner.delete_container(ContainerKind::Segment, id)?;
                report.deleted_segments.push(id);
            } else {
                report.kept_unreferenced.push(id);
            }
        }
        for (raw, _) in list_hex_ids(&self.inner.path("map"), "").map_err(io_err)? {
            let Ok(id) = ObjId::from_raw(raw) else {
                continue;
            };
            if req.named_packs.contains(&id) {
                continue;
            }
            if raw > req.high_water_mark {
                self.inner.delete_container(ContainerKind::MapPack, id)?;
                report.deleted_packs.push(id);
            } else {
                report.kept_unreferenced.push(id);
            }
        }
        Ok(report)
    }
}

impl Inner {
    /// The sealed file of a container.
    pub(crate) fn container_path(&self, kind: ContainerKind, id: ObjId) -> PathBuf {
        self.path(&match kind {
            ContainerKind::Segment => lpath::segment_sealed(id),
            ContainerKind::MapPack => lpath::map_pack_sealed(id),
        })
    }

    /// The `.open` file of a container being written.
    pub(crate) fn open_path(&self, kind: ContainerKind, id: ObjId) -> PathBuf {
        self.path(&match kind {
            ContainerKind::Segment => lpath::segment_open(id),
            ContainerKind::MapPack => lpath::map_pack_open(id),
        })
    }
}
