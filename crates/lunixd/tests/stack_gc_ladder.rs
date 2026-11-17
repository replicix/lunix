//! The space ladder's compact step on the real stack (plan M12 step 12,
//! DESIGN §4.3 and §11, `docs/reference/gc.md` "urgent pass"): GC's urgent
//! pass, registered by `open_volume` for `LadderStepKind::Compact`, runs on
//! the gate's thread through the engine's coordinator when a reservation is
//! refused, and frees the objects a pass condemned once their floor grace
//! ran, without waiting for the configured grace.
//!
//! A churned volume runs a pass up to its grace (dead rounds condemned), then
//! stops stepping GC for longer than the floor grace; a tight `cache_max`
//! with `on_full = error` refuses one reservation, which starts the ladder.
//! The urgent pass then deletes: segment files, the `Segments` charge and
//! the status counters go down or up as they should, the volume reads back
//! equal and `lunix-fsck` finds nothing at rest.
//!
//! A second test fills a volume: the host file system (a test `FsOps` that
//! answers `ENOSPC` to segment syncs above a byte bound) refuses segments,
//! so cuts fail for space and the small journal fills; writers park. The
//! coordinator runs the urgent pass when a cut fails for space (the ladder's
//! compact step from the engine side, plan M17.B review S-2), which frees
//! the dead rounds, and the workload completes without any normal pass.
//!
//! ```text
//! cargo build --release -p lunix-fsck
//! cargo test --release -p lunixd --features testsupport --test stack_gc_ladder
//! ```

#![cfg(feature = "testsupport")]

use std::fs::File;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use lunix_engine::EngineConfig;
use lunix_format::local::OnFull;
use lunix_format::CryptoMode;
use lunix_gc::GcConfig;
use lunix_iface::gc::{GcControl, GcState};
use lunix_iface::space::{ReserveOutcome, SpaceGate, SpaceKind};
use lunix_store::{FsOps, RealFs};
use lunixd::stack::{OpenParams, VolumeStack};
use lunixd::testsupport::{close, fsck, fsck_binary, payload, Driver, Scratch, VolumeBuilder};

/// The smallest grace the collector takes: 10 s with `read_timeout` 0. It is
/// also the urgent pass's floor grace.
const GRACE: Duration = Duration::from_secs(10);

fn gc_tune(c: &mut EngineConfig) {
    if let Some(p) = &mut c.gc_params {
        p.read_timeout = Duration::ZERO;
        p.grace = GRACE;
    }
}

fn fast_gc(o: &mut OpenParams) {
    o.gc = Some(GcConfig {
        pass_interval: Duration::from_secs(1),
        ..GcConfig::default()
    });
}

fn segment_bytes(stack: &VolumeStack) -> (usize, u64) {
    let mut n = 0;
    let mut b = 0;
    for e in std::fs::read_dir(stack.voldir.join("segments"))
        .expect("segments")
        .flatten()
    {
        n += 1;
        b += e.metadata().map(|m| m.len()).unwrap_or(0);
    }
    (n, b)
}

