//! One queue's request handler (DESIGN §3.1). Everything here runs on the
//! queue's thread and never blocks: waits park the tag, and `poll` resumes
//! it from a journal completion, a read helper, a space waiter, a flush
//! group or a coordinator wake-up.
//!
//! Write path: `SpaceGate::reserve(JournalReserved, record_bytes)`, then,
//! under this queue's in-flight lock, `JournalLane::try_reserve` (which
//! assigns `write_seq`) and the registration of the number as in flight,
//! then `append`. On the record's `Append` completion the overlay gets the
//! range, the number leaves the in-flight set and the request completes (a
//! FUA write first joins the next flush group). `Full` and `NoSpace` from
//! the journal give the space back and park the tag; no number is consumed.
//!
//! The bounded full volume (ADR-32): a write parked in the gate fails
//! `ENOSPC` when its waiter expires (`EIO` when the volume went read-only);
//! a write parked for a full ring fails at its `full_timeout` or at once when
//! the engine stops (`crate::full`). DISCARD and WRITE_ZEROES take their
//! 64-byte hole records from the reserved region (`reserve_headroom`), so
//! they never wait for space, and queue only behind other hole requests
//! parked for the ring, never behind parked writes (two requests in flight
//! together have no order).

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::os::fd::{AsFd, BorrowedFd};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, MutexGuard};

use lunix_format::journal::RecordKind;
use lunix_format::WriteSeq;
use lunix_iface::block::{Completer, IoOp, IoRequest, IoStatus, QueueHandler, QueueId, Tag};
use lunix_iface::journal::{
    record_bytes, JournalCompletion, JournalLane, JournalLoc, SlotOutcome, Token,
};
use lunix_iface::map::{entry_hint, Cached, ChunkNo};
use lunix_iface::overlay::{BlockRange, Piece, PieceSource};
use lunix_iface::space::{
    ReserveClass, ReserveOutcome, SpaceKind, Waiter, WaiterState, SYNC_MAX_BYTES,
};
use lunix_iface::wake::WakeFd;

use crate::engine::Inner;
use crate::read::ReadJob;
use crate::stats::add;

const BLOCK: u64 = 4096;
const EIO: i32 = 5;
const EINVAL: i32 = 22;
const ENOSPC: i32 = 28;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// A completion produced off the queue thread.
pub(crate) enum Deferred {
    /// A read helper finished one part of a read.
    ReadPart {
        tag: Tag,
        gen: u64,
        req_off: usize,
        result: std::result::Result<Vec<u8>, i32>,
    },
    /// The flush group of a FLUSH or FUA waiter finished.
    Flush { tag: Tag, gen: u64, ok: bool },
}

/// The part of a queue other threads see.
pub(crate) struct QueueShared {
    pub(crate) id: QueueId,
    wake: WakeFd,
    /// Write sequence numbers reserved on this lane and not yet in the
    /// overlay. C1 waits on it.
    inflight: Mutex<BTreeSet<u64>>,
    deferred: Mutex<Vec<Deferred>>,
    /// The queue holds a request parked for a full journal ring (the
    /// background coordinator wakes it for the deadline).
    pub(crate) ring_parked: std::sync::atomic::AtomicBool,
}

