//! The reader-racing-delete stress of the short segment grace (milestone
//! pgrace M3; ADR-32 amendment, rule change F3), through the real stack:
//! the real space gate, engine, store, collector and crypto (the engine
//! testkit has no AEAD).
//!
//! A capped volume is held in `slowdown` by writers that overwrite a hot set
//! of stamped blocks far faster than the configured grace (60 s) lets the
//! collector free the garbage, with `gc_pressure_grace_ms = 2000`,
//! `gc_fast_recheck` and `gc_condemn_on_cut = on`: while the gate paces, a
//! condemned segment is deleted 2 s after its flag. Readers read random
//! ranges of the hot set all the time, and the store's read hook (`lunix_store::ReadHook`) holds about 5 % of
//! the guest read helpers' `read_extent` calls for 0 to 8 short graces
//! (16 s; a segment is deleted about 6 s after a read took its hint at the
//! earliest, so holds of the specified 0 to 3 graces never lost one), before
//! the read (the part keeps its hint while its segment is collected) or after
//! it (the part keeps the file open across the delete), so that read parts
//! outlive the grace.
//!
//! Checked on every cell (plain, integrity, encrypted; the default 16-byte id
//! prefix and the 32-byte one (a lean `id_len = 0` volume cannot be created
//! in v1: `create_volume` refuses it, so that path is covered by the engine
//! property test and the collector oracles); partial-chunk writers in one cell, so that the
//! destage base reads run against the same deletes):
//!
//! * every 4 KiB block a read returns is a version of that block that the
//!   write history allows for the request's interval: a write that overlaps
//!   it, or one that completed before it was submitted and was not replaced
//!   by a write submitted after it completed (never another block's bytes,
//!   never an older version, never zeros);
//! * no request fails; `read_revalidate_exhausted == 0`;
//!   `extent_auth_failures == 0` (no damage is injected, so a false tamper
//!   count is a bug);
//! * the short grace ran (`pgrace_deletes > 0`) and never early
//!   (`pgrace_min_wait_ms >= 2000`), reads hit it (`read_revalidated > 0`),
//!   and a hint that failed `NotFound` was held at least as long as the
//!   grace (`read_stale_age_min_ms >= 2000`);
//! * after the writers stop every block reads back at its last version, and
//!   `lunix-fsck --deep` is clean after the close.
//!
//! ```text
//! cargo build --release -p lunix-fsck
//! cargo test --release -p lunixd --features testsupport --test stack_pgrace -- --test-threads=2
//! ```
//!
//! `LUNIX_PGRACE_SECS` sets the length of a cell (60); the `#[ignore]` soak
//! runs one cell for `LUNIX_PGRACE_SOAK_SECS` (600).

#![cfg(all(feature = "testsupport", feature = "crashpoints"))]

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use lunix_engine::{CondemnOnCut, EngineConfig, Pressure};
use lunix_format::local::{OnFull, VolumeJson};
use lunix_format::{CryptoMode, IdLen};
use lunix_gc::GcConfig;
use lunix_iface::block::{
    BlockEngine, Completer, IoOp, IoRequest, IoStatus, QueueConfig, QueueHandler, QueueId, Tag,
};
use lunix_iface::space::{SpaceGate, ThrottleState};
use lunix_store::{ReadHook, ReadStage};
use lunixd::stack::{OpenParams, VolumeStack};
use lunixd::testsupport::{close, fsck_binary, Scratch, TestRng, TestVolume, VolumeBuilder};

const CS: u32 = 64 << 10;
const IO: u32 = 256 << 10;
const BLOCK: u64 = 4096;
const SIZE: u64 = 256 << 20;
const CAP: u64 = 96 << 20;
/// The hot set the writers rewrite.
const HOT: u64 = 4 << 20;
const HOT_BLOCKS: u64 = HOT / BLOCK;
/// The short grace under test.
const SHORT: Duration = Duration::from_millis(2_000);
const WRITES_IN_FLIGHT: usize = 12;
const READS_IN_FLIGHT: usize = 32;

fn env_secs(name: &str, default: u64) -> Duration {
    Duration::from_secs(
        std::env::var(name)
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(default),
    )
}

