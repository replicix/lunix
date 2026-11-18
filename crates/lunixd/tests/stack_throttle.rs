//! The bounded full volume on the real stack (ADR-32 T1 and T2; the note
//! `docs/explanation/research/2026-11-18-write-backpressure.md` §9.3), no
//! root: the real space gate, engine, store and collector, driven through the
//! engine's queue handlers with many writes in flight.
//!
//! **E1 replayed.** The incident of 2026-11-18: a store at its limit with
//! nothing to reclaim, 128 requests in flight, one write parked in the gate
//! with no deadline (`on_full = wait`, a null `full_timeout_ms`: path P1). A
//! restart could not be clean: the quiesce drains only when no started
//! request is held, and nothing completed the parked one (path P3); the
//! restart with `on_full = error` came only after a kill and a forced device
//! deletion. Had the new policy been applied live, the parked waiters would
//! have kept the old one (path P2). Each test below fills a volume to its
//! `cache_max` with incompressible data that is never overwritten (no
//! garbage: GC cannot help), leaves writes parked, and ends them one way:
//!
//! * the deadline: they fail `ENOSPC` within `full_timeout` (10 s here) and
//!   the volume latches full, later writes fail at once (P1);
//! * `lunix space --on-full error` (`reconfigure`): they fail at once (P2);
//! * the quiesce rule (`begin_close`, what `QuiesceAll` calls before
//!   `QUIESCE_DEV`): they fail at once and the engine's final commit runs
//!   (P3; the drain itself needs a ublk device, so root, and is the M15
//!   full-volume cell); `Engine::begin_close`, called beside it, covers the
//!   writes parked for the ring (engine tests);
//! * a kill (the stack dropped without a commit), `on_full = error` written
//!   to `volume.json`, a reopen and the writes reissued: each fails at once
//!   and nothing parks.
//!
//! **A discard frees space** (P4, M6): on the latched volume a discard of
//! everything written is admitted, the coordinator cuts it and turns the
//! retained window, the urgent pass (asked for by the refused writes, with
//! its back-off) deletes the dead segments after the floor grace, the latch
//! opens and a write succeeds; `lunix-fsck` finds nothing at rest.
//!
//! ```text
//! cargo build --release -p lunix-fsck
//! cargo test --release -p lunixd --features testsupport --test stack_throttle
//! ```

#![cfg(feature = "testsupport")]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use lunix_engine::EngineConfig;
use lunix_format::local::{OnFull, VolumeJson};
use lunix_format::paths::local as lpath;
use lunix_format::CryptoMode;
use lunix_gc::GcConfig;
use lunix_iface::block::{
    BlockEngine, Completer, IoOp, IoRequest, IoStatus, QueueConfig, QueueHandler, QueueId, Tag,
};
use lunix_iface::gc::GcControl;
use lunix_iface::space::SpaceGate;
use lunixd::stack::{OpenParams, VolumeStack};
use lunixd::testsupport::{close, fsck, fsck_binary, payload, Scratch, TestVolume, VolumeBuilder};

const ENOSPC: i32 = 28;
const CS: u32 = 64 << 10;
const SIZE: u64 = 128 << 20;
const CAP: u64 = 64 << 20;
const IO: u32 = 256 << 10;
/// The `full_timeout_ms` of the tests that wait it out.
const FULL_TIMEOUT_MS: u64 = 10_000;
const FULL_TIMEOUT: Duration = Duration::from_millis(FULL_TIMEOUT_MS);
/// What "at once" may take on a loaded host.
const SLACK: Duration = Duration::from_secs(5);
/// A write still parked this long (more than two commit intervals of 1 s)
/// waits for space for good.
const PARKED: Duration = Duration::from_millis(2500);

/// The completions of one queue handler.
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

/// One queue with many requests in flight, as a ublk queue hands them over.
struct Queue {
    h: Box<dyn QueueHandler>,
    done: Done,
    next: u16,
    outstanding: Vec<Tag>,
    /// Request buffers of the requests that completed inside `on_request`
    /// (a read answered there is in its own buffer).
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
        let tag = Tag(self.next);
        self.next = (self.next + 1) % 128;
        assert!(!self.outstanding.contains(&tag), "tag {tag:?} reused");
        let mut buf = data.to_vec();
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