impl QueueShared {
    pub(crate) fn new(id: QueueId, wake: WakeFd) -> Self {
        Self {
            id,
            wake,
            inflight: Mutex::new(BTreeSet::new()),
            deferred: Mutex::new(Vec::new()),
            ring_parked: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Hands a completion to the queue and wakes it. Any thread.
    pub(crate) fn post(&self, d: Deferred) {
        lock(&self.deferred).push(d);
        self.wake.signal();
    }

    pub(crate) fn signal(&self) {
        self.wake.signal();
    }

    pub(crate) fn has_inflight_upto(&self, c: u64) -> bool {
        lock(&self.inflight).range(..=c).next().is_some()
    }
}

/// Where a mutation waits for its journal space.
enum SpaceWait {
    /// Nothing charged for the next part.
    None,
    /// `bytes` are charged for the next part.
    Charged(u64),
    /// Parked in the gate for `bytes`.
    Waiting(Waiter, u64),
}

struct Mutation {
    record: RecordKind,
    /// The request's data, copied only when the write had to wait.
    data: Option<Vec<u8>>,
    next_block: u64,
    end_block: u64,
    outstanding: u32,
    space: SpaceWait,
    /// `clock.monotonic()` at arrival: every wait counts from it.
    arrived: std::time::Duration,
    /// Parked for a full journal ring since this `clock.monotonic()`.
    ring_since: Option<std::time::Duration>,
}

struct ReadState {
    buf: Vec<u8>,
    pending: u32,
    failed: Option<i32>,
}

enum Kind {
    Mutate(Mutation),
    Read(ReadState),
    /// A FLUSH, or a FUA write whose records are all in: waits for its group.
    Flush,
    /// Placeholder while a read's state is taken out to be updated.
    Taken,
}

struct Req {
    req: IoRequest,
    gen: u64,
    /// `clock.monotonic()` at arrival (writes, discards and write-zeroes:
    /// the acknowledgement's latency counts from it; ZERO for the others).
    arrived: std::time::Duration,
    /// The byte count a success reports.
    ok_bytes: u32,
    kind: Kind,
}

/// An appended record waiting for its completion.
struct Pending {
    tag: Tag,
    gen: u64,
    range: BlockRange,
    loc: JournalLoc,
    record: RecordKind,
}

/// A journal read of an overlay piece.
struct JRead {
    tag: Tag,
    gen: u64,
    req_off: usize,
    range: BlockRange,
    /// The piece's record: a failed read may fall back to the map only if
    /// the durable tip covers it.
    write_seq: WriteSeq,
}

/// The buffer of the request `on_request` is handling.
type Inline<'a> = Option<(Tag, &'a mut [u8])>;

enum Step {
    /// Every part is appended (or the request completed).
    Done,
    /// Waiting for space or the journal.
    Parked,
}

pub(crate) struct Queue {
    inner: Arc<Inner>,
    shared: Arc<QueueShared>,
    lane: Box<dyn JournalLane>,
    max_buf: u32,
    reqs: HashMap<Tag, Req>,
    appends: HashMap<u64, Pending>,
    /// Writes waiting for space or the ring, FIFO.
    parked: VecDeque<Tag>,
    /// DISCARD and WRITE_ZEROES waiting for the ring, FIFO among themselves.
    parked_holes: VecDeque<Tag>,
    jreads: HashMap<u64, JRead>,
    /// Scratch for the parked requests that wait for the ring (reused).
    ring_scratch: Vec<(Tag, std::time::Duration, std::time::Duration)>,
    /// What this queue last added to the engine's count of parked requests
    /// that wait for the ring.
    ring_count: i64,
    /// Requests of this queue the ring refused a slot and that still wait
    /// (the ones with `ring_since`): the scans below run only when it is not 0.
    ring_refused: u32,
    next_token: u64,
    next_gen: u64,
    jbuf: Vec<JournalCompletion>,
    dbuf: Vec<Deferred>,
    pieces: Vec<Piece>,
}

impl Queue {
    pub(crate) fn new(
        inner: Arc<Inner>,
        shared: Arc<QueueShared>,
        lane: Box<dyn JournalLane>,
        max_buf: u32,
    ) -> Self {
        Self {
            inner,
            shared,
            lane,
            max_buf,
            reqs: HashMap::new(),
            appends: HashMap::new(),
            parked: VecDeque::new(),
            parked_holes: VecDeque::new(),
            jreads: HashMap::new(),
            ring_scratch: Vec::new(),
            ring_count: 0,
            ring_refused: 0,
            next_token: 1,
            next_gen: 1,
            jbuf: Vec::new(),
            dbuf: Vec::new(),
            pieces: Vec::new(),
        }
    }

    fn valid(&self, req: &IoRequest) -> bool {
        if req.op == IoOp::Flush {
            return true;
        }
        let len = u64::from(req.len);
        let in_range = req
            .offset_bytes
            .checked_add(len)
            .is_some_and(|end| end <= self.inner.size());
        let buf_ok = !matches!(req.op, IoOp::Read | IoOp::Write) || req.len <= self.max_buf;
        len > 0
            && req.offset_bytes.is_multiple_of(BLOCK)
            && len.is_multiple_of(BLOCK)
            && in_range
            && buf_ok
    }

    fn handle(&mut self, req: IoRequest, buf: &mut [u8], done: &mut dyn Completer) {
        if !self.valid(&req) {
            done.complete(req.tag, IoStatus::err(EINVAL));
            return;
        }
        let st = &self.inner.stats;
        match req.op {
            IoOp::Flush => add(&st.ops_flush, 1),
            IoOp::Read => add(&st.ops_read, 1),
            IoOp::Write => add(&st.ops_write, 1),
            IoOp::Discard => add(&st.ops_discard, 1),
            IoOp::WriteZeroes => add(&st.ops_write_zeroes, 1),
        }
        let gen = self.next_gen;
        self.next_gen += 1;
        match req.op {
            IoOp::Flush => {
                self.reqs.insert(
                    req.tag,
                    Req {
                        req,
                        gen,
                        arrived: std::time::Duration::ZERO,
                        ok_bytes: 0,
                        kind: Kind::Flush,
                    },
                );
                self.inner
                    .flush
                    .join(Arc::clone(&self.shared), req.tag, gen);
                self.inner
                    .flush
                    .kick(&self.inner.journal, &self.inner.stats);
            }
            IoOp::Read => self.start_read(req, gen, buf, done),
            IoOp::Write | IoOp::Discard | IoOp::WriteZeroes => {
                if self.inner.read_only() {
                    done.complete(req.tag, IoStatus::err(EIO));
                    return;
                }
                let record = if req.op == IoOp::Write {
                    add(&st.bytes_written, u64::from(req.len));
                    RecordKind::Write
                } else {
                    RecordKind::Hole
                };
                let first = req.offset_bytes / BLOCK;
                let arrived = self.inner.clock.monotonic();
                self.reqs.insert(
                    req.tag,
                    Req {
                        req,
                        gen,
                        arrived,
                        ok_bytes: req.len,
                        kind: Kind::Mutate(Mutation {
                            record,
                            data: None,
                            next_block: first,
                            end_block: first + u64::from(req.len) / BLOCK,
                            outstanding: 0,
                            space: SpaceWait::None,
                            arrived,
                            ring_since: None,
                        }),
                    },
                );
                // FIFO behind the parked requests of the same class: a hole
                // never waits behind a write parked for space (ADR-32).
                let is_hole = record == RecordKind::Hole;
                let behind = if is_hole {
                    &self.parked_holes
                } else {
                    &self.parked
                };
                // The write an fsync waits on overtakes the writes of its own
                // queue that wait in the gate for their pace or for space:
                // two requests in flight have no order, and the gate admits
                // it at once inside the band (ADR-32; review M2). Not past a
                // write that waits for the journal ring. An overtaker that
                // has to park itself (the ring is full, or the gate is in
                // `stop`) sits behind the front request in `parked`, and
                // `retry_parked` looks only at the front: it waits out that
                // request's pace, at most its delay cap (re-review m2-a).
                let overtakes = !behind.is_empty()
                    && record == RecordKind::Write
                    && req.fua
                    && u64::from(req.len) <= SYNC_MAX_BYTES
                    && !self.ring_wait_parked();
                let behind = if is_hole {
                    &self.parked_holes
                } else {
                    &self.parked
                };
                let step = if behind.is_empty() || overtakes {
                    self.advance(req.tag, Some(&*buf), done)
                } else {
                    Step::Parked
                };
                if let Step::Parked = step {
                    if let Some(Req {
                        kind: Kind::Mutate(m),
                        ..
                    }) = self.reqs.get_mut(&req.tag)
                    {
                        if m.record == RecordKind::Write && m.data.is_none() {
                            m.data = Some(buf.get(..req.len as usize).unwrap_or(buf).to_vec());
                        }
                    }
                    if is_hole {
                        self.parked_holes.push_back(req.tag);
                    } else {
                        self.parked.push_back(req.tag);
                    }
                }
            }
        }
    }

