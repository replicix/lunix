//! GC crash loops through the real stack (plan M17.B gate, M17 rule 10):
//! every `gc.*` point, and the store and index points only GC reaches
//! (`store.evidence.*`, `index.after_condemn_flag`), is hit by a child
//! process that `SIGKILL`s itself there, at the first, a middle and the last
//! hit of a workload that churns the device with commits and leaves the
//! collector time for whole passes (condemn, grace, recheck, delete,
//! compaction, evidence). After each kill, as in `stack_kill.rs` (whose
//! harness this file reuses): `lunix-fsck` reports no error and no
//! `index_lost`, the volume opens under `refuse` and equals the oracle after
//! a prefix at least as long as the durable one, takes writes, closes
//! cleanly, reopens equal, and `lunix-fsck` is exit 0 at rest.
//!
//! The collector runs a pass every second with the smallest grace it takes
//! (10 s with `read_timeout` 0); the child ticks the engine (each tick one
//! GC step, or a cut for pending moves) through idle phases longer than the
//! grace.
//!
//! ```text
//! cargo build --release -p lunix-fsck
//! LUNIX_KILL_HITS=all cargo test --release -p lunixd --features testsupport,crashpoints \
//!   --test stack_gc_kill -- --nocapture
//! ```
//!
//! `LUNIX_KILL_HITS`, `LUNIX_KILL_MIB` (default 48) and `LUNIX_KILL_POINTS`
//! as in `stack_kill.rs`.

#![cfg(all(feature = "testsupport", feature = "crashpoints"))]

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write as _;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use std::sync::atomic::Ordering;
use std::sync::Arc;

use lunix_engine::{CommitOutcome, EngineConfig, OnJournalDamage};
use lunix_format::local::OnFull;
use lunix_format::{CryptoMode, WriteSeq};
use lunix_gc::GcConfig;
use lunix_iface::block::BlockEngine;
use lunix_iface::space::{ReserveOutcome, SpaceGate, SpaceKind};
use lunixd::testsupport::{
    assert_no_gc_work, close, damage_journal, fsck, fsck_binary, payload, DeviceModel, Driver,
    ModelOp, Scratch, TestRng, TestVolume, VolumeBuilder,
};

const BLOCK: u64 = 4096;
const SIZE0: u64 = 64 << 20;
const QUEUES: u16 = 2;
/// The prep child's kill: inside its first commit, after the first segment
/// is sealed.
const PREP_KILL: &str = "destage.after_segment_seal:1";
/// The points only GC reaches.
const PREFIXES: [&str; 6] = [
    "gc.",
    "index.retire.",
    "store.evidence.",
    "store.delete.",
    "store.segment_delete.",
    "index.after_condemn_flag",
];
const CHILD_ENV: &str = "LUNIX_GC_KILL_CHILD";
/// Seconds of an idle phase: longer than the grace (10 s) plus a pass.
const IDLE_SECS: u64 = 13;
/// The hot region rewritten every round.
const HOT: u64 = 8 << 20;

