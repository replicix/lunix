//! One journal lane: reservation, batch packing, the io_uring write and read
//! paths, completions. Owned by one queue thread; never blocks (its only
//! locks are short critical sections shared with the flush coordinator and
//! reclaim).
//!
//! # Placement
//!
//! A lane keeps one *open batch*: a pooled aligned buffer into which records
//! are packed back to back. `try_reserve` decides where the record goes (the
//! open batch, or a new batch, possibly in the next ring file) and allocates
//! the global `write_seq` in the same step, so `append` cannot fail for lack of
//! room. Because a lane has at most one outstanding slot, reservation order is
//! append order, which keeps `write_seq` rising along the file.
//!
//! # When a batch is written
//!
//! A batch is submitted when the next record does not fit (so it is full), by
//! an `append` that finds no batch in flight, and by a `poll` that finds fewer
//! than `max_inflight`. An idle lane therefore writes every record at once; a
//! burst writes its first record alone and packs the rest into the batch the
//! queue thread's next `poll` (or the first completion) sends out.

use std::collections::BTreeSet;
use std::os::fd::AsRawFd;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use io_uring::{opcode, types, IoUring};
use lunix_format::journal::{Record, RecordHeader, RecordKind, RECORD_HEADER_LEN};
use lunix_format::WriteSeq;
use lunix_iface::block::IoStatus;
use lunix_iface::journal::{
    record_bytes, Appended, JournalCompletion, JournalLane, JournalLoc, LaneId, Slot, SlotOutcome,
    Token,
};
use lunix_iface::wake::WakeFd;
use lunix_iface::{Error, Result};

use crate::aligned::AlignedBuf;
use crate::config::{TraceEvent, BATCH_CAP};
use crate::ring::{lock, RingFile, Role};
use crate::shared::Shared;
use crate::sys::{round4k, B};

const READ_BIT: u64 = 1 << 63;
const RING_ENTRIES: u32 = 512;
const MAX_CACHED_READ_BUFS: usize = 8;

struct Active {
    slot: usize,
    ordinal: u32,
    gen: u64,
    file: Arc<RingFile>,
    /// Where the next batch starts (a block multiple), once the open batch is
    /// sealed.
    next_off: u64,
}

struct OpenBatch {
    buf: usize,
    file_off: u64,
    /// Bytes packed so far, including a reserved but not yet appended record.
    len: usize,
    /// `(write_seq, encoded length)` of the appended records.
    seqs: Vec<(u64, u32)>,
}

struct Outstanding {
    seq: u64,
    token: u64,
    kind: RecordKind,
    blocks: u32,
    rec_len: usize,
    buf: usize,
    buf_off: usize,
    file_off: u64,
    gen: u64,
    ordinal: u32,
}

struct Batch {
    buf: usize,
    len: usize,
    slot: usize,
    file: Arc<RingFile>,
    seqs: Vec<(u64, u32)>,
}

struct ReadReq {
    abuf: AlignedBuf,
    buf: Vec<u8>,
    token: Token,
    delta: usize,
}

pub(crate) struct FileLane {
    sh: Arc<Shared>,
    id: u16,
    ring: IoUring,
    _wake: WakeFd,
    pool: Vec<AlignedBuf>,
    free_bufs: Vec<usize>,
    fixed: bool,
    active: Option<Active>,
    open: Option<OpenBatch>,
    outstanding: Option<Outstanding>,
    batches: Vec<Option<Batch>>,
    batch_free: Vec<usize>,
    reads: Vec<Option<ReadReq>>,
    read_free: Vec<usize>,
    read_cache: Vec<AlignedBuf>,
    unfinished: BTreeSet<u64>,
    last_accepted: u64,
    out: Vec<JournalCompletion>,
    scratch: Vec<u8>,
    cq: Vec<(u64, i32)>,
    inflight_writes: usize,
    inflight_reads: usize,
    next_token: u64,
}

