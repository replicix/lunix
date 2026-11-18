//! Crash loops through the real crates (plan M12, gate): every `engine.*`,
//! `store.*`, `journal.*`, `index.*`, `destage.*` and `gc.*` crash point a
//! workload reaches is hit by a child process that `SIGKILL`s itself there
//! (`LUNIX_CRASH_AT=<point>:<hit>`), at the first hit, a middle hit and the
//! last hit of the workload. After each kill:
//!
//! * `lunix-fsck` on the killed volume reports no error (leftovers and
//!   unreferenced containers are warnings);
//! * the volume opens with `on_journal_damage = refuse` (a kill cannot
//!   damage a durable record), and the device equals the oracle after some
//!   prefix of the workload's requests at least as long as the durable one
//!   the child recorded (completed FLUSH, FUA, commit cut, grow);
//! * after a clean close the store self-check is clean and `lunix-fsck`
//!   reports no error.
//!
//! The workload (a child of this test binary, deterministic from its seed):
//! a **prep** child writes, flushes and commits, and is killed in that
//! commit after its first segment is sealed (an unnamed segment above the
//! high-water mark, a journal to replay); a leftover `*.tmp` is planted; the
//! **run** child then opens it (recovery: replay, sweep, floor), writes
//! (mixed sizes up to 1 MiB, compressible and repeated data, FUA, discards,
//! write-zeroes) over two queues with flushes, idle pauses (journal marks),
//! commits and ticks, grows once, drops and reopens once in the middle, and
//! closes. Enough data passes through the journal to reclaim ring files.
//!
//! Needs the `testsupport` and `crashpoints` features and a built
//! `lunix-fsck`:
//!
//! ```text
//! cargo build -p lunix-fsck
//! LUNIX_KILL_HITS=all cargo test -p lunixd --features testsupport,crashpoints \
//!   --test stack_kill -- --nocapture
//! ```
//!
//! `LUNIX_KILL_HITS` is `all` (first, middle and last hit: the gate) or
//! `first` (the default, one kill per point); `LUNIX_KILL_MIB` (default 160)
//! is the data the run child writes; `LUNIX_KILL_POINTS` (comma separated
//! prefixes) narrows the points.

#![cfg(all(feature = "testsupport", feature = "crashpoints"))]

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write as _;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use std::sync::Arc;

use lunix_engine::{CommitOutcome, EngineConfig, OnJournalDamage};
use lunix_format::{CryptoMode, WriteSeq};
use lunix_gc::GcConfig;
use lunix_iface::block::BlockEngine;
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
/// `gc.*` (and the store and index points only GC reaches) are killed by
/// `stack_gc_kill.rs`, whose workload gives the collector whole passes.
const PREFIXES: [&str; 5] = ["engine.", "store.", "journal.", "index.", "destage."];
/// Points of the prefixes above that only GC reaches (`stack_gc_kill.rs`).
/// Unlinks happen here only when GC deletes, at wall-clock times: their
/// counts are not reproducible from run to run (re-review M-5.4).
const GC_ONLY: [&str; 4] = [
    "index.after_condemn_flag",
    "store.evidence.",
    "store.delete.",
    "store.segment_delete.",
];
const CHILD_ENV: &str = "LUNIX_KILL_CHILD";

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
    let total = env_or("LUNIX_KILL_MIB", 160) << 20;
    let rounds = 12u64;
    let per_round = total / rounds;
    let mut run = Vec::new();
    let mut size = SIZE0;
    for r in 0..rounds {
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
            "voldir={}\nmode={mode}\ncs={}\nkey_file={key}\nseed={}\nphase={}\nprogress={}\n",
            self.voldir.display(),
            self.cs,
            self.seed,
            self.phase,
            self.progress.display()
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
        }
    }
}

/// GC runs through the workload (a pass every second, the smallest grace),
/// so kills at engine, index and store points land while a pass is in flight:
/// relocations in the memtable, pending moves, an unnamed output, condemned
/// flags set, a delete half done (M17.B stack review S-4).
fn gc_tune(c: &mut EngineConfig) {
    if let Some(p) = &mut c.gc_params {
        p.read_timeout = Duration::ZERO;
        p.grace = Duration::from_secs(10);
    }
}