    /// A read of `IO` bytes at `offset_bytes`.
    fn read(&mut self, offset_bytes: u64) -> Vec<u8> {
        let t = self.submit(IoOp::Read, offset_bytes, IO, &vec![0; IO as usize]);
        let s = self.wait(t, Duration::from_secs(10));
        assert!(s.is_ok(), "read at {offset_bytes}: {s:?}");
        match self.inline.remove(&t) {
            Some(b) => b,
            None => self.done.bufs.get(&t).cloned().unwrap_or_default(),
        }
    }

    fn write(&mut self, offset_bytes: u64, seed: u64) -> Tag {
        let data = payload(seed << 2, IO as usize);
        self.submit(IoOp::Write, offset_bytes, IO, &data)
    }

    fn poll(&mut self) {
        self.h.poll(&mut self.done);
    }

    fn take(&mut self, tag: Tag) -> Option<IoStatus> {
        let i = self.done.completed.iter().position(|(t, _)| *t == tag)?;
        self.outstanding.retain(|t| *t != tag);
        Some(self.done.completed.swap_remove(i).1)
    }

    /// Polls until `tag` completes, at most `limit`.
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

    /// Polls until every outstanding request completes, at most `limit`;
    /// returns their statuses in tag order of submission.
    fn wait_all(&mut self, limit: Duration) -> Vec<IoStatus> {
        let tags = self.outstanding.clone();
        tags.into_iter().map(|t| self.wait(t, limit)).collect()
    }
}