impl FileLane {
    /// Builds the lane's ring and pool. The lane resumes in the ring's active
    /// file at its recorded offset, or takes the first free file.
    pub(crate) fn new(sh: Arc<Shared>, id: u16, wake: WakeFd) -> Result<Self> {
        let ring = IoUring::new(RING_ENTRIES).map_err(Error::Io)?;
        ring.submitter()
            .register_eventfd(wake.raw_fd())
            .map_err(Error::Io)?;
        let mut pool = Vec::with_capacity(sh.cfg.pool_buffers);
        for _ in 0..sh.cfg.pool_buffers {
            pool.push(AlignedBuf::zeroed(BATCH_CAP).map_err(Error::Io)?);
        }
        let iovs: Vec<libc::iovec> = pool
            .iter()
            .map(|b| libc::iovec {
                iov_base: b.as_ptr().cast_mut().cast(),
                iov_len: b.len(),
            })
            .collect();
        // SAFETY: every iovec describes a live, uniquely owned buffer of the
        // pool; the pool outlives the ring (both live in `FileLane`, and the
        // ring is dropped first because it is declared before the pool is
        // moved out), and its buffers never move. Registration can fail when
        // RLIMIT_MEMLOCK is small; the lane then uses plain writes.
        let fixed = unsafe { ring.submitter().register_buffers(&iovs) }.is_ok();
        let free_bufs = (0..pool.len()).rev().collect();

        let active = {
            let ring_state = &sh.rings.lanes[usize::from(id)];
            let mut inner = lock(&ring_state.inner);
            let slot = match inner.active {
                Some(s) => s,
                None => {
                    let s = inner
                        .free
                        .pop_front()
                        .ok_or(Error::invalid("lane has no usable ring file"))?;
                    inner.slots[s].role = Role::Active;
                    inner.active = Some(s);
                    inner.resume_off = B;
                    s
                }
            };
            Active {
                slot,
                ordinal: inner.slots[slot].ordinal,
                gen: inner.slots[slot].gen,
                file: Arc::clone(&ring_state.files[slot]),
                next_off: inner.resume_off,
            }
        };

        Ok(Self {
            sh,
            id,
            ring,
            _wake: wake,
            pool,
            free_bufs,
            fixed,
            active: Some(active),
            open: None,
            outstanding: None,
            batches: Vec::new(),
            batch_free: Vec::new(),
            reads: Vec::new(),
            read_free: Vec::new(),
            read_cache: Vec::new(),
            unfinished: BTreeSet::new(),
            last_accepted: 0,
            out: Vec::new(),
            scratch: Vec::with_capacity(BATCH_CAP),
            cq: Vec::new(),
            inflight_writes: 0,
            inflight_reads: 0,
            next_token: 1,
        })
    }

    fn active(&self) -> &Active {
        // The lane always has an active file once constructed.
        self.active.as_ref().unwrap_or_else(|| unreachable!())
    }

    /// Decides where a record of `rec_len` bytes goes. `Ok(None)` is `Full`.
    fn place(&mut self, rec_len: usize) -> Result<Option<(usize, usize, u64)>> {
        let file_len = self.sh.rings.file_len;
        if let Some(open) = &mut self.open {
            let new_len = open.len + rec_len;
            if new_len <= BATCH_CAP && open.file_off + round4k(new_len as u64) <= file_len {
                let at = (open.buf, open.len, open.file_off + open.len as u64);
                open.len = new_len;
                return Ok(Some(at));
            }
        }
        if self.free_bufs.is_empty() {
            return Ok(None);
        }
        let next_start = match &self.open {
            Some(o) => o.file_off + round4k(o.len as u64),
            None => self.active().next_off,
        };
        let fits = next_start + round4k(rec_len as u64) <= file_len;
        let sh = Arc::clone(&self.sh);
        let ring = &sh.rings.lanes[usize::from(self.id)];
        if !fits && lock(&ring.inner).free.is_empty() {
            return Ok(None);
        }
        if self.open.is_some() {
            self.submit_open()?;
        }
        if !fits {
            let (slot, ordinal, gen, file) = {
                let mut inner = lock(&ring.inner);
                let prev = self.active().slot;
                let Some(slot) = inner.free.pop_front() else {
                    return Ok(None);
                };
                if inner.slots[prev].role == Role::Active {
                    inner.slots[prev].role = Role::Used;
                }
                inner.slots[slot].role = Role::Active;
                inner.active = Some(slot);
                inner.resume_off = B;
                (
                    slot,
                    inner.slots[slot].ordinal,
                    inner.slots[slot].gen,
                    Arc::clone(&ring.files[slot]),
                )
            };
            self.active = Some(Active {
                slot,
                ordinal,
                gen,
                file,
                next_off: B,
            });
        }
        let Some(buf) = self.free_bufs.pop() else {
            return Ok(None);
        };
        let start = self.active().next_off;
        self.open = Some(OpenBatch {
            buf,
            file_off: start,
            len: rec_len,
            seqs: Vec::new(),
        });
        Ok(Some((buf, 0, start)))
    }