/// Refuses one reservation (a `cache_max` at what is charged, `on_full =
/// error`), which starts the ladder on the gate's thread, and waits for the
/// urgent pass to finish. Restores the settings.
fn run_ladder(stack: &VolumeStack) {
    let saved = stack.json.space.clone();
    let mut tight = saved.clone();
    tight.on_full = OnFull::Error;
    tight.cache_max = Some(stack.space.snapshot().used().max(1));
    stack.space.reconfigure(&tight).expect("tight settings");
    let passes = stack.engine.status().stats.passes;
    let errors = stack.space.step_errors();
    let out = stack
        .space
        .reserve(SpaceKind::JournalReserved, 1 << 20, None)
        .expect("reserve");
    assert!(matches!(out, ReserveOutcome::NoSpace), "{out:?}");
    let start = Instant::now();
    while stack.engine.status().stats.passes == passes {
        assert_eq!(stack.space.step_errors(), errors, "the compact step failed");
        assert!(
            start.elapsed() < Duration::from_secs(120),
            "the urgent pass did not finish"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    stack.space.reconfigure(&saved).expect("settings back");
}

#[test]
fn the_ladder_compact_step_runs_the_urgent_pass_and_frees_condemned_segments() {
    let t = Scratch::new("m12c-ladder");
    let cs = 64u32 << 10;
    let region = 8u64 << 20;
    let size = 64u64 << 20;
    let mut b = VolumeBuilder::new(t.path())
        .manual()
        .mode(CryptoMode::Integrity)
        .chunk_size(cs)
        .size_bytes(size);
    let manual = b.engine.clone();
    b.engine = Some(Arc::new(move |c: &mut EngineConfig| {
        if let Some(m) = &manual {
            m(c);
        }
        gc_tune(c);
    }));
    let vol = b.create().expect("create");
    let stack = vol.open_with(fast_gc).expect("open");
    let mut d = Driver::new(&stack).expect("driver");

    // Six whole rewrites, a commit after each: the first rounds die whole.
    let mut last = Vec::new();
    for r in 0..6u64 {
        last.clear();
        for c in 0..region / u64::from(cs) {
            let data = payload((r << 32) | c, cs as usize);
            assert!(d.write(0, c * u64::from(cs), &data, false).is_ok());
            last.extend_from_slice(&data);
        }
        assert!(d.flush(0).is_ok());
        stack.engine.commit_now(false).expect("commit");
    }

    // A pass up to its grace: the dead rounds are condemned.
    let start = Instant::now();
    loop {
        stack.engine.tick().expect("tick");
        let s = stack.engine.status();
        if s.state == Some(GcState::Grace) && s.condemned_pending > 0 {
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(60),
            "no pass reached its grace: {s:?}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    // No GC step for longer than the floor grace (the engine is manual).
    std::thread::sleep(GRACE + Duration::from_millis(500));
    let condemned = stack.engine.status().condemned_pending;
    let (files, seg_bytes) = segment_bytes(&stack);
    let charged = stack.space.snapshot().by_kind[SpaceKind::Segments.index()];
    let runs = stack.space.ladder_runs();

    run_ladder(&stack);

    let s = stack.engine.status();
    let urgent = s.last_pass.clone().expect("the urgent pass summary");
    let (files2, seg_bytes2) = segment_bytes(&stack);
    let charged2 = stack.space.snapshot().by_kind[SpaceKind::Segments.index()];
    eprintln!(
        "ladder: {condemned} condemned; urgent pass {urgent:?}; segments {files} files {seg_bytes} B -> {files2} files {seg_bytes2} B; charged {charged} -> {charged2}"
    );
    assert!(stack.space.ladder_runs() > runs);
    assert!(
        urgent.segments_deleted > 0 && urgent.bytes_freed > 0,
        "{urgent:?}"
    );
    assert!(files2 < files && seg_bytes2 < seg_bytes);
    assert!(charged2 < charged, "Segments charge {charged} -> {charged2}");
    assert!(s.stats.bytes_freed >= urgent.bytes_freed);

    // Moves (if it compacted) are named by the next cuts; the data is intact.
    for _ in 0..3 {
        stack.engine.tick().expect("tick");
        stack.engine.commit_now(true).expect("commit");
    }
    let all = d.read_all(size).expect("read");
    assert!(all[..region as usize] == last[..], "the region differs");
    drop(d);
    close(stack).expect("close");
    if std::env::var_os("LUNIX_STACK_REQUIRE_FSCK").is_some() {
        assert!(fsck_binary().is_some(), "lunix-fsck is not built");
    }
    if let Some(f) = fsck(&vol) {
        assert!(f.no_errors(), "lunix-fsck:\n{}", f.text);
    }
    let stack = vol.open().expect("reopen");
    let mut d = Driver::new(&stack).expect("driver");
    let all = d.read_all(size).expect("read");
    assert!(all[..region as usize] == last[..], "the region differs after reopen");
    drop(d);
    close(stack).expect("close");
}

/// The host file system with a bound on sealed segment bytes: a segment
/// sync that would leave more than `cap` bytes of `.seg` files answers
/// `ENOSPC` (the store maps it to `NoSpace`, the destage to `AfterSpace`).
#[derive(Debug)]
struct BoundedFs {
    segments: PathBuf,
    cap: u64,
    refused: AtomicU64,
}

impl BoundedFs {
    fn sealed_bytes(&self) -> u64 {
        std::fs::read_dir(&self.segments)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".seg"))
            .map(|e| e.metadata().map(|m| m.len()).unwrap_or(0))
            .sum()
    }
}

impl FsOps for BoundedFs {
    fn fdatasync(&self, file: &File) -> std::io::Result<()> {
        let path = std::fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd()))
            .unwrap_or_default();
        if path.parent() == Some(self.segments.as_path()) {
            let len = file.metadata()?.len();
            if self.sealed_bytes() + len > self.cap {
                self.refused.fetch_add(1, Ordering::Relaxed);
                return Err(std::io::Error::from_raw_os_error(libc_enospc()));
            }
        }
        RealFs.fdatasync(file)
    }

    fn rename(&self, from: &Path, to: &Path, noreplace: bool) -> std::io::Result<()> {
        RealFs.rename(from, to, noreplace)
    }

    fn fsync_dir(&self, dir: &Path) -> std::io::Result<()> {
        RealFs.fsync_dir(dir)
    }

    fn unlink(&self, path: &Path) -> std::io::Result<()> {
        RealFs.unlink(path)
    }
}