    /// True when a parked request waits for the journal ring.
    fn ring_wait_parked(&self) -> bool {
        self.ring_refused > 0
    }

    fn fail(&mut self, tag: Tag, errno: i32, done: &mut dyn Completer) {
        if let Some(r) = self.reqs.remove(&tag) {
            if let Kind::Mutate(m) = &r.kind {
                if let Some(since) = m.ring_since {
                    self.inner.end_ring_wait(since);
                    self.ring_refused = self.ring_refused.saturating_sub(1);
                }
                if let SpaceWait::Charged(b) = m.space {
                    self.inner.space.release(SpaceKind::JournalReserved, b);
                } else if let SpaceWait::Waiting(w, b) = &m.space {
                    if !w.cancel() && w.state() == WaiterState::Granted {
                        self.inner.space.release(SpaceKind::JournalReserved, *b);
                    }
                }
            }
            done.complete(tag, IoStatus::err(errno));
        }
    }

    /// Reserves and appends the next parts of a mutation until all are
    /// appended or one has to wait. `buf` is the request buffer when called
    /// from `on_request`.
    fn advance(&mut self, tag: Tag, buf: Option<&[u8]>, done: &mut dyn Completer) -> Step {
        loop {
            let inner = Arc::clone(&self.inner);
            let Some(r) = self.reqs.get_mut(&tag) else {
                return Step::Done;
            };
            let gen = r.gen;
            let lane = usize::from(self.shared.id.0);
            let req_lba = r.req.offset_bytes / BLOCK;
            // The write an fsync waits on is not paced inside the band
            // (ADR-32): a FUA write of at most 256 KiB.
            let class =
                if r.req.fua && r.req.op == IoOp::Write && u64::from(r.req.len) <= SYNC_MAX_BYTES {
                    ReserveClass::Sync
                } else {
                    ReserveClass::Bulk
                };
            let Kind::Mutate(m) = &mut r.kind else {
                return Step::Done;
            };
            // A retry after a ring refusal paid its pace the first time.
            let class = if m.ring_since.is_some() {
                ReserveClass::Retry
            } else {
                class
            };
            if m.next_block >= m.end_block {
                return Step::Done;
            }
            let remaining = m.end_block - m.next_block;
            let blocks = match m.record {
                RecordKind::Write => remaining.min(256),
                // At most half a cut's leaves per hole record, so a cut can
                // stop between records.
                RecordKind::Hole => remaining.min(u64::from(u32::MAX)).min(
                    (inner.cfg.max_leaves_per_cut / 2)
                        .max(1)
                        .saturating_mul(u64::from(inner.cfg.map_fanout.max(1)))
                        .saturating_mul(inner.cfg.chunk_blocks()),
                ),
                RecordKind::Mark => unreachable!("a request never maps to a mark"),
            };
            let blocks32 = u32::try_from(blocks).unwrap_or(u32::MAX);
            let charge = record_bytes(m.record, blocks32);
            // Space for the record (DESIGN §4.3 admission).
            match std::mem::replace(&mut m.space, SpaceWait::None) {
                SpaceWait::Charged(b) => m.space = SpaceWait::Charged(b),
                SpaceWait::Waiting(w, b) => match w.state() {
                    WaiterState::Pending => {
                        m.space = SpaceWait::Waiting(w, b);
                        return Step::Parked;
                    }
                    WaiterState::Granted => {
                        inner.note_admitted();
                        m.space = SpaceWait::Charged(b);
                    }
                    WaiterState::TimedOut | WaiterState::Cancelled => {
                        // A read-only volume expires its waiters too: that
                        // is `EIO`, the full volume `ENOSPC` (ADR-32).
                        let errno = if inner.read_only() {
                            EIO
                        } else {
                            inner.note_refused();
                            ENOSPC
                        };
                        self.fail(tag, errno, done);
                        return Step::Done;
                    }
                },
                // A hole record frees space: it is charged without a limit
                // check (its 64 bytes go into a preallocated ring file; the
                // ring, which has a deadline, is its bound), never parked and
                // never refused for space (ADR-32, P4; review M5).
                SpaceWait::None if m.record == RecordKind::Hole => {
                    match inner.space.charge(SpaceKind::JournalReserved, charge) {
                        Ok(()) => m.space = SpaceWait::Charged(charge),
                        Err(_) => {
                            self.fail(tag, EIO, done);
                            return Step::Done;
                        }
                    }
                }
                // The wait counts from the request's arrival, also when it
                // queued behind a parked write first (review M1).
                SpaceWait::None => {
                    match inner.space.reserve_since(
                        SpaceKind::JournalReserved,
                        charge,
                        Some(&self.shared.wake),
                        m.arrived,
                        class,
                    ) {
                        Ok(ReserveOutcome::Reserved) => {
                            inner.note_admitted();
                            m.space = SpaceWait::Charged(charge);
                        }
                        Ok(ReserveOutcome::Parked(w)) => {
                            let _ = inner.full.set_blocked(true, inner.clock.monotonic());
                            m.space = SpaceWait::Waiting(w, charge);
                            return Step::Parked;
                        }
                        // Paced, not refused: the volume is slow, not full
                        // (no window turn, no refusal flag).
                        Ok(ReserveOutcome::Paced(w)) => {
                            m.space = SpaceWait::Waiting(w, charge);
                            return Step::Parked;
                        }
                        Ok(ReserveOutcome::NoSpace) => {
                            inner.note_refused();
                            self.fail(tag, ENOSPC, done);
                            return Step::Done;
                        }
                        Err(_) => {
                            self.fail(tag, EIO, done);
                            return Step::Done;
                        }
                    }
                }
            }
            let SpaceWait::Charged(charged) = m.space else {
                return Step::Parked;
            };
            // The number and its in-flight registration in one step (C1).
            let reserved = {
                let mut infl = lock(&self.shared.inflight);
                match self.lane.try_reserve(m.record, blocks32) {
                    Ok(SlotOutcome::Reserved(slot)) => {
                        infl.insert(slot.write_seq.0);
                        Ok(Some(slot))
                    }
                    Ok(SlotOutcome::Full | SlotOutcome::NoSpace) => Ok(None),
                    Err(e) => Err(e),
                }
            };
            let slot = match reserved {
                Ok(Some(slot)) => slot,
                Ok(None) => {
                    inner.space.release(SpaceKind::JournalReserved, charged);
                    m.space = SpaceWait::None;
                    // Every wait has a deadline (ADR-32, P6).
                    if let Some(errno) = inner.ring_wait_verdict(m.arrived, lane) {
                        self.fail(tag, errno, done);
                        return Step::Done;
                    }
                    let now = inner.clock.monotonic();
                    if m.ring_since.is_none() {
                        m.ring_since = Some(now);
                        self.ring_refused += 1;
                        inner.cuts.ring_waiters.fetch_add(1, Ordering::Relaxed);
                    }
                    inner.note_ring_wait(now.saturating_sub(m.ring_since.unwrap_or(now)));
                    inner.journal_full.store(true, Ordering::Release);
                    return Step::Parked;
                }
                Err(e) => {
                    m.space = SpaceWait::None;
                    inner.space.release(SpaceKind::JournalReserved, charged);
                    tracing::error!(error = %e, "journal reservation failed");
                    inner.space.fatal_io("journal reservation");
                    self.fail(tag, EIO, done);
                    return Step::Done;
                }
            };
            m.space = SpaceWait::None;
            if let Some(since) = m.ring_since.take() {
                inner.end_ring_wait(since);
                self.ring_refused = self.ring_refused.saturating_sub(1);
            }
            let seq = slot.write_seq;
            if m.record == RecordKind::Hole {
                inner.full.note_hole(seq.0);
            }
            inner.points.hooks().sequenced(self.shared.id, tag, seq);
            let lba = m.next_block;
            let range = BlockRange { lba, blocks };
            let data: &[u8] = match m.record {
                RecordKind::Hole => &[],
                RecordKind::Mark => unreachable!("a request never maps to a mark"),
                RecordKind::Write => {
                    let from = usize::try_from((lba - req_lba) * BLOCK).unwrap_or(usize::MAX);
                    let len = usize::try_from(blocks * BLOCK).unwrap_or(usize::MAX);
                    let src = m.data.as_deref().or(buf).unwrap_or(&[]);
                    src.get(from..from.saturating_add(len)).unwrap_or(&[])
                }
            };
            match self.lane.append(slot, lba, data) {
                Ok(app) => {
                    m.next_block += blocks;
                    m.outstanding += 1;
                    inner.journal_charged.fetch_add(charged, Ordering::AcqRel);
                    inner
                        .jcharges
                        .push(usize::from(self.shared.id.0), seq.0, charged);
                    let record = m.record;
                    self.appends.insert(
                        seq.0,
                        Pending {
                            tag,
                            gen,
                            range,
                            loc: app.loc,
                            record,
                        },
                    );
                }
                Err(e) => {
                    tracing::error!(error = %e, "journal append failed");
                    inner.space.release(SpaceKind::JournalReserved, charged);
                    inner.poison(seq.0, "journal append");
                    lock(&self.shared.inflight).remove(&seq.0);
                    inner.notify_progress();
                    self.fail(tag, EIO, done);
                    return Step::Done;
                }
            }
        }
    }

