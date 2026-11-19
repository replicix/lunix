//! The space ladder's compact step on the real stack (plan M12 step 12,
//! DESIGN §4.3 and §11, `docs/reference/gc.md` "urgent pass"): GC's urgent
//! pass, registered by `open_volume` for `LadderStepKind::Compact`, runs on
//! the gate's thread through the engine's coordinator when a reservation is
//! refused, and frees the objects a pass condemned once their floor grace
//! ran, without waiting for the configured grace.
//!
//! On a churned volume a tight `cache_max` with `on_full = error` refuses one
//! reservation, which starts the ladder: its urgent pass condemns the dead
//! rounds. With no GC step for longer than the floor grace (manual engine), a
//! second refusal starts the ladder again, and that urgent pass deletes:
//! segment files, the `Segments` charge and the status counters go down or up
//! as they should, the volume reads back equal and `lunix-fsck` finds nothing
//! at rest.
//!
//! A second test fills a volume: the host file system (a test `FsOps` that
//! answers `ENOSPC` to segment syncs above a byte bound) refuses segments,
//! so cuts fail for space and the small journal fills; writers park. The
//! coordinator runs the urgent pass when a cut fails for space (the ladder's
//! compact step from the engine side, plan M17.B review S-2), which frees
//! the dead rounds, and the workload completes without any normal pass.
//!
//! A third test has compaction moves pending when the first cut fails for
//! space (M12c review B-1): the re-hints are applied only after a destage
//! succeeded, so the failed cut leaves the map at the tip, the urgent pass
//! runs, and a fail / succeed / fail sequence ends with the volume equal to
//! the oracle.
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
use lunix_iface::gc::GcControl;
use lunix_iface::space::{ReserveOutcome, SpaceGate, SpaceKind};
use lunix_iface::ErrorKind;
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