    /// Undoes the placement of the (last, unwritten) record `o`.
    fn unreserve(&mut self, o: &Outstanding) {
        if let Some(open) = &mut self.open {
            open.len = open.len.saturating_sub(o.rec_len);
            if open.len == 0 {
                let buf = open.buf;
                self.open = None;
                self.free_bufs.push(buf);
            }
        }
        let file = &self.active().file;
        let _ = file
            .rec_bytes
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
                Some(v.saturating_sub(o.rec_len as u64))
            });
        lock(&self.sh.flights[usize::from(self.id)])
            .pending
            .remove(&o.seq);
    }

    /// Gives up the reservation `o` after a failure. The journal is poisoned
    /// **before** the sequence number leaves the pending set: a group flush that
    /// sees the lanes clear must already see the poison, or it could complete
    /// over a sequence number that was never written.
    fn abort(&mut self, o: &Outstanding, err: Error) -> Error {
        self.sh.poison(o.seq);
        self.unreserve(o);
        err
    }

    /// Writes the open batch if the policy allows: from `append` only when no
    /// batch is in flight (an idle lane writes at once), from `poll` while
    /// fewer than `max_inflight` are.
    fn maybe_submit(&mut self, from_append: bool) -> Result<()> {
        let limit = if from_append {
            1
        } else {
            self.sh.cfg.max_inflight
        };
        let ready = self.outstanding.is_none()
            && self.inflight_writes < limit
            && self.open.as_ref().is_some_and(|o| !o.seqs.is_empty());
        if ready {
            self.submit_open()?;
        }
        Ok(())
    }

    fn push_sqe(&mut self, sqe: &io_uring::squeue::Entry) -> std::io::Result<()> {
        // SAFETY: the entry refers to memory (a pool buffer or a read buffer)
        // that stays alive and unmoved until its completion is reaped: both
        // are owned by `self` and only released in `finish_write` and
        // `finish_read` (or leaked on a failed submit).
        let first = unsafe { self.ring.submission().push(sqe) };
        if first.is_ok() {
            return Ok(());
        }
        self.ring.submit()?;
        // SAFETY: as above.
        unsafe { self.ring.submission().push(sqe) }
            .map_err(|_| std::io::Error::from_raw_os_error(libc::EBUSY))
    }

    /// Seals and queues the open batch.
    fn submit_open(&mut self) -> Result<()> {
        let Some(open) = self.open.take() else {
            return Ok(());
        };
        if open.seqs.is_empty() {
            self.free_bufs.push(open.buf);
            return Ok(());
        }
        let len = round4k(open.len as u64) as usize;
        self.pool[open.buf].as_mut_slice()[open.len..len].fill(0);
        let (slot, file, fd) = {
            let a = self.active.as_mut().unwrap_or_else(|| unreachable!());
            a.next_off = open.file_off + len as u64;
            (a.slot, Arc::clone(&a.file), a.file.file.as_raw_fd())
        };
        let ptr = self.pool[open.buf].as_ptr();
        let id = match self.batch_free.pop() {
            Some(i) => i,
            None => {
                self.batches.push(None);
                self.batches.len() - 1
            }
        };
        let sqe = if self.fixed {
            opcode::WriteFixed::new(types::Fd(fd), ptr, len as u32, open.buf as u16)
                .offset(open.file_off)
                .build()
        } else {
            opcode::Write::new(types::Fd(fd), ptr, len as u32)
                .offset(open.file_off)
                .build()
        }
        .user_data(id as u64);
        let batch = Batch {
            buf: open.buf,
            len,
            slot,
            file,
            seqs: open.seqs,
        };
        self.sh.cfg.emit(|| TraceEvent::Write {
            lane: self.id,
            fd,
            offset: open.file_off,
            len,
            addr: ptr as usize,
        });
        if let Err(e) = self
            .push_sqe(&sqe)
            .and_then(|()| self.ring.submit().map(|_| ()))
        {
            // The entry may still sit in the submission queue: leak the
            // buffer (do not reuse it) and fail the batch.
            self.fail_batch(batch, e.raw_os_error().unwrap_or(libc::EIO), false);
            self.batch_free.push(id);
            return Err(Error::Io(e));
        }
        batch.file.inflight.fetch_add(1, Ordering::AcqRel);
        self.inflight_writes += 1;
        self.batches[id] = Some(batch);
        lunix_iface::crashpoint!("journal.after_append_submit");
        Ok(())
    }

    /// A batch whose write failed or could not be queued: poison at its
    /// lowest `write_seq`, complete its records with an error.
    fn fail_batch(&mut self, batch: Batch, errno: i32, recycle: bool) {
        let first = batch.seqs.first().map_or(0, |s| s.0);
        self.sh.poison(first);
        {
            let mut fl = lock(&self.sh.flights[usize::from(self.id)]);
            for &(seq, _) in &batch.seqs {
                fl.pending.remove(&seq);
            }
        }
        for &(seq, _) in &batch.seqs {
            self.unfinished.remove(&seq);
            self.out.push(JournalCompletion::Append {
                write_seq: WriteSeq(seq),
                status: IoStatus::err(errno),
            });
        }
        if recycle {
            self.free_bufs.push(batch.buf);
        }
        self.sh.coord.notify();
    }

    fn finish_write(&mut self, id: usize, res: i32) {
        let Some(batch) = self.batches.get_mut(id).and_then(Option::take) else {
            return;
        };
        self.batch_free.push(id);
        self.inflight_writes -= 1;
        batch.file.inflight.fetch_sub(1, Ordering::AcqRel);
        if usize::try_from(res).is_ok_and(|n| n == batch.len) {
            {
                let mut fl = lock(&self.sh.flights[usize::from(self.id)]);
                for &(seq, _) in &batch.seqs {
                    fl.pending.remove(&seq);
                }
                fl.dirty.insert(batch.slot);
                if let Some(&(first, _)) = batch.seqs.first() {
                    fl.unsynced_min = fl.unsynced_min.min(first);
                }
            }
            for &(seq, rec) in &batch.seqs {
                self.unfinished.remove(&seq);
                self.out.push(JournalCompletion::Append {
                    write_seq: WriteSeq(seq),
                    status: IoStatus::ok(rec),
                });
            }
            self.free_bufs.push(batch.buf);
            self.sh.coord.notify();
        } else {
            let errno = if res < 0 { -res } else { libc::EIO };
            self.fail_batch(batch, errno, true);
        }
    }

    fn finish_read(&mut self, id: usize, res: i32) {
        let Some(req) = self.reads.get_mut(id).and_then(Option::take) else {
            return;
        };
        self.read_free.push(id);
        self.inflight_reads -= 1;
        let ReadReq {
            abuf,
            mut buf,
            token,
            delta,
        } = req;
        let want = delta + buf.len();
        let status = if usize::try_from(res).is_ok_and(|n| n >= want) {
            buf.copy_from_slice(&abuf.as_slice()[delta..want]);
            IoStatus::ok(buf.len() as u32)
        } else if res < 0 {
            IoStatus::err(-res)
        } else {
            IoStatus::err(libc::EIO)
        };
        if self.read_cache.len() < MAX_CACHED_READ_BUFS {
            self.read_cache.push(abuf);
        }
        self.out
            .push(JournalCompletion::Read { token, status, buf });
    }

    fn reap(&mut self) {
        self.cq.clear();
        for cqe in self.ring.completion() {
            self.cq.push((cqe.user_data(), cqe.result()));
        }
        let cq = std::mem::take(&mut self.cq);
        for &(ud, res) in &cq {
            if ud & READ_BIT != 0 {
                self.finish_read((ud & !READ_BIT) as usize, res);
            } else {
                self.finish_write(ud as usize, res);
            }
        }
        self.cq = cq;
    }
}