// ------------------------------------------------------------ the queue

struct Done {
    bufs: HashMap<Tag, Vec<u8>>,
    completed: Vec<(Tag, IoStatus)>,
}

impl Completer for Done {
    fn complete(&mut self, tag: Tag, status: IoStatus) {
        assert!(
            !self.completed.iter().any(|(t, _)| *t == tag),
            "tag {tag:?} completed twice"
        );
        self.completed.push((tag, status));
    }

    fn tag_buf(&mut self, tag: Tag) -> &mut [u8] {
        self.bufs.entry(tag).or_insert_with(|| vec![0; IO as usize])
    }
}

/// One queue handler with many requests in flight.
struct Queue {
    h: Box<dyn QueueHandler>,
    done: Done,
    next: u16,
    outstanding: Vec<Tag>,
    /// The buffers of requests that completed inside `on_request`.
    inline: HashMap<Tag, Vec<u8>>,
}

impl Queue {
    fn new(stack: &VolumeStack) -> Self {
        let h = stack
            .engine
            .queue_handler(&QueueConfig {
                queue: QueueId(0),
                nr_queues: 1,
                depth: 128,
                max_io_buf_bytes: IO,
            })
            .expect("queue handler");
        Self {
            h,
            done: Done {
                bufs: HashMap::new(),
                completed: Vec::new(),
            },
            next: 0,
            outstanding: Vec::new(),
            inline: HashMap::new(),
        }
    }

    fn submit(&mut self, op: IoOp, offset_bytes: u64, len: u32, data: &[u8]) -> Tag {
        // The next tag no request in flight holds (paced writes stay for
        // seconds while the counter goes round).
        while self.outstanding.contains(&Tag(self.next)) {
            self.next = (self.next + 1) % 128;
        }
        let tag = Tag(self.next);
        self.next = (self.next + 1) % 128;
        let mut buf = if data.is_empty() {
            vec![0; len as usize]
        } else {
            data.to_vec()
        };
        self.h.on_request(
            IoRequest {
                tag,
                op,
                fua: false,
                offset_bytes,
                len,
            },
            &mut buf,
            &mut self.done,
        );
        if self.done.completed.iter().any(|(t, _)| *t == tag) {
            self.inline.insert(tag, buf);
        }
        self.outstanding.push(tag);
        tag
    }

    fn poll(&mut self) {
        self.h.poll(&mut self.done);
    }

    fn take(&mut self, tag: Tag) -> Option<IoStatus> {
        let i = self.done.completed.iter().position(|(t, _)| *t == tag)?;
        self.outstanding.retain(|t| *t != tag);
        Some(self.done.completed.swap_remove(i).1)
    }

    /// The first `len` bytes a completed read returned.
    fn read_buf(&mut self, tag: Tag, len: usize) -> Vec<u8> {
        let mut b = match self.inline.remove(&tag) {
            Some(b) => b,
            None => self.done.bufs.remove(&tag).unwrap_or_default(),
        };
        b.truncate(len);
        b
    }

