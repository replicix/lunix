//! [`LocalVolume`]: the volume directory, its process lock, the object-id
//! allocator and the open-time orphan sweep.

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use lunix_format::local::{DeviceRecord, LocalPins, VolumeJson};
use lunix_format::paths::local as lpath;
use lunix_format::segment::ContainerKind;
use lunix_format::volume::VolumeDescriptor;
use lunix_format::{ObjId, VolumeUuid};
use lunix_iface::crypto::MetaCodec;
use lunix_iface::store::{IdAllocator, Leftover, SweepReport, SweepRequest, VolumeDir};
use lunix_iface::{Error, Result};
use replicix_platform_lite::{open_lock_file, FileLock, FlockLock, LockGuard};

use crate::container::Container;
use crate::fsops::{
    io_err, io_or_missing, list_hex_ids, publish, read_dir_names, unlink_durable, FsOps, Publish,
    RealFs, STATE_POINTS,
};

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
    /// Sealed containers kept open for `locate` and `read_extent`.
    pub max_open_files: usize,
}

impl Default for StoreOptions {
    fn default() -> Self {
        Self {
            fs: Arc::new(RealFs),
            allow_tmpfs: std::env::var_os("LUNIX_ALLOW_TMPFS_FOR_TESTS").is_some(),
            segment_target: SEGMENT_TARGET,
            max_open_files: 256,
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
            let exhausted = Error::Refused {
                why: "object id counter exhausted",
            };
            let next = if cur == 0 {
                ObjId::new(0, 1)
            } else {
                ObjId::from_raw(cur).and_then(ObjId::next)
            }
            .map_err(|_| exhausted)?;
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
    pub(crate) cache: Mutex<ContainerCache>,
    warnings: Mutex<Vec<String>>,
    // Held for the life of the volume; dropping the last handle releases it.
    _lock: LockGuard,
}

impl Inner {
    pub(crate) fn path(&self, rel: &str) -> PathBuf {
        self.dir.join(rel)
    }

    pub(crate) fn warn(&self, msg: impl Into<String>) {
        lock(&self.warnings).push(msg.into());
    }

    pub(crate) fn cache(&self) -> MutexGuard<'_, ContainerCache> {
        lock(&self.cache)
    }
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
    fs::read(dir.join(lpath::volume())).map_err(|e| io_or_missing(e, "volume"))
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

fn fsync_dir_of(fs_ops: &dyn FsOps, path: &Path) {
    if let Some(parent) = path.parent() {
        let _ = fs_ops.fsync_dir(parent);
    }
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
        fs::create_dir_all(dir).map_err(io_err)?;
        let magic = check_fs(dir, opts.allow_tmpfs)?;
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
        fsync_dir_of(&*this.inner.fs, dir);
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
        let highest = this.highest_id_on_disk()?;
        this.inner.ids.observe(highest);
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
                cache: Mutex::new(ContainerCache::default()),
                warnings: Mutex::new(Vec::new()),
                _lock: guard,
            }),
        }
    }

    /// Warnings the store raised since the last call (an ignored checkpoint, a
    /// NOCOW failure). The daemon logs them; the store has no logger of its own.
    pub fn take_warnings(&self) -> Vec<String> {
        std::mem::take(&mut *lock(&self.inner.warnings))
    }

    /// The highest object id of any segment, pack or index-run file present,
    /// sealed or not (`.seg`, `.open`, `.tmp` all count: an id is never reused).
    fn highest_id_on_disk(&self) -> Result<u64> {
        let mut highest = 0u64;
        for sub in ["segments", "map", "index"] {
            for (name, _) in read_dir_names(&self.inner.path(sub)).map_err(io_err)? {
                if let Some(v) = name.get(..16).and_then(crate::fsops::parse_hex16) {
                    highest = highest.max(v);
                }
            }
        }
        Ok(highest)
    }

    fn read_state_file(&self, rel: &str) -> Result<Option<Vec<u8>>> {
        match fs::read(self.inner.path(rel)) {
            Ok(b) => Ok(Some(b)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(io_err(e)),
        }
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
                } else if name.ends_with(".tmp") || name.ends_with(".open") {
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
            .read_state_file(&lpath::volume_json())?
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
        match self.read_state_file(&lpath::state_device())? {
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
        match self.read_state_file(&lpath::state_pins())? {
            Some(b) => Ok(Some(LocalPins::decode(&b)?)),
            None => Ok(None),
        }
    }

    fn store_local_pins(&self, pins: &LocalPins) -> Result<()> {
        let mut b = Vec::with_capacity(LocalPins::LEN);
        pins.encode(&mut b)?;
        self.replace_state_file(&lpath::state_pins(), &b)
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