fn env_or(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

// ------------------------------------------------------------ the workload

#[derive(Clone, Copy, Debug)]
enum Step {
    Write {
        q: usize,
        off: u64,
        len: u32,
        seed: u64,
        fua: bool,
    },
    Zero {
        q: usize,
        off: u64,
        len: u32,
        discard: bool,
    },
    Flush {
        q: usize,
    },
    Commit {
        force: bool,
    },
    Tick,
    Grow(u64),
    Sleep(u64),
    Reopen,
    /// Ticks for that many seconds, a forced commit every 2 s (GC steps,
    /// pending moves cut, sources go one commit after their output).
    Idle(u64),
    /// The space ladder's compact step: a refused reservation starts the
    /// ladder, which runs GC's urgent pass on the gate's thread (M12c).
    Ladder,
}

struct Plan {
    prep: Vec<Step>,
    run: Vec<Step>,
}

fn writes(rng: &mut TestRng, cs: u64, size: u64, n: usize, out: &mut Vec<Step>) -> u64 {
    let mut bytes = 0;
    let blocks = size / BLOCK;
    let cb = cs / BLOCK;
    for _ in 0..n {
        let q = rng.below(u64::from(QUEUES)) as usize;
        let len_b = match rng.below(5) {
            0 => 1 + rng.below(cb),
            1 => cb,
            2 => 4 * cb,
            _ => 1 + rng.below(256),
        }
        .min(256);
        let start = if rng.below(2) == 0 {
            rng.below(blocks / cb) * cb
        } else {
            rng.below(blocks)
        }
        .min(blocks - len_b);
        let aligned = (start * BLOCK).is_multiple_of(cs) && (len_b * BLOCK).is_multiple_of(cs);
        let seed = if aligned && rng.below(3) == 0 {
            rng.below(8)
        } else {
            rng.next_u64()
        };
        out.push(Step::Write {
            q,
            off: start * BLOCK,
            len: (len_b * BLOCK) as u32,
            seed,
            fua: rng.below(16) == 0,
        });
        bytes += len_b * BLOCK;
        if rng.below(12) == 0 {
            let zl = (1 + rng.below(4 * cb)).min(blocks);
            let zs = rng.below(blocks - zl + 1);
            out.push(Step::Zero {
                q,
                off: zs * BLOCK,
                len: (zl * BLOCK) as u32,
                discard: rng.below(2) == 0,
            });
        }
    }
    bytes
}

fn plan(cs: u64, seed: u64) -> Plan {
    let mut rng = TestRng::new(seed);
    let mut prep = Vec::new();
    writes(&mut rng, cs, SIZE0, 24, &mut prep);
    prep.push(Step::Flush { q: 0 });
    prep.push(Step::Commit { force: false });
    let total = env_or("LUNIX_KILL_MIB", 48) << 20;
    let rounds = 6u64;
    let per_round = total / rounds;
    let mut run = Vec::new();
    let mut size = SIZE0;
    let hot = HOT / cs;
    for r in 0..rounds {
        // A hot region rewritten in its own cut: its older segments die
        // whole (condemn, index flags, delete) or keep a quarter
        // (compaction).
        for c in 0..hot {
            if r % 2 == 1 && c % 4 == 0 {
                continue;
            }
            run.push(Step::Write {
                q: 0,
                off: c * cs,
                len: cs as u32,
                seed: rng.next_u64(),
                fua: false,
            });
        }
        run.push(Step::Flush { q: 0 });
        run.push(Step::Commit { force: false });
        let mut done = 0;
        while done < per_round {
            done += writes(&mut rng, cs, size, 8, &mut run);
        }
        run.push(Step::Flush {
            q: (r % 2) as usize,
        });
        if r % 3 == 1 {
            // Idle after a flush: the journal witnesses it with a mark.
            run.push(Step::Sleep(120));
        }
        if r == rounds / 3 {
            size += 16 * cs;
            run.push(Step::Grow(size));
        }
        if r == rounds / 2 {
            run.push(Step::Reopen);
        }
        run.push(Step::Commit { force: r % 4 == 3 });
        run.push(Step::Tick);
        if r >= 1 {
            run.push(Step::Idle(IDLE_SECS));
        }
        if r == 2 || r == 4 {
            // After an idle phase longer than the floor grace: the urgent
            // pass deletes what a pass condemned, condemns and compacts.
            run.push(Step::Ladder);
        }
    }
    writes(&mut rng, cs, size, 8, &mut run);
    run.push(Step::Flush { q: 1 });
    Plan { prep, run }
}

/// The oracle requests of `steps`, appended to `model`.
fn model_ops(steps: &[Step], model: &mut DeviceModel) {
    for s in steps {
        match *s {
            Step::Write { off, len, seed, .. } => {
                model.apply(ModelOp::Write {
                    offset: off,
                    len,
                    seed,
                });
            }
            Step::Zero { off, len, .. } => {
                model.apply(ModelOp::Zero { offset: off, len });
            }
            Step::Grow(size) => {
                model.apply(ModelOp::Grow(size));
            }
            _ => {}
        }
    }
}

// ------------------------------------------------------------- the child

struct Spec {
    voldir: PathBuf,
    mode: CryptoMode,
    cs: u32,
    key_file: Option<PathBuf>,
    seed: u64,
    phase: String,
    progress: PathBuf,
    /// The recheck-only arm (`GcConfig::fast_recheck`).
    fast: bool,
}

impl Spec {
    fn write(&self, path: &Path) {
        let mode = match self.mode {
            CryptoMode::Plain => "plain",
            CryptoMode::Integrity => "integrity",
            CryptoMode::Encrypted => "encrypted",
        };
        let key = self
            .key_file
            .as_ref()
            .map(|k| k.display().to_string())
            .unwrap_or_default();
        let s = format!(
            "voldir={}\nmode={mode}\ncs={}\nkey_file={key}\nseed={}\nphase={}\nprogress={}\nfast={}\n",
            self.voldir.display(),
            self.cs,
            self.seed,
            self.phase,
            self.progress.display(),
            u8::from(self.fast)
        );
        std::fs::write(path, s).expect("write spec");
    }

    fn read(path: &Path) -> Self {
        let text = std::fs::read_to_string(path).expect("read spec");
        let kv: BTreeMap<&str, &str> = text.lines().filter_map(|l| l.split_once('=')).collect();
        let get = |k: &str| *kv.get(k).unwrap_or_else(|| panic!("spec has no {k}"));
        Self {
            voldir: PathBuf::from(get("voldir")),
            mode: match get("mode") {
                "plain" => CryptoMode::Plain,
                "integrity" => CryptoMode::Integrity,
                _ => CryptoMode::Encrypted,
            },
            cs: get("cs").parse().expect("cs"),
            key_file: Some(get("key_file"))
                .filter(|k| !k.is_empty())
                .map(PathBuf::from),
            seed: get("seed").parse().expect("seed"),
            phase: get("phase").to_owned(),
            progress: PathBuf::from(get("progress")),
            fast: get("fast") == "1",
        }
    }
}

/// GC settings for a test: the smallest grace the collector accepts.
fn gc_tune(c: &mut EngineConfig) {
    if let Some(p) = &mut c.gc_params {
        p.read_timeout = Duration::ZERO;
        p.grace = Duration::from_secs(10);
    }
}

/// A pass every second, on the daemon's pipelined schedule
/// (`GcConfig::daemon`, gcrate review M-2); `LUNIX_GC_PIPELINE=0` runs the
/// whole sequence for every pass instead.
///
/// `recheck`: the recheck-only arm (milestone fastrecheck): recheck-only
/// passes without waiting for pressure, between regular passes that come at
/// most every 5 s, so the new point `gc.recheck.before_delete` and the
/// deletes of such a pass are reached.
///
/// Every arm runs with `gc_condemn_on_cut = shadow` and `gc_fast_recheck` on
/// (milestone refs: the configuration the condemn-on-cut stage will ship
/// with); the plain arms see no pressure, so their recheck-only passes only
/// run in the recheck arm, which forces them.
fn fast_gc(o: &mut lunixd::stack::OpenParams, recheck: bool) {
    o.gc_condemn_on_cut = lunix_engine::CondemnOnCut::Shadow;
    o.gc_fast_recheck = Some(Duration::from_secs(1));
    let base = if env_or("LUNIX_GC_PIPELINE", 1) == 0 {
        GcConfig::default()
    } else {
        GcConfig::daemon()
    };
    o.gc = Some(if recheck {
        GcConfig {
            pass_interval: Duration::from_secs(10),
            busy_interval: Duration::from_secs(5),
            fast_recheck: Some(Duration::from_secs(1)),
            fast_recheck_always: true,
            ..base
        }
    } else {
        GcConfig {
            pass_interval: Duration::from_secs(1),
            busy_interval: Duration::from_secs(1),
            ..base
        }
    });
}

fn builder(parent: &Path, mode: CryptoMode, cs: u32) -> VolumeBuilder {
    let mut b = VolumeBuilder::new(parent)
        .manual()
        .mode(mode)
        .chunk_size(cs)
        .size_bytes(SIZE0)
        .queues(QUEUES);
    let manual = b.engine.clone();
    b.engine = Some(Arc::new(move |c: &mut EngineConfig| {
        if let Some(t) = &manual {
            t(c);
        }
        gc_tune(c);
    }));
    b
}

/// Appends the durable prefix length and the driver's durable `write_seq`
/// to the progress file (no sync: a `SIGKILL` keeps the page cache).
fn progress(f: &mut std::fs::File, n: usize, d: &Driver) {
    writeln!(f, "{n} {}", d.durable().0).expect("progress");
}

/// Runs `steps` on the volume, `base` requests already logged before them.
/// The reference table of the shadow mode matched every mark so far.
fn assert_refs_clean(stack: &lunixd::stack::VolumeStack, what: &str) {
    let t = stack
        .engine
        .gc_status_handle()
        .get()
        .refs
        .map(|b| *b)
        .expect("the table exists");
    assert_eq!(t.drift_segments, 0, "{what}: refs_drift_segments: {t:?}");
    assert_eq!(
        t.report_mismatches, 0,
        "{what}: refs report mismatches: {t:?}"
    );
}

/// Ticks the engine until the table is `Ready` (a pass is due at once after
/// an attach, and its mark is the table's base).
fn wait_refs_ready(stack: &lunixd::stack::VolumeStack, what: &str) {
    let start = Instant::now();
    loop {
        stack.engine.tick().expect("tick after recovery");
        let t = *stack.engine.gc_status_handle().get().refs.expect("table");
        if t.state == lunix_iface::gc::RefsState::Ready {
            return;
        }
        assert!(
            start.elapsed() < Duration::from_secs(120),
            "{what}: the table is not Ready after 120 s: {t:?}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn execute(vol: &TestVolume, steps: &[Step], base: usize, prog: &mut std::fs::File, recheck: bool) {
    let mut stack = vol
        .open_with(move |o| fast_gc(o, recheck))
        .expect("child open");
    let mut d = Driver::new(&stack).expect("driver");
    // Recovery kept (and the first lane open synced) everything before.
    progress(prog, base, &d);
    let mut n = base;
    let mut bounds: Vec<u64> = Vec::new();
    for s in steps {
        match *s {
            Step::Write {
                q,
                off,
                len,
                seed,
                fua,
            } => {
                let st = d.write(q, off, &payload(seed, len as usize), fua);
                assert!(st.is_ok(), "child write: {st:?}");
                n += 1;
                bounds.push(d.issued().0);
                if fua {
                    progress(prog, n, &d);
                }
            }
            Step::Zero {
                q,
                off,
                len,
                discard,
            } => {
                let st = if discard {
                    d.discard(q, off, len)
                } else {
                    d.write_zeroes(q, off, len)
                };
                assert!(st.is_ok(), "child zero: {st:?}");
                n += 1;
                bounds.push(d.issued().0);
            }
            Step::Flush { q } => {
                assert!(d.flush(q).is_ok(), "child flush");
                progress(prog, n, &d);
            }
            Step::Commit { force } => {
                let out = stack.engine.commit_now(force).expect("child commit");
                if let CommitOutcome::Committed { cut_seq, .. } = out {
                    let k = bounds.partition_point(|b| *b <= cut_seq.0);
                    progress(prog, base + k, &d);
                }
            }
            Step::Tick => {
                let out = stack.engine.tick().expect("child tick");
                if let CommitOutcome::Committed { cut_seq, .. } = out {
                    let k = bounds.partition_point(|b| *b <= cut_seq.0);
                    progress(prog, base + k, &d);
                }
            }
            Step::Grow(size) => {
                stack.engine.grow(size).expect("child grow");
                n += 1;
                // The grow's FULL commit covers everything before it.
                bounds.fill(0);
                bounds.push(0);
                progress(prog, n, &d);
            }
            Step::Sleep(ms) => std::thread::sleep(Duration::from_millis(ms)),
            Step::Idle(secs) => {
                let end = Instant::now() + Duration::from_secs(secs);
                let mut i = 0u64;
                while Instant::now() < end {
                    i += 1;
                    let out = if i.is_multiple_of(20) {
                        stack.engine.commit_now(true).expect("child commit")
                    } else {
                        stack.engine.tick().expect("child tick")
                    };
                    if let CommitOutcome::Committed { cut_seq, .. } = out {
                        let k = bounds.partition_point(|b| *b <= cut_seq.0);
                        progress(prog, base + k, &d);
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
            Step::Ladder => run_ladder(&stack),
            Step::Reopen => {
                assert_refs_clean(&stack, "child before a reopen");
                drop(d);
                drop(stack);
                stack = vol
                    .open_with(move |o| fast_gc(o, recheck))
                    .expect("child reopen");
                d = Driver::new(&stack).expect("driver");
                // Every request so far was recovered and synced.
                bounds.fill(0);
                progress(prog, n, &d);
            }
        }
    }
    progress(prog, n, &d);
    assert_refs_clean(&stack, "child at the end");
    drop(d);
    close(stack).expect("child close");
    writeln!(prog, "{n} 0").expect("progress");
}

/// Refuses one reservation (a `cache_max` at what is charged, `on_full =
/// error`), which starts the space ladder on the gate's thread, and waits for
/// its compact step (the urgent pass) to end. Restores the settings.
fn run_ladder(stack: &lunixd::stack::VolumeStack) {
    let saved = lunixd::stack::effective_space(
        &stack.json,
        lunix_iface::block::BlockEngine::size_bytes(&*stack.engine),
    );
    let mut tight = saved.clone();
    tight.on_full = OnFull::Error;
    tight.cache_max = Some(stack.space.snapshot().used().max(1));
    stack
        .space
        .reconfigure(&tight)
        .expect("child tight settings");
    let runs = stack.compact_runs.load(Ordering::Acquire);
    let errors = stack.space.step_errors();
    let out = stack
        .space
        .reserve(SpaceKind::JournalReserved, 1 << 20, None)
        .expect("child reserve");
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
    stack
        .space
        .reconfigure(&saved)
        .expect("child settings back");
}

/// The child entry: does nothing unless this process was started by a kill
/// test with a spec.
#[test]
fn gc_kill_child() {
    let Some(spec_path) = std::env::var_os(CHILD_ENV) else {
        return;
    };
    // Warnings go to the child's log: the parent fails on a reference-table
    // warning even when the child was killed before it could assert.
    lunixd::log::StderrLog::install(tracing::Level::WARN);
    let spec = Spec::read(Path::new(&spec_path));
    let parent = spec.voldir.parent().expect("parent").to_path_buf();
    let vol = TestVolume::at(
        &spec.voldir,
        builder(&parent, spec.mode, spec.cs),
        spec.key_file.clone(),
    )
    .expect("volume");
    let p = plan(u64::from(spec.cs), spec.seed);
    let mut prog = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&spec.progress)
        .expect("progress file");
    match spec.phase.as_str() {
        "prep" => execute(&vol, &p.prep, 0, &mut prog, spec.fast),
        _ => execute(
            &vol,
            &p.run,
            p.prep.iter().filter(is_op).count(),
            &mut prog,
            spec.fast,
        ),
    }
    lunix_iface::crashpoint::dump_counts().expect("dump counts");
}

fn is_op(s: &&Step) -> bool {
    matches!(s, Step::Write { .. } | Step::Zero { .. } | Step::Grow(_))
}

// ------------------------------------------------------------ the parent

enum ChildEnd {
    Killed,
    Exited(BTreeMap<String, u64>),
}

fn spawn_child(spec: &Spec, dir: &Path, crash_at: Option<&str>) -> ChildEnd {
    let spec_path = dir.join(format!("spec-{}", spec.phase));
    spec.write(&spec_path);
    let counts = dir.join(format!("counts-{}", spec.phase));
    let _ = std::fs::remove_file(&counts);
    let mut cmd = Command::new(std::env::current_exe().expect("current exe"));
    cmd.args([
        "--exact",
        "gc_kill_child",
        "--nocapture",
        "--test-threads=1",
    ])
    .env(CHILD_ENV, &spec_path)
    .env("LUNIX_CRASH_COUNT", &counts)
    .env_remove("LUNIX_CRASH_AT");
    if let Some(p) = crash_at {
        cmd.env("LUNIX_CRASH_AT", p);
    }
    let log = dir.join(format!("child-{}.log", spec.phase));
    let out = std::fs::File::create(&log).expect("child log");
    let err = out.try_clone().expect("child log");
    let mut child = cmd
        .stdout(out)
        .stderr(err)
        .spawn()
        .expect("spawn the child");
    let start = Instant::now();
    let status = loop {
        if let Some(st) = child.try_wait().expect("wait") {
            break st;
        }
        if start.elapsed() > Duration::from_secs(1800) {
            let _ = child.kill();
            let _ = child.wait();
            panic!(
                "the {} child ({crash_at:?}) hung: {}",
                spec.phase,
                std::fs::read_to_string(&log).unwrap_or_default()
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_no_refs_warning(&log, &spec.phase, crash_at);
    if status.signal() == Some(libc::SIGKILL) {
        return ChildEnd::Killed;
    }
    assert!(
        status.success(),
        "the {} child ({crash_at:?}) failed ({status}): {}",
        spec.phase,
        std::fs::read_to_string(&log).unwrap_or_default()
    );
    let text = std::fs::read_to_string(&counts).unwrap_or_default();
    ChildEnd::Exited(
        text.lines()
            .filter_map(|l| {
                let (k, v) = l.split_once(' ')?;
                Some((k.to_owned(), v.parse().ok()?))
            })
            .collect(),
    )
}

/// The table of the shadow mode logs a warning when a mark or a map
/// transaction contradicts it; a killed child cannot assert, so its log is
/// read by the parent.
fn assert_no_refs_warning(log: &Path, phase: &str, crash_at: Option<&str>) {
    let text = std::fs::read_to_string(log).unwrap_or_default();
    let lines: Vec<&str> = text
        .lines()
        .filter(|l| l.contains("reference table"))
        .collect();
    assert!(
        lines.is_empty(),
        "the {phase} child ({crash_at:?}) logged reference-table warnings: {lines:?}"
    );
}

struct Config {
    mode: CryptoMode,
    cs: u32,
    seed: u64,
    fast: bool,
}

struct Prepared {
    vol: TestVolume,
    dir: PathBuf,
    progress: PathBuf,
}

/// A fresh volume, the prep child killed in its commit, a planted leftover.
fn prepare(cfg: &Config, dir: &Path) -> Prepared {
    std::fs::create_dir_all(dir).expect("run dir");
    let vol = builder(dir, cfg.mode, cfg.cs).create().expect("create");
    let progress = dir.join("progress");
    let spec = Spec {
        voldir: vol.voldir.clone(),
        mode: cfg.mode,
        cs: cfg.cs,
        key_file: vol.key_file.clone(),
        seed: cfg.seed,
        phase: "prep".into(),
        progress: progress.clone(),
        fast: cfg.fast,
    };
    match spawn_child(&spec, dir, Some(PREP_KILL)) {
        ChildEnd::Killed => {}
        ChildEnd::Exited(_) => panic!("the prep child was not killed at {PREP_KILL}"),
    }
    std::fs::write(vol.voldir.join("segments/m17b-planted.tmp"), [0x5au8; 4096])
        .expect("plant a leftover");
    Prepared {
        vol,
        dir: dir.to_path_buf(),
        progress,
    }
}

fn run_child(cfg: &Config, p: &Prepared, crash_at: Option<&str>) -> ChildEnd {
    let spec = Spec {
        voldir: p.vol.voldir.clone(),
        mode: cfg.mode,
        cs: cfg.cs,
        key_file: p.vol.key_file.clone(),
        seed: cfg.seed,
        phase: "run".into(),
        progress: p.progress.clone(),
        fast: cfg.fast,
    };
    spawn_child(&spec, &p.dir, crash_at)
}

#[derive(Default, Debug)]
struct Tally {
    kills: u64,
    /// Requests of the plan not on the device after the kill (not yet
    /// submitted, or not durable and lost).
    lost_requests: u64,
    fsck_findings: BTreeMap<String, u64>,
    /// Findings at rest (after recovery and a clean close), with the runs
    /// that produced them.
    at_rest: BTreeMap<String, Vec<String>>,
    /// Verified runs.
    runs: u64,
    /// Journal records damaged above the durable bound after a kill.
    damaged_records: u64,
}

/// The `lunix-fsck` findings a phase may show. Anything else, and any
/// error, fails the run.
///
/// * After a kill: what a crash leaves (temporary files, unnamed or
///   unreferenced containers, an index behind the chain, journal residue);
///   `index_extra` only after a kill between a segment's unlink and its
///   index update of a GC delete phase (`store.segment_delete.*`, which
///   the open's sweep does not fire, and `gc.delete.*`;
///   gc.md crash table: the open's catch-up drops the entries, at rest they
///   are gone).
/// * At rest (after recovery and a clean close): notes only (exit 0).
const ALLOWED_AFTER_KILL: &[&str] = &["leftover", "crash_leftover", "orphan", "index_missing"];
const ALLOWED_IN_THE_WINDOW: &[&str] = &[
    "leftover",
    "crash_leftover",
    "orphan",
    "index_missing",
    "index_extra",
    "index_stale",
];
/// Segments one delete step may leave recorded late: the collector's
/// `RETIRE_BATCH`.
use lunix_gc::RETIRE_BATCH;

/// An id allocator for opening a copy of the index (the allocator is only
/// asked to observe ids).
#[derive(Debug, Default)]
struct Ids(std::sync::atomic::AtomicU64);

impl lunix_iface::store::IdAllocator for Ids {
    fn allocate(&self) -> lunix_iface::Result<lunix_format::ObjId> {
        Ok(lunix_format::ObjId::new(
            0,
            self.0.fetch_add(1, Ordering::SeqCst) + 1,
        )?)
    }
    fn high_water(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
    fn observe(&self, raw: u64) {
        self.0
            .fetch_max(raw & lunix_format::ObjId::MAX_COUNTER, Ordering::SeqCst);
    }
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("mkdir");
    for e in std::fs::read_dir(from).expect("readdir").flatten() {
        let dest = to.join(e.file_name());
        if e.file_type().expect("type").is_dir() {
            copy_tree(&e.path(), &dest);
        } else {
            std::fs::copy(e.path(), dest).expect("copy");
        }
    }
}

/// On a copy of the killed volume's index: whether it has entries into
/// segments that have no file. Panics unless every such entry's segment is
/// flagged condemned (dedup never reaches it) and at most `RETIRE_BATCH`
/// segments are in that state.
fn entries_into_missing_segments(vol: &TestVolume, what: &str) -> bool {
    let existing: BTreeSet<u64> = std::fs::read_dir(vol.voldir.join("segments"))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            u64::from_str_radix(n.split('.').next()?, 16).ok()
        })
        .collect();
    let t = Scratch::new("gckwin");
    copy_tree(&vol.voldir.join("index"), &t.path().join("index"));
    let ix = lunix_index::LsmIndex::open(t.path(), Arc::new(Ids::default()), vol.builder.id_len)
        .unwrap_or_else(|e| panic!("{what}: open a copy of the killed index: {e}"));
    let mut missing: BTreeMap<u64, bool> = BTreeMap::new();
    lunix_iface::index::ChunkIndex::iter_entries(&ix, &mut |e| {
        let seg = e.hint.seg.raw();
        if !existing.contains(&seg) {
            let flagged = missing.entry(seg).or_insert(true);
            *flagged &= e.condemned;
        }
    })
    .expect("iter_entries");
    let unflagged: Vec<u64> = missing
        .iter()
        .filter(|(_, f)| !**f)
        .map(|(s, _)| *s)
        .collect();
    assert!(
        unflagged.is_empty(),
        "{what}: entries into segments with no file whose flag is not set: {unflagged:x?}"
    );
    assert!(
        missing.len() <= RETIRE_BATCH,
        "{what}: {} segments with entries and no file (limit {RETIRE_BATCH})",
        missing.len()
    );
    !missing.is_empty()
}

const ALLOWED_AT_REST: &[&str] = &[];

fn fsck_clean(vol: &TestVolume, when: &str, what: &str, allowed: &[&str], tally: &mut Tally) {
    let f = fsck(vol).unwrap_or_else(|| panic!("{what}: lunix-fsck is not built"));
    assert!(
        f.no_errors(),
        "{what}: lunix-fsck {when} found errors:\n{}",
        f.text
    );
    assert!(
        !f.findings.iter().any(|(_, c)| c == "index_lost"),
        "{what}: lunix-fsck {when}: index_lost:\n{}",
        f.text
    );
    for (sev, code) in f.findings {
        assert!(
            sev == "note" || allowed.contains(&code.as_str()),
            "{what}: lunix-fsck {when}: unexpected {sev} {code}:\n{}",
            f.text
        );
        if when == "at-rest" && sev != "note" {
            let w = tally.at_rest.entry(format!("{sev}:{code}")).or_default();
            if !w.contains(&what.to_owned()) {
                w.push(what.to_owned());
            }
        }
        *tally
            .fsck_findings
            .entry(format!("{when}:{sev}:{code}"))
            .or_insert(0) += 1;
    }
}

/// The checks after a kill (or a clean end): fsck on the killed volume, the
/// journal damaged above the child's durable bound (a power loss on top of
/// the kill), recovery under `refuse` against the oracle, writes and a
/// read-back after recovery, a clean close, a second open, fsck at rest.
fn verify(cfg: &Config, p: &Prepared, what: &str, tally: &mut Tally) {
    // A step unlinks its segments and records them in the index in one
    // update at its end, so a kill in a delete phase can leave unlinked
    // segments whose entries are still there (`index_extra`, `index_stale`
    // when another copy of an id exists), completed by the next pass. What
    // makes that safe is checked on the killed image, not by the kill point's
    // name: every such entry's segment is still flagged, and at most
    // `RETIRE_BATCH` segments are in that state.
    let allowed = if entries_into_missing_segments(&p.vol, what) {
        ALLOWED_IN_THE_WINDOW
    } else {
        ALLOWED_AFTER_KILL
    };
    fsck_clean(&p.vol, "after-kill", what, allowed, tally);
    let mut durable = 0usize;
    let mut durable_seq = 0u64;
    for l in std::fs::read_to_string(&p.progress)
        .unwrap_or_default()
        .lines()
    {
        let mut w = l.split_whitespace();
        if let Some(n) = w.next().and_then(|v| v.parse().ok()) {
            durable = durable.max(n);
        }
        if let Some(q) = w.next().and_then(|v| v.parse().ok()) {
            durable_seq = durable_seq.max(q);
        }
    }
    if std::env::var_os("LUNIX_KILL_NO_DAMAGE").is_none() {
        let mut rng = TestRng::new(cfg.seed ^ (tally.runs << 20));
        let c = damage_journal(&p.vol.voldir, WriteSeq(durable_seq), &mut rng);
        tally.damaged_records += c.records_damaged as u64;
    }
    tally.runs += 1;
    let stack = p
        .vol
        .open_with(|o| {
            fast_gc(o, cfg.fast);
            o.on_journal_damage = OnJournalDamage::Refuse;
        })
        .unwrap_or_else(|e| panic!("{what}: open after the kill (refuse): {e}"));
    let size = stack.engine.size_bytes();
    let mut d = Driver::new(&stack).expect("driver");
    let mut actual = d
        .read_all(size)
        .unwrap_or_else(|s| panic!("{what}: full read: {s:?}"));
    let pl = plan(u64::from(cfg.cs), cfg.seed);
    let mut model = DeviceModel::new(SIZE0);
    model_ops(&pl.prep, &mut model);
    model_ops(&pl.run, &mut model);
    model.durable_upto(durable);
    let Some(n) = model.first_match(&actual) else {
        panic!(
            "{what}: the device is no prefix >= {durable} of {} requests (size {size})",
            model.len()
        );
    };
    tally.lost_requests += (model.len() - n) as u64;
    // The table is rebuilt from the first mark after the open; once it is
    // `Ready` the writes below change the map, so a later mark compares.
    wait_refs_ready(&stack, what);
    // The recovered volume takes writes and reads them back, across a clean
    // close and a second open.
    let cs = u64::from(cfg.cs);
    for i in 0..8u64 {
        let off = (i * 37 % (size / cs)) * cs + (i % 2) * BLOCK;
        let data = payload(0xa5a5 ^ i, 2 * BLOCK as usize);
        assert!(
            d.write((i % 2) as usize, off, &data, false).is_ok(),
            "{what}: write after recovery"
        );
        actual[off as usize..off as usize + data.len()].copy_from_slice(&data);
    }
    assert!(d.flush(0).is_ok(), "{what}: flush after recovery");
    let back = d
        .read_all(size)
        .unwrap_or_else(|s| panic!("{what}: read-back: {s:?}"));
    assert!(back == actual, "{what}: read-back after recovery differs");
    settle_gc(&stack, &mut d, &actual, what);
    // Recovery: the table was rebuilt from the first mark and matched every
    // later one.
    let t = stack
        .engine
        .gc_status_handle()
        .get()
        .refs
        .map(|b| *b)
        .expect("the table exists");
    assert_eq!(
        t.state,
        lunix_iface::gc::RefsState::Ready,
        "{what}: no mark rebased the table after recovery: {t:?}"
    );
    // The base mark and at least one later mark that compared (the writes
    // before `settle_gc` changed the map): drift 0 is not vacuous.
    assert!(
        t.rebases >= 2,
        "{what}: only {} mark(s) rebased the table after recovery: {t:?}",
        t.rebases
    );
    assert_refs_clean(&stack, what);
    drop(d);
    close(stack).unwrap_or_else(|e| panic!("{what}: close: {e}"));
    let stack = p
        .vol
        .open_with(|o| {
            fast_gc(o, cfg.fast);
            o.on_journal_damage = OnJournalDamage::Refuse;
        })
        .unwrap_or_else(|e| panic!("{what}: second open: {e}"));
    let mut d = Driver::new(&stack).expect("driver");
    let again = d
        .read_all(size)
        .unwrap_or_else(|s| panic!("{what}: second read: {s:?}"));
    assert!(
        again == actual,
        "{what}: the device differs after a clean close and reopen"
    );
    // GC finishes what the close's commit left, then the close cuts nothing.
    settle_gc(&stack, &mut d, &actual, what);
    assert_refs_clean(&stack, what);
    drop(d);
    close(stack).unwrap_or_else(|e| panic!("{what}: close: {e}"));
    let r = p.vol.check().expect("store self-check runs");
    assert!(r.is_clean(), "{what}: store self-check: {r:?}");
    fsck_clean(&p.vol, "at-rest", what, ALLOWED_AT_REST, tally);
    assert_no_gc_work(&p.vol, what);
}

/// After recovery: ticks through a grace and whole passes until GC has
/// nothing condemned (the list is loaded with a fresh grace, the flags set
/// again, the recheck un-condemns what the replayed journal made live, the
/// deletes complete), then the device is read again against `actual`.
fn settle_gc(stack: &lunixd::stack::VolumeStack, d: &mut Driver, actual: &[u8], what: &str) {
    // Quiescent: nothing condemned, and no cut (moves or a cut the collector
    // asked for) for a grace and more; a deadline, not a fixed window.
    let start = Instant::now();
    let mut last_cut = Instant::now();
    loop {
        if matches!(
            stack.engine.tick().expect("tick after recovery"),
            CommitOutcome::Committed { .. }
        ) {
            last_cut = Instant::now();
        }
        let listed = lunix_iface::store::GcStateStore::load_condemned(&stack.store)
            .expect("condemned list")
            .as_ref()
            .and_then(lunix_gc::CondemnedList::from_file)
            .map_or(0, |l| l.entries.len());
        if listed == 0 && last_cut.elapsed() > Duration::from_secs(13) {
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(180),
            "{what}: GC does not settle after recovery ({listed} condemned)"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    stack.engine.release_read_cache();
    let back = d
        .read_all(actual.len() as u64)
        .unwrap_or_else(|s| panic!("{what}: read after GC: {s:?}"));
    assert!(
        back == actual,
        "{what}: the device differs after GC ran on the recovered volume"
    );
}

/// Registered GC points the workload does not reach. The urgent pass's own
/// (`gc.urgent.after_mark`) is reached through the space ladder
/// (`Step::Ladder`, M12c). Any unreached point fails the loop. The recheck-only
/// point is reached by the recheck arm only.
const EXPECTED_UNREACHED: &[&str] = &["gc.recheck.before_delete"];
/// What the recheck-only arm kills at: its own point and the deletes (which a
/// recheck-only pass runs too).
const RECHECK_ARM_POINTS: [&str; 3] = ["gc.recheck.", "gc.delete.", "gc.commit_delete."];

fn kill_loop(mode: CryptoMode, cs: u32) {
    kill_loop_with(mode, cs, false);
}

fn kill_loop_with(mode: CryptoMode, cs: u32, fast: bool) {
    // fsck is part of every recovery check: a missing binary fails the loop.
    assert!(
        fsck_binary().is_some(),
        "lunix-fsck is not built: cargo build --release -p lunix-fsck"
    );
    // The recheck arm kills at the first, a middle and the last hit of its
    // points, whatever `LUNIX_KILL_HITS` says.
    let all_hits = fast || std::env::var("LUNIX_KILL_HITS").is_ok_and(|v| v == "all");
    let only: Vec<String> = if fast {
        RECHECK_ARM_POINTS.iter().map(|p| (*p).to_owned()).collect()
    } else {
        std::env::var("LUNIX_KILL_POINTS")
            .map(|v| v.split(',').map(str::to_owned).collect())
            .unwrap_or_default()
    };
    let cfg = Config {
        mode,
        cs,
        seed: 0x6c17_0000 ^ u64::from(cs) ^ ((mode as u64) << 40) ^ (u64::from(fast) << 48),
        fast,
    };
    let tag = format!(
        "m17b-gckill-{}{mode:?}-{}k",
        if fast { "recheck-" } else { "" },
        cs >> 10
    )
    .to_lowercase();
    let t = Scratch::new(&tag);
    let started = Instant::now();
    let mut tally = Tally::default();

    // The counting run: no kill, every point's hit count.
    let p = prepare(&cfg, &t.path().join("count"));
    let counts = match run_child(&cfg, &p, None) {
        ChildEnd::Exited(c) => c,
        ChildEnd::Killed => panic!("the counting run was killed"),
    };
    verify(&cfg, &p, "counting run", &mut tally);
    drop(p);
    std::fs::remove_dir_all(t.path().join("count")).expect("cleanup");

    let points: Vec<&'static str> = lunix_iface::crashpoint::registered()
        .into_iter()
        .filter(|n| PREFIXES.iter().any(|p| n.starts_with(p)))
        .filter(|n| only.is_empty() || only.iter().any(|o| n.starts_with(o.as_str())))
        .collect();
    let reached: BTreeMap<&str, u64> = points
        .iter()
        .filter_map(|p| counts.get(*p).map(|c| (*p, *c)))
        .collect();
    eprintln!(
        "kill loop {mode:?} {} KiB: counting run hits {reached:?}",
        cs >> 10
    );
    let mut unreached = Vec::new();
    let mut killed: BTreeMap<&str, BTreeSet<u64>> = BTreeMap::new();
    let mut runs = 0u64;
    for point in &points {
        let c = counts.get(*point).copied().unwrap_or(0);
        if c == 0 {
            unreached.push(*point);
            continue;
        }
        let mut hits: Vec<u64> = if all_hits {
            vec![1, c.div_ceil(2), c]
        } else {
            vec![1]
        };
        hits.dedup();
        for mut hit in hits {
            let dir = t.path().join(format!("r{runs}"));
            runs += 1;
            let mut p = prepare(&cfg, &dir);
            let mut end = run_child(&cfg, &p, Some(&format!("{point}:{hit}")));
            // Timing (journal marks) changes some counts from run to run: a
            // run that is not killed is retried at its own last hit, up to
            // three times.
            for _ in 0..3 {
                let ChildEnd::Exited(c2) = &end else { break };
                let c2 = c2.get(*point).copied().unwrap_or(0);
                if c2 == 0 || c2 >= hit {
                    break;
                }
                drop(p);
                std::fs::remove_dir_all(&dir).expect("cleanup");
                hit = c2;
                p = prepare(&cfg, &dir);
                end = run_child(&cfg, &p, Some(&format!("{point}:{hit}")));
            }
            let what = format!("{mode:?} {} KiB, {point}:{hit}", cs >> 10);
            match end {
                ChildEnd::Killed => {
                    tally.kills += 1;
                    killed.entry(point).or_default().insert(hit);
                }
                ChildEnd::Exited(_) => eprintln!("{what}: not reached in this run"),
            }
            verify(&cfg, &p, &what, &mut tally);
            drop(p);
            std::fs::remove_dir_all(&dir).expect("cleanup");
        }
    }
    eprintln!(
        "kill loop {mode:?} {} KiB: {} points, {} reached, {runs} runs, {} kills in {:.0?}; \
         killed at {killed:?}; unreached {unreached:?}; {tally:?}",
        cs >> 10,
        points.len(),
        points.len() - unreached.len(),
        tally.kills,
        started.elapsed()
    );
    for (point, hits) in &killed {
        let c = counts[*point];
        let want = if all_hits { c.min(3) } else { 1 };
        // The recheck-only point's hit count moves with the timing (2 to 5):
        // a run that finds fewer hits is retried at its own last, so the
        // first, a middle and the last of a count that varies collapse. The
        // delete points keep the full requirement.
        let want = if fast && point.starts_with("gc.recheck.") {
            want.min(2)
        } else {
            want
        };
        assert!(
            hits.len() as u64 >= want.min(c),
            "{point}: killed at {hits:?} of {c} hits"
        );
    }
    let missing: Vec<&&str> = points
        .iter()
        .filter(|p| counts.get(**p).copied().unwrap_or(0) > 0 && !killed.contains_key(**p))
        .collect();
    assert!(missing.is_empty(), "reached but never killed: {missing:?}");
    if fast {
        assert!(
            reached.contains_key("gc.recheck.before_delete"),
            "the recheck-only pass was never run: {reached:?}"
        );
    }
    if only.is_empty() {
        let unexpected: Vec<&&str> = unreached
            .iter()
            .filter(|p| !EXPECTED_UNREACHED.contains(p))
            .collect();
        assert!(
            unexpected.is_empty(),
            "registered crash points the workload no longer reaches: {unexpected:?}"
        );
    }
}

#[test]
fn gc_kill_loop_plain_16k() {
    kill_loop(CryptoMode::Plain, 16 << 10);
}

#[test]
fn gc_kill_loop_plain_64k() {
    kill_loop(CryptoMode::Plain, 64 << 10);
}

#[test]
fn gc_kill_loop_integrity_16k() {
    kill_loop(CryptoMode::Integrity, 16 << 10);
}

#[test]
fn gc_kill_loop_integrity_64k() {
    kill_loop(CryptoMode::Integrity, 64 << 10);
}

#[test]
fn gc_kill_loop_encrypted_16k() {
    kill_loop(CryptoMode::Encrypted, 16 << 10);
}

#[test]
fn gc_kill_loop_encrypted_64k() {
    kill_loop(CryptoMode::Encrypted, 64 << 10);
}

/// The recheck-only arm: `GcConfig::fast_recheck` with
/// `fast_recheck_always`, killed at `gc.recheck.before_delete` and the
/// delete points (milestone fastrecheck).
#[test]
fn gc_kill_loop_recheck_plain_16k() {
    kill_loop_with(CryptoMode::Plain, 16 << 10, true);
}

#[test]
fn gc_kill_loop_recheck_plain_64k() {
    kill_loop_with(CryptoMode::Plain, 64 << 10, true);
}

#[test]
fn gc_kill_loop_recheck_integrity_16k() {
    kill_loop_with(CryptoMode::Integrity, 16 << 10, true);
}