    fn start_read(&mut self, req: IoRequest, gen: u64, buf: &mut [u8], done: &mut dyn Completer) {
        let lba = req.offset_bytes / BLOCK;
        let blocks = u64::from(req.len) / BLOCK;
        add(&self.inner.stats.bytes_read, u64::from(req.len));
        let mut st = ReadState {
            buf: vec![0; req.len as usize],
            pending: 0,
            failed: None,
        };
        let mut pieces = std::mem::take(&mut self.pieces);
        self.inner
            .overlay
            .lookup(BlockRange { lba, blocks }, &mut pieces);
        for p in &pieces {
            let req_off = usize::try_from((p.range.lba - lba) * BLOCK).unwrap_or(0);
            match p.source {
                PieceSource::Hole => {}
                PieceSource::Journal(loc) => {
                    let token = self.next_token;
                    self.next_token += 1;
                    let len = usize::try_from(p.range.blocks * BLOCK).unwrap_or(0);
                    match self.lane.submit_read(loc, vec![0; len], Token(token)) {
                        Ok(()) => {
                            st.pending += 1;
                            self.jreads.insert(
                                token,
                                JRead {
                                    tag: req.tag,
                                    gen,
                                    req_off,
                                    range: p.range,
                                    write_seq: p.write_seq,
                                },
                            );
                        }
                        Err(_) => self.journal_read_failed(
                            req.tag,
                            gen,
                            lba,
                            p.range,
                            p.write_seq,
                            EIO,
                            &mut st,
                        ),
                    }
                }
                PieceSource::None => self.map_parts(req.tag, gen, lba, p.range, &mut st),
            }
        }
        self.pieces = pieces;
        if st.pending == 0 {
            match buf.get_mut(..st.buf.len()) {
                Some(dst) => {
                    dst.copy_from_slice(&st.buf);
                    done.complete(req.tag, IoStatus::ok(req.len));
                }
                None => done.complete(req.tag, IoStatus::err(EINVAL)),
            }
            return;
        }
        self.reqs.insert(
            req.tag,
            Req {
                req,
                gen,
                arrived: std::time::Duration::ZERO,
                ok_bytes: req.len,
                kind: Kind::Read(st),
            },
        );
    }