    fn wait(&mut self, tag: Tag, limit: Duration) -> IoStatus {
        let start = Instant::now();
        loop {
            self.poll();
            if let Some(s) = self.take(tag) {
                return s;
            }
            assert!(
                start.elapsed() < limit,
                "tag {tag:?} did not complete within {limit:?}"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

// ----------------------------------------------------- stamped blocks

/// The 4 KiB block `lba` at version `ver`: both numbers in the first 16
/// bytes, incompressible bytes from them after that.
fn stamp(lba: u64, ver: u64) -> Vec<u8> {
    let mut v = Vec::with_capacity(BLOCK as usize);
    v.extend_from_slice(&lba.to_le_bytes());
    v.extend_from_slice(&ver.to_le_bytes());
    let mut r = TestRng::new(lba.wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ ver.rotate_left(32));
    while v.len() < BLOCK as usize {
        v.extend_from_slice(&r.next_u64().to_le_bytes());
    }
    v.truncate(BLOCK as usize);
    v
}

fn stamp_range(first: u64, blocks: u64, ver: u64) -> Vec<u8> {
    (first..first + blocks)
        .flat_map(|l| stamp(l, ver))
        .collect()
}

/// One write of a block, on the logical clock of the loop.
#[derive(Clone, Copy, Debug)]
struct Write {
    ver: u64,
    sub: u64,
    done: Option<u64>,
}

/// What the writers did to each block of the hot set.
struct History {
    blocks: Vec<Vec<Write>>,
}

impl History {
    fn new() -> Self {
        Self {
            blocks: vec![Vec::new(); HOT_BLOCKS as usize],
        }
    }

    fn submit(&mut self, first: u64, n: u64, ver: u64, now: u64) {
        for l in first..first + n {
            self.blocks[l as usize].push(Write {
                ver,
                sub: now,
                done: None,
            });
        }
    }

    fn complete(&mut self, first: u64, n: u64, ver: u64, now: u64, oldest_read: u64) {
        for l in first..first + n {
            let w = &mut self.blocks[l as usize];
            if let Some(x) = w.iter_mut().find(|x| x.ver == ver) {
                x.done = Some(now);
            }
            // What no read in flight or to come can still return: a write
            // that completed before the oldest read was submitted and that a
            // later write, submitted after it completed, replaced (also
            // before that read).
            let replacers: Vec<u64> = w
                .iter()
                .filter(|y| y.done.is_some_and(|d| d <= oldest_read))
                .map(|y| y.sub)
                .collect();
            w.retain(|x| match x.done {
                Some(d) if d <= oldest_read => !replacers.iter().any(|&s| s >= d),
                _ => true,
            });
        }
    }

    /// The versions of block `lba` a read submitted at `sub` and completed
    /// at `done` may return: a write that overlaps the read, or one that
    /// completed before it was submitted and was not replaced, before that,
    /// by a write submitted after it completed. Writes that overlap each
    /// other may land in either order.
    fn allowed(&self, lba: u64, sub: u64, done: u64) -> Vec<u64> {
        let w = &self.blocks[lba as usize];
        let before: Vec<&Write> = w
            .iter()
            .filter(|x| x.done.is_some_and(|d| d <= sub))
            .collect();
        w.iter()
            .filter(|x| x.sub <= done)
            .filter(|x| match x.done {
                Some(d) if d <= sub => !before.iter().any(|y| y.sub >= d),
                _ => true,
            })
            .map(|x| x.ver)
            .collect()
    }
}

/// Checks every block of a read result against the history.
fn check_read(h: &History, first: u64, data: &[u8], sub: u64, done: u64, what: &str) {
    assert_eq!(data.len() % BLOCK as usize, 0, "{what}");
    for (i, blk) in data.chunks(BLOCK as usize).enumerate() {
        let lba = first + i as u64;
        let got_lba = u64::from_le_bytes(blk[..8].try_into().unwrap());
        let ver = u64::from_le_bytes(blk[8..16].try_into().unwrap());
        assert_eq!(got_lba, lba, "{what}: block {lba} holds block {got_lba}");
        let allowed = h.allowed(lba, sub, done);
        assert!(
            allowed.contains(&ver),
            "{what}: block {lba} is version {ver}, the history allows {allowed:?}"
        );
        assert_eq!(
            blk,
            &stamp(lba, ver)[..],
            "{what}: block {lba} v{ver} is damaged"
        );
    }
}

// ------------------------------------------------------------ the volume

fn gc_grace_60(c: &mut EngineConfig) {
    // Held parts sleep for seconds: enough helpers that the others read.
    c.read_helpers = 32;
    if let Some(p) = &mut c.gc_params {
        p.read_timeout = Duration::ZERO;
        p.grace = Duration::from_secs(60);
    }
}

fn volume(t: &Scratch, mode: CryptoMode, id_len: IdLen) -> TestVolume {
    let mut b = VolumeBuilder::new(t.path())
        .mode(mode)
        .chunk_size(CS)
        .size_bytes(SIZE);
    b.id_len = id_len;
    b.engine = Some(Arc::new(gc_grace_60));
    b.json = Some(Box::new(|j: &mut VolumeJson| {
        j.space.cache_max = Some(CAP);
        j.space.on_full = OnFull::Wait;
        j.space.full_timeout_ms = Some(30_000);
        j.commit.interval_ms = 1000;
        j.max_io_buf_bytes = IO;
        j.gc_rate = 1 << 20;
        // Smaller than the hot set: most reads go to the store.
        j.read_cache_max = 2 << 20;
    }));
    b.create().expect("create")
}

/// How many guest `read_extent` calls the hook saw, and how many it held
/// (before the open, after it).
static CALLS: AtomicU64 = AtomicU64::new(0);
static HELD_BEFORE: AtomicU64 = AtomicU64::new(0);
static HELD_AFTER: AtomicU64 = AtomicU64::new(0);

/// The store's read hook (`lunix_store::ReadStage`): on a guest read helper,
/// 5 % of the reads wait before the open (the part keeps its hint taken from
/// the map while its segment is collected) and 2.5 % wait after it (the part
/// keeps the file open across the delete), for 0 to 8 short graces.
fn read_hook() -> ReadHook {
    Arc::new(move |stage, hint| {
        let on_helper = std::thread::current()
            .name()
            .is_some_and(|n| n.starts_with("lunix-read-"));
        if !on_helper {
            return;
        }
        let n = if stage == ReadStage::BeforeOpen {
            CALLS.fetch_add(1, Ordering::Relaxed)
        } else {
            CALLS.load(Ordering::Relaxed)
        };
        let mut r = TestRng::new(n ^ hint.off as u64 ^ 0x7067_7261_6365);
        let (every, counter) = match stage {
            ReadStage::BeforeOpen => (20, &HELD_BEFORE),
            ReadStage::AfterOpen => (40, &HELD_AFTER),
        };
        if r.below(every) != 0 {
            return;
        }
        counter.fetch_add(1, Ordering::Relaxed);
        let graces = std::env::var("LUNIX_PGRACE_HOLD_GRACES")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(8);
        std::thread::sleep(Duration::from_millis(
            r.below(graces * SHORT.as_millis() as u64) + 1,
        ));
    })
}

fn pgrace_open(o: &mut OpenParams) {
    o.gc = Some(GcConfig {
        pass_interval: Duration::from_secs(1),
        busy_interval: Duration::from_secs(1),
        ..GcConfig::daemon()
    });
    o.gc_fast_recheck = Some(Duration::from_secs(1));
    o.gc_condemn_on_cut = CondemnOnCut::On;
    o.gc_pressure_grace = Some(SHORT);
    o.segment_read_hook = Some(read_hook());
}

// -------------------------------------------------------------- the cell

struct ReadRec {
    tag: Tag,
    first: u64,
    blocks: u64,
    sub: u64,
}

struct WriteRec {
    tag: Tag,
    first: u64,
    blocks: u64,
    ver: u64,
}

fn cell(name: &str, mode: CryptoMode, id_len: IdLen, partial: bool, run: Duration) {
    assert!(
        fsck_binary().is_some(),
        "lunix-fsck is not built: cargo build --release -p lunix-fsck"
    );
    let t = Scratch::new(name);
    let vol = volume(&t, mode, id_len);
    let stack = vol.open_with(pgrace_open).expect("open");
    let mut q = Queue::new(&stack);
    let mut hist = History::new();
    let mut rng = TestRng::new(0x7067_7261 ^ u64::from(CS) ^ ((mode as u64) << 40));
    let mut clock = 0u64;
    let mut ver = 0u64;

    // Every block of the hot set starts as a write that completed.
    for i in 0..HOT / u64::from(IO) {
        ver += 1;
        let first = i * u64::from(IO) / BLOCK;
        let n = u64::from(IO) / BLOCK;
        hist.submit(first, n, ver, clock);
        let tag = q.submit(IoOp::Write, first * BLOCK, IO, &stamp_range(first, n, ver));
        let s = q.wait(tag, Duration::from_secs(60));
        assert!(s.is_ok(), "the initial fill failed: {s:?}");
        clock += 1;
        hist.complete(first, n, ver, clock, clock);
    }

    let mut writes: Vec<WriteRec> = Vec::new();
    let mut reads: Vec<ReadRec> = Vec::new();
    let mut states = std::collections::BTreeSet::new();
    let mut raised = false;
    let mut next_sample = Instant::now();
    let (mut n_writes, mut n_reads) = (0u64, 0u64);
    let start = Instant::now();
    while start.elapsed() < run {
        clock += 1;
        while writes.len() < WRITES_IN_FLIGHT {
            ver += 1;
            let (first, n) = if partial && rng.below(4) == 0 {
                // A partial-chunk write: 16 KiB at a 4 KiB boundary.
                let n = 4;
                (rng.below(HOT_BLOCKS - n + 1), n)
            } else {
                let n = u64::from(IO) / BLOCK;
                (rng.below(HOT / u64::from(IO)) * n, n)
            };
            hist.submit(first, n, ver, clock);
            let tag = q.submit(
                IoOp::Write,
                first * BLOCK,
                (n * BLOCK) as u32,
                &stamp_range(first, n, ver),
            );
            writes.push(WriteRec {
                tag,
                first,
                blocks: n,
                ver,
            });
            n_writes += 1;
        }
        while reads.len() < READS_IN_FLIGHT {
            let n = 1 + rng.below(u64::from(IO) / BLOCK);
            let first = rng.below(HOT_BLOCKS - n + 1);
            let tag = q.submit(IoOp::Read, first * BLOCK, (n * BLOCK) as u32, &[]);
            reads.push(ReadRec {
                tag,
                first,
                blocks: n,
                sub: clock,
            });
            n_reads += 1;
        }
        q.poll();
        clock += 1;
        let oldest_read = reads.iter().map(|r| r.sub).min().unwrap_or(clock);
        let mut i = 0;
        while i < writes.len() {
            match q.take(writes[i].tag) {
                Some(s) => {
                    assert!(s.is_ok(), "a write failed: {s:?}");
                    let w = writes.swap_remove(i);
                    hist.complete(w.first, w.blocks, w.ver, clock, oldest_read);
                }
                None => i += 1,
            }
        }
        let mut i = 0;
        while i < reads.len() {
            match q.take(reads[i].tag) {
                Some(s) => {
                    assert!(s.is_ok(), "a read failed: {s:?}");
                    let r = reads.swap_remove(i);
                    let data = q.read_buf(r.tag, (r.blocks * BLOCK) as usize);
                    assert_eq!(data.len() as u64, r.blocks * BLOCK, "short read");
                    check_read(&hist, r.first, &data, r.sub, clock, "a read");
                }
                None => i += 1,
            }
        }
        if Instant::now() >= next_sample {
            next_sample += Duration::from_millis(250);
            let snap = stack.space.snapshot();
            states.insert(snap.throttle.state.name());
            if snap.throttle.state == ThrottleState::Slowdown
                && stack.engine.gc_pressure() == Pressure::Raised
            {
                raised = true;
            }
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    // Drain: everything in flight completes and is checked.
    for r in reads.drain(..) {
        let s = q.wait(r.tag, Duration::from_secs(60));
        assert!(s.is_ok(), "a read failed while draining: {s:?}");
        clock += 1;
        let data = q.read_buf(r.tag, (r.blocks * BLOCK) as usize);
        check_read(&hist, r.first, &data, r.sub, clock, "a drained read");
    }
    for w in writes.drain(..) {
        let s = q.wait(w.tag, Duration::from_secs(60));
        assert!(s.is_ok(), "a write failed while draining: {s:?}");
        clock += 1;
        hist.complete(w.first, w.blocks, w.ver, clock, clock);
    }

    // Every block reads back at its last version.
    for first in (0..HOT_BLOCKS).step_by((IO as u64 / BLOCK) as usize) {
        let n = u64::from(IO) / BLOCK;
        let tag = q.submit(IoOp::Read, first * BLOCK, IO, &[]);
        let s = q.wait(tag, Duration::from_secs(60));
        assert!(s.is_ok(), "the final read failed: {s:?}");
        clock += 1;
        let data = q.read_buf(tag, IO as usize);
        assert_eq!(data.len() as u64, n * BLOCK);
        check_read(&hist, first, &data, clock - 1, clock, "the final read");
    }

    let es = stack.engine.stats();
    let gc = stack.engine.gc_status_handle().get();
    let rc = *gc.reclaim;
    eprintln!(
        "pgrace {name}: guest read_extent calls {} held before the open {} and after it {};",
        CALLS.load(Ordering::Relaxed),
        HELD_BEFORE.load(Ordering::Relaxed),
        HELD_AFTER.load(Ordering::Relaxed)
    );
    eprintln!(
        "pgrace {name}: {n_writes} writes, {n_reads} reads in {:?}, states {states:?}, \
         revalidated {} (exhausted {}), stale age {}..{} ms, auth failures {}, \
         pgrace deletes {} ({} bytes, least wait {} ms, un-condemned {}), segment grace {} ms, \
         gc {:?}",
        start.elapsed(),
        es.read_revalidated,
        es.read_revalidate_exhausted,
        es.read_stale_age_min_ms,
        es.read_stale_age_max_ms,
        es.extent_auth_failures,
        rc.pgrace_deletes,
        rc.pgrace_bytes,
        rc.pgrace_min_wait_ms,
        rc.pgrace_uncondemned,
        gc.segment_grace_ms,
        gc.stats
    );
    drop(q);
    close(stack).expect("close");

    assert!(
        states.contains("slowdown") && raised,
        "the gate never paced with the collector raised: {states:?}"
    );
    assert_eq!(es.read_revalidate_exhausted, 0, "a read ran out of retries");
    assert_eq!(es.extent_auth_failures, 0, "a false tamper count");
    assert!(
        rc.pgrace_deletes > 0,
        "the short grace deleted nothing: the run proves nothing ({rc:?})"
    );
    assert!(
        rc.pgrace_min_wait_ms >= SHORT.as_millis() as u64,
        "a segment went before its short grace: {rc:?}"
    );
    assert!(
        es.read_revalidated > 0,
        "no read part lost its segment: the delay hit nothing"
    );
    assert!(
        es.read_stale_age_min_ms >= SHORT.as_millis() as u64,
        "a segment vanished {} ms after a read took its hint",
        es.read_stale_age_min_ms
    );

    let bin = fsck_binary().expect("lunix-fsck");
    let mut cmd = std::process::Command::new(bin);
    cmd.args(vol.fsck_args().expect("fsck credentials"))
        .arg("--deep")
        .arg(&vol.voldir);
    let out = cmd.output().expect("run lunix-fsck");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.code().is_some_and(|c| c < 2) && !text.lines().any(|l| l.starts_with("error")),
        "lunix-fsck --deep:\n{text}"
    );
}

fn secs() -> Duration {
    env_secs("LUNIX_PGRACE_SECS", 60)
}

#[test]
fn plain_default_ids() {
    cell("pg-plain", CryptoMode::Plain, IdLen::Sixteen, false, secs());
}

#[test]
fn plain_id32() {
    cell(
        "pg-plain-id32",
        CryptoMode::Plain,
        IdLen::ThirtyTwo,
        false,
        secs(),
    );
}

#[test]
fn integrity_default_ids_with_partial_chunk_writers() {
    cell(
        "pg-int",
        CryptoMode::Integrity,
        IdLen::Sixteen,
        true,
        secs(),
    );
}

#[test]
fn integrity_id32() {
    cell(
        "pg-int-id32",
        CryptoMode::Integrity,
        IdLen::ThirtyTwo,
        false,
        secs(),
    );
}

#[test]
fn encrypted_default_ids() {
    cell(
        "pg-enc",
        CryptoMode::Encrypted,
        IdLen::Sixteen,
        false,
        secs(),
    );
}

#[test]
fn encrypted_id32() {
    cell(
        "pg-enc-id32",
        CryptoMode::Encrypted,
        IdLen::ThirtyTwo,
        false,
        secs(),
    );
}

/// The long form for the soak: one encrypted cell with partial writers.
#[test]
#[ignore = "the soak: LUNIX_PGRACE_SOAK_SECS (600) seconds"]
fn soak_encrypted_with_partial_chunk_writers() {
    cell(
        "pg-soak",
        CryptoMode::Encrypted,
        IdLen::Sixteen,
        true,
        env_secs("LUNIX_PGRACE_SOAK_SECS", 600),
    );
}