impl JournalLane for FileLane {
    fn lane(&self) -> LaneId {
        LaneId(self.id)
    }

    fn try_reserve(&mut self, kind: RecordKind, blocks: u32) -> Result<SlotOutcome> {
        if self.sh.poisoned_at().is_some() {
            return Err(Shared::poisoned_error());
        }
        if let Some(o) = &self.outstanding {
            self.sh.poison(o.seq);
            return Err(Error::invalid("a slot of this lane is still outstanding"));
        }
        let well_formed = match kind {
            RecordKind::Write => (1..=lunix_format::journal::MAX_WRITE_BLOCKS).contains(&blocks),
            RecordKind::Hole => blocks >= 1,
        };
        if !well_formed {
            return Err(Error::invalid("block count out of range"));
        }
        let rec_len = record_bytes(kind, blocks) as usize;
        let Some((buf, buf_off, file_off)) = self.place(rec_len)? else {
            return Ok(SlotOutcome::Full);
        };
        let seq = {
            let mut fl = lock(&self.sh.flights[usize::from(self.id)]);
            let s = self.sh.counter.fetch_add(1, Ordering::SeqCst) + 1;
            fl.pending.insert(s);
            s
        };
        let (ordinal, gen) = {
            let a = self.active();
            a.file.max_seq.fetch_max(seq, Ordering::AcqRel);
            a.file.rec_bytes.fetch_add(rec_len as u64, Ordering::AcqRel);
            a.file
                .hi_off
                .fetch_max(round4k(file_off + rec_len as u64), Ordering::AcqRel);
            (a.ordinal, a.gen)
        };
        let token = (u64::from(self.id) << 48) | self.next_token;
        self.next_token += 1;
        self.outstanding = Some(Outstanding {
            seq,
            token,
            kind,
            blocks,
            rec_len,
            buf,
            buf_off,
            file_off,
            gen,
            ordinal,
        });
        Ok(SlotOutcome::Reserved(Slot::new(
            WriteSeq(seq),
            kind,
            blocks,
            token,
        )))
    }

