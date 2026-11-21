//! ADR-33 T8 through the real stack (no root, no block device): segment seals
//! run beside the packer, and a cut still never writes its map pack or FULL
//! before every segment it names is sealed and durable.
//!
//! The store's durability calls are replaced by a recording [`FsOps`] that
//! makes a segment's `fdatasync` slow (a network volume), so the interleavings
//! the fast local disk hides are the ones that run.

mod common;

use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::volume::{create, params};
use common::{scratch, Driver};
use lunix_format::CryptoMode;
use lunix_iface::ErrorKind;
use lunix_store::{FsOps, RealFs};
use lunixd::stack::{open_volume, VolumeStack};

const CS: u32 = 64 << 10;
const MIB: usize = 1 << 20;

fn random(seed: u64, len: usize) -> Vec<u8> {
    let mut x = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 24) as u8
        })
        .collect()
}

fn env_or(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

#[derive(Default)]
struct Seen {
    /// Segment `fdatasync` calls started, and those running now.
    seg_syncs: usize,
    in_flight: usize,
    /// Segments renamed to their sealed name.
    renamed: usize,
    /// Map packs and commit objects published, and what was wrong then.
    map_objects: usize,
    violations: Vec<String>,
}

/// The host file system with a slow segment `fdatasync`, which notes every
/// publication of a map pack or commit object and what the segments were
/// doing at that moment.
#[derive(Debug)]
struct SegFs {
    delay: Duration,
    /// Fail the n-th (from 1) segment `fdatasync` (0: none).
    fail_nth: AtomicUsize,
    /// Fail the n-th (from 1) directory `fsync` of `segments/`, after the
    /// rename (0: none).
    fail_dirsync_nth: AtomicUsize,
    dirsyncs: AtomicUsize,
    voldir: PathBuf,
    seen: Mutex<Seen>,
}

impl std::fmt::Debug for Seen {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Seen")
    }
}

impl SegFs {
    fn new(voldir: &Path, delay: Duration) -> Arc<Self> {
        Arc::new(Self {
            delay,
            fail_nth: AtomicUsize::new(0),
            fail_dirsync_nth: AtomicUsize::new(0),
            dirsyncs: AtomicUsize::new(0),
            voldir: voldir.to_path_buf(),
            seen: Mutex::default(),
        })
    }

    fn seen(&self) -> std::sync::MutexGuard<'_, Seen> {
        self.seen.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn open_segments(&self) -> Vec<String> {
        std::fs::read_dir(self.voldir.join("segments"))
            .map(|d| {
                d.flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .filter(|n| n.ends_with(".open"))
                    .collect()
            })
            .unwrap_or_default()
    }
}

fn is_segment_file(file: &File) -> bool {
    std::fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd()))
        .is_ok_and(|p| p.to_string_lossy().contains("/segments/"))
}

impl FsOps for SegFs {
    fn fdatasync(&self, file: &File) -> io::Result<()> {
        if !is_segment_file(file) {
            return RealFs.fdatasync(file);
        }
        let nth = {
            let mut s = self.seen();
            s.seg_syncs += 1;
            s.in_flight += 1;
            s.seg_syncs
        };
        std::thread::sleep(self.delay);
        let r = if self.fail_nth.load(Ordering::SeqCst) == nth {
            Err(io::Error::other("injected segment fdatasync failure"))
        } else {
            RealFs.fdatasync(file)
        };
        self.seen().in_flight -= 1;
        r
    }

    fn rename(&self, from: &Path, to: &Path, noreplace: bool) -> io::Result<()> {
        let t = to.to_string_lossy();
        if t.contains("/map/") || t.contains("/state/commits/") {
            let open = self.open_segments();
            let mut s = self.seen();
            s.map_objects += 1;
            if s.in_flight > 0 || !open.is_empty() {
                let v = format!(
                    "{} published with {} segment fdatasync(s) running and open segments {open:?}",
                    t, s.in_flight
                );
                s.violations.push(v);
            }
        }
        let r = RealFs.rename(from, to, noreplace);
        if t.contains("/segments/") && r.is_ok() {
            self.seen().renamed += 1;
        }
        r
    }

    fn fsync_dir(&self, dir: &Path) -> io::Result<()> {
        if dir.ends_with("segments") {
            let n = self.dirsyncs.fetch_add(1, Ordering::SeqCst) + 1;
            if self.fail_dirsync_nth.load(Ordering::SeqCst) == n {
                return Err(io::Error::other("injected segments/ fsync failure"));
            }
        }
        RealFs.fsync_dir(dir)
    }

    fn unlink(&self, path: &Path) -> io::Result<()> {
        RealFs.unlink(path)
    }
}

struct Rig {
    stack: VolumeStack,
    driver: Driver,
    fs: Arc<SegFs>,
    data: Vec<Vec<u8>>,
    voldir: PathBuf,
}

/// A volume with `mib` MiB of random data written and flushed, ready for a
/// cut: three or more full segments for 40 MiB and above.
fn rig(t: &Path, delay_ms: u64, seals: usize, mib: usize) -> Rig {
    let v = create(t, CryptoMode::Integrity, CS);
    let fs = SegFs::new(&v.voldir, Duration::from_millis(delay_ms));
    let mut p = params(&v, CryptoMode::Integrity);
    p.store_fs = Some(fs.clone());
    p.destage_seals = Some(seals);
    p.journal_tweak = Some(Arc::new(|j| j.mark_idle = None));
    let stack = open_volume(p).expect("open");
    let mut driver = Driver::new(&stack.engine, 2, MIB as u32);
    let data: Vec<Vec<u8>> = (0..mib).map(|i| random(900 + i as u64, MIB)).collect();
    for (i, b) in data.iter().enumerate() {
        assert!(driver.write_on(i % 2, (i * MIB) as u64, b).is_ok());
    }
    assert!(driver.flush().is_ok());
    Rig {
        stack,
        driver,
        fs,
        data,
        voldir: v.voldir,
    }
}

