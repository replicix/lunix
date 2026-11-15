//! The M11 model test: random sequences of writes, reads, flushes, FUA
//! writes, discards, write-zeroes, forced cuts, timer ticks, grows and
//! crashes (plain, at every engine crash point, and inside recovery) against
//! the flat device oracle.
//!
//! * While running, every read equals the oracle.
//! * After a crash and reopen the device equals the oracle as of some prefix
//!   of the writes in `write_seq` order (the log is kept in `write_seq` order:
//!   entries are recorded when the engine reports the sequence number, which
//!   it does at `try_reserve`), and that prefix contains every write a
//!   completed FLUSH or FUA covered and everything a durable commit covered.
//! * Every engine crash point is reached by at least 5 workloads.
//!
//! `LUNIX_MODEL_CASES` (default 100, so `cargo test --workspace` stays
//! near two minutes) and `LUNIX_MODEL_OPS` (default 2000) set the size; the
//! plan's gate is run with `LUNIX_MODEL_CASES=500` (500 x 2000 per chunk size).

mod common;

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;
use std::time::Duration;

use common::*;
use lunix_engine::{CommitOutcome, CRASH_POINTS};
use lunix_iface::block::{BlockEngine, IoOp, Tag};
use lunix_testkit::strategies::IoSpec;
use lunix_testkit::{DeviceOracle, JournalMode, LogOp, OracleLog};
use proptest::prelude::*;
use proptest::test_runner::{Config, TestCaseError, TestRunner};

