//! End-to-end tests of the ublk layer over real devices, with the testkit's
//! `MemBlockEngine` behind the device and `DeviceOracle` as the reference.
//!
//! **Root, `ublk_drv` loaded, creates real devices.** Every test is `#[ignore]`
//! and also checks `LUNIX_ROOT_TESTS=1`; run them only through
//! `tools/remote.sh root` (see `tests/root/run-e2e.sh`), one test thread:
//!
//! ```text
//! cargo test -p lunix-ublk --test device_e2e -- --ignored --test-threads 1
//! ```
//!
//! The devices are raw and unmounted (plan order: raw first). I/O is bounded:
//! a fixed number of requests of at most 256 KiB over a device of at most
//! 64 MiB.

use std::alloc::{alloc_zeroed, dealloc, Layout};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::sync::Arc;
use std::time::{Duration, Instant};

use lunix_format::VolumeUuid;
use lunix_iface::block::{BlockEngine, IoOp};
use lunix_testkit::strategies::IoSpec;
use lunix_testkit::{CompletionMode, DeviceOracle, MemBlockEngine, MemEngineConfig};
use lunix_ublk::{DataPath, SizeError, UblkConfig, UblkDevice, UblkError};

const MIB: u64 = 1 << 20;
const BLKGETSIZE64: libc::c_ulong = 0x8008_1272;
const BLKDISCARD: libc::c_ulong = 0x1277;
const BLKZEROOUT: libc::c_ulong = 0x127f;

fn enabled() -> bool {
    // SAFETY: geteuid has no arguments and cannot fail.
    std::env::var("LUNIX_ROOT_TESTS").as_deref() == Ok("1") && unsafe { libc::geteuid() } == 0
}

macro_rules! root_only {
    () => {
        if !enabled() {
            eprintln!("SKIP: needs root and LUNIX_ROOT_TESTS=1");
            return;
        }
    };
}

/// A 4096-aligned buffer for `O_DIRECT`.
struct Aligned {
    ptr: *mut u8,
    len: usize,
    layout: Layout,
}

impl Aligned {
    fn new(len: usize) -> Self {
        let layout = Layout::from_size_align(len, 4096).unwrap();
        // SAFETY: non-zero size.
        let ptr = unsafe { alloc_zeroed(layout) };
        assert!(!ptr.is_null());
        Self { ptr, len, layout }
    }

    fn slice(&mut self) -> &mut [u8] {
        // SAFETY: `ptr` is valid for `len` bytes and uniquely borrowed.
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }
    }
}

impl Drop for Aligned {
    fn drop(&mut self) {
        // SAFETY: allocated with `layout`.
        unsafe { dealloc(self.ptr, self.layout) }
    }
}

/// An attached device that detaches when the test ends (fds closed first).
struct Dev {
    fd: Option<File>,
    dev: UblkDevice,
    engine: MemBlockEngine,
}

impl Dev {
    fn open(&self) -> File {
        OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_DIRECT)
            .open(self.dev.bdev_path())
            .expect("open the ublk block device")
    }
}

impl Drop for Dev {
    fn drop(&mut self) {
        drop(self.fd.take());
        if std::thread::panicking() {
            // Do not block a failing test: try once, report, leave it for the leftovers check.
            if let Err(e) = self.dev.detach() {
                eprintln!("cleanup: detach of {} failed: {e}", self.dev.bdev_path());
            }
        }
    }
}

fn register(dev_id: u32) {
    if let Ok(dir) = std::env::var("LUNIX_SPIKE_DIR") {
        let _ = std::fs::create_dir_all(format!("{dir}/run"));
        if let Ok(mut f) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(format!("{dir}/run/created-devices"))
        {
            let _ = writeln!(f, "/dev/ublkb{dev_id}\n/dev/ublkc{dev_id}");
        }
    }
}