/// The daemon's pipelined schedule (`GcConfig::daemon`), a pass at most
/// every second (milestone gcrate, re-review M-1).
fn daemon_gc(o: &mut OpenParams) {
    o.gc = Some(GcConfig {
        pass_interval: Duration::from_secs(1),
        busy_interval: Duration::from_secs(1),
        ..GcConfig::daemon()
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
/// compact step (the urgent pass) to end. Restores the settings.
fn run_ladder(stack: &VolumeStack) {
    let saved = lunixd::stack::effective_space(
        &stack.json,
        lunix_iface::block::BlockEngine::size_bytes(&*stack.engine),
    );
    let mut tight = saved.clone();
    tight.on_full = OnFull::Error;
    tight.cache_max = Some(stack.space.snapshot().used().max(1));
    stack.space.reconfigure(&tight).expect("tight settings");
    let runs = stack.compact_runs.load(Ordering::Acquire);
    let errors = stack.space.step_errors();
    let out = stack
        .space
        .reserve(SpaceKind::JournalReserved, 1 << 20, None)
        .expect("reserve");
    assert!(matches!(out, ReserveOutcome::NoSpace), "{out:?}");
    let start = Instant::now();
    while stack.compact_runs.load(Ordering::Acquire) == runs {
        assert_eq!(stack.space.step_errors(), errors, "the compact step failed");
        assert!(
            start.elapsed() < Duration::from_secs(120),
            "the urgent pass did not finish"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    // A failed run never bumps `compact_runs` (the loop would see the
    // error first); checked once more all the same.
    assert_eq!(stack.space.step_errors(), errors, "the compact step failed");
    stack.space.reconfigure(&saved).expect("settings back");
}

#[test]
fn the_ladder_compact_step_runs_the_urgent_pass_and_frees_condemned_segments() {
    ladder_compact_step("m12c-ladder", fast_gc, false);
}

/// The same on the daemon's pipelined schedule.
#[test]
fn the_ladder_compact_step_on_the_daemon_schedule() {
    ladder_compact_step("gcrate-ladder", daemon_gc, true);
}

/// `scheduled`: the collector runs the daemon's pipelined schedule, so a pass
/// the commits start finishes without the grace: the test asserts that one
/// ran before any ladder run (gcrate final review F-5). The whole-sequence
/// schedule waits out its grace first and cannot.
fn ladder_compact_step(tag: &str, gc: fn(&mut OpenParams), scheduled: bool) {
    let t = Scratch::new(tag);
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
    let stack = vol.open_with(gc).expect("open");
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

    if scheduled {
        // No ladder run yet, so the passes counted are the scheduled ones:
        // a schedule that never started a pass would pass this test on the
        // urgent passes alone.
        let start = Instant::now();
        while stack.engine.status().stats.passes == 0 {
            assert!(
                start.elapsed() < Duration::from_secs(30),
                "no scheduled pipelined pass finished: {:?}",
                stack.engine.status()
            );
            std::thread::sleep(Duration::from_millis(250));
            stack.engine.tick().expect("tick");
        }
        assert_eq!(stack.compact_runs.load(Ordering::Acquire), 0);
    }

    // A first ladder run: its urgent pass condemns the dead rounds (a pass
    // the commits above started may have condemned only part of them), and
    // they start their floor grace.
    run_ladder(&stack);
    let s = stack.engine.status();
    assert!(s.condemned_pending > 0, "nothing condemned: {s:?}");
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
    assert!(
        charged2 < charged,
        "Segments charge {charged} -> {charged2}"
    );
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
    assert!(
        all[..region as usize] == last[..],
        "the region differs after reopen"
    );
    drop(d);
    close(stack).expect("close");
}

/// The host file system with a bound on sealed segment bytes: a segment
/// sync that would leave more than `cap` bytes of `.seg` files answers
/// `ENOSPC` (the store maps it to `NoSpace`, the destage to `AfterSpace`).
#[derive(Debug)]
struct BoundedFs {
    segments: PathBuf,
    /// `u64::MAX`: no bound.
    cap: AtomicU64,
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
        let path =
            std::fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd())).unwrap_or_default();
        if path.parent() == Some(self.segments.as_path()) {
            let len = file.metadata()?.len();
            if self.sealed_bytes() + len > self.cap.load(Ordering::Acquire) {
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
        cap: AtomicU64::new(6 * region),
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
        fs.cap.load(Ordering::Relaxed),
        start.elapsed()
    );
    assert!(
        fs.refused.load(Ordering::Relaxed) > 0,
        "the volume never filled"
    );
    assert!(!stack.engine.is_stopped());
    assert!(s.stats.passes >= 2, "no urgent pass ran: {s:?}");
    assert!(
        s.stats.bytes_freed > 0,
        "the urgent pass freed nothing: {s:?}"
    );
    assert!(fs.sealed_bytes() <= fs.cap.load(Ordering::Relaxed));
    drop(d);
    close(stack).expect("close");
    if let Some(f) = fsck(&vol) {
        assert!(f.no_errors(), "lunix-fsck:\n{}", f.text);
    }
    let stack = vol.open().expect("reopen");
    let mut d = Driver::new(&stack).expect("driver");
    let all = d.read_all(size).expect("read");
    assert!(
        all[..region as usize] == last[..],
        "the region differs after reopen"
    );
    drop(d);
    close(stack).expect("close");
}

#[test]
fn moves_pending_when_cuts_fail_for_space_leave_the_map_at_the_tip() {
    let t = Scratch::new("m12c-moves");
    let cs = 64u32 << 10;
    let cs64 = u64::from(cs);
    let region = 8u64 << 20;
    let size = 64u64 << 20;
    let mut b = VolumeBuilder::new(t.path())
        .manual()
        .mode(CryptoMode::Integrity)
        .chunk_size(cs)
        .size_bytes(size);
    b.track_syncs = false;
    let manual = b.engine.clone();
    b.engine = Some(Arc::new(move |c: &mut EngineConfig| {
        if let Some(m) = &manual {
            m(c);
        }
        gc_tune(c);
    }));
    let vol = b.create().expect("create");
    let fs = Arc::new(BoundedFs {
        segments: vol.voldir.join("segments"),
        cap: AtomicU64::new(u64::MAX),
        refused: AtomicU64::new(0),
    });
    let store_fs: Arc<dyn FsOps> = fs.clone();
    let stack = vol
        .open_with(|o| {
            o.store_fs = Some(store_fs);
            // Passes only on request (and the first one, due at open).
            o.gc = Some(GcConfig {
                pass_interval: Duration::from_secs(3600),
                ..GcConfig::default()
            });
        })
        .expect("open");
    let mut d = Driver::new(&stack).expect("driver");
    let mut oracle = vec![0u8; region as usize];
    let mut write = |d: &mut Driver, c: u64, seed: u64| {
        let data = payload(seed, cs as usize);
        assert!(d.write(0, c * cs64, &data, false).is_ok());
        oracle[(c * cs64) as usize..((c + 1) * cs64) as usize].copy_from_slice(&data);
    };

    // The region whole, then three chunks of four again: the first segment
    // keeps a quarter live. Three forced FULLs take its whole-live root out
    // of GC's retained set.
    for c in 0..region / cs64 {
        write(&mut d, c, c);
    }
    assert!(d.flush(0).is_ok());
    stack.engine.commit_now(false).expect("commit");
    for c in (0..region / cs64).filter(|c| c % 4 != 0) {
        write(&mut d, c, (1 << 32) | c);
    }
    assert!(d.flush(0).is_ok());
    stack.engine.commit_now(false).expect("commit");
    for _ in 0..3 {
        stack.engine.commit_now(true).expect("forced commit");
    }

    // A pass on request compacts that segment; no cut since: its moves are
    // pending.
    let pass = stack.engine.run_pass(false).expect("gc pass");
    let s = stack.engine.status();
    eprintln!("moves: pass {pass:?}; status {s:?}");
    assert!(pass.segments_compacted > 0, "{pass:?}");
    assert!(s.moved_pending > 0, "no move pending: {s:?}");

    // Fail / succeed / fail / succeed. Every failed cut leaves the map at the
    // tip: a dry run answers (not `Busy`) and the urgent pass ran.
    for round in 0..2u64 {
        fs.cap.store(fs.sealed_bytes(), Ordering::Release);
        for c in (0..region / cs64).step_by(8) {
            write(&mut d, c, ((2 + round) << 32) | c);
        }
        assert!(d.flush(0).is_ok());
        let passes = stack.engine.status().stats.passes;
        // The urgent pass runs at the first failed commit, or once its
        // backoff after the last one (which freed nothing) has run out.
        let start = Instant::now();
        loop {
            let e = stack
                .engine
                .commit_now(false)
                .expect_err("the cut must fail for space");
            assert_eq!(e.kind(), ErrorKind::NoSpace, "{e}");
            let dry = stack
                .engine
                .run_pass(true)
                .unwrap_or_else(|e| panic!("round {round}: the map is not at the tip: {e}"));
            assert!(dry.dry_run);
            let s = stack.engine.status();
            if s.stats.passes > passes {
                break;
            }
            assert!(
                start.elapsed() < Duration::from_secs(90),
                "round {round}: no urgent pass after the failed cuts: {s:?}"
            );
            std::thread::sleep(Duration::from_millis(500));
        }
        assert!(!stack.engine.is_stopped());
        fs.cap.store(u64::MAX, Ordering::Release);
        stack
            .engine
            .commit_now(false)
            .expect("the cut after space came back");
        stack.engine.commit_now(true).expect("forced commit");
    }
    assert!(fs.refused.load(Ordering::Relaxed) > 0);
    drop(d);
    close(stack).expect("close");
    if let Some(f) = fsck(&vol) {
        assert!(f.no_errors(), "lunix-fsck:\n{}", f.text);
    }
    let stack = vol.open().expect("reopen");
    let mut d = Driver::new(&stack).expect("driver");
    let all = d.read_all(size).expect("read");
    assert!(
        all[..region as usize] == oracle[..],
        "the region differs after reopen"
    );
    drop(d);
    close(stack).expect("close");
}