fn env_or(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

type Raw = (u8, u64, u64, u64);

struct Model {
    vol: Vol,
    run: Option<Running>,
    cs: u64,
    max_chunks: u64,
    /// Every logged op since creation, in `write_seq` order.
    ops: Vec<LogOp>,
    log: OracleLog,
    cur: DeviceOracle,
    min_n: usize,
    initial: u64,
    crashed: Vec<&'static str>,
}

fn fail(msg: String) -> TestCaseError {
    TestCaseError::fail(msg)
}

impl Model {
    fn new(cs: u32, seed: u64) -> Self {
        let chunks = 8;
        let mut vol = Vol::create(cs, chunks, 2, JournalMode::Mixed(seed));
        // Half the workloads run with journal marks: write_seq numbers the
        // engine never issued, interleaved with its own.
        vol.marks = seed % 2 == 1;
        let run = vol.open().map_err(|e| e.0).expect("open");
        let size = chunks * u64::from(cs);
        Self {
            vol,
            run: Some(run),
            cs: u64::from(cs),
            max_chunks: 24,
            ops: Vec::new(),
            log: OracleLog::new(size),
            cur: DeviceOracle::new(size),
            min_n: 0,
            initial: size,
            crashed: Vec::new(),
        }
    }

    fn r(&mut self) -> &mut Running {
        self.run.as_mut().expect("running")
    }

    fn size(&self) -> u64 {
        self.cur.size_bytes()
    }

    fn record(&mut self, io: &IoSpec) -> usize {
        let size = self.size();
        assert!(self.log.record(io, size), "an accepted request is valid");
        self.ops.push(LogOp::Mutate(*io));
        self.cur.apply(io);
        self.log.len()
    }

    /// Moves the sequenced requests into the log; returns tag -> log length.
    fn drain_seqs(
        &mut self,
        specs: &HashMap<Tag, IoSpec>,
        at: &mut HashMap<Tag, usize>,
    ) -> Result<(), TestCaseError> {
        let seqs = self.vol.hooks.take_seqs();
        let mut last = 0;
        for (_, tag, seq) in seqs {
            if seq.0 <= last {
                return Err(fail(format!(
                    "write_seq not increasing: {seq:?} after {last}"
                )));
            }
            last = seq.0;
            let io = *specs
                .get(&tag)
                .ok_or_else(|| fail(format!("sequenced unknown tag {tag:?}")))?;
            let n = self.record(&io);
            at.insert(tag, n);
        }
        Ok(())
    }

    fn spec(&self, (k, a, b, seed): Raw) -> IoSpec {
        let blocks = self.size() / BLOCK;
        let cb = self.cs / BLOCK;
        let op = match k % 10 {
            0..=3 => IoOp::Write,
            4 => IoOp::Write,
            5 | 6 => IoOp::Flush,
            7 => IoOp::Discard,
            _ => IoOp::WriteZeroes,
        };
        let fua = k % 10 == 4;
        let len = match a % 6 {
            0 => 1,
            1 => cb,
            2 => cb + 1,
            3 => cb.saturating_sub(1).max(1),
            4 => 2 * cb + 1,
            _ => 1 + b % (2 * cb),
        }
        .min(blocks);
        let start = match b % 3 {
            0 => (a / 7 % (blocks / cb)) * cb,
            _ => a / 3 % (blocks - len + 1),
        }
        .min(blocks - len);
        IoSpec {
            op,
            fua,
            offset_bytes: if op == IoOp::Flush { 0 } else { start * BLOCK },
            len: if op == IoOp::Flush {
                0
            } else {
                u32::try_from(len * BLOCK).expect("len")
            },
            seed,
        }
    }

    fn batch(&mut self, raws: &[Raw]) -> Result<(), TestCaseError> {
        let start_len = self.log.len();
        let mut specs = HashMap::new();
        let mut tags = Vec::new();
        let mut at = HashMap::new();
        for (i, raw) in raws.iter().enumerate() {
            let io = self.spec(*raw);
            let data = if io.op == IoOp::Write {
                io.payload()
            } else {
                Vec::new()
            };
            let tag = self.r().tag();
            specs.insert(tag, io);
            self.r()
                .submit_as(tag, i, io.op, io.fua, io.offset_bytes, io.len, &data);
            tags.push((tag, io));
            self.drain_seqs(&specs, &mut at)?;
        }
        let only: Vec<Tag> = tags.iter().map(|t| t.0).collect();
        let statuses = self.r().wait(&only);
        self.drain_seqs(&specs, &mut at)?;
        for ((tag, io), (st, _)) in tags.iter().zip(statuses) {
            if !st.is_ok() {
                return Err(fail(format!("{io:?} failed: {st:?}")));
            }
            if io.op == IoOp::Flush {
                self.min_n = self.min_n.max(start_len);
            }
            if io.op == IoOp::Write && io.fua {
                let n = *at
                    .get(tag)
                    .ok_or_else(|| fail("fua write not sequenced".into()))?;
                self.min_n = self.min_n.max(n);
            }
        }
        Ok(())
    }

    fn read(&mut self, (_, a, b, _): Raw) -> Result<(), TestCaseError> {
        let blocks = self.size() / BLOCK;
        let len = (1 + a % 40).min(blocks);
        let start = b % (blocks - len + 1);
        let want = self.cur.read(start * BLOCK, len * BLOCK);
        let (st, got) = self
            .r()
            .read((a % 2) as usize, start * BLOCK, (len * BLOCK) as u32);
        if !st.is_ok() || got != want {
            return Err(fail(format!(
                "read of {len} blocks at block {start} differs from the oracle ({st:?})"
            )));
        }
        Ok(())
    }

    fn committed(&mut self, out: CommitOutcome) {
        if let CommitOutcome::Committed { .. } = out {
            self.min_n = self.log.len();
        }
    }

    fn reopen(&mut self, seed: u64) -> Result<(), TestCaseError> {
        if let Some(r) = self.run.take() {
            let p = r.stop();
            self.vol.absorb(p, seed);
        }
        let mut r = match self.vol.open() {
            Ok(r) => r,
            Err((e, _)) => return Err(fail(format!("reopen failed: {e}"))),
        };
        let actual = r.read_all();
        let Some(n) = self.log.first_match(&actual, self.min_n) else {
            return Err(fail(format!(
                "after a crash the device is no prefix >= {} of {} logged writes",
                self.min_n,
                self.log.len()
            )));
        };
        self.ops.truncate(n);
        self.log = OracleLog::new(self.initial);
        let mut cur = DeviceOracle::new(self.initial);
        for op in &self.ops {
            match op {
                LogOp::Mutate(io) => {
                    self.log.record(io, cur.size_bytes());
                    cur.apply(io);
                }
                LogOp::Grow(s) => {
                    self.log.record_grow(*s);
                    cur.grow(*s);
                }
            }
        }
        self.cur = cur;
        self.min_n = n;
        if r.engine.size_bytes() != self.size() {
            return Err(fail("size after reopen differs".into()));
        }
        self.run = Some(r);
        Ok(())
    }

    fn crash_in_commit(&mut self, point: &'static str, seed: u64) -> Result<(), TestCaseError> {
        self.vol.hooks.arm(point, 1);
        let res = self.r().engine.commit_now(true);
        self.vol.hooks.disarm();
        if res.is_ok() || self.vol.hooks.crashed() != Some(point) {
            return Err(fail(format!("commit did not stop at {point}: {res:?}")));
        }
        self.crashed.push(point);
        // From C6 on the commit is durable and covers every logged write.
        let durable = ["engine.c6", "engine.c7", "engine.c8", "engine.c9"];
        if durable.iter().any(|p| point.starts_with(p)) {
            self.min_n = self.log.len();
        }
        self.reopen(seed)
    }

    fn crash_in_recovery(&mut self, point: &'static str, seed: u64) -> Result<(), TestCaseError> {
        if let Some(r) = self.run.take() {
            let p = r.stop();
            self.vol.absorb(p, seed);
        }
        self.vol.hooks.arm(point, 1);
        match self.vol.open() {
            Ok(r) => {
                self.vol.hooks.disarm();
                self.run = Some(r);
                return Err(fail(format!("open did not stop at {point}")));
            }
            Err((_, p)) => {
                self.vol.hooks.disarm();
                if self.vol.hooks.crashed() != Some(point) {
                    return Err(fail(format!("open failed before {point}")));
                }
                self.crashed.push(point);
                self.vol.absorb(p, seed ^ 0x55);
            }
        }
        self.reopen(seed)
    }

    fn step(&mut self, raw: Raw, next: &[Raw]) -> Result<usize, TestCaseError> {
        let (k, a, b, seed) = raw;
        match k % 100 {
            0..=54 => {
                let n = 1 + (a % 4) as usize;
                let mut batch = vec![raw];
                batch.extend(next.iter().take(n - 1).copied());
                self.batch(&batch)?;
                return Ok(batch.len());
            }
            55..=79 => self.read(raw)?,
            80..=83 => {
                let out = self
                    .r()
                    .engine
                    .commit_now(true)
                    .map_err(|e| fail(format!("commit: {e}")))?;
                self.committed(out);
            }
            84..=88 => {
                self.vol.clock.advance(Duration::from_secs(b % 40));
                let out = self
                    .r()
                    .engine
                    .tick()
                    .map_err(|e| fail(format!("tick: {e}")))?;
                self.committed(out);
            }
            89..=90 => {
                let chunks = self.size() / self.cs;
                if chunks < self.max_chunks {
                    let new = (chunks + 1 + a % 8).min(self.max_chunks) * self.cs;
                    self.r()
                        .engine
                        .grow(new)
                        .map_err(|e| fail(format!("grow: {e}")))?;
                    self.log.record_grow(new);
                    self.ops.push(LogOp::Grow(new));
                    self.cur.grow(new);
                    self.min_n = self.log.len();
                }
            }
            91..=94 => self.reopen(seed)?,
            95..=97 => {
                let commit_points: Vec<&'static str> = CRASH_POINTS
                    .iter()
                    .copied()
                    .filter(|p| !p.starts_with("engine.recover"))
                    .collect();
                let p = commit_points[(a % commit_points.len() as u64) as usize];
                self.crash_in_commit(p, seed)?;
            }
            _ => {
                let rec: Vec<&'static str> = CRASH_POINTS
                    .iter()
                    .copied()
                    .filter(|p| p.starts_with("engine.recover"))
                    .collect();
                let p = rec[(a % rec.len() as u64) as usize];
                self.crash_in_recovery(p, seed)?;
            }
        }
        // The fakes' tick: the triggers (journal fill above all).
        let out = self
            .r()
            .engine
            .tick()
            .map_err(|e| fail(format!("tick: {e}")))?;
        self.committed(out);
        Ok(1)
    }
}

fn run_model(cs: u32) {
    let cases = env_or("LUNIX_MODEL_CASES", 100);
    let ops = env_or("LUNIX_MODEL_OPS", 2000);
    let mut runner = TestRunner::new(Config {
        cases: u32::try_from(cases).expect("cases"),
        failure_persistence: None,
        max_shrink_iters: 64,
        ..Config::default()
    });
    let strat = (
        any::<u64>(),
        proptest::collection::vec((any::<u8>(), any::<u64>(), any::<u64>(), any::<u64>()), ops),
    );
    let coverage: Mutex<BTreeMap<&'static str, u64>> = Mutex::new(BTreeMap::new());
    runner
        .run(&strat, |(seed, raws)| {
            let mut m = Model::new(cs, seed);
            let mut i = 0;
            while i < raws.len() {
                i += m.step(raws[i], &raws[i + 1..])?;
            }
            m.reopen(seed ^ 1)?;
            let v = m.vol.violations();
            if !v.is_empty() {
                return Err(fail(format!("durable-order violations: {v:?}")));
            }
            let mut cov = coverage.lock().unwrap_or_else(|p| p.into_inner());
            let mut seen = m.crashed.clone();
            seen.sort_unstable();
            seen.dedup();
            for p in seen {
                *cov.entry(p).or_insert(0) += 1;
            }
            Ok(())
        })
        .unwrap_or_else(|e| panic!("model test (chunk size {cs}): {e}"));
    let cov = coverage.into_inner().unwrap_or_else(|p| p.into_inner());
    let need = if cases >= 100 {
        5
    } else {
        u64::from(cases >= 50)
    };
    for p in CRASH_POINTS {
        let n = cov.get(p).copied().unwrap_or(0);
        assert!(
            n >= need,
            "crash point {p} reached by {n} workloads (need {need}): {cov:?}"
        );
    }
    eprintln!("chunk size {cs}: {cases} cases x {ops} ops; crash point coverage {cov:?}");
}

#[test]
fn model_16k() {
    run_model(16 << 10);
}

#[test]
fn model_64k() {
    run_model(64 << 10);
}

/// Deterministic case (review major 5): writes still in flight when a cut
/// is taken (`Manual` completions), and reads inside the C2-C9 window. With
/// C1's wait removed the commit claims a cut that misses the in-flight
/// writes and the reopen loses them.
fn in_flight_at_a_cut(cs: u32) {
    use lunix_iface::journal::LaneId;
    let csz = u64::from(cs);
    let mut v = Vol::create(cs, 8, 2, JournalMode::Manual);
    let mut r = v.open().map_err(|e| e.0).expect("open");
    // A committed base.
    let base = pattern(1, cs as usize);
    let t = r.submit(0, IoOp::Write, false, 0, cs, &base);
    while !r.is_done(t) {
        r.journal.complete_appends(LaneId(0), 64);
        r.pump();
    }
    r.engine.commit_now(false).expect("commit");
    // Three writes whose appends stay in flight.
    let a = pattern(2, 8192);
    let b = pattern(3, 4096);
    let tags = [
        r.submit(0, IoOp::Write, false, 4096, 8192, &a),
        r.submit(1, IoOp::Write, false, csz + 4096, 4096, &b),
        r.submit(0, IoOp::Write, false, 2 * csz, 4096, &b),
    ];
    assert!(tags.iter().all(|t| !r.is_done(*t)), "appends are held");
    let Running {
        engine,
        queues,
        done,
        journal,
        ..
    } = &mut r;
    let outcome = std::thread::scope(|s| {
        let h = s.spawn(|| engine.commit_now(false));
        std::thread::sleep(Duration::from_millis(100));
        assert!(!h.is_finished(), "C1 waits for the in-flight writes");
        journal.complete_appends(LaneId(0), 64);
        journal.complete_appends(LaneId(1), 64);
        while !h.is_finished() {
            for q in queues.iter_mut() {
                q.poll(done);
            }
            std::thread::yield_now();
        }
        h.join().expect("commit thread")
    });
    let committed = outcome.expect("commit");
    for t in tags {
        assert!(r.wait(&[t])[0].0.is_ok());
    }
    let mut want = base.clone();
    want[4096..12288].copy_from_slice(&a);
    match committed {
        CommitOutcome::Committed { cut_seq, .. } => assert!(cut_seq.0 >= 4, "{cut_seq:?}"),
        CommitOutcome::Idle => panic!("idle"),
    }
    // A read inside the C2-C9 window: a partial overwrite of chunk 0, the
    // commit stopped after the destage, at the map pack and after C6.
    for point in [
        "engine.c3.destaged",
        "engine.c5.map_sealed",
        "engine.c6.committed",
    ] {
        let c = pattern(10, 4096);
        let t = r.submit(1, IoOp::Write, false, 3 * 4096, 4096, &c);
        while !r.is_done(t) {
            r.journal.complete_appends(LaneId(1), 64);
            r.pump();
        }
        assert!(r.wait(&[t])[0].0.is_ok());
        want[3 * 4096..4 * 4096].copy_from_slice(&c);
        v.hooks.arm(point, 1);
        assert!(r.engine.commit_now(false).is_err(), "{point}");
        let t = r.submit(0, IoOp::Read, false, 0, cs, &[]);
        let (st, got) = r.wait(&[t]).remove(0);
        assert!(st.is_ok(), "{point}: {st:?}");
        assert_eq!(got, want, "read inside the window at {point}");
        // The engine stopped: reopen with nothing unflushed surviving.
        let p = r.stop();
        v.store = p.store.crash(3);
        v.index = p.index.image();
        v.journal = p
            .journal
            .crash_with(3, lunix_testkit::CrashModel::lose_all());
        r = v.open().map_err(|e| e.0).expect("reopen");
        // The committed cut held the formerly in-flight writes; the last
        // overwrite was never flushed, so it may be gone (C6 keeps it).
        let t = r.submit(0, IoOp::Read, false, 0, cs, &[]);
        let (_, got) = r.wait(&[t]).remove(0);
        assert_eq!(
            got[4096..12288],
            a[..],
            "{point}: the in-flight writes are in the commit"
        );
        let t = r.submit(0, IoOp::Read, false, csz + 4096, 4096, &[]);
        assert_eq!(r.wait(&[t]).remove(0).1, b, "{point}");
        if point == "engine.c6.committed" {
            assert_eq!(got, want, "C6 made the overwrite durable");
        }
        want = got;
    }
}

#[test]
fn in_flight_writes_at_a_cut_16k() {
    in_flight_at_a_cut(16 << 10);
}

#[test]
fn in_flight_writes_at_a_cut_64k() {
    in_flight_at_a_cut(64 << 10);
}