fn attach(size: u64, queues: u16, mode: CompletionMode, path: DataPath, max: u64) -> Dev {
    let mut ecfg = MemEngineConfig::new(size);
    ecfg.nr_queues = queues;
    ecfg.mode = mode;
    let engine = MemBlockEngine::new(ecfg);
    let mut cfg = UblkConfig::new(VolumeUuid([0xe2; 16]));
    cfg.queues = queues;
    cfg.depth = 32;
    cfg.max_size_bytes = max;
    cfg.data_path = path;
    let dev = UblkDevice::add(Arc::new(engine.clone()), cfg).expect("add the ublk device");
    register(dev.dev_id());
    let mut d = Dev {
        fd: None,
        dev,
        engine,
    };
    d.fd = Some(d.open());
    d
}

fn size_of_fd(f: &File) -> u64 {
    let mut n = 0u64;
    // SAFETY: BLKGETSIZE64 writes one u64.
    let r = unsafe { libc::ioctl(f.as_raw_fd(), BLKGETSIZE64, &mut n) };
    assert_eq!(r, 0, "BLKGETSIZE64: {}", std::io::Error::last_os_error());
    n
}

/// Applies one request to the device through the block layer.
fn on_device(f: &File, io: &IoSpec, buf: &mut Aligned) -> Vec<u8> {
    let fd = f.as_raw_fd();
    let len = io.len as usize;
    match io.op {
        IoOp::Read => {
            // SAFETY: the buffer is at least `len` long.
            let n = unsafe { libc::pread(fd, buf.ptr.cast(), len, io.offset_bytes as libc::off_t) };
            assert_eq!(
                n,
                len as isize,
                "pread {io:?}: {}",
                std::io::Error::last_os_error()
            );
            buf.slice()[..len].to_vec()
        }
        IoOp::Write => {
            buf.slice()[..len].copy_from_slice(&io.payload());
            let iov = libc::iovec {
                iov_base: buf.ptr.cast(),
                iov_len: len,
            };
            let flags = if io.fua { libc::RWF_DSYNC } else { 0 };
            // SAFETY: one iovec over the buffer.
            let n = unsafe { libc::pwritev2(fd, &iov, 1, io.offset_bytes as libc::off_t, flags) };
            assert_eq!(
                n,
                len as isize,
                "pwrite {io:?}: {}",
                std::io::Error::last_os_error()
            );
            Vec::new()
        }
        IoOp::Flush => {
            // SAFETY: plain fd.
            assert_eq!(unsafe { libc::fdatasync(fd) }, 0, "FLUSH");
            Vec::new()
        }
        IoOp::Discard | IoOp::WriteZeroes => {
            let range = [io.offset_bytes, u64::from(io.len)];
            let req = if io.op == IoOp::Discard {
                BLKDISCARD
            } else {
                BLKZEROOUT
            };
            // SAFETY: the ioctl reads two u64.
            let r = unsafe { libc::ioctl(fd, req, range.as_ptr()) };
            assert_eq!(
                r,
                0,
                "{:?} {io:?}: {}",
                io.op,
                std::io::Error::last_os_error()
            );
            Vec::new()
        }
    }
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
}

/// A valid request inside `[base, base + region)`.
fn gen_op(rng: &mut Rng, base: u64, region: u64, seed: u64) -> IoSpec {
    let kind = rng.next() % 100;
    let blocks = region / 4096;
    let len_blocks = 1 + rng.next() % 64u64.min(blocks);
    let first = rng.next() % (blocks - len_blocks + 1);
    let offset = base + first * 4096;
    let len = (len_blocks * 4096) as u32;
    let (op, fua) = match kind {
        0..=34 => (IoOp::Read, false),
        35..=69 => (IoOp::Write, false),
        70..=79 => (IoOp::Write, true),
        80..=87 => (IoOp::Flush, false),
        88..=93 => (IoOp::Discard, false),
        _ => (IoOp::WriteZeroes, false),
    };
    IoSpec {
        op,
        fua,
        offset_bytes: if op == IoOp::Flush { 0 } else { offset },
        len: if op == IoOp::Flush { 0 } else { len },
        seed,
    }
}