    /// The map side of a read for `range` (blocks the overlay does not hold):
    /// holes are zeros, resident entries are served from the decoded-chunk
    /// cache, everything else goes to a read helper.
    ///
    /// A helper starts from the entry seen here, and may read a newer one.
    /// If a cut replaces the entry before the read runs, the old extent is
    /// still right: the new extent differs from it only in blocks the cut
    /// wrote, and the overlay holds every such block until C9 (after C6), so
    /// this lookup did not route them here. If the read of the entry seen
    /// here fails with NotFound, Corrupt or Auth (GC deleted the segment, or
    /// compaction moved the chunk), the helper looks the chunk up in the map
    /// again and retries up to `REVALIDATE_MAX` times (C7, `read.rs`), so the
    /// retry may read an entry newer than the one seen here. Parts of one
    /// request can therefore reflect different map instants.
    fn map_parts(
        &mut self,
        tag: Tag,
        gen: u64,
        req_lba: u64,
        range: BlockRange,
        st: &mut ReadState,
    ) {
        let cb = self.inner.cfg.chunk_blocks();
        let end = range.lba + range.blocks;
        let mut b = range.lba;
        // Stamped on the first part that needs a helper: the cached hits and
        // holes of a request pay nothing for it.
        let mut born = None;
        while b < end {
            let ci = b / cb;
            let part_end = end.min((ci + 1) * cb);
            let chunk_off = usize::try_from((b - ci * cb) * BLOCK).unwrap_or(0);
            let len = usize::try_from((part_end - b) * BLOCK).unwrap_or(0);
            let req_off = usize::try_from((b - req_lba) * BLOCK).unwrap_or(0);
            let entry = match self.inner.map.try_get_cached(ChunkNo(ci)) {
                Cached::Hole => {
                    st.buf[req_off..req_off + len].fill(0);
                    b = part_end;
                    continue;
                }
                Cached::Hit(e) => {
                    let key = (entry_hint(&e), e.prefix);
                    if let Some(d) = self.inner.cache.get(&key) {
                        match d.get(chunk_off..chunk_off + len) {
                            Some(src) => st.buf[req_off..req_off + len].copy_from_slice(src),
                            None => st.failed = Some(EIO),
                        }
                        b = part_end;
                        continue;
                    }
                    Some(e)
                }
                Cached::Miss => None,
            };
            st.pending += 1;
            let born = *born.get_or_insert_with(|| self.inner.clock.monotonic());
            self.inner.pool.submit(ReadJob {
                queue: Arc::clone(&self.shared),
                tag,
                gen,
                req_off,
                ci,
                chunk_off,
                len,
                entry,
                born,
            });
            b = part_end;
        }
    }

