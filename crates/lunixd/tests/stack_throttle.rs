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
use lunix_iface::space::{SpaceGate, SpaceKind, ThrottleState};
use lunix_store::{FsOps, RealFs};
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
/// A write still parked this long (more than two commit intervals of 1 s),
/// with no cut in flight for as long, waits for space for good.
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
        self.submit_with(op, false, offset_bytes, len, data)
    }

    fn submit_with(
        &mut self,
        op: IoOp,
        fua: bool,
        offset_bytes: u64,
        len: u32,
        data: &[u8],
    ) -> Tag {
        let tag = Tag(self.next);
        self.next = (self.next + 1) % 128;
        assert!(!self.outstanding.contains(&tag), "tag {tag:?} reused");
        let mut buf = data.to_vec();
        self.h.on_request(
            IoRequest {
                tag,
                op,
                fua,
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
        // The last time the journal held bytes a cut has yet to reclaim. A
        // cut in flight charges its segment bounds beside the journal it
        // reclaims and ends at C9, however long a loaded host makes it; with
        // no journal bytes left, nothing is left to end and the wait is for
        // space for good.
        let mut journal_seen = start;
        loop {
            q.poll();
            if let Some(s) = q.take(t) {
                assert!(s.is_ok(), "a fill write failed: {s:?}");
                break;
            }
            let snap = stack.space.snapshot();
            if snap.by_kind[SpaceKind::JournalReserved.index()] > 0 {
                journal_seen = Instant::now();
            }
            if start.elapsed() > PARKED
                && journal_seen.elapsed() > PARKED
                && snap.throttle.space_waiters > 0
            {
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
    let mut s = lunixd::stack::effective_space(
        &stack.json,
        lunix_iface::block::BlockEngine::size_bytes(&*stack.engine),
    );
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

#[test]
fn a_percentage_cap_is_resolved_at_every_open() {
    // FREEZE F2 (review M1 of p1/bstop-fu): a percentage volume gets its cap
    // from the size once the engine opened, at the first open and after a
    // restart. 200 % of 64 MiB wins over the floor (64 MiB + 2 x 1 MiB).
    let t = Scratch::new("bstop-pct");
    let mut b = VolumeBuilder::new(t.path())
        .mode(CryptoMode::Integrity)
        .chunk_size(CS)
        .size_bytes(64 << 20);
    b.json = Some(Box::new(|j: &mut VolumeJson| {
        j.space.cache_max = None;
        j.space.cache_max_percent = Some(200);
        j.space.free_floor = lunix_format::local::FreeFloor::Bytes(0);
        j.commit.dirty_bytes = 1 << 20;
    }));
    let vol = b.create().expect("create");
    for open in ["the first open", "a reopen"] {
        let stack = vol.open().expect("open");
        assert_eq!(stack.json.space.cache_max_percent, Some(200));
        assert_eq!(stack.space.snapshot().limit, 128 << 20, "{open}");
        close(stack).expect("close");
    }
}

// ---------------------------------------------------------------- ADR-32 T3, T4

/// A 128 MiB volume capped at 64 MiB: the soft mark is half the limit
/// (32 MiB), `gc_rate` 1 MiB/s so that `r_mid` is its floor of 16 MiB/s.
fn band_volume(t: &Scratch) -> TestVolume {
    let mut b = VolumeBuilder::new(t.path())
        .mode(CryptoMode::Integrity)
        .chunk_size(CS)
        .size_bytes(SIZE);
    b.engine = Some(Arc::new(gc_tune));
    b.json = Some(Box::new(|j: &mut VolumeJson| {
        j.space.cache_max = Some(CAP);
        j.space.on_full = OnFull::Wait;
        j.space.full_timeout_ms = Some(FULL_TIMEOUT_MS);
        j.commit.interval_ms = 1000;
        j.max_io_buf_bytes = IO;
        j.gc_rate = 1 << 20;
    }));
    b.create().expect("create")
}

#[test]
fn a_volume_entering_the_band_is_paced_and_not_refused() {
    let t = Scratch::new("pace-band");
    let vol = band_volume(&t);
    let stack = vol.open_with(daemon_gc).expect("open");
    // 15 % of 64 MiB is under the 1 GiB floor of the mark: half the limit.
    assert_eq!(stack.space.soft_mark(), CAP / 2);
    let mut q = Queue::new(&stack);
    let mut off = 0u64;
    let mut seed = 1u64;
    let mut states = std::collections::BTreeSet::new();
    let mut raised = false;
    let mut inflight: Vec<Tag> = Vec::new();
    // Fresh blocks only: no garbage, nothing for GC to free. Write until
    // the headroom is under 8 MiB.
    let start = Instant::now();
    loop {
        while inflight.len() < 16 {
            inflight.push(q.write(off, seed));
            off += u64::from(IO);
            seed += 1;
        }
        q.poll();
        let mut i = 0;
        while i < inflight.len() {
            match q.take(inflight[i]) {
                Some(s) => {
                    assert!(s.is_ok(), "a write was refused inside the band: {s:?}");
                    inflight.swap_remove(i);
                }
                None => i += 1,
            }
        }
        let snap = stack.space.snapshot();
        states.insert(snap.throttle.state.name());
        raised |= stack.engine.gc_pressure() != lunix_engine::Pressure::Normal;
        if snap.throttle.headroom < 8 << 20 {
            break;
        }
        assert!(off + 16 * u64::from(IO) < SIZE, "never got near the limit");
        assert!(
            start.elapsed() < Duration::from_secs(60),
            "too slow: {snap:?}"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    q.wait_all(Duration::from_secs(20))
        .iter()
        .for_each(|s| assert!(s.is_ok(), "{s:?}"));
    let written = off;
    // Deep in the band: a few more bulk blocks, then a FLUSH and a small
    // FUA write. Neither waits for the bulk writes' pace.
    // A cut that ends meanwhile gives the double count back (C9) and the
    // band is left behind: the bulk blocks are owed a delay only if the
    // pace held at their submit, and the FUA write a bypass only if it
    // held on both sides of its own.
    let deep = |stack: &VolumeStack| {
        let th = stack.space.snapshot().throttle;
        th.state == ThrottleState::Slowdown && th.rate.is_some_and(|r| r <= 16 << 20)
    };
    let deep_before = deep(&stack);
    let head = stack.space.snapshot().throttle.headroom;
    let n = ((head.saturating_sub(3 << 20)) / u64::from(IO)).min(8);
    let bulk: Vec<Tag> = (0..n)
        .map(|i| {
            let t = q.write(off, seed + i);
            off += u64::from(IO);
            t
        })
        .collect();
    // The pressure follows the gate's state at the engine's tick, and a tick
    // can wait behind a cut that a loaded host stretches to seconds, so the
    // polls above may all have come between two ticks. With writers parked
    // in the band the next tick raises it: wait for that, not for luck.
    let tick = Instant::now();
    while !raised && tick.elapsed() < Duration::from_secs(20) {
        q.poll();
        raised = stack.engine.gc_pressure() != lunix_engine::Pressure::Normal;
        std::thread::sleep(Duration::from_millis(5));
    }
    let deep_bulk = deep_before && deep(&stack);
    let fua_data = payload(77, 4096);
    let in_band = |stack: &VolumeStack| {
        let th = stack.space.snapshot().throttle;
        th.state == ThrottleState::Slowdown && th.rate.is_some()
    };
    let band_before = in_band(&stack);
    let t0 = Instant::now();
    let fua = q.submit_with(IoOp::Write, true, 3 * u64::from(CS) + 4096, 4096, &fua_data);
    // The gate decides at the submit. A cut that ends meanwhile gives the
    // space back (the double count of a cut in flight goes at C9) and the
    // write finds no pace to bypass: the counter is owed only when the band
    // held on both sides of it.
    let band_across = band_before && in_band(&stack);
    let flush = q.submit(IoOp::Flush, 0, 0, &[]);
    assert!(q.wait(fua, SLACK).is_ok());
    assert!(q.wait(flush, SLACK).is_ok());
    assert!(
        t0.elapsed() < SLACK,
        "a FUA write and a FLUSH waited {:?}",
        t0.elapsed()
    );
    for t in bulk {
        assert!(q.wait(t, Duration::from_secs(30)).is_ok());
    }
    let snap = stack.space.snapshot();
    let t = snap.throttle;
    eprintln!("band: {states:?}, {t:?}, written {} MiB", written >> 20);
    assert!(
        states.contains("slowdown"),
        "the gate never paced: {states:?}"
    );
    assert!(!states.contains("full"), "{states:?}");
    assert!(
        (t.delayed_writes > 0 && t.delayed_ms > 0) || !deep_bulk,
        "{t:?}"
    );
    assert!(t.delay_max_ms <= 10_000 + 2_000, "the cap is 10 s: {t:?}");
    assert_eq!((t.enospc, t.fulls), (0, 0), "{t:?}");
    assert!(
        t.bypassed_fua >= 1 || !band_across,
        "the FUA write was paced: {t:?}"
    );
    assert!(raised, "GC was never given more");
    assert!(snap.used() <= snap.limit + snap.headroom_total);
    assert!(t.throttled_ms > 0);
    assert!(!stack.space.is_full());
    // What was written reads back, the FUA write included.
    for b in 0..written / u64::from(IO) {
        let mut want = payload((b + 1) << 2, IO as usize);
        if b * u64::from(IO) <= 3 * u64::from(CS) + 4096
            && 3 * u64::from(CS) + 4096 < (b + 1) * u64::from(IO)
        {
            let o = (3 * u64::from(CS) + 4096 - b * u64::from(IO)) as usize;
            want[o..o + 4096].copy_from_slice(&fua_data);
        }
        assert_eq!(q.read(b * u64::from(IO)), want, "block {b}");
    }
    drop(q);
    close(stack).expect("close");
    if let Some(f) = fsck(&vol) {
        assert!(f.no_errors(), "lunix-fsck:\n{}", f.text);
    }
}

/// GC settings of the slow-GC test: a configured grace of 60 s, which is
/// more than the test runs; only the floor grace (10 s with `read_timeout`
/// 0) under pressure can delete anything in time.
fn gc_grace_60(c: &mut EngineConfig) {
    if let Some(p) = &mut c.gc_params {
        p.read_timeout = Duration::ZERO;
        p.grace = Duration::from_secs(60);
    }
}

/// A 256 MiB volume capped at 128 MiB: the soft mark is half the limit
/// (64 MiB). A 16 MiB working set is rewritten over and over.
fn slow_gc_volume(t: &Scratch) -> TestVolume {
    let mut b = VolumeBuilder::new(t.path())
        .mode(CryptoMode::Integrity)
        .chunk_size(CS)
        .size_bytes(256 << 20);
    b.engine = Some(Arc::new(gc_grace_60));
    b.json = Some(Box::new(|j: &mut VolumeJson| {
        j.space.cache_max = Some(128 << 20);
        j.space.on_full = OnFull::Wait;
        j.space.full_timeout_ms = Some(30_000);
        j.commit.interval_ms = 1000;
        j.max_io_buf_bytes = IO;
        j.gc_rate = 1 << 20;
    }));
    b.create().expect("create")
}

/// GC pressure (T4) and the slow-GC cell of the note's §9.3: rewriting a
/// working set far faster than a grace of 60 s lets GC free garbage, on a
/// capped volume. The gate paces the writer instead of refusing it; while it
/// does, the collector runs with the floor grace (10 s), so garbage is
/// deleted well before the configured 60 s; the store stays inside the cap;
/// when the writer stops the gate opens again and the collector returns to
/// its normal figures; every block reads back and `lunix-fsck` is clean.
#[test]
fn gc_pressure_raises_gc_budget_and_rate_and_the_slow_gc_cell_is_paced_not_refused() {
    use lunix_engine::Pressure;
    let t = Scratch::new("pace-slowgc");
    let vol = slow_gc_volume(&t);
    let stack = vol.open_with(daemon_gc).expect("open");
    let mut q = Queue::new(&stack);
    let blocks = (16u64 << 20) / u64::from(IO);
    let mut last = vec![0u64; blocks as usize];
    let mut inflight: Vec<(Tag, u64, u64)> = Vec::new();
    let mut states = std::collections::BTreeSet::new();
    let mut raised_in_slowdown = false;
    let mut peak = 0u64;
    let mut freed_at = None;
    let mut n = 0u64;
    let start = Instant::now();
    let run = Duration::from_secs(40);
    let mut next_sample = Instant::now();
    while start.elapsed() < run {
        while inflight.len() < 16 {
            let blk = n % blocks;
            n += 1;
            inflight.push((q.write(blk * u64::from(IO), n), blk, n));
        }
        q.poll();
        let mut i = 0;
        while i < inflight.len() {
            match q.take(inflight[i].0) {
                Some(s) => {
                    assert!(s.is_ok(), "a write failed in the slow-GC cell: {s:?}");
                    last[inflight[i].1 as usize] = inflight[i].2;
                    inflight.swap_remove(i);
                }
                None => i += 1,
            }
        }
        if Instant::now() >= next_sample {
            next_sample += Duration::from_millis(250);
            let snap = stack.space.snapshot();
            peak = peak.max(snap.used());
            assert!(
                snap.used() <= snap.limit + snap.headroom_total,
                "the store left its cap: {snap:?}"
            );
            states.insert(snap.throttle.state.name());
            if snap.throttle.state == ThrottleState::Slowdown
                && stack.engine.gc_pressure() == Pressure::Raised
            {
                raised_in_slowdown = true;
            }
            if freed_at.is_none() && stack.engine.status().stats.bytes_freed > 0 {
                freed_at = Some((start.elapsed(), snap.throttle.state));
            }
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    for (tag, blk, seed) in inflight.drain(..) {
        let s = q.wait(tag, Duration::from_secs(40));
        assert!(s.is_ok(), "{s:?}");
        last[blk as usize] = seed;
    }
    let snap = stack.space.snapshot();
    let th = snap.throttle;
    eprintln!(
        "slow gc: {n} writes in {:?}, peak {} MiB of {} MiB, states {states:?}, {th:?}, first delete {freed_at:?}, gc {:?}",
        start.elapsed(),
        peak >> 20,
        snap.limit >> 20,
        stack.engine.status().stats
    );
    assert!(
        states.contains("slowdown"),
        "the gate never paced: {states:?}"
    );
    assert!(th.delayed_writes > 0 && th.delayed_ms > 0, "{th:?}");
    assert!(
        th.delay_max_ms <= 10_000 + 2_000,
        "the delay cap is 10 s: {th:?}"
    );
    assert_eq!(
        (th.enospc, th.fulls),
        (0, 0),
        "no write was refused: {th:?}"
    );
    assert!(raised_in_slowdown, "GC was not raised while the gate paced");
    // Only the floor grace can delete inside the run: the configured one is
    // 60 s.
    let (at, during) = freed_at.expect("GC deleted nothing in the run");
    assert!(at < run, "{at:?}");
    assert!(
        during != ThrottleState::Open,
        "the first delete came with the gate open at {at:?}; nothing shows the floor grace"
    );
    // The writer stopped: the garbage goes, the gate opens, and the
    // collector goes back to its normal figures 5 s later.
    let wait = Instant::now();
    loop {
        let s = stack.space.snapshot().throttle.state;
        if s == ThrottleState::Open && stack.engine.gc_pressure() == Pressure::Normal {
            break;
        }
        assert!(
            wait.elapsed() < Duration::from_secs(90),
            "no recovery: {:?}, pressure {:?}",
            stack.space.snapshot(),
            stack.engine.gc_pressure()
        );
        std::thread::sleep(Duration::from_millis(250));
    }
    for (b, seed) in last.iter().enumerate() {
        assert_eq!(
            q.read(b as u64 * u64::from(IO)),
            payload(seed << 2, IO as usize),
            "block {b}"
        );
    }
    drop(q);
    close(stack).expect("close");
    if let Some(f) = fsck(&vol) {
        assert!(f.no_errors(), "lunix-fsck:\n{}", f.text);
    }
}

/// Milestone gate, the M15 latency finding on the real stack: a deep queue
/// of overwrites against a slow band. The writes queue behind one another in
/// the engine's queue, so `delay_max_ms` must be the wait from their arrival,
/// not from the moment each reaches the front. The assertion is relative (the
/// status figure against the slowest wait the test itself saw) and does not
/// bound any duration; every block reads back.
#[test]
fn deep_overwrites_report_their_wait_from_arrival() {
    let t = Scratch::new("gate-deep");
    let vol = slow_gc_volume(&t);
    // A slow band (64 KiB/s inside it): 32 writes of 256 KiB queued behind
    // one another would take 128 s in strict FIFO; the cap is 10 s.
    let stack = vol.open_with(slow_band).expect("open");
    let mut q = Queue::new(&stack);
    let blocks = (16u64 << 20) / u64::from(IO);
    let mut last = vec![0u64; blocks as usize];
    let mut inflight: Vec<(Tag, u64, u64, Instant)> = Vec::new();
    let mut slowest = Duration::ZERO;
    let mut n = 0u64;
    let run = Duration::from_secs(25);
    let start = Instant::now();
    while start.elapsed() < run {
        while inflight.len() < 32 {
            let blk = n % blocks;
            n += 1;
            inflight.push((q.write(blk * u64::from(IO), n), blk, n, Instant::now()));
        }
        q.poll();
        let mut i = 0;
        while i < inflight.len() {
            match q.take(inflight[i].0) {
                Some(s) => {
                    assert!(s.is_ok(), "a write failed: {s:?}");
                    last[inflight[i].1 as usize] = inflight[i].2;
                    slowest = slowest.max(inflight[i].3.elapsed());
                    inflight.swap_remove(i);
                }
                None => i += 1,
            }
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    for (tag, blk, seed, at) in inflight.drain(..) {
        let s = q.wait(tag, Duration::from_secs(40));
        assert!(s.is_ok(), "{s:?}");
        last[blk as usize] = seed;
        slowest = slowest.max(at.elapsed());
    }
    let snap = stack.space.snapshot();
    let th = snap.throttle;
    eprintln!(
        "deep: {n} writes in {:?}, slowest {slowest:?}, {th:?}, gc {:?}",
        start.elapsed(),
        stack.engine.status().stats
    );
    assert!(th.delayed_writes > 0, "the gate never paced: {th:?}");
    // The figure is the real wait, from arrival: not below what the slowest
    // write waited, less what is not the gate's (commit and append). A quiet
    // host adds about a second to the 10 s cap, a host with CPU and disk
    // load more than 5 s (15.5 s seen), so the allowance is three slacks.
    assert!(
        Duration::from_millis(th.delay_max_ms) + 3 * SLACK >= slowest,
        "delay_max_ms {} says less than the {slowest:?} a write waited",
        th.delay_max_ms
    );
    for (b, seed) in last.iter().enumerate() {
        assert_eq!(
            q.read(b as u64 * u64::from(IO)),
            payload(seed << 2, IO as usize),
            "block {b}"
        );
    }
    drop(q);
    close(stack).expect("close");
    if let Some(f) = fsck(&vol) {
        assert!(f.no_errors(), "lunix-fsck:\n{}", f.text);
    }
}

/// Review M2 (the FUA bypass on its own queue) and the `throttle = false`
/// switch (m11).
fn slow_band(o: &mut OpenParams) {
    daemon_gc(o);
    // A band slow enough to hold a 256 KiB write for seconds on a small
    // volume: `r_min` at its lowest, `r_mid` the same.
    o.throttle.min_rate = 65_536;
    o.throttle_mid_rate = Some(65_536);
}

#[test]
fn a_fua_write_overtakes_the_paced_writes_of_its_own_queue() {
    let t = Scratch::new("pace-fua-queue");
    let vol = band_volume(&t);
    let stack = vol.open_with(slow_band).expect("open");
    let mut q = Queue::new(&stack);
    // 20 MiB of fresh data goes in at full speed (the soft mark is 32 MiB).
    let mut off = 0u64;
    let mut seed = 1u64;
    let mut inflight: Vec<Tag> = Vec::new();
    while off < 20 << 20 {
        while inflight.len() < 16 && off < 20 << 20 {
            inflight.push(q.write(off, seed));
            off += u64::from(IO);
            seed += 1;
        }
        q.poll();
        inflight.retain(|t| match q.take(*t) {
            Some(s) => {
                assert!(s.is_ok(), "{s:?}");
                false
            }
            None => true,
        });
        std::thread::sleep(Duration::from_millis(1));
    }
    q.wait_all(Duration::from_secs(20));
    // Cut and commit, so that the journal's bytes are not counted twice.
    stack.engine.commit_now(false).expect("commit");
    // The operator tightens the cap to 3 MiB of headroom: deep in the band,
    // at 64 KiB/s a 256 KiB write takes 4 s (its delay cap is 10 s).
    let mut s = lunixd::stack::effective_space(
        &stack.json,
        lunix_iface::block::BlockEngine::size_bytes(&*stack.engine),
    );
    s.cache_max = Some(stack.space.snapshot().used() + (3 << 20));
    stack.space.reconfigure(&s).expect("reconfigure");
    let snap = stack.space.snapshot();
    assert!(snap.throttle.rate.is_some(), "not in the band: {snap:?}");
    // Eight bulk writes queue up behind each other (one reserve at a time per
    // queue), then a small FUA write on the same queue.
    let bulk: Vec<Tag> = (0..8)
        .map(|i| {
            let t = q.write(off, seed + i);
            off += u64::from(IO);
            t
        })
        .collect();
    for _ in 0..50 {
        q.poll();
        std::thread::sleep(Duration::from_millis(2));
    }
    // The bucket's first megabyte goes through; the rest wait for the pace.
    assert!(
        q.done.completed.len() < bulk.len(),
        "the bulk writes are paced and wait: {:?}",
        q.done.completed
    );
    let t0 = Instant::now();
    let fua = q.submit_with(IoOp::Write, true, 0, 4096, &payload(99, 4096));
    let st = q.wait(fua, Duration::from_secs(20));
    let took = t0.elapsed();
    assert!(st.is_ok(), "{st:?}");
    assert!(
        took < Duration::from_secs(2),
        "the FUA write waited {took:?} behind the paced writes of its queue"
    );
    assert!(stack.space.snapshot().throttle.bypassed_fua >= 1);
    for t in bulk {
        assert!(q.wait(t, Duration::from_secs(60)).is_ok());
    }
    drop(q);
    close(stack).expect("close");
}

#[test]
fn with_the_band_off_the_volume_is_never_paced() {
    let t = Scratch::new("pace-off");
    let vol = band_volume(&t);
    let stack = vol
        .open_with(|o: &mut OpenParams| {
            daemon_gc(o);
            o.throttle.enabled = false;
        })
        .expect("open");
    assert_eq!(stack.space.soft_mark(), 0);
    let mut q = Queue::new(&stack);
    let mut off = 0u64;
    let mut seed = 1u64;
    let mut inflight: Vec<Tag> = Vec::new();
    let mut stopped = 0u32;
    // 44 MiB of the 64 MiB cap: well inside what would be the band.
    while off < 44 << 20 {
        while inflight.len() < 16 && off < 44 << 20 {
            inflight.push(q.write(off, seed));
            off += u64::from(IO);
            seed += 1;
        }
        q.poll();
        inflight.retain(|t| match q.take(*t) {
            Some(s) => {
                assert!(s.is_ok(), "{s:?}");
                false
            }
            None => true,
        });
        let th = stack.space.snapshot().throttle;
        // The band is off, so the state is `Open` or, while a cut in flight
        // holds its segment bounds and the journal it reclaims at once
        // (charged twice until C9, as `fill_until_parked` notes), the
        // bounded `Stop` of a write that waits for that space. That stop is
        // not pacing: no delay, no rate, no paced waiter. It ends at C9
        // (every write below must complete `Ok`, within `full_timeout`, and
        // the volume must never latch); how soon depends on the host's load,
        // so the test does not assert `Open` at every poll.
        match th.state {
            ThrottleState::Open => {}
            ThrottleState::Stop => {
                assert!(th.space_waiters > 0, "a stop without a waiter: {th:?}");
                stopped += 1;
            }
            other => panic!("paced or latched with the band off: {other:?} {th:?}"),
        }
        // The gate counts every request it admitted after a wait as delayed,
        // a wait for space included, so a delay is pacing only without a
        // stop to explain it.
        assert_eq!((th.rate, th.paced_waiters), (None, 0), "{th:?}");
        assert!(th.stops > 0 || th.delayed_writes == 0, "paced: {th:?}");
        assert_eq!(th.delay_capped, 0, "{th:?}");
        std::thread::sleep(Duration::from_millis(1));
    }
    q.wait_all(Duration::from_secs(20));
    assert_eq!(stack.space.fulls(), 0, "the volume latched full");
    eprintln!("pace-off: {stopped} polls in the bounded stop");
    assert_eq!(stack.engine.gc_pressure(), lunix_engine::Pressure::Normal);
    drop(q);
    close(stack).expect("close");
}

// ------------------------------------------------------------------ ring pace
//
// Milestone jstall (T6): sustained ingest on a small ring with a slow drain,
// on the real gate and engine. The coordinator hands the gate the drain rate
// of its cuts while the ring is nearly full or a write waited for it; writes
// see a short pace instead of the stop of two cuts. Nothing may fail with
// `EIO`, the pace must engage and be counted apart from the space throttle,
// and the longest completion stays near two cut durations plus the delay cap.

/// The host file system with a slow `fdatasync` and directory sync: every
/// seal and every commit object takes real time, so a cut is seconds long.
#[derive(Debug)]
struct SlowFs {
    delay: Duration,
}

impl FsOps for SlowFs {
    fn fdatasync(&self, file: &std::fs::File) -> std::io::Result<()> {
        std::thread::sleep(self.delay);
        RealFs.fdatasync(file)
    }

    fn rename(
        &self,
        from: &std::path::Path,
        to: &std::path::Path,
        noreplace: bool,
    ) -> std::io::Result<()> {
        RealFs.rename(from, to, noreplace)
    }

    fn fsync_dir(&self, dir: &std::path::Path) -> std::io::Result<()> {
        std::thread::sleep(self.delay);
        RealFs.fsync_dir(dir)
    }

    fn unlink(&self, path: &std::path::Path) -> std::io::Result<()> {
        RealFs.unlink(path)
    }
}

fn ring_pace_volume(t: &Scratch) -> TestVolume {
    let mut b = VolumeBuilder::new(t.path())
        .mode(CryptoMode::Integrity)
        .chunk_size(CS)
        .size_bytes(SIZE);
    b.engine = Some(Arc::new(gc_tune));
    b.json = Some(Box::new(|j: &mut VolumeJson| {
        // A cap far from the store: no space pressure, only the ring.
        j.space.cache_max = Some(1 << 30);
        j.space.on_full = OnFull::Wait;
        j.space.full_timeout_ms = Some(FULL_TIMEOUT_MS);
        j.commit.interval_ms = 1000;
        j.max_io_buf_bytes = IO;
    }));
    b.create().expect("create")
}

#[test]
fn sustained_ingest_on_a_small_ring_with_a_slow_drain_is_paced_and_never_fails() {
    let t = Scratch::new("ring-pace");
    let vol = ring_pace_volume(&t);
    let stack = vol
        .open_with(|o| {
            o.store_fs = Some(Arc::new(SlowFs {
                delay: Duration::from_millis(150),
            }));
            // Four ring files of 4 MiB: 16 MiB of ring against 4 MiB of
            // writes in flight.
            o.journal_tweak = Some(Arc::new(|j: &mut lunix_journal::JournalConfig| {
                j.file_len = 4 << 20;
                j.journal_max = 16 << 20;
            }));
        })
        .expect("open");
    let mut q = Queue::new(&stack);
    let run = Duration::from_secs(25);
    let started = Instant::now();
    let mut sent: HashMap<Tag, Instant> = HashMap::new();
    let (mut seed, mut writes, mut longest) = (1u64, 0u64, Duration::ZERO);
    let (mut max_rate, mut max_cut_ms, mut max_waiters) = (0u64, 0u64, 0u64);
    let mut last_sample = Instant::now();
    loop {
        let running = started.elapsed() < run;
        // 16 writes of 256 KiB in flight, over a 32 MiB region.
        while running && sent.len() < 16 {
            let off = (seed % 128) * u64::from(IO);
            let tag = q.write(off, seed);
            sent.insert(tag, Instant::now());
            seed += 1;
        }
        q.poll();
        let tags: Vec<Tag> = sent.keys().copied().collect();
        for tag in tags {
            if let Some(st) = q.take(tag) {
                assert!(st.is_ok(), "a write failed after {writes}: {st:?}");
                longest = longest.max(sent.remove(&tag).expect("sent").elapsed());
                writes += 1;
            }
        }
        if last_sample.elapsed() > Duration::from_millis(100) {
            last_sample = Instant::now();
            let snap = stack.space.snapshot();
            max_rate = max_rate.max(snap.throttle.ring_pace_rate);
            let cuts = stack.engine.cut_stats();
            max_cut_ms = max_cut_ms.max(cuts.last_cut_ms);
            max_waiters = max_waiters.max(cuts.ring_waiters);
        }
        if !running && sent.is_empty() {
            break;
        }
        assert!(
            started.elapsed() < run + Duration::from_secs(90),
            "the writes did not drain: {} in flight",
            sent.len()
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    let snap = stack.space.snapshot();
    let cuts = stack.engine.cut_stats();
    let ws = stack.engine.write_stats();
    eprintln!(
        "ring pace: {writes} writes, longest {longest:?}, max pace {max_rate} B/s, \
         max cut {max_cut_ms} ms, max ring waiters {max_waiters}, {:?}, {cuts:?}, freed {:?}",
        snap.throttle, ws.freed_files_by_lane
    );
    assert!(writes > 100, "the volume took {writes} writes");
    assert!(max_rate > 0, "the ring pace never engaged");
    assert!(
        snap.throttle.ring_paced_writes > 0,
        "no write was delayed by the ring pace: {:?}",
        snap.throttle
    );
    assert_eq!(
        snap.throttle.state,
        lunix_iface::space::ThrottleState::Open,
        "the space throttle is not in play"
    );
    assert!(ws.freed_files_by_lane.iter().sum::<u64>() > 0);
    // Two cuts (the one in flight and the next) plus the delay cap.
    let bound = Duration::from_millis(2 * max_cut_ms) + Duration::from_secs(10) + SLACK;
    assert!(
        longest <= bound,
        "the longest completion {longest:?} is above {bound:?} (cut {max_cut_ms} ms)"
    );
    drop(q);
    close(stack).expect("close");
}