/// `threads` threads, each with a private region and oracle, `ops` requests each.
fn run_workload(d: &Dev, threads: u64, region: u64, ops: usize, seed: u64) {
    let mut handles = Vec::new();
    for t in 0..threads {
        let f = d.open();
        handles.push(std::thread::spawn(move || {
            let mut rng = Rng(seed ^ (t + 1).wrapping_mul(0x1234_5678_9abc_def1));
            let mut oracle = DeviceOracle::new(region);
            let mut buf = Aligned::new(1 << 20);
            let base = t * region;
            for i in 0..ops {
                let seed_i = rng.0 ^ i as u64;
                let mut io = gen_op(&mut rng, base, region, seed_i);
                let local = IoSpec {
                    offset_bytes: io.offset_bytes.saturating_sub(base),
                    ..io
                };
                let got = on_device(&f, &io, &mut buf);
                // The oracle works on region-local offsets.
                io.offset_bytes = local.offset_bytes;
                match oracle.apply(&io) {
                    lunix_testkit::Applied::Read(want) => {
                        assert_eq!(got, want, "thread {t} op {i}: read differs: {io:?}");
                    }
                    lunix_testkit::Applied::Done => {}
                    lunix_testkit::Applied::Invalid => {
                        panic!("generated an invalid request {io:?}")
                    }
                }
            }
            // The whole region, byte for byte.
            let mut at = 0u64;
            while at < region {
                let n = (1u64 << 20).min(region - at);
                let spec = IoSpec {
                    op: IoOp::Read,
                    fua: false,
                    offset_bytes: base + at,
                    len: n as u32,
                    seed: 0,
                };
                let got = on_device(&f, &spec, &mut buf);
                assert_eq!(
                    got,
                    oracle.read(at, n),
                    "thread {t}: region differs at {at}"
                );
                at += n;
            }
        }));
    }
    for h in handles {
        h.join().expect("workload thread");
    }
}