    /// Completes a read whose parts are all in. `inline` is the request
    /// buffer while `on_request` of that tag runs: a read that finishes
    /// there is answered in its own buffer, a later one in `tag_buf`.
    fn maybe_finish_read(&mut self, tag: Tag, done: &mut dyn Completer, inline: &mut Inline<'_>) {
        let finished = matches!(self.reqs.get(&tag), Some(Req { kind: Kind::Read(st), .. }) if st.pending == 0);
        if !finished {
            return;
        }
        let Some(r) = self.reqs.remove(&tag) else {
            return;
        };
        let Kind::Read(st) = r.kind else {
            return;
        };
        if let Some(e) = st.failed {
            done.complete(tag, IoStatus::err(e));
            return;
        }
        let dst = match inline {
            Some((t, b)) if *t == tag => b.get_mut(..st.buf.len()),
            _ => done.tag_buf(tag).get_mut(..st.buf.len()),
        };
        match dst {
            Some(dst) => {
                dst.copy_from_slice(&st.buf);
                done.complete(tag, IoStatus::ok(r.ok_bytes));
            }
            None => done.complete(tag, IoStatus::err(EINVAL)),
        }
    }

    fn on_append(&mut self, seq: WriteSeq, status: IoStatus, done: &mut dyn Completer) {
        let Some(p) = self.appends.remove(&seq.0) else {
            return;
        };
        let inner = Arc::clone(&self.inner);
        if status.is_ok() {
            match p.record {
                RecordKind::Write => inner.overlay.insert_write(p.range, p.loc, seq),
                RecordKind::Hole => inner.overlay.insert_hole(p.range, seq),
                RecordKind::Mark => unreachable!("a request never maps to a mark"),
            }
        } else {
            inner.poison(seq.0, "journal append completion");
        }
        // Only now: C1 must see the write in the overlay before it is gone
        // from the in-flight set.
        lock(&self.shared.inflight).remove(&seq.0);
        inner.notify_progress();
        let Some(r) = self.reqs.get_mut(&p.tag) else {
            return;
        };
        if r.gen != p.gen {
            return;
        }
        if !status.is_ok() {
            self.fail(p.tag, EIO, done);
            return;
        }
        let fua = r.req.fua && r.req.op == IoOp::Write;
        let Kind::Mutate(m) = &mut r.kind else {
            return;
        };
        m.outstanding = m.outstanding.saturating_sub(1);
        if m.outstanding > 0 || m.next_block < m.end_block {
            return;
        }
        if fua {
            r.kind = Kind::Flush;
            inner.flush.join(Arc::clone(&self.shared), p.tag, p.gen);
            inner.flush.kick(&inner.journal, &inner.stats);
        } else if let Some(r) = self.reqs.remove(&p.tag) {
            self.note_write_ack(&r);
            done.complete(p.tag, IoStatus::ok(r.ok_bytes));
        }
    }

    /// A write is acknowledged: its time in the daemon, from arrival, goes
    /// into the latency histogram (one atomic add on this queue's own lines).
    fn note_write_ack(&self, r: &Req) {
        if r.req.op == IoOp::Write {
            let d = self.inner.clock.monotonic().saturating_sub(r.arrived);
            self.inner.latency.note(usize::from(self.shared.id.0), d);
        }
    }

    fn on_jread(
        &mut self,
        token: Token,
        status: IoStatus,
        data: Vec<u8>,
        done: &mut dyn Completer,
        inline: &mut Inline<'_>,
    ) {
        let Some(j) = self.jreads.remove(&token.0) else {
            return;
        };
        let Some(r) = self.reqs.get_mut(&j.tag) else {
            return;
        };
        if r.gen != j.gen {
            return;
        }
        let req_lba = r.req.offset_bytes / BLOCK;
        if !matches!(r.kind, Kind::Read(_)) {
            return;
        }
        // Take the state out while `map_parts` borrows the queue.
        let Kind::Read(mut st) = std::mem::replace(&mut r.kind, Kind::Taken) else {
            return;
        };
        st.pending = st.pending.saturating_sub(1);
        if status.is_ok() {
            match st.buf.get_mut(j.req_off..j.req_off + data.len()) {
                Some(dst) => dst.copy_from_slice(&data),
                None => st.failed = Some(EIO),
            }
        } else {
            let errno = status.errno().unwrap_or(EIO);
            self.journal_read_failed(j.tag, j.gen, req_lba, j.range, j.write_seq, errno, &mut st);
        }
        if let Some(r) = self.reqs.get_mut(&j.tag) {
            r.kind = Kind::Read(st);
        }
        self.maybe_finish_read(j.tag, done, inline);
    }

