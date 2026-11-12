//! Tests of the store: the shared contract suite, the gate sweeps and the
//! durable-order checks. Helpers live here.

mod accounting;
mod codec;
mod contract;
mod crash;
mod damage;
mod misc;
mod roundtrip;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use lunix_format::local::VolumeJson;
use lunix_format::volume::{CodecPolicy, VolumeDescriptor};
use lunix_format::{ChunkId, Codec, CryptoMode, FamilyId, IdLen, Profile, TimeMs, VolumeUuid};
use lunix_iface::contract::store::StoreFixture;
use lunix_iface::store::{
    CheckpointStore, CommitStore, GcStateStore, PackStore, SegmentStore, VolumeDir,
};

use crate::{FsOps, LocalVolume, RealFs, StoreOptions};
use codec::{build_extent, TestCodec};

/// The uuid of every test volume (the contract suite's checkpoint slots name it).
pub(crate) const UUID: [u8; 16] = [7; 16];

/// A scratch root on the same disk as the build, not in `/tmp`.
pub(crate) fn test_root() -> PathBuf {
    let exe = std::env::current_exe().expect("exe path");
    let root = exe
        .parent()
        .and_then(Path::parent)
        .expect("target profile dir")
        .join("store-tests");
    std::fs::create_dir_all(&root).expect("scratch root");
    root
}

pub(crate) fn tempdir() -> tempfile::TempDir {
    tempfile::tempdir_in(test_root()).expect("tempdir")
}

pub(crate) fn test_opts() -> StoreOptions {
    StoreOptions {
        allow_tmpfs: true,
        ..StoreOptions::default()
    }
}

pub(crate) fn descriptor(mode: CryptoMode, chunk_size: u32) -> Vec<u8> {
    let d = VolumeDescriptor {
        crypto_mode: mode,
        profile: Profile::Default,
        block_size: 4096,
        chunk_size,
        map_fanout: 512,
        id_len: IdLen::new(16).expect("id len"),
        codec_policy: CodecPolicy::Auto,
        codec_level: 3,
        volume_uuid: VolumeUuid(UUID),
        family_id: FamilyId([9; 16]),
        kmk_fingerprint: if mode == CryptoMode::Plain {
            [0; 16]
        } else {
            [5; 16]
        },
        created_at: TimeMs(1_700_000_000_000),
        lineage: Vec::new(),
        mac: [0; 32],
    };
    let mut v = Vec::new();
    d.encode(&mut v).expect("descriptor");
    v
}

pub(crate) fn json(dir: &Path) -> VolumeJson {
    VolumeJson::new("test", dir.to_str().expect("utf-8 dir"))
}

pub(crate) fn create(dir: &Path, mode: CryptoMode, chunk: u32, opts: StoreOptions) -> LocalVolume {
    LocalVolume::create(
        dir,
        &descriptor(mode, chunk),
        &json(dir),
        TestCodec::new(mode, UUID),
        opts,
    )
    .expect("create volume")
}

pub(crate) fn try_open(
    dir: &Path,
    mode: CryptoMode,
    opts: StoreOptions,
) -> lunix_iface::Result<LocalVolume> {
    LocalVolume::open(dir, TestCodec::new(mode, UUID), opts)
}

/// Opens, retrying while `Busy` for a few seconds: a test in this process may
/// be between `fork` and `exec` of a child, and that child briefly holds a copy
/// of every open descriptor, flock included.
pub(crate) fn open(
    dir: &Path,
    mode: CryptoMode,
    opts: StoreOptions,
) -> lunix_iface::Result<LocalVolume> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        match try_open(dir, mode, opts.clone()) {
            Err(e)
                if e.kind() == lunix_iface::ErrorKind::Busy
                    && std::time::Instant::now() < deadline =>
            {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            other => return other,
        }
    }
}

/// Payload bytes that depend on `(id, len)`.
pub(crate) fn payload(id: &ChunkId, len: usize) -> Vec<u8> {
    (0..len)
        .map(|i| id.0[i % 32].wrapping_add(i as u8).wrapping_mul(31))
        .collect()
}