    fn append(&mut self, slot: Slot, lba: u64, data: &[u8]) -> Result<Appended> {
        let parts = slot.take();
        let ours = self
            .outstanding
            .as_ref()
            .is_some_and(|o| o.token == parts.token && o.seq == parts.write_seq.0);
        if !ours {
            let at = self
                .outstanding
                .as_ref()
                .map_or(parts.write_seq.0, |o| o.seq.min(parts.write_seq.0));
            self.sh.poison(at);
            return Err(Error::invalid("slot was not issued by this lane"));
        }
        let Some(o) = self.outstanding.take() else {
            unreachable!("checked above");
        };
        let want = match o.kind {
            RecordKind::Write => o.blocks as usize * 4096,
            RecordKind::Hole => 0,
        };
        if data.len() != want || lba.checked_add(u64::from(o.blocks)).is_none() {
            return Err(self.abort(&o, Error::invalid("data does not match the slot")));
        }
        if self.sh.fail_any.load(Ordering::Relaxed) && lock(&self.sh.fail_next).remove(&self.id) {
            return Err(self.abort(
                &o,
                Error::Io(std::io::Error::other("injected append failure")),
            ));
        }
        if self.sh.poisoned_at().is_some() {
            return Err(self.abort(&o, Shared::poisoned_error()));
        }
        self.scratch.clear();
        let rec = Record {
            header: RecordHeader {
                kind: o.kind,
                write_seq: WriteSeq(o.seq),
                epoch: 0,
                lba,
                len: o.blocks,
                gen: o.gen,
                off: o.file_off,
            },
            data,
        };
        if let Err(e) = rec.encode(&mut self.scratch) {
            return Err(self.abort(&o, Error::Format(e)));
        }
        debug_assert_eq!(self.scratch.len(), o.rec_len);
        self.pool[o.buf].as_mut_slice()[o.buf_off..o.buf_off + o.rec_len]
            .copy_from_slice(&self.scratch);
        if let Some(open) = &mut self.open {
            open.seqs.push((o.seq, o.rec_len as u32));
        }
        self.unfinished.insert(o.seq);
        self.last_accepted = o.seq;
        self.maybe_submit(true)?;
        Ok(Appended {
            write_seq: WriteSeq(o.seq),
            loc: JournalLoc {
                lane: LaneId(self.id),
                ordinal: o.ordinal,
                offset: o.file_off + RECORD_HEADER_LEN as u64,
            },
        })
    }