    /// A journal read of an overlay piece failed. If the durable tip covers
    /// the piece's record, the record was destaged and possibly reclaimed
    /// after the lookup, and the map holds that version or a newer one
    /// (the overlay is released only after the commit): read the map.
    /// Otherwise the journal is the only copy of an acknowledged write, and
    /// serving the map's older version would be stale data under a success
    /// status: fail the read.
    #[allow(clippy::too_many_arguments)]
    fn journal_read_failed(
        &mut self,
        tag: Tag,
        gen: u64,
        req_lba: u64,
        range: BlockRange,
        write_seq: WriteSeq,
        errno: i32,
        st: &mut ReadState,
    ) {
        if write_seq.0 <= self.inner.durable_cut.load(Ordering::Acquire) {
            self.map_parts(tag, gen, req_lba, range, st);
        } else {
            tracing::error!(
                write_seq = write_seq.0,
                lba = range.lba,
                errno,
                "journal read of an undestaged record failed"
            );
            st.failed = Some(if errno > 0 { errno } else { EIO });
        }
    }

    fn on_deferred(&mut self, d: Deferred, done: &mut dyn Completer, inline: &mut Inline<'_>) {
        match d {
            Deferred::ReadPart {
                tag,
                gen,
                req_off,
                result,
            } => {
                let Some(r) = self.reqs.get_mut(&tag) else {
                    return;
                };
                if r.gen != gen {
                    return;
                }
                let Kind::Read(st) = &mut r.kind else {
                    return;
                };
                st.pending = st.pending.saturating_sub(1);
                match result {
                    Ok(data) => match st.buf.get_mut(req_off..req_off + data.len()) {
                        Some(dst) => dst.copy_from_slice(&data),
                        None => st.failed = Some(EIO),
                    },
                    Err(errno) => st.failed = Some(errno),
                }
                self.maybe_finish_read(tag, done, inline);
            }
            Deferred::Flush { tag, gen, ok } => {
                let matches = self
                    .reqs
                    .get(&tag)
                    .is_some_and(|r| r.gen == gen && matches!(r.kind, Kind::Flush));
                if !matches {
                    return;
                }
                if let Some(r) = self.reqs.remove(&tag) {
                    if ok {
                        self.note_write_ack(&r);
                    }
                    let status = if ok {
                        IoStatus::ok(r.ok_bytes)
                    } else {
                        IoStatus::err(EIO)
                    };
                    done.complete(tag, status);
                }
            }
        }
    }

    /// Collects into `ring_scratch` the parked requests that wait for the
    /// ring, with their arrival and the start of their ring wait: in a FIFO
    /// that holds one refused a ring slot, that one (from its refusal) and the
    /// requests behind it that were never tried (they cannot go before it;
    /// from the later of their arrival and the first refusal of the FIFO).
    /// A request that waits in the gate (`Waiting`) is not one. Only called
    /// while `ring_refused` is not 0.
    fn collect_ring_blocked(&mut self) {
        self.ring_scratch.clear();
        for q in [&self.parked, &self.parked_holes] {
            let first = q
                .iter()
                .filter_map(|t| match self.reqs.get(t) {
                    Some(Req {
                        kind: Kind::Mutate(m),
                        ..
                    }) => m.ring_since,
                    _ => None,
                })
                .min();
            let Some(first) = first else {
                continue;
            };
            for t in q {
                if let Some(Req {
                    kind: Kind::Mutate(m),
                    ..
                }) = self.reqs.get(t)
                {
                    if let Some(since) = m.ring_since {
                        self.ring_scratch.push((*t, m.arrived, since));
                    } else if matches!(m.space, SpaceWait::None) {
                        self.ring_scratch
                            .push((*t, m.arrived, m.arrived.max(first)));
                    }
                }
            }
        }
    }

    /// Adds this queue's change of the count of requests parked for the ring
    /// to the engine's gauge.
    fn set_ring_count(&mut self, n: usize) {
        let n = i64::try_from(n).unwrap_or(i64::MAX);
        if n != self.ring_count {
            self.inner
                .cuts
                .ring_parked_writes
                .fetch_add(n - self.ring_count, Ordering::Relaxed);
            self.ring_count = n;
        }
    }

    /// Judges every parked request that waits for the ring against its
    /// deadline (not only the front of a FIFO): the ones past it fail. Runs
    /// at each wake and on the timer thread's signal.
    fn fail_overdue_ring_waits(&mut self, done: &mut dyn Completer) -> bool {
        // A deadline counts from the later of the arrival and the ring's last
        // progress, so the oldest request is the first one due: when it is
        // not, none is (the common case costs one verdict).
        let lane = usize::from(self.shared.id.0);
        let Some(oldest) = self.ring_scratch.iter().map(|&(_, a, _)| a).min() else {
            return false;
        };
        if self.inner.ring_wait_verdict(oldest, lane).is_none() {
            return false;
        }
        let mut failed = false;
        let mut i = 0;
        while let Some(&(tag, arrived, _)) = self.ring_scratch.get(i) {
            i += 1;
            if let Some(errno) = self.inner.ring_wait_verdict(arrived, lane) {
                self.fail(tag, errno, done);
                failed = true;
            }
        }
        failed
    }

