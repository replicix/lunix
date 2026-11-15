//! The stack model test (plan M12, gate): the M11 model test again, with the
//! real journal, overlay, map, index, store, space, destage and crypto
//! crates wired by `lunixd::stack`, driven through the engine's queue
//! handlers by `lunixd::testsupport` (no root, no device).
//!
//! Random sequences of writes (incompressible, half and highly compressible,
//! repeated chunks for dedup), FUA writes, discards, write-zeroes, flushes,
//! reads, forced and plain commits, ticks, grows, clean reopens and crashes,
//! against the flat-device oracle:
//!
//! * While running, every read equals the oracle.
//! * A crash drops the engine, damages every journal lane past its last
//!   flush point at byte level and deletes, truncates or keeps every file
//!   that was never synced (`TestVolume::crash_driven`). After the reopen the
//!   device equals the oracle after some prefix of the writes in `write_seq`
//!   order (the driver submits one request at a time), and that prefix holds
//!   every write a completed FLUSH or FUA write covered and everything a
//!   durable commit or grow covered.
//! * Half the sequences open with `on_journal_damage = refuse`: the damage
//!   lies above the flushed watermark, so it is crash residue and the open
//!   must succeed (ADR-4). One refusal is tolerated and counted
//!   (`e1_refusals`): damage at or below the floor that a later flush's
//!   watermark passed (defect E1 in `lunix-engine`, the M12b report and
//!   `stack_support::refuse_ignores_torn_records_below_the_floor`); the
//!   sequence then goes on with a `wipe` open. Any other refusal fails.
//! * A clean reopen gives back the whole oracle; the store self-check is
//!   clean at the end of every sequence, and `lunix-fsck` reports no error
//!   when its binary is built (`LUNIX_FSCK` or the target directory;
//!   `LUNIX_STACK_REQUIRE_FSCK=1` makes a missing binary a failure).
//!
//! Runs only with the `testsupport` feature:
//!
//! ```text
//! cargo build --release -p lunix-fsck
//! LUNIX_STACK_CASES=100 LUNIX_STACK_OPS=2000 cargo test --release -p lunixd \
//!   --features testsupport --test stack_model -- --nocapture --test-threads=2
//! ```
//!
//! `LUNIX_STACK_CASES` (default 3) and `LUNIX_STACK_OPS` (default 300) set
//! the size; the plan's gate is 100 x 2000 per chunk size and mode.
//! `LUNIX_STACK_SEED=<hex>` reruns one sequence (its seed is in every failure
//! message).

#![cfg(feature = "testsupport")]

use std::collections::BTreeMap;
use std::path::Path;

use lunix_engine::{CommitOutcome, OnJournalDamage};
use lunix_format::CryptoMode;
use lunix_iface::block::BlockEngine;
use lunix_iface::store::VolumeDir;
use lunixd::stack::{StackError, VolumeStack};
use lunixd::testsupport::{
    close, fsck, CrashDamage, DeviceModel, Driver, ModelOp, Scratch, TestRng, TestVolume,
    VolumeBuilder,
};

const BLOCK: u64 = 4096;