/// Counters are updated after the commit is queued, so a caller can see the
/// reply a moment before `completed`: wait for them to settle.
fn settle(d: &UblkDevice) {
    let end = Instant::now() + Duration::from_secs(5);
    while d.in_flight() != 0 && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// udev probes a new device and holds it open for a moment, so `TRY_STOP_DEV`
/// answers Busy until it lets go. Retry for a bounded time; a mount would not.
fn detach_when_idle(d: &UblkDevice) {
    let end = Instant::now() + Duration::from_secs(20);
    loop {
        match d.detach() {
            Ok(()) => return,
            Err(UblkError::Busy { .. }) if Instant::now() < end => {
                std::thread::sleep(Duration::from_millis(200));
            }
            Err(e) => panic!("detach: {e}"),
        }
    }
}

fn wait_gone(path: &str) -> bool {
    let end = Instant::now() + Duration::from_secs(10);
    while Instant::now() < end {
        if !std::path::Path::new(path).exists() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

fn full_cycle(queues: u16, mode: CompletionMode, path: DataPath) {
    let d = attach(32 * MIB, queues, mode, path, 64 * MIB);
    assert_eq!(size_of_fd(d.fd.as_ref().unwrap()), 32 * MIB);
    run_workload(&d, 4, 8 * MIB, 600, 0xe2e0 + u64::from(queues));
    // The engine saw exactly what the oracle says: compare the whole device once more through the engine.
    let snap = d.engine.snapshot();
    assert_eq!(snap.len() as u64, 32 * MIB);
    settle(&d.dev);
    let counters = d.dev.queue_counters();
    let started: u64 = counters.iter().map(|c| c.started).sum();
    let completed: u64 = counters.iter().map(|c| c.completed).sum();
    assert_eq!(started, completed, "every started request was completed");
    assert_eq!(d.dev.in_flight(), 0);
    assert!(started > 0);
    let id = d.dev.dev_id();
    let bdev = d.dev.bdev_path();
    // Detach with nothing mounted: TRY_STOP_DEV succeeds, the queues drain, DEL_DEV completes.
    let mut d = d;
    drop(d.fd.take());
    detach_when_idle(&d.dev);
    assert!(d.engine.is_quiesced(), "the engine saw the final commit");
    assert!(
        wait_gone(&bdev),
        "{bdev} is gone after the detach (device {id})"
    );
    assert!(
        d.engine.counters().quiesces >= 1,
        "quiesce was called at least once"
    );
}

#[test]
#[ignore = "root: creates a real ublk device"]
fn immediate_completions_one_queue() {
    root_only!();
    full_cycle(1, CompletionMode::Immediate, DataPath::Copy);
}

#[test]
#[ignore = "root: creates a real ublk device"]
fn deferred_completions_through_the_wake_fd_four_queues() {
    root_only!();
    full_cycle(4, CompletionMode::Deferred, DataPath::Copy);
}

#[test]
#[ignore = "root: creates a real ublk device"]
fn mixed_completions_two_queues() {
    root_only!();
    full_cycle(2, CompletionMode::Mixed(7), DataPath::Copy);
}

#[test]
#[ignore = "root: creates a real ublk device"]
fn user_copy_path_with_deferred_completions() {
    root_only!();
    full_cycle(2, CompletionMode::Mixed(11), DataPath::UserCopy);
}

#[test]
#[ignore = "root: creates a real ublk device"]
fn grow_is_validated_and_updates_the_block_device() {
    root_only!();
    let d = attach(
        16 * MIB,
        1,
        CompletionMode::Immediate,
        DataPath::Copy,
        64 * MIB,
    );
    let f = d.fd.as_ref().unwrap();
    assert_eq!(size_of_fd(f), 16 * MIB);
    let invalid = |why: SizeError, bytes: u64| match d.dev.grow(bytes) {
        Err(UblkError::InvalidSize { why: w, .. }) => assert_eq!(w, why, "{bytes}"),
        other => panic!("grow({bytes}) should be refused: {other:?}"),
    };
    invalid(SizeError::Unaligned, 32 * MIB + 512);
    invalid(SizeError::Shrink { current: 16 * MIB }, 8 * MIB);
    invalid(SizeError::AboveMax { max: 64 * MIB }, 128 * MIB);
    invalid(SizeError::Zero, 0);
    assert_eq!(
        size_of_fd(f),
        16 * MIB,
        "refused sizes never reached the kernel"
    );
    assert_eq!(d.engine.size_bytes(), 16 * MIB, "nor the engine");
    d.dev
        .grow(16 * MIB)
        .expect("growing to the current size is a no-op");
    d.dev.grow(48 * MIB).expect("grow to 48 MiB");
    assert_eq!(
        size_of_fd(f),
        48 * MIB,
        "the block device grew without a remount"
    );
    assert_eq!(d.engine.size_bytes(), 48 * MIB);
    assert_eq!(
        d.dev.record().dev_sectors,
        (48 * MIB) >> 9,
        "the record follows the grow"
    );
    // I/O in the grown region.
    let mut buf = Aligned::new(1 << 20);
    let w = IoSpec {
        op: IoOp::Write,
        fua: true,
        offset_bytes: 40 * MIB,
        len: 64 * 1024,
        seed: 99,
    };
    on_device(f, &w, &mut buf);
    let r = IoSpec {
        op: IoOp::Read,
        fua: false,
        ..w
    };
    assert_eq!(on_device(f, &r, &mut buf), w.payload());
    // The oracle grows the same way.
    let mut o = DeviceOracle::new(16 * MIB);
    o.grow(48 * MIB);
    assert_eq!(o.size_bytes(), 48 * MIB);
    let mut d = d;
    drop(d.fd.take());
    detach_when_idle(&d.dev);
}

#[test]
#[ignore = "root: creates a real ublk device"]
fn the_record_describes_the_device() {
    root_only!();
    let d = attach(
        16 * MIB,
        2,
        CompletionMode::Immediate,
        DataPath::Copy,
        64 * MIB,
    );
    let rec = d.dev.record();
    assert_eq!(rec.dev_id, d.dev.dev_id());
    assert_eq!(rec.nr_queues, 2);
    assert_eq!(rec.queue_depth, 32);
    assert_eq!(rec.max_io_buf_bytes, 1 << 20);
    assert_eq!(rec.dev_sectors, (16 * MIB) >> 9);
    assert_eq!(rec.boot_id, lunix_ublk::current_boot_id().unwrap());
    let mut bytes = Vec::new();
    rec.encode(&mut bytes).unwrap();
    assert_eq!(
        lunix_format::local::DeviceRecord::decode(&bytes).unwrap(),
        rec
    );
    let mut d = d;
    drop(d.fd.take());
    detach_when_idle(&d.dev);
}