    fn submit_read(&mut self, loc: JournalLoc, buf: Vec<u8>, token: Token) -> Result<()> {
        let rings = &self.sh.rings;
        if rings.lanes.get(usize::from(loc.lane.0)).is_none() {
            return Err(Error::invalid(
                "journal location names a lane that does not exist",
            ));
        }
        let len = buf.len() as u64;
        let end = loc
            .offset
            .checked_add(len)
            .ok_or(Error::invalid("journal location overflows"))?;
        if loc.offset < B || end > rings.file_len {
            return Err(Error::invalid("journal location outside the ring file"));
        }
        let Some(file) = rings.find_in_use(loc.lane.0, loc.ordinal) else {
            // Reclaimed (or never in use): an error completion, as documented.
            self.out.push(JournalCompletion::Read {
                token,
                status: IoStatus::err(libc::ENOENT),
                buf,
            });
            return Ok(());
        };
        if buf.is_empty() {
            self.out.push(JournalCompletion::Read {
                token,
                status: IoStatus::ok(0),
                buf,
            });
            return Ok(());
        }
        let a0 = crate::sys::floor4k(loc.offset);
        let a1 = round4k(end);
        let need = (a1 - a0) as usize;
        let mut abuf = match self.read_cache.iter().position(|b| b.len() >= need) {
            Some(i) => self.read_cache.swap_remove(i),
            None => AlignedBuf::zeroed(need).map_err(Error::Io)?,
        };
        let ptr = abuf.as_mut_ptr();
        let id = match self.read_free.pop() {
            Some(i) => i,
            None => {
                self.reads.push(None);
                self.reads.len() - 1
            }
        };
        let sqe = opcode::Read::new(types::Fd(file.file.as_raw_fd()), ptr, need as u32)
            .offset(a0)
            .build()
            .user_data(READ_BIT | id as u64);
        self.reads[id] = Some(ReadReq {
            abuf,
            buf,
            token,
            delta: (loc.offset - a0) as usize,
        });
        self.inflight_reads += 1;
        if let Err(e) = self
            .push_sqe(&sqe)
            .and_then(|()| self.ring.submit().map(|_| ()))
        {
            // Nothing was queued (or it will complete harmlessly into a buffer
            // we keep): hand the buffer back with an error.
            if let Some(req) = self.reads[id].take() {
                self.inflight_reads -= 1;
                self.out.push(JournalCompletion::Read {
                    token,
                    status: IoStatus::err(e.raw_os_error().unwrap_or(libc::EIO)),
                    buf: req.buf,
                });
                // The aligned buffer may still be referenced by a queued
                // entry: leak it rather than reuse it.
                std::mem::forget(req.abuf);
            }
            self.read_free.push(id);
        }
        Ok(())
    }

    fn poll(&mut self, out: &mut Vec<JournalCompletion>) {
        self.reap();
        // A failure here is recorded by `fail_batch` (poison and error
        // completions); there is nobody to return it to.
        let _ = self.maybe_submit(false);
        out.append(&mut self.out);
    }

    fn completed_watermark(&self) -> WriteSeq {
        WriteSeq(match self.unfinished.first() {
            Some(&m) => m - 1,
            None => self.last_accepted,
        })
    }
}

impl Drop for FileLane {
    fn drop(&mut self) {
        if let Some(o) = self.outstanding.take() {
            // A slot nobody appended: nothing may be promised past it.
            self.sh.poison(o.seq);
            self.unreserve(&o);
        }
        let _ = self.submit_open();
        while self.inflight_writes + self.inflight_reads > 0 {
            if self.ring.submit_and_wait(1).is_err() {
                break;
            }
            self.reap();
        }
        let ring = &self.sh.rings.lanes[usize::from(self.id)];
        if let Some(a) = &self.active {
            let mut inner = lock(&ring.inner);
            inner.resume_off = a.next_off;
        }
        if let Some(open) = lock(&self.sh.st).lane_open.get_mut(usize::from(self.id)) {
            *open = false;
        }
    }
}