fn fast_gc(o: &mut lunixd::stack::OpenParams) {
    o.gc = Some(GcConfig {
        pass_interval: Duration::from_secs(1),
        ..GcConfig::default()
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
fn execute(vol: &TestVolume, steps: &[Step], base: usize, prog: &mut std::fs::File) {
    let mut stack = vol.open_with(fast_gc).expect("child open");
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
            Step::Reopen => {
                drop(d);
                drop(stack);
                stack = vol.open_with(fast_gc).expect("child reopen");
                d = Driver::new(&stack).expect("driver");
                // Every request so far was recovered and synced.
                bounds.fill(0);
                progress(prog, n, &d);
            }
        }
    }
    progress(prog, n, &d);
    drop(d);
    close(stack).expect("child close");
    writeln!(prog, "{n} 0").expect("progress");
}

/// The child entry: does nothing unless this process was started by a kill
/// test with a spec.
#[test]
fn kill_child() {
    let Some(spec_path) = std::env::var_os(CHILD_ENV) else {
        return;
    };
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
        "prep" => execute(&vol, &p.prep, 0, &mut prog),
        _ => execute(&vol, &p.run, p.prep.iter().filter(is_op).count(), &mut prog),
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
    cmd.args(["--exact", "kill_child", "--nocapture", "--test-threads=1"])
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
        if start.elapsed() > Duration::from_secs(900) {
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

struct Config {
    mode: CryptoMode,
    cs: u32,
    seed: u64,
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
    };
    match spawn_child(&spec, dir, Some(PREP_KILL)) {
        ChildEnd::Killed => {}
        ChildEnd::Exited(_) => panic!("the prep child was not killed at {PREP_KILL}"),
    }
    std::fs::write(vol.voldir.join("segments/m12b-planted.tmp"), [0x5au8; 4096])
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
///   unreferenced containers, an index behind the chain, journal residue).
/// * At rest (after recovery and a clean close): notes only (exit 0).
const ALLOWED_AFTER_KILL: &[&str] = &["leftover", "crash_leftover", "orphan", "index_missing"];
const ALLOWED_AT_REST: &[&str] = &[];

fn fsck_clean(vol: &TestVolume, when: &str, what: &str, allowed: &[&str], tally: &mut Tally) {
    let f = fsck(vol).unwrap_or_else(|| panic!("{what}: lunix-fsck is not built"));
    assert!(
        f.no_errors(),
        "{what}: lunix-fsck {when} found errors:\n{}",
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
    fsck_clean(&p.vol, "after-kill", what, ALLOWED_AFTER_KILL, tally);
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
            fast_gc(o);
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
    drop(d);
    close(stack).unwrap_or_else(|e| panic!("{what}: close: {e}"));
    let stack = p
        .vol
        .open_with(|o| {
            fast_gc(o);
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

/// Registered points no workload of this loop reaches: the set-aside of a
/// tip (`fsck --truncate-tip` only). GC's own points are `stack_gc_kill.rs`'s.
/// Any other unreached point fails the loop.
const EXPECTED_UNREACHED: &[&str] = &[
    "store.set_aside.after_dirsync",
    "store.set_aside.after_rename",
    "store.set_aside.before_rename",
];

fn kill_loop(mode: CryptoMode, cs: u32) {
    // fsck is part of every recovery check: a missing binary fails the loop.
    assert!(
        fsck_binary().is_some(),
        "lunix-fsck is not built: cargo build --release -p lunix-fsck"
    );
    let all_hits = std::env::var("LUNIX_KILL_HITS").is_ok_and(|v| v == "all");
    let only: Vec<String> = std::env::var("LUNIX_KILL_POINTS")
        .map(|v| v.split(',').map(str::to_owned).collect())
        .unwrap_or_default();
    let cfg = Config {
        mode,
        cs,
        seed: 0x6b11_0000 ^ u64::from(cs) ^ ((mode as u64) << 40),
    };
    let tag = format!("m12b-kill-{mode:?}-{}k", cs >> 10).to_lowercase();
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
        .filter(|n| !GC_ONLY.iter().any(|p| n.starts_with(p)))
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
fn kill_loop_plain_16k() {
    kill_loop(CryptoMode::Plain, 16 << 10);
}

#[test]
fn kill_loop_plain_64k() {
    kill_loop(CryptoMode::Plain, 64 << 10);
}

#[test]
fn kill_loop_integrity_16k() {
    kill_loop(CryptoMode::Integrity, 16 << 10);
}

#[test]
fn kill_loop_integrity_64k() {
    kill_loop(CryptoMode::Integrity, 64 << 10);
}

#[test]
fn kill_loop_encrypted_16k() {
    kill_loop(CryptoMode::Encrypted, 16 << 10);
}

#[test]
fn kill_loop_encrypted_64k() {
    kill_loop(CryptoMode::Encrypted, 64 << 10);
}