fn segment_files(voldir: &Path) -> Vec<String> {
    std::fs::read_dir(voldir.join("segments"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect()
}

/// Checks the files on disk: no map pack or commit object is published while
/// a segment `fdatasync` runs or an `.open` segment exists. It does not tell
/// the wait in `run` from the one in `finish` (either one keeps the disk
/// order; the destage-level test of the map's commit tells them apart).
#[test]
fn the_map_pack_and_the_commit_wait_for_every_segment_of_the_cut() {
    let Some(t) = scratch("seal-order") else {
        return;
    };
    let mut r = rig(t.path(), 250, 4, 48);
    r.stack.engine.commit_now(false).expect("commit");
    let s = r.fs.seen();
    assert!(s.renamed >= 3, "the cut sealed {} segments", s.renamed);
    assert!(s.map_objects >= 2, "a map pack and a commit were published");
    assert!(s.violations.is_empty(), "{:#?}", s.violations);
    drop(s);
    assert_eq!(r.driver.read(0, MIB as u32), r.data[0]);
}

#[test]
fn a_failed_segment_seal_fails_the_cut_and_a_retry_loses_no_write() {
    let Some(t) = scratch("seal-fail") else {
        return;
    };
    let mut r = rig(t.path(), 60, 4, 48);
    let tip = r.stack.engine.tip();
    // The second seal fails after the first and third are under way.
    r.fs.fail_nth.store(2, Ordering::SeqCst);
    let err = r.stack.engine.commit_now(false).expect_err("a seal failed");
    assert_eq!(err.kind(), ErrorKind::Io, "{err}");
    assert_eq!(r.stack.engine.tip(), tip, "nothing was committed");
    assert!(!r.stack.engine.is_stopped(), "an Io error is retried");
    assert!(
        segment_files(&r.voldir).is_empty(),
        "the failed cut's segments are gone: {:?}",
        segment_files(&r.voldir)
    );
    assert_eq!(r.fs.seen().map_objects, 0, "no map pack, no commit");
    // The journal still holds the writes: the retry commits them all.
    r.fs.fail_nth.store(0, Ordering::SeqCst);
    r.stack.engine.commit_now(false).expect("retry");
    assert_ne!(r.stack.engine.tip(), tip);
    for (i, b) in r.data.iter().enumerate() {
        assert_eq!(&r.driver.read((i * MIB) as u64, MIB as u32), b, "MiB {i}");
    }
}

#[test]
fn a_failure_of_the_directory_fsync_after_the_rename_leaves_no_segment() {
    let Some(t) = scratch("seal-dirsync") else {
        return;
    };
    let mut r = rig(t.path(), 0, 4, 48);
    let tip = r.stack.engine.tip();
    r.fs.fail_dirsync_nth.store(2, Ordering::SeqCst);
    let err = r.stack.engine.commit_now(false).expect_err("a seal failed");
    assert_eq!(err.kind(), ErrorKind::Io, "{err}");
    assert_eq!(r.stack.engine.tip(), tip);
    // The segment whose seal failed had its sealed name already: the failed
    // cut deleted it with the others.
    assert!(
        segment_files(&r.voldir).is_empty(),
        "{:?}",
        segment_files(&r.voldir)
    );
    assert_eq!(r.fs.seen().map_objects, 0);
    r.stack.engine.commit_now(false).expect("retry");
    for (i, b) in r.data.iter().enumerate() {
        assert_eq!(&r.driver.read((i * MIB) as u64, MIB as u32), b, "MiB {i}");
    }
}

/// Cut time with a slow segment `fdatasync`: inline seals against seals beside
/// the packer. A benchmark, not a gate: it asserts no duration (run it with
/// `--ignored`; `LUNIX_SEAL_DELAY_MS` and `LUNIX_SEAL_MIB` size the run).
#[test]
#[ignore = "benchmark; prints the cut times"]
fn seals_beside_the_packer_shorten_a_cut_with_a_slow_fdatasync() {
    let delay = env_or("LUNIX_SEAL_DELAY_MS", 200);
    let mib = env_or("LUNIX_SEAL_MIB", 56) as usize;
    let mut took = Vec::new();
    for seals in [0usize, 1, 4] {
        let Some(t) = scratch(&format!("seal-bench-{seals}")) else {
            return;
        };
        let r = rig(t.path(), delay, seals, mib);
        let started = Instant::now();
        r.stack.engine.commit_now(false).expect("commit");
        let ms = started.elapsed().as_millis();
        let c = r.stack.engine.cut_stats();
        eprintln!(
            "seal bench: seals_in_flight_max={seals} {mib} MiB, fdatasync {delay} ms: cut {ms} ms, \
             seals {}, in flight max {}, busy {} ms, wait at finish {} ms, packer stalled {} ms",
            c.seals, c.seal_in_flight_max, c.seal_busy_ms, c.seal_wait_ms, c.seal_stall_ms
        );
        assert!(r.fs.seen().violations.is_empty());
        took.push((seals, ms, c));
    }
    assert!(
        took[0].2.seals >= 3,
        "the run sealed {} segments",
        took[0].2.seals
    );
    assert_eq!(took[0].2.seal_in_flight_max, 1, "inline: one at a time");
    assert!(took[2].2.seal_in_flight_max >= 2, "{:?}", took[2].2);
}