/// A data extent (codec 1, compressed length `clen`) for a volume of `mode`.
pub(crate) fn data_extent(mode: CryptoMode, chunk: u32, id: &ChunkId, clen: usize) -> Vec<u8> {
    build_extent(mode, Codec::Zstd, 3, chunk, id, &payload(id, clen))
}

pub(crate) fn node_extent(mode: CryptoMode, hash: &[u8; 32], clen: usize) -> Vec<u8> {
    let id = ChunkId(*hash);
    build_extent(mode, Codec::Raw, 0, clen as u32, &id, &payload(&id, clen))
}

pub(crate) fn cid(n: u32) -> ChunkId {
    let mut b = [0u8; 32];
    b[..4].copy_from_slice(&n.to_be_bytes());
    b[4..].fill(0xa5);
    ChunkId(b)
}

/// The fixture of the contract suite: one volume in a scratch directory.
pub(crate) struct Fixture {
    dir: tempfile::TempDir,
    mode: CryptoMode,
    chunk: u32,
    vol: Option<LocalVolume>,
}

impl Fixture {
    pub(crate) fn new(mode: CryptoMode, chunk: u32) -> Self {
        let dir = tempdir();
        let vol = create(dir.path(), mode, chunk, test_opts());
        Self {
            dir,
            mode,
            chunk,
            vol: Some(vol),
        }
    }

    fn v(&self) -> &LocalVolume {
        self.vol.as_ref().expect("volume is open")
    }
}

impl StoreFixture for Fixture {
    fn segments(&self) -> &dyn SegmentStore {
        self.v()
    }
    fn packs(&self) -> &dyn PackStore {
        self.v()
    }
    fn commits(&self) -> &dyn CommitStore {
        self.v()
    }
    fn checkpoint(&self) -> &dyn CheckpointStore {
        self.v()
    }
    fn gc_state(&self) -> &dyn GcStateStore {
        self.v()
    }
    fn volume(&self) -> &dyn VolumeDir {
        self.v()
    }

    fn data_extent(&self, id: &ChunkId, payload_len: usize) -> Vec<u8> {
        data_extent(self.mode, self.chunk, id, payload_len)
    }

    fn node_extent(&self, hash: &[u8; 32], payload_len: usize) -> Vec<u8> {
        node_extent(self.mode, hash, payload_len)
    }

    fn reopen(&mut self) {
        self.vol = None;
        self.vol = Some(open(self.dir.path(), self.mode, test_opts()).expect("reopen"));
    }
}

/// One host call as the instrumented file system saw it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum Ev {
    Fdatasync,
    Rename { from: String, to: String },
    FsyncDir(String),
    Unlink(String),
}

/// Records the order of the durability calls, then does them for real.
#[derive(Debug, Default)]
pub(crate) struct RecordingFs {
    pub(crate) events: Mutex<Vec<Ev>>,
}

fn leaf(p: &Path) -> String {
    p.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_owned()
}

impl RecordingFs {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub(crate) fn take(&self) -> Vec<Ev> {
        std::mem::take(&mut *self.events.lock().expect("events"))
    }

    fn push(&self, e: Ev) {
        self.events.lock().expect("events").push(e);
    }
}

impl FsOps for RecordingFs {
    fn fdatasync(&self, file: &std::fs::File) -> std::io::Result<()> {
        self.push(Ev::Fdatasync);
        RealFs.fdatasync(file)
    }
    fn rename(&self, from: &Path, to: &Path, noreplace: bool) -> std::io::Result<()> {
        self.push(Ev::Rename {
            from: leaf(from),
            to: leaf(to),
        });
        RealFs.rename(from, to, noreplace)
    }
    fn fsync_dir(&self, dir: &Path) -> std::io::Result<()> {
        self.push(Ev::FsyncDir(leaf(dir)));
        RealFs.fsync_dir(dir)
    }
    fn unlink(&self, path: &Path) -> std::io::Result<()> {
        self.push(Ev::Unlink(leaf(path)));
        RealFs.unlink(path)
    }
}