/// The GC settings of the stack tests: the smallest grace (10 s with
/// `read_timeout` 0), which is also the urgent pass's floor grace.
fn gc_tune(c: &mut EngineConfig) {
    if let Some(p) = &mut c.gc_params {
        p.read_timeout = Duration::ZERO;
        p.grace = Duration::from_secs(10);
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

/// A 128 MiB volume capped at 64 MiB of store, commits every second,
/// `on_full = wait` with `full_timeout_ms` as given (null: the daemon
/// default).
fn volume(t: &Scratch, full_timeout_ms: Option<u64>) -> TestVolume {
    let mut b = VolumeBuilder::new(t.path())
        .mode(CryptoMode::Integrity)
        .chunk_size(CS)
        // The production ring files (64 MiB, as large as the cap): the
        // engine releases `JournalReserved` per cut, not per ring file
        // (review M4).
        .size_bytes(SIZE);
    b.engine = Some(Arc::new(gc_tune));
    b.json = Some(Box::new(move |j: &mut VolumeJson| {
        j.space.cache_max = Some(CAP);
        j.space.on_full = OnFull::Wait;
        j.space.full_timeout_ms = full_timeout_ms;
        j.commit.interval_ms = 1000;
        j.max_io_buf_bytes = IO;
    }));
    b.create().expect("create")
}

/// Writes fresh 256 KiB blocks, one at a time, until one waits for space
/// for good: still not done [`PARKED`] after it went in, with the gate
/// holding a waiter. A shorter wait is the double count of a cut in flight
/// (its segment bounds and the journal it reclaims are both charged until
/// C9), which the next commit ends. Returns the offset after the last
/// block written and the parked tag.
fn fill_until_parked(q: &mut Queue, stack: &VolumeStack) -> (u64, Tag) {
    let mut off = 0u64;
    let mut seed = 1u64;
    loop {
        assert!(off + u64::from(IO) <= SIZE, "the volume never filled");
        let t = q.write(off, seed);
        seed += 1;
        off += u64::from(IO);
        let start = Instant::now();
        loop {
            q.poll();
            if let Some(s) = q.take(t) {
                assert!(s.is_ok(), "a fill write failed: {s:?}");
                break;
            }
            if start.elapsed() > PARKED && stack.space.snapshot().waiters > 0 {
                return (off, t);
            }
            assert!(
                start.elapsed() < Duration::from_secs(60),
                "a fill write neither completed nor parked"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

/// Fills to the cap and leaves `extra` more writes in flight behind the
/// parked one (FIFO in the queue: they wait behind it).
fn fill_and_park(q: &mut Queue, stack: &VolumeStack, extra: u64) -> u64 {
    let (mut off, _parked) = fill_until_parked(q, stack);
    for i in 0..extra {
        q.write(off % SIZE, 10_000 + i);
        off += u64::from(IO);
    }
    for _ in 0..100 {
        q.poll();
    }
    assert_eq!(
        q.done.completed.len(),
        0,
        "nothing completes while the head waits: {:?}",
        q.done.completed
    );
    assert_eq!(q.outstanding.len() as u64, extra + 1);
    off
}

fn all_enospc(statuses: &[IoStatus]) {
    for s in statuses {
        assert_eq!(s.errno(), Some(ENOSPC), "{statuses:?}");
    }
}

#[test]
fn e1_parked_writes_fail_with_enospc_within_full_timeout_and_the_volume_latches() {
    let t = Scratch::new("bstop-deadline");
    let vol = volume(&t, Some(FULL_TIMEOUT_MS));
    let stack = vol.open_with(fast_gc).expect("open");
    let mut q = Queue::new(&stack);
    fill_and_park(&mut q, &stack, 31);
    let start = Instant::now();
    let st = q.wait_all(FULL_TIMEOUT + SLACK);
    let took = start.elapsed();
    all_enospc(&st);
    // The head parked before the clock started: within full_timeout of its
    // arrival, plus slack for a loaded host.
    assert!(took < FULL_TIMEOUT + SLACK, "took {took:?}");
    // And not much earlier: the wait is the configured one.
    assert!(took + PARKED + SLACK > FULL_TIMEOUT, "took {took:?}");
    assert!(stack.space.is_full(), "the volume latched full");
    assert_eq!(stack.space.fulls(), 1);
    // Latched: a write fails at once, nothing parks.
    let t0 = Instant::now();
    let w = q.write(0, 7);
    assert_eq!(q.wait(w, Duration::from_secs(5)).errno(), Some(ENOSPC));
    assert!(t0.elapsed() < SLACK, "{:?}", t0.elapsed());
    assert_eq!(stack.space.snapshot().waiters, 0);
    // Reads go on.
    assert_eq!(q.read(0), payload(1 << 2, IO as usize));
    drop(q);
    close(stack).expect("close");
}

#[test]
fn e1_a_switch_to_on_full_error_releases_the_parked_writes_at_once() {
    let t = Scratch::new("bstop-policy");
    // A null full_timeout_ms: the daemon default (60 s) applies.
    let vol = volume(&t, None);
    let stack = vol.open_with(fast_gc).expect("open");
    assert_eq!(stack.space.full_timeout(), Duration::from_secs(60));
    let mut q = Queue::new(&stack);
    fill_and_park(&mut q, &stack, 31);
    let mut s = stack.json.space.clone();
    s.on_full = OnFull::Error;
    let start = Instant::now();
    stack.space.reconfigure(&s).expect("reconfigure");
    let st = q.wait_all(Duration::from_secs(5));
    all_enospc(&st);
    assert!(start.elapsed() < SLACK, "{:?}", start.elapsed());
    assert_eq!(stack.space.snapshot().waiters, 0);
    drop(q);
    close(stack).expect("close");
}

#[test]
fn e1_the_quiesce_rule_fails_the_parked_writes_and_the_final_commit_runs() {
    let t = Scratch::new("bstop-quiesce");
    let vol = volume(&t, None);
    let stack = vol.open_with(fast_gc).expect("open");
    let mut q = Queue::new(&stack);
    fill_and_park(&mut q, &stack, 31);
    let start = Instant::now();
    stack.space.begin_close();
    stack.engine.begin_close(Duration::from_secs(20));
    let st = q.wait_all(SLACK);
    all_enospc(&st);
    assert!(start.elapsed() < SLACK);
    // Nothing is held: the queue would drain; the final commit runs.
    assert!(q.outstanding.is_empty());
    drop(q);
    stack.engine.quiesce().expect("the final commit");
    let voldir = stack.voldir.clone();
    drop(stack);
    lunixd::testsupport::wait_unlocked(&voldir);
}

#[test]
fn e1_a_kill_and_a_restart_with_on_full_error_fail_the_reissued_writes_at_once() {
    let t = Scratch::new("bstop-restart");
    let vol = volume(&t, None);
    let stack = vol.open_with(fast_gc).expect("open");
    let mut q = Queue::new(&stack);
    let off = fill_and_park(&mut q, &stack, 15);
    // What was acknowledged is flushed (a FLUSH never waits for a parked
    // write), then a kill: the handlers and the stack go without a final
    // commit.
    let f = q.submit(IoOp::Flush, 0, 0, &[]);
    assert!(q.wait(f, Duration::from_secs(10)).is_ok());
    drop(q);
    let _ = vol.crash(stack, 7);
    // The operator writes on_full = error before the restart.
    let path = vol.voldir.join(lpath::volume_json());
    let mut json =
        VolumeJson::from_json(&std::fs::read_to_string(&path).expect("read")).expect("volume.json");
    json.space.on_full = OnFull::Error;
    std::fs::write(&path, json.to_json_pretty().expect("json")).expect("write");
    let stack = vol.open_with(fast_gc).expect("reopen");
    assert_eq!(stack.json.space.on_full, OnFull::Error);
    let mut q = Queue::new(&stack);
    // The reissued writes: each is answered at once, nothing parks. The
    // journal is charged per cut before and after the kill (review M4), so
    // the reopen finds the store at its cap; a cut's bounds held at the kill
    // or the reopen's true-up may leave room for one.
    let mut enospc = 0;
    for i in 0..16u64 {
        let t0 = Instant::now();
        let w = q.write((off + i * u64::from(IO)) % SIZE, 20_000 + i);
        let s = q.wait(w, Duration::from_secs(5));
        assert!(t0.elapsed() < SLACK, "{:?}", t0.elapsed());
        assert_eq!(stack.space.snapshot().waiters, 0, "nothing parks");
        if s.errno() == Some(ENOSPC) {
            enospc += 1;
        } else {
            assert!(s.is_ok(), "{s:?}");
        }
    }
    assert!(enospc >= 15, "only {enospc} of 16 refused");
    drop(q);
    close(stack).expect("close");
}

#[test]
fn a_discard_on_a_full_volume_brings_the_space_back_and_writes_succeed() {
    discard_on_a_full_volume("bstop-discard", fast_gc);
}

/// The same on the daemon's pipelined schedule: the ladder's urgent passes
/// interrupt pipelined passes, the window turn's FULLs move the tip under
/// them.
#[test]
fn a_discard_on_a_full_volume_brings_the_space_back_on_the_daemon_schedule() {
    discard_on_a_full_volume("gcrate-discard", daemon_gc);
}

fn discard_on_a_full_volume(tag: &str, gc: fn(&mut OpenParams)) {
    let t = Scratch::new(tag);
    let vol = volume(&t, Some(FULL_TIMEOUT_MS));
    let stack = vol.open_with(gc).expect("open");
    let mut q = Queue::new(&stack);
    let written = fill_and_park(&mut q, &stack, 3);
    all_enospc(&q.wait_all(FULL_TIMEOUT + SLACK));
    assert!(stack.space.is_full());
    // Discard everything written, in requests of at most the buffer size.
    let start = Instant::now();
    let mut off = 0;
    while off < written.min(SIZE) {
        let d = q.submit(IoOp::Discard, off, IO, &[]);
        assert!(
            q.wait(d, Duration::from_secs(5)).is_ok(),
            "discard at {off}"
        );
        off += u64::from(IO);
    }
    // Refused writes keep asking the ladder; the coordinator cuts the
    // discards and turns the window; the urgent pass deletes after the floor
    // grace. The note's bound is about 45 s with the defaults.
    let mut tries = 0u64;
    let ok_at = loop {
        let w = q.write(0, 30_000 + tries);
        tries += 1;
        let s = q.wait(w, Duration::from_secs(10));
        if s.is_ok() {
            break start.elapsed();
        }
        assert_eq!(s.errno(), Some(ENOSPC), "{s:?}");
        assert!(
            start.elapsed() < Duration::from_secs(90),
            "the space never came back: {:?}, gc {:?}",
            stack.space.snapshot(),
            stack.engine.status()
        );
        std::thread::sleep(Duration::from_millis(500));
    };
    eprintln!(
        "discard to a successful write: {ok_at:?} ({tries} tries); gc {:?}",
        stack.engine.status().stats
    );
    assert!(!stack.space.is_full());
    assert!(stack.engine.status().stats.bytes_freed > 0, "gc deleted");
    // The volume works again: the rest of what was discarded reads as
    // zeros, the new write reads back.
    assert!(q.read(u64::from(IO)).iter().all(|b| *b == 0));
    assert_eq!(q.read(0), payload((30_000 + tries - 1) << 2, IO as usize));
    drop(q);
    close(stack).expect("close");
    if std::env::var_os("LUNIX_STACK_REQUIRE_FSCK").is_some() {
        assert!(fsck_binary().is_some(), "lunix-fsck is not built");
    }
    if let Some(f) = fsck(&vol) {
        assert!(f.no_errors(), "lunix-fsck:\n{}", f.text);
    }
}