/// `ENOSPC` on Linux.
const fn libc_enospc() -> i32 {
    28
}

#[test]
fn a_full_volume_recovers_through_the_urgent_pass_when_cuts_fail_for_space() {
    let t = Scratch::new("m12c-full");
    let cs = 64u32 << 10;
    let region = 4u64 << 20;
    let size = 64u64 << 20;
    let rounds = 16u64;
    // Background coordinator on (the daemon's engine), small journal: a cut
    // that fails leaves the writers parked on a full ring.
    let mut b = VolumeBuilder::new(t.path())
        .mode(CryptoMode::Integrity)
        .chunk_size(cs)
        .size_bytes(size)
        .small_journal(4);
    b.track_syncs = false;
    b.engine = Some(Arc::new(gc_tune));
    let vol = b.create().expect("create");
    let fs = Arc::new(BoundedFs {
        segments: vol.voldir.join("segments"),
        cap: 6 * region,
        refused: AtomicU64::new(0),
    });
    let store_fs: Arc<dyn FsOps> = fs.clone();
    let stack = vol
        .open_with(|o| {
            o.store_fs = Some(store_fs);
            // No normal pass after the first (empty) one: only the urgent
            // pass can free space.
            o.gc = Some(GcConfig {
                pass_interval: Duration::from_secs(3600),
                ..GcConfig::default()
            });
        })
        .expect("open");
    let start = Instant::now();
    while stack.engine.status().stats.passes == 0 {
        assert!(start.elapsed() < Duration::from_secs(30), "no first pass");
        std::thread::sleep(Duration::from_millis(20));
    }
    let mut d = Driver::new(&stack).expect("driver");
    let mut last = Vec::new();
    let mut failed_commits = 0u32;
    for r in 0..rounds {
        last.clear();
        for c in 0..region / u64::from(cs) {
            let data = payload((r << 32) | c, cs as usize);
            let st = d.write(0, c * u64::from(cs), &data, false);
            assert!(st.is_ok(), "round {r} chunk {c}: {st:?}");
            last.extend_from_slice(&data);
        }
        assert!(d.flush(0).is_ok());
        if stack.engine.commit_now(false).is_err() {
            failed_commits += 1;
        }
    }
    // The last cut goes through once the urgent pass freed enough.
    let start = Instant::now();
    while let Err(e) = stack.engine.commit_now(false) {
        assert!(
            start.elapsed() < Duration::from_secs(120),
            "the volume stays full: {e}"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
    let s = stack.engine.status();
    eprintln!(
        "full volume: {} segment syncs refused, {failed_commits} explicit commits failed, gc {s:?}, sealed {} B (cap {} B), {:?}",
        fs.refused.load(Ordering::Relaxed),
        fs.sealed_bytes(),
        fs.cap,
        start.elapsed()
    );
    assert!(fs.refused.load(Ordering::Relaxed) > 0, "the volume never filled");
    assert!(!stack.engine.is_stopped());
    assert!(s.stats.passes >= 2, "no urgent pass ran: {s:?}");
    assert!(s.stats.bytes_freed > 0, "the urgent pass freed nothing: {s:?}");
    assert!(fs.sealed_bytes() <= fs.cap);
    drop(d);
    close(stack).expect("close");
    if let Some(f) = fsck(&vol) {
        assert!(f.no_errors(), "lunix-fsck:\n{}", f.text);
    }
    let stack = vol.open().expect("reopen");
    let mut d = Driver::new(&stack).expect("driver");
    let all = d.read_all(size).expect("read");
    assert!(all[..region as usize] == last[..], "the region differs after reopen");
    drop(d);
    close(stack).expect("close");
}