fn env_or(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

#[derive(Default, Debug)]
struct Stats {
    ops: u64,
    writes: u64,
    commits: u64,
    crashes: u64,
    plain_drops: u64,
    reopens: u64,
    grows: u64,
    lost_ops: u64,
    damaged_records: u64,
    kept_records: u64,
    files_removed: u64,
    replayed: u64,
    dedup_hits: u64,
    stored_ulen: u64,
    stored_clen: u64,
    fsck_runs: u64,
    e1_refusals: u64,
    fsck_warnings: BTreeMap<String, u64>,
    time: BTreeMap<&'static str, (u64, std::time::Duration)>,
}

struct Case {
    vol: TestVolume,
    run: Option<(VolumeStack, Driver)>,
    model: DeviceModel,
    rng: TestRng,
    seed: u64,
    step: usize,
    cs: u64,
    max_size: u64,
    stats: Stats,
    last_crash: Option<CrashDamage>,
    run_report: String,
    /// Per logged request, an upper bound of its `write_seq` (what the
    /// journal had issued right after it completed): a commit whose cut is
    /// at or above it covers the request.
    bounds: Vec<u64>,
}

impl Case {
    fn ctx(&self) -> String {
        format!(
            "[{:?} cs {} seed {:#x} step {}]",
            self.vol.builder.mode, self.cs, self.seed, self.step
        )
    }

    fn new(parent: &Path, mode: CryptoMode, cs: u32, seed: u64) -> Self {
        let cs64 = u64::from(cs);
        let mut b = VolumeBuilder::new(parent)
            .manual()
            .mode(mode)
            .chunk_size(cs)
            .size_bytes(16 * cs64)
            .queues(2);
        if seed % 2 == 1 {
            b.on_journal_damage = OnJournalDamage::Refuse;
        }
        if mode == CryptoMode::Encrypted && seed % 4 == 1 {
            b = b.passphrase("stack model passphrase");
        }
        let vol = b.create().expect("create");
        let stack = vol.open().expect("first open");
        let d = Driver::new(&stack).expect("driver");
        Self {
            vol,
            run: Some((stack, d)),
            model: DeviceModel::new(16 * cs64),
            rng: TestRng::new(seed),
            seed,
            step: 0,
            cs: cs64,
            max_size: 48 * cs64,
            stats: Stats::default(),
            last_crash: None,
            run_report: String::new(),
            bounds: Vec::new(),
        }
    }

    fn stack(&self) -> &VolumeStack {
        &self.run.as_ref().expect("running").0
    }

    fn d(&mut self) -> &mut Driver {
        &mut self.run.as_mut().expect("running").1
    }

    fn q(&mut self) -> usize {
        self.rng.below(2) as usize
    }

    fn range(&mut self, max_blocks: u64) -> (u64, u32) {
        let blocks = self.model.size() / BLOCK;
        let cb = self.cs / BLOCK;
        let len = match self.rng.below(6) {
            0 => 1,
            1 => cb,
            2 => cb + 1,
            3 => cb.saturating_sub(1).max(1),
            4 => 2 * cb + 1,
            _ => 1 + self.rng.below(2 * cb),
        }
        .min(blocks)
        .min(max_blocks);
        let start = if self.rng.below(3) == 0 {
            (self.rng.below(blocks / cb) * cb).min(blocks - len)
        } else {
            self.rng.below(blocks - len + 1)
        };
        (start * BLOCK, u32::try_from(len * BLOCK).expect("len"))
    }

    fn write(&mut self) {
        let max = u64::from(self.d().max_io()) / BLOCK;
        let (off, len) = self.range(max);
        // Whole aligned chunks take their seed from a small pool (dedup);
        // the rest are fresh. Seed bits pick the compressibility.
        let aligned = off.is_multiple_of(self.cs) && u64::from(len).is_multiple_of(self.cs);
        let seed = if aligned && self.rng.below(2) == 0 {
            self.rng.below(12)
        } else {
            self.rng.next_u64()
        };
        let fua = self.rng.below(10) == 0;
        let q = self.q();
        let data = lunixd::testsupport::payload(seed, len as usize);
        let st = self.d().write(q, off, &data, fua);
        assert!(
            st.is_ok(),
            "{} write at {off} len {len}: {st:?}",
            self.ctx()
        );
        let n = self.model.apply(ModelOp::Write {
            offset: off,
            len,
            seed,
        });
        let b = self.d().issued().0;
        self.bounds.push(b);
        if fua {
            self.model.durable_upto(n);
        }
        self.stats.writes += 1;
    }

    fn zero(&mut self) {
        let (off, len) = self.range(4 * self.cs / BLOCK);
        let q = self.q();
        let st = if self.rng.below(2) == 0 {
            self.d().discard(q, off, len)
        } else {
            self.d().write_zeroes(q, off, len)
        };
        assert!(st.is_ok(), "{} zero at {off} len {len}: {st:?}", self.ctx());
        self.model.apply(ModelOp::Zero { offset: off, len });
        let b = self.d().issued().0;
        self.bounds.push(b);
    }

    fn read(&mut self) {
        let blocks = self.model.size() / BLOCK;
        let len = (1 + self.rng.below(64)).min(blocks);
        let start = self.rng.below(blocks - len + 1);
        let q = self.q();
        let got = self.d().read(q, start * BLOCK, (len * BLOCK) as u32);
        let want = self.model.read(start * BLOCK, len * BLOCK);
        match got {
            Ok(g) => assert!(
                g == want,
                "{} read of {len} blocks at block {start} differs from the oracle",
                self.ctx()
            ),
            Err(s) => panic!("{} read failed: {s:?}", self.ctx()),
        }
    }

    fn committed(&mut self, out: CommitOutcome) {
        if let CommitOutcome::Committed { cut_seq, .. } = out {
            if std::env::var_os("LUNIX_STACK_TRACE").is_some() {
                eprintln!("  committed {out:?}");
            }
            // The cut covers every request whose number is at or below it
            // (the engine may cut below `last_issued` to respect the map's
            // dirty-leaf cap).
            let n = self.bounds.partition_point(|b| *b <= cut_seq.0);
            self.model.durable_upto(n);
            self.stats.commits += 1;
        }
    }

    fn grow(&mut self) {
        let size = self.model.size();
        if size >= self.max_size {
            return;
        }
        let new = (size + self.cs * (1 + self.rng.below(8))).min(self.max_size);
        let r = self.stack().engine.grow(new);
        assert!(r.is_ok(), "{} grow to {new}: {r:?}", self.ctx());
        self.model.apply(ModelOp::Grow(new));
        // The grow's FULL commit covers everything before it.
        self.bounds.fill(0);
        self.bounds.push(0);
        self.model.all_durable();
        self.stats.grows += 1;
    }

    fn collect(&mut self, stack: &VolumeStack) {
        let st = stack.engine.stats();
        self.stats.dedup_hits += st.dedup_hits;
        self.stats.stored_ulen += st.stored_ulen;
        self.stats.stored_clen += st.stored_clen;
    }

    /// Opens after a stop and checks the device against the oracle: exactly
    /// when `exact`, otherwise as some prefix at least as long as the
    /// durable one.
    fn reopen(&mut self, exact: bool) {
        let t0 = std::time::Instant::now();
        let opened = self.vol.open();
        let e = self.stats.time.entry("x-open").or_default();
        e.0 += 1;
        e.1 += t0.elapsed();
        let stack = match opened {
            Ok(s) => s,
            Err(StackError::JournalDamage)
                if self.vol.builder.on_journal_damage == OnJournalDamage::Refuse =>
            {
                // Defect E1 (lunix-engine `refuse_worthy`, M12b report): a
                // damaged range whose `next_seq` is at or below the watermark
                // is refused even when recovery found every number above the
                // floor (no gap), so the range can only hold numbers at or
                // below the floor or numbers another lane holds: it costs
                // nothing. A refusal with a gap at or below the watermark is
                // a real one and fails the test.
                let s = self
                    .vol
                    .open_with(|p| p.on_journal_damage = OnJournalDamage::Wipe)
                    .unwrap_or_else(|e| panic!("{} reopen under wipe failed: {e}", self.ctx()));
                let rep = s.engine.recovery_report();
                let w = rep.flushed.0;
                let range_rule = rep
                    .damaged
                    .iter()
                    .any(|d| d.next_seq.is_some_and(|n| n.0 <= w));
                // With a gap, recovery stops at it: `next_seq` is the gap.
                let gap_rule = rep.ignored_after_gap.is_some() && rep.next_seq.0 <= w;
                let e1 = range_rule && !gap_rule;
                assert!(
                    e1,
                    "{} refused with a gap at or below the watermark: {rep:?}; last crash {:?}",
                    self.ctx(),
                    self.last_crash
                );
                self.stats.e1_refusals += 1;
                s
            }
            Err(e) => panic!(
                "{} reopen failed: {e}; last crash {:?}",
                self.ctx(),
                self.last_crash
            ),
        };
        self.stats.replayed += stack.engine.recovery_report().replayed as u64;
        self.run_report = format!("{:?}", stack.engine.recovery_report());
        assert_eq!(
            stack.engine.size_bytes(),
            self.model.size(),
            "{} size after reopen",
            self.ctx()
        );
        let mut d = Driver::new(&stack).expect("driver");
        let actual = match d.read_all(self.model.size()) {
            Ok(a) => a,
            Err(s) => panic!("{} full read after reopen failed: {s:?}", self.ctx()),
        };
        if exact {
            assert!(
                actual == self.model.bytes(),
                "{} the device differs from the oracle after a clean reopen",
                self.ctx()
            );
        } else {
            let Some(n) = self.model.first_match(&actual) else {
                let want = self.model.bytes();
                let bad: Vec<u64> = (0..want.len() / BLOCK as usize)
                    .filter(|&b| {
                        let r = b * BLOCK as usize..(b + 1) * BLOCK as usize;
                        actual.get(r.clone()) != want.get(r)
                    })
                    .map(|b| b as u64)
                    .take(16)
                    .collect();
                let tail: Vec<&ModelOp> = self.model.ops().iter().rev().take(6).collect();
                panic!(
                    "{} after a crash the device is no prefix >= {} of {} logged requests; \
                     last crash {:?}; report {:?}; blocks differing from the full oracle {bad:?}; \
                     last ops (newest first) {tail:?}",
                    self.ctx(),
                    self.model.durable(),
                    self.model.len(),
                    self.last_crash,
                    self.run_report,
                );
            };
            self.stats.lost_ops += (self.model.len() - n) as u64;
            self.model.truncate(n);
            // Everything kept is durable now; the bounds of the old numbers
            // must not compare with the new ones.
            self.bounds.truncate(n);
            self.bounds.fill(0);
        }
        self.run = Some((stack, d));
    }

    fn clean_reopen(&mut self) {
        let (stack, d) = self.run.take().expect("running");
        drop(d);
        self.collect(&stack);
        if let Err(e) = close(stack) {
            panic!("{} close: {e}", self.ctx());
        }
        self.model.all_durable();
        self.bounds.fill(0);
        self.stats.reopens += 1;
        self.reopen(true);
    }

    fn crash(&mut self, damage: bool) {
        let (stack, d) = self.run.take().expect("running");
        self.collect(&stack);
        if damage {
            let seed = self.rng.next_u64();
            let t0 = std::time::Instant::now();
            let c: CrashDamage = if std::env::var_os("LUNIX_STACK_NO_DAMAGE").is_some() {
                drop(d);
                drop(stack);
                CrashDamage::default()
            } else {
                self.vol.crash_driven(d, stack, seed)
            };
            let e = self.stats.time.entry("x-damage").or_default();
            e.0 += 1;
            e.1 += t0.elapsed();
            self.last_crash = Some(c);
            self.stats.crashes += 1;
            self.stats.damaged_records += c.records_damaged as u64;
            self.stats.kept_records += c.records_kept as u64;
            self.stats.files_removed += c.files_removed as u64;
        } else {
            drop(d);
            drop(stack);
            self.last_crash = None;
            self.stats.plain_drops += 1;
        }
        self.reopen(false);
    }

    fn step(&mut self) {
        self.stats.ops += 1;
        let k = self.rng.below(1000);
        if std::env::var_os("LUNIX_STACK_TRACE").is_some() {
            use lunix_iface::journal::Journal;
            let li = self.run.as_ref().map(|r| r.0.journal.last_issued().0);
            eprintln!(
                "step {} k {k} ops {} durable {} last_issued {li:?} tip {:?}",
                self.step,
                self.model.len(),
                self.model.durable(),
                self.run.as_ref().map(|r| r.0.engine.tip())
            );
        }
        let t0 = std::time::Instant::now();
        let kind = match k {
            0..=599 => "write",
            600..=679 => "flush",
            680..=879 => "read",
            880..=974 => "commit",
            975..=984 => "grow",
            985..=987 => "reopen",
            _ => "crash",
        };
        match k {
            0..=539 => self.write(),
            540..=599 => self.zero(),
            600..=679 => {
                let q = self.q();
                let st = self.d().flush(q);
                assert!(st.is_ok(), "{} flush: {st:?}", self.ctx());
                self.model.all_durable();
            }
            680..=879 => self.read(),
            880..=939 => {
                let force = self.rng.below(2) == 0;
                let out = self.stack().engine.commit_now(force);
                let out = out.unwrap_or_else(|e| panic!("{} commit: {e}", self.ctx()));
                self.committed(out);
            }
            940..=974 => {
                let out = self.stack().engine.tick();
                let out = out.unwrap_or_else(|e| panic!("{} tick: {e}", self.ctx()));
                self.committed(out);
            }
            975..=984 => self.grow(),
            985..=987 => self.clean_reopen(),
            988..=997 => self.crash(true),
            _ => self.crash(false),
        }
        let out = self.stack().engine.tick();
        let out = out.unwrap_or_else(|e| panic!("{} tick: {e}", self.ctx()));
        self.committed(out);
        let e = self.stats.time.entry(kind).or_default();
        e.0 += 1;
        e.1 += t0.elapsed();
    }

    fn finish(mut self) -> Stats {
        // A last crash, then a clean close: every sequence ends with a check
        // of the volume at rest.
        self.crash(true);
        self.clean_reopen();
        let (stack, d) = self.run.take().expect("running");
        drop(d);
        let floor = stack.store.rollback_floor().expect("floor");
        assert!(floor.is_some(), "{} no rollback floor", self.ctx());
        self.collect(&stack);
        close(stack).expect("close");
        let r = self.vol.check().expect("store self-check runs");
        assert!(r.is_clean(), "{} store self-check: {r:?}", self.ctx());
        if let Some(f) = fsck(&self.vol) {
            assert!(
                f.no_errors(),
                "{} lunix-fsck found errors:\n{}",
                self.ctx(),
                f.text
            );
            self.stats.fsck_runs += 1;
            for (sev, code) in f.findings {
                *self
                    .stats
                    .fsck_warnings
                    .entry(format!("{sev}:{code}"))
                    .or_insert(0) += 1;
            }
        }
        self.stats
    }
}

fn run_model(mode: CryptoMode, cs: u32) {
    if std::env::var_os("LUNIX_STACK_REQUIRE_FSCK").is_some() {
        assert!(
            lunixd::testsupport::fsck_binary().is_some(),
            "lunix-fsck is not built"
        );
    }
    let cases = env_or("LUNIX_STACK_CASES", 3);
    let ops = env_or("LUNIX_STACK_OPS", 300);
    let one = std::env::var("LUNIX_STACK_SEED")
        .ok()
        .and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok());
    let tag = format!("model-{mode:?}-{}k", cs >> 10).to_lowercase();
    let t = Scratch::new(&format!("m12b-{tag}"));
    let mode_id: u64 = match mode {
        CryptoMode::Plain => 1,
        CryptoMode::Integrity => 2,
        CryptoMode::Encrypted => 3,
    };
    let base = 0x5ac4_0000_0000_0000u64 ^ (u64::from(cs) << 24) ^ (mode_id << 44);
    let seeds: Vec<u64> = match one {
        Some(s) => vec![s],
        None => (0..cases)
            .map(|i| TestRng::new(base + i).next_u64())
            .collect(),
    };
    let mut total = Stats::default();
    let started = std::time::Instant::now();
    for (i, seed) in seeds.iter().enumerate() {
        let dir = t.path().join(format!("c{i}"));
        let mut c = Case::new(&dir, mode, cs, *seed);
        while c.stats.ops < ops {
            c.step += 1;
            c.step();
        }
        let s = c.finish();
        total.ops += s.ops;
        total.writes += s.writes;
        total.commits += s.commits;
        total.crashes += s.crashes;
        total.plain_drops += s.plain_drops;
        total.reopens += s.reopens;
        total.grows += s.grows;
        total.lost_ops += s.lost_ops;
        total.damaged_records += s.damaged_records;
        total.kept_records += s.kept_records;
        total.files_removed += s.files_removed;
        total.replayed += s.replayed;
        total.dedup_hits += s.dedup_hits;
        total.stored_ulen += s.stored_ulen;
        total.stored_clen += s.stored_clen;
        total.fsck_runs += s.fsck_runs;
        total.e1_refusals += s.e1_refusals;
        for (k, v) in s.fsck_warnings {
            *total.fsck_warnings.entry(k).or_insert(0) += v;
        }
        for (k, (n, d)) in s.time {
            let e = total.time.entry(k).or_default();
            e.0 += n;
            e.1 += d;
        }
        std::fs::remove_dir_all(&dir).expect("remove case dir");
    }
    eprintln!(
        "stack model {mode:?} chunk {} KiB: {} sequences x {ops} ops in {:.0?}: {total:?}",
        cs >> 10,
        seeds.len(),
        started.elapsed()
    );
    assert!(total.crashes > 0 && total.commits > 0, "{total:?}");
    if ops >= 1000 {
        assert!(
            total.damaged_records > 0,
            "no crash damaged a record: {total:?}"
        );
        assert!(total.dedup_hits > 0, "no dedup hit: {total:?}");
        assert!(
            total.stored_clen < total.stored_ulen,
            "nothing compressed: {total:?}"
        );
    }
}

#[test]
fn stack_model_plain_16k() {
    run_model(CryptoMode::Plain, 16 << 10);
}

#[test]
fn stack_model_plain_64k() {
    run_model(CryptoMode::Plain, 64 << 10);
}

#[test]
fn stack_model_integrity_16k() {
    run_model(CryptoMode::Integrity, 16 << 10);
}

#[test]
fn stack_model_integrity_64k() {
    run_model(CryptoMode::Integrity, 64 << 10);
}

#[test]
fn stack_model_encrypted_16k() {
    run_model(CryptoMode::Encrypted, 16 << 10);
}

#[test]
fn stack_model_encrypted_64k() {
    run_model(CryptoMode::Encrypted, 64 << 10);
}