    fn retry_parked(&mut self, done: &mut dyn Completer) -> bool {
        // A stopped or read-only engine fails what it parked at once
        // (ADR-32): nothing would ever admit it.
        let any_parked = !(self.parked.is_empty() && self.parked_holes.is_empty());
        if any_parked && self.inner.read_only() {
            let tags: Vec<Tag> = self
                .parked
                .drain(..)
                .chain(self.parked_holes.drain(..))
                .collect();
            for tag in tags {
                self.fail(tag, EIO, done);
            }
            self.shared.ring_parked.store(false, Ordering::Relaxed);
            self.ring_refused = 0;
            self.set_ring_count(0);
            return true;
        }
        // The waits as of this wake, before anything is admitted: from the
        // start of each request's ring wait (the longest wait counters of the
        // status; the time in the gate before the ring is not in them).
        if any_parked && self.ring_refused > 0 {
            self.collect_ring_blocked();
            let now = self.inner.clock.monotonic();
            let oldest = self
                .ring_scratch
                .iter()
                .map(|&(_, _, start)| now.saturating_sub(start))
                .max();
            if let Some(w) = oldest {
                self.inner.note_ring_wait(w);
            }
        }
        let mut progressed = false;
        for holes in [true, false] {
            loop {
                let q = if holes {
                    &mut self.parked_holes
                } else {
                    &mut self.parked
                };
                let Some(&tag) = q.front() else {
                    break;
                };
                if !self.reqs.contains_key(&tag) {
                    q.pop_front();
                    continue;
                }
                match self.advance(tag, None, done) {
                    Step::Parked => break,
                    Step::Done => {
                        let q = if holes {
                            &mut self.parked_holes
                        } else {
                            &mut self.parked
                        };
                        q.pop_front();
                        progressed = true;
                    }
                }
            }
        }
        // What is still waiting for the ring is judged against its deadline
        // all the same: a request behind the front (a small FUA write that
        // overtook, one queued behind) has no other chance to be seen.
        if self.ring_refused > 0 {
            self.collect_ring_blocked();
            if self.fail_overdue_ring_waits(done) {
                let reqs = &self.reqs;
                self.parked.retain(|t| reqs.contains_key(t));
                self.parked_holes.retain(|t| reqs.contains_key(t));
                if self.ring_refused > 0 {
                    self.collect_ring_blocked();
                } else {
                    self.ring_scratch.clear();
                }
                progressed = true;
            }
        } else {
            self.ring_scratch.clear();
        }
        let ring = self.ring_refused > 0;
        if self.shared.ring_parked.load(Ordering::Relaxed) != ring {
            self.shared.ring_parked.store(ring, Ordering::Relaxed);
        }
        let n = self.ring_scratch.len();
        self.set_ring_count(n);
        progressed
    }

    fn poll_inner(&mut self, done: &mut dyn Completer, mut inline: Inline<'_>) {
        self.shared.wake.drain();
        loop {
            let mut progressed = false;
            let mut jbuf = std::mem::take(&mut self.jbuf);
            self.lane.poll(&mut jbuf);
            for c in jbuf.drain(..) {
                progressed = true;
                match c {
                    JournalCompletion::Append { write_seq, status } => {
                        self.on_append(write_seq, status, done);
                    }
                    JournalCompletion::Read { token, status, buf } => {
                        self.on_jread(token, status, buf, done, &mut inline);
                    }
                }
            }
            self.jbuf = jbuf;
            let mut dbuf = std::mem::take(&mut self.dbuf);
            std::mem::swap(&mut dbuf, &mut *lock(&self.shared.deferred));
            for d in dbuf.drain(..) {
                progressed = true;
                self.on_deferred(d, done, &mut inline);
            }
            self.dbuf = dbuf;
            progressed |= self.retry_parked(done);
            self.inner
                .flush
                .kick(&self.inner.journal, &self.inner.stats);
            if !progressed {
                break;
            }
        }
    }
}

impl Drop for Queue {
    /// A handler dropped with records in flight (the ublk layer should drain
    /// first, W1/W2) would leave their numbers in the in-flight set forever,
    /// and each later C1 would wait out its timeout; their data may never
    /// reach the overlay, so no cut may pass them: they poison the volume.
    fn drop(&mut self) {
        let inflight: Vec<u64> = std::mem::take(&mut *lock(&self.shared.inflight))
            .into_iter()
            .collect();
        if let Some(&first) = inflight.first() {
            self.inner
                .poison(first, "queue handler dropped with writes in flight");
        }
        self.inner.notify_progress();
        // Requests still parked for the ring leave the waiter count.
        for r in self.reqs.values() {
            if let Kind::Mutate(m) = &r.kind {
                if let Some(since) = m.ring_since {
                    self.inner.end_ring_wait(since);
                }
            }
        }
        self.set_ring_count(0);
        let idx = usize::from(self.shared.id.0);
        if let Some(slot) = lock(&self.inner.queues).get_mut(idx) {
            *slot = None;
        }
    }
}

impl QueueHandler for Queue {
    fn on_request(&mut self, req: IoRequest, buf: &mut [u8], done: &mut dyn Completer) {
        let hooks = Arc::clone(&self.inner);
        hooks.points.hooks().queue_thread(true);
        self.handle(req, buf, done);
        // Completions that are already there (an immediate append, a
        // journal read) finish the request inline.
        self.poll_inner(done, Some((req.tag, buf)));
        hooks.points.hooks().queue_thread(false);
    }

    fn poll(&mut self, done: &mut dyn Completer) {
        let hooks = Arc::clone(&self.inner);
        hooks.points.hooks().queue_thread(true);
        self.poll_inner(done, None);
        hooks.points.hooks().queue_thread(false);
    }

    fn wake_fd(&self) -> BorrowedFd<'_> {
        self.shared.wake.as_fd()
    }
}
