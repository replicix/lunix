//! The resident journal window and the read-around (ADR-33, T1 and T2).
//!
//! # What it is
//!
//! A cache of bytes that are already in the ring files. It never changes what
//! is durable, when, or what a crash leaves: the acknowledgement, the group
//! flush, FUA, the C6 point and recovery do not look at it.
//!
//! - **Written bytes (T1).** A lane copies each batch's record bytes when it
//!   seals the batch and indexes the copy when the batch write completes,
//!   before the `Append` completions are delivered. The index is per lane,
//!   keyed by `(ordinal, file offset)` of the batch start. A hit is therefore
//!   exactly the bytes handed to the write, and a ring block damaged after the
//!   write cannot reach a reader through it. Entries leave when reclaim
//!   starts to recycle their ring file.
//! - **Read-around (T2).** A miss in `read_at` (after a restart, past the
//!   cap) reads the 1 MiB-aligned window of the ring file around the
//!   location, once per window however many threads miss in it, and parses it
//!   with the recovery reader's record checks: a record is accepted only if
//!   its header carries the file's generation and its own offset and its
//!   crc32c over header and data verifies, and a lost record is
//!   resynchronised past 8 bytes at a time. It does **not** apply the
//!   reader's `write_seq` order or its stop at a zero block, so it also
//!   accepts valid records beyond a recovery stop point that the wipe will
//!   zero (see "Wipes"). Only accepted records become resident
//!   (within the cap) or are served; a damaged record fails the read with
//!   [`Error::Corrupt`] and nothing of it is copied. A record cut by the edge
//!   of the window is completed with one more read (forward for a record that
//!   starts inside the window, backward for one that starts before it).
//!
//! Nothing is read ahead of need and nothing is read back after a write.
//!
//! # What is not checked, and what a small cap costs
//!
//! A guest read (`submit_read`) that misses the window is the single
//! asynchronous read of its piece, **without** a checksum: ADR-33 adds no read
//! anywhere else, and the bytes of a guest read never reach a chunk. Only
//! `read_at` (destage, recovery replay) checks.
//!
//! Past the cap every `read_at` miss in a window that was not made resident
//! reads and checks the whole 1 MiB again (the threads that miss at the same
//! moment share one read). For small records that is up to 256 times the
//! bytes the old per-piece read took: a `journal_resident_max` below the
//! backlog a cut reads can make destage slower than before the window. The
//! default (`journal_max`) never binds.
//!
//! # Wipes
//!
//! Recovery plans a wipe of what lies beyond its stop point, and the first
//! lane open runs it. [`Window::clear`] drops every entry and bumps an epoch
//! that a read-around captures before it reads: its records are offered to the
//! window only under that epoch, so nothing read before the wipe is indexed
//! after it. Independently, a batch whose write completes evicts every entry
//! it overlaps even when the cap did not admit its copy, so no entry ever
//! overlaps bytes written later.
//!
//! # Memory
//!
//! `resident` counts the bytes of the index plus the copies of batches in
//! flight. A batch (or a record) that would pass the cap is not admitted; the
//! oldest entries stay, since the next cut needs them. The memory is plain
//! heap, not locked.
//!
//! # Locking
//!
//! One mutex per lane around its index, never held across I/O. Lock order:
//! index, then a ring's `inner`; reclaim drops `inner` before it releases a
//! file's entries, so the two never nest the other way.
//!
//! # Records and record starts
//!
//! A record start is 8-byte aligned in the format; this writer produces only
//! multiples of 64 (header 64, data in blocks), so a header never straddles a
//! 4 KiB boundary. The parser does not rely on it: a header cut by the end of
//! the window is read in with the completing read.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use lunix_format::journal::{
    Record, RecordKind, BLOCK, MAX_WRITE_BLOCKS, RECORD_HEADER_LEN, RECORD_MAGIC,
};
use lunix_iface::journal::JournalLoc;
use lunix_iface::{Error, Result};

use crate::aligned::AlignedBuf;
use crate::ring::{io_err, lock, RingFile, Rings};
use crate::sys::{self, round4k, B};

/// The read-around window: one aligned megabyte of a ring file.
pub(crate) const READ_AROUND: u64 = 1 << 20;
/// The longest encoded record: header plus 256 blocks.
const MAX_ENC: u64 = RECORD_HEADER_LEN as u64 + MAX_WRITE_BLOCKS as u64 * BLOCK as u64;
const HDR: usize = RECORD_HEADER_LEN;

type Lane = BTreeMap<(u32, u64), Arc<[u8]>>;
type FlightKey = (u16, u32, u64);

/// Counters of the resident window (ADR-33).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct WindowStats {
    /// Bytes resident now (the index and the batch copies in flight).
    pub resident_bytes: u64,
    /// The cap on `resident_bytes`.
    pub cap_bytes: u64,
    /// Reads (`read_at`, `submit_read`) served from the window.
    pub hits: u64,
    /// Reads that were not (served from the file).
    pub misses: u64,
    /// Batches not admitted because the cap was reached.
    pub batches_skipped: u64,
    /// 1 MiB read-around reads issued.
    pub window_reads: u64,
    /// Reads that completed a record cut by a window edge.
    pub completing_reads: u64,
    /// Reads failed because the record's checksum did not verify (or no valid
    /// record covered the location).
    pub check_failures: u64,
}

/// The window of one journal.
pub(crate) struct Window {
    cap: u64,
    resident: AtomicU64,
    /// Bumped by [`Window::clear`]: a read-around that began before it may
    /// not insert after it.
    epoch: AtomicU64,
    lanes: Vec<Mutex<Lane>>,
    flights: Mutex<HashMap<FlightKey, Arc<Flight>>>,
    hits: AtomicU64,
    misses: AtomicU64,
    skipped: AtomicU64,
    window_reads: AtomicU64,
    completing_reads: AtomicU64,
    check_failures: AtomicU64,
}

/// What a read-around needs to know about its target.
#[derive(Clone, Copy)]
struct Cx<'a> {
    rings: &'a Rings,
    file: &'a RingFile,
    gen: u64,
    loc: JournalLoc,
    /// The wipe epoch when the read began.
    epoch: u64,
}

/// One read-around in progress; the result is `Some(None)` when it failed.
struct Flight {
    done: Mutex<Option<Option<Arc<Parsed>>>>,
    cv: Condvar,
}

impl Flight {
    fn wait(&self) -> Option<Arc<Parsed>> {
        let mut g = lock(&self.done);
        loop {
            if let Some(r) = g.as_ref() {
                return r.clone();
            }
            g = self
                .cv
                .wait(g)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }
}

/// The leader of a flight; ends it (with `None`) when dropped unfinished.
struct Lead<'a> {
    win: &'a Window,
    key: FlightKey,
    flight: Arc<Flight>,
    finished: bool,
}

impl Lead<'_> {
    fn end(&mut self, result: Option<Arc<Parsed>>) {
        if self.finished {
            return;
        }
        self.finished = true;
        lock(&self.win.flights).remove(&self.key);
        *lock(&self.flight.done) = Some(result);
        self.flight.cv.notify_all();
    }
}

impl Drop for Lead<'_> {
    fn drop(&mut self) {
        self.end(None);
    }
}

enum Claim<'a> {
    Lead(Lead<'a>),
    Follow(Arc<Flight>),
}

/// What one window read yielded.
struct Parsed {
    /// The wipe epoch when this window was read.
    epoch: u64,
    /// File offset of `raw[0]`.
    base: u64,
    raw: AlignedBuf,
    /// Accepted data records `(start, bytes)` by start.
    recs: Vec<(u64, Arc<[u8]>)>,
    /// A header that matched but whose record runs past the window.
    tail: Option<u64>,
    /// Headers that matched (generation, own offset) whose record failed its
    /// checksum: `(start, encoded length)`.
    bad: Vec<(u64, u64)>,
}

/// The result of scanning a byte range for records.
#[derive(Default)]
struct Scan {
    /// Accepted data records `(start, encoded length)`.
    recs: Vec<(u64, usize)>,
    tail: Option<u64>,
    bad: Vec<(u64, u64)>,
}

fn u64_at(b: &[u8], o: usize) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[o..o + 8]);
    u64::from_le_bytes(a)
}

/// The kind and encoded length of the record whose header is `h` (at least
/// `HDR` bytes) at file offset `q`, when its magic, generation and own offset
/// match; `None` otherwise.
fn peek(h: &[u8], gen: u64, q: u64) -> Option<(RecordKind, u64)> {
    if h[..4] != *RECORD_MAGIC || u64_at(h, 40) != gen || u64_at(h, 48) != q {
        return None;
    }
    let mut l = [0u8; 4];
    l.copy_from_slice(&h[32..36]);
    let len = u32::from_le_bytes(l);
    match h[5] {
        1 if (1..=MAX_WRITE_BLOCKS).contains(&len) => Some((
            RecordKind::Write,
            HDR as u64 + u64::from(len) * BLOCK as u64,
        )),
        2 if len >= 1 => Some((RecordKind::Hole, HDR as u64)),
        3 if len == 0 => Some((RecordKind::Mark, HDR as u64)),
        _ => None,
    }
}

/// Scans `buf` (file offset `base`) from file offset `from` the way the
/// normative reader does: a record is where magic, generation, own offset
/// and checksum agree, and the reader resynchronises 8 bytes at a time.
fn scan_records(buf: &[u8], base: u64, gen: u64, from: u64) -> Scan {
    let mut out = Scan::default();
    let mut p = usize::try_from(from - base).unwrap_or(usize::MAX);
    while p.saturating_add(4) <= buf.len() {
        if buf[p..p + 4] != *RECORD_MAGIC {
            p += 8;
            continue;
        }
        let q = base + p as u64;
        if p + HDR > buf.len() {
            out.tail = Some(q);
            break;
        }
        let Some((kind, enc)) = peek(&buf[p..p + HDR], gen, q) else {
            p += 8;
            continue;
        };
        let enc_u = enc as usize;
        if p + enc_u > buf.len() {
            out.tail = Some(q);
            break;
        }
        match Record::parse(&buf[p..p + enc_u]) {
            Ok((_, used)) if used == enc_u => {
                if kind == RecordKind::Write {
                    out.recs.push((q, enc_u));
                }
                p += enc_u;
            }
            _ => {
                out.bad.push((q, enc));
                p += 8;
            }
        }
    }
    out
}

fn covers(start: u64, enc: u64, t: u64, end: u64) -> bool {
    t >= start + HDR as u64 && end <= start + enc
}

fn corrupt(detail: &'static str) -> Error {
    Error::Corrupt {
        what: "journal record",
        detail,
    }
}

impl Parsed {
    fn find(&self, t: u64, end: u64) -> Option<(u64, &Arc<[u8]>)> {
        let i = self.recs.partition_point(|(s, _)| *s <= t).checked_sub(1)?;
        let (s, b) = &self.recs[i];
        covers(*s, b.len() as u64, t, end).then_some((*s, b))
    }

    fn bad_covers(&self, t: u64, end: u64) -> bool {
        self.bad.iter().any(|&(q, enc)| q < t && end <= q + enc)
    }
}

impl Window {
    pub(crate) fn new(lanes: usize, cap: u64) -> Self {
        Self {
            cap,
            resident: AtomicU64::new(0),
            epoch: AtomicU64::new(0),
            lanes: (0..lanes).map(|_| Mutex::new(BTreeMap::new())).collect(),
            flights: Mutex::new(HashMap::new()),
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            skipped: AtomicU64::new(0),
            window_reads: AtomicU64::new(0),
            completing_reads: AtomicU64::new(0),
            check_failures: AtomicU64::new(0),
        }
    }

    pub(crate) fn stats(&self) -> WindowStats {
        let r = Ordering::Relaxed;
        WindowStats {
            resident_bytes: self.resident.load(r),
            cap_bytes: self.cap,
            hits: self.hits.load(r),
            misses: self.misses.load(r),
            batches_skipped: self.skipped.load(r),
            window_reads: self.window_reads.load(r),
            completing_reads: self.completing_reads.load(r),
            check_failures: self.check_failures.load(r),
        }
    }

    /// Reserves `n` bytes under the cap.
    fn try_reserve(&self, n: u64) -> bool {
        self.resident
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |r| {
                r.checked_add(n).filter(|&v| v <= self.cap)
            })
            .is_ok()
    }

    fn release(&self, n: u64) {
        self.resident.fetch_sub(n, Ordering::AcqRel);
    }

    /// A lane seals a batch of `n` record bytes: true when its copy is
    /// admitted (the reservation is then owned by the batch until
    /// [`Self::insert_batch`] or [`Self::drop_reservation`]).
    pub(crate) fn reserve_batch(&self, n: u64) -> bool {
        if self.try_reserve(n) {
            true
        } else {
            self.skipped.fetch_add(1, Ordering::Relaxed);
            false
        }
    }

    /// A batch whose write failed gives its reservation back.
    pub(crate) fn drop_reservation(&self, n: u64) {
        self.release(n);
    }

    /// Removes every entry that overlaps `[start, end)` of one file and
    /// returns them, to be dropped by the caller outside the lane's lock. The
    /// bytes are released here.
    fn remove_overlaps(
        &self,
        map: &mut Lane,
        ordinal: u32,
        start: u64,
        end: u64,
    ) -> Vec<Arc<[u8]>> {
        let lo = match map.range(..(ordinal, start)).next_back() {
            Some((&(o, s), b)) if o == ordinal && s + b.len() as u64 > start => (o, s),
            _ => (ordinal, start),
        };
        let keys: Vec<(u32, u64)> = map.range(lo..(ordinal, end)).map(|(k, _)| *k).collect();
        let mut out = Vec::with_capacity(keys.len());
        for k in keys {
            if let Some(b) = map.remove(&k) {
                self.release(b.len() as u64);
                out.push(b);
            }
        }
        out
    }

    /// Indexes the copy of a batch whose write completed. Any entry it
    /// overlaps (a record a read-around made resident while the write was in
    /// flight, or before a wipe) is replaced.
    pub(crate) fn insert_batch(&self, lane: u16, ordinal: u32, start: u64, bytes: Arc<[u8]>) {
        let Some(m) = self.lanes.get(usize::from(lane)) else {
            self.release(bytes.len() as u64);
            return;
        };
        let end = start + bytes.len() as u64;
        let dropped = {
            let mut map = lock(m);
            let d = self.remove_overlaps(&mut map, ordinal, start, end);
            map.insert((ordinal, start), bytes);
            d
        };
        drop(dropped);
    }

    /// A batch that was not admitted still invalidates what it overwrote: no
    /// entry may overlap bytes written later, whatever the cap.
    pub(crate) fn evict_range(&self, lane: u16, ordinal: u32, start: u64, end: u64) {
        let Some(m) = self.lanes.get(usize::from(lane)) else {
            return;
        };
        let dropped = self.remove_overlaps(&mut lock(m), ordinal, start, end);
        drop(dropped);
    }

    /// Makes one verified record resident when it fits the cap, does not
    /// overlap an entry, its file is still the one it was read from and no
    /// wipe happened since `epoch`.
    fn insert_record(&self, cx: &Cx<'_>, start: u64, b: Arc<[u8]>) {
        let (rings, lane, ordinal, gen) = (cx.rings, cx.loc.lane.0, cx.loc.ordinal, cx.gen);
        // Under the lane's lock: reclaim takes the file out of use before it
        // releases the file's entries (which needs this lock), so an entry
        // inserted here is released by it.
        let in_use = || {
            rings
                .find_in_use_gen(lane, ordinal)
                .is_some_and(|(_, g)| g == gen)
        };
        self.insert_checked(lane, ordinal, start, b, cx.epoch, in_use);
    }

    fn insert_checked(
        &self,
        lane: u16,
        ordinal: u32,
        start: u64,
        b: Arc<[u8]>,
        epoch: u64,
        in_use: impl FnOnce() -> bool,
    ) {
        let Some(m) = self.lanes.get(usize::from(lane)) else {
            return;
        };
        let n = b.len() as u64;
        let end = start + n;
        let mut map = lock(m);
        if self.epoch.load(Ordering::Acquire) != epoch {
            return;
        }
        let before = map
            .range(..=(ordinal, start))
            .next_back()
            .is_some_and(|(&(o, s), e)| o == ordinal && s + e.len() as u64 > start);
        if before || map.range((ordinal, start)..(ordinal, end)).next().is_some() {
            return;
        }
        if !in_use() {
            return;
        }
        if self.try_reserve(n) {
            map.insert((ordinal, start), b);
        }
    }

    /// Drops every entry of one ring file (reclaim is about to recycle it).
    pub(crate) fn release_file(&self, lane: u16, ordinal: u32) {
        let Some(m) = self.lanes.get(usize::from(lane)) else {
            return;
        };
        let dropped = self.remove_overlaps(&mut lock(m), ordinal, 0, u64::MAX);
        drop(dropped);
    }

    /// Drops everything (recovery and the wipe re-establish the files) and
    /// invalidates every read-around that began before.
    pub(crate) fn clear(&self) {
        self.epoch.fetch_add(1, Ordering::AcqRel);
        let mut freed = 0u64;
        let mut dropped = Vec::new();
        for m in &self.lanes {
            let mut map = lock(m);
            freed += map.values().map(|b| b.len() as u64).sum::<u64>();
            dropped.push(std::mem::take(&mut *map));
        }
        self.release(freed);
        drop(dropped);
    }

    fn find(&self, loc: JournalLoc, len: usize) -> Option<(Arc<[u8]>, usize)> {
        let map = lock(self.lanes.get(usize::from(loc.lane.0))?);
        let (&(o, start), b) = map.range(..=(loc.ordinal, loc.offset)).next_back()?;
        if o != loc.ordinal {
            return None;
        }
        let idx = usize::try_from(loc.offset - start).ok()?;
        (idx.checked_add(len)? <= b.len()).then(|| (Arc::clone(b), idx))
    }

    /// Copies the bytes at `loc` out of the window; false when not resident.
    /// Counts nothing.
    pub(crate) fn copy_hit(&self, loc: JournalLoc, buf: &mut [u8]) -> bool {
        match self.find(loc, buf.len()) {
            Some((b, i)) => {
                buf.copy_from_slice(&b[i..i + buf.len()]);
                true
            }
            None => false,
        }
    }

    pub(crate) fn count(&self, hit: bool) {
        let c = if hit { &self.hits } else { &self.misses };
        c.fetch_add(1, Ordering::Relaxed);
    }

    fn claim(&self, key: FlightKey) -> Claim<'_> {
        let mut g = lock(&self.flights);
        if let Some(f) = g.get(&key) {
            return Claim::Follow(Arc::clone(f));
        }
        let flight = Arc::new(Flight {
            done: Mutex::new(None),
            cv: Condvar::new(),
        });
        g.insert(key, Arc::clone(&flight));
        Claim::Lead(Lead {
            win: self,
            key,
            flight,
            finished: false,
        })
    }

    /// Serves `buf` from the file through the read-around (a miss of
    /// [`Self::copy_hit`]): reads the window of `loc` once however many
    /// threads miss in it, accepts only records whose checksum verifies,
    /// makes them resident within the cap, completes a record cut by the
    /// window's edge, and copies the bytes of `loc` out of the verified
    /// record.
    ///
    /// # Errors
    /// `Io` for a failed read, `Corrupt` when no record whose checksum
    /// verifies covers `loc..loc + buf.len()`; `buf` is not written then.
    pub(crate) fn read_around(
        &self,
        rings: &Rings,
        file: &RingFile,
        gen: u64,
        loc: JournalLoc,
        buf: &mut [u8],
    ) -> Result<()> {
        let cx = Cx {
            rings,
            file,
            gen,
            loc,
            epoch: self.epoch.load(Ordering::Acquire),
        };
        match self.read_around_inner(&cx, buf) {
            Err(e) if matches!(e, Error::Corrupt { .. }) => {
                // A location whose file was recycled meanwhile reads zeros or
                // the next incarnation: not damage.
                if rings
                    .find_in_use_gen(loc.lane.0, loc.ordinal)
                    .is_none_or(|(_, g)| g != gen)
                {
                    return Err(Error::not_found("journal data"));
                }
                self.check_failures.fetch_add(1, Ordering::Relaxed);
                Err(e)
            }
            r => r,
        }
    }

    fn read_around_inner(&self, cx: &Cx<'_>, buf: &mut [u8]) -> Result<()> {
        let loc = cx.loc;
        let key = (loc.lane.0, loc.ordinal, loc.offset / READ_AROUND);
        loop {
            match self.claim(key) {
                Claim::Follow(f) => {
                    let Some(p) = f.wait() else {
                        // The leader failed: try again, possibly as the leader.
                        continue;
                    };
                    return match self.resolve(cx, &p, buf) {
                        // The leader's snapshot may predate the target (a
                        // batch that was not admitted is written after any
                        // read of its window): report damage only from a
                        // read of our own.
                        Err(Error::Corrupt { .. }) => {
                            let own = self.fill(cx)?;
                            self.resolve(cx, &own, buf)
                        }
                        r => r,
                    };
                }
                Claim::Lead(mut lead) => {
                    // The window may have become resident since the caller
                    // looked.
                    if self.copy_hit(loc, buf) {
                        return Ok(());
                    }
                    let parsed = Arc::new(self.fill(cx)?);
                    lead.end(Some(Arc::clone(&parsed)));
                    return self.resolve(cx, &parsed, buf);
                }
            }
        }
    }

    /// Reads and parses the window around `loc`, and makes what fits
    /// resident.
    fn fill(&self, cx: &Cx<'_>) -> Result<Parsed> {
        let Cx {
            rings,
            file,
            gen,
            loc,
            epoch,
        } = *cx;
        let w0 = loc.offset / READ_AROUND * READ_AROUND;
        let w1 = (w0 + READ_AROUND).min(rings.file_len);
        if w1 <= w0 {
            return Err(Error::invalid("journal location outside the ring file"));
        }
        let raw = sys::read_aligned(&file.file, w0, (w1 - w0) as usize).map_err(io_err)?;
        self.window_reads.fetch_add(1, Ordering::Relaxed);
        let scan = scan_records(raw.as_slice(), w0, gen, w0.max(B));
        let mut recs = Vec::with_capacity(scan.recs.len());
        for &(q, enc) in &scan.recs {
            let at = (q - w0) as usize;
            let bytes: Arc<[u8]> = Arc::from(&raw.as_slice()[at..at + enc]);
            self.insert_record(cx, q, Arc::clone(&bytes));
            recs.push((q, bytes));
        }
        // A record the edge of the window cuts is completed now, with one
        // more read: it is the next record of the walk through the file, and
        // without it the walk would read this window again for it. A record
        // that cannot be completed or verified is left for the read that
        // needs it, which reports it.
        if let Some(q) = scan.tail {
            let head = &raw.as_slice()[(q - w0) as usize..];
            if let Ok(bytes) = self.complete(cx, w1, head, q) {
                recs.push((q, bytes));
            }
        }
        Ok(Parsed {
            epoch,
            base: w0,
            raw,
            recs,
            tail: scan.tail,
            bad: scan.bad,
        })
    }

    /// Finds the verified record that holds `loc..loc + buf.len()`, completing
    /// a cut record when the target needs it, and copies the bytes out.
    fn resolve(&self, cx: &Cx<'_>, p: &Parsed, buf: &mut [u8]) -> Result<()> {
        // What `p` yields may only be offered to the window under the epoch
        // `p` was read in.
        let cx = &Cx {
            epoch: p.epoch,
            ..*cx
        };
        let loc = cx.loc;
        let (t, end) = (loc.offset, loc.offset + buf.len() as u64);
        if let Some((s, b)) = p.find(t, end) {
            let i = (t - s) as usize;
            buf.copy_from_slice(&b[i..i + buf.len()]);
            return Ok(());
        }
        if p.bad_covers(t, end) {
            return Err(corrupt("record checksum mismatch"));
        }
        let have_end = p.base + p.raw.len() as u64;
        if let Some(q) = p.tail.filter(|&q| q < t) {
            let head = &p.raw.as_slice()[(q - p.base) as usize..];
            match self.complete(cx, have_end, head, q) {
                Ok(rec) if covers(q, rec.len() as u64, t, end) => {
                    let i = (t - q) as usize;
                    buf.copy_from_slice(&rec[i..i + buf.len()]);
                    return Ok(());
                }
                Err(e) if !matches!(e, Error::Corrupt { .. }) => return Err(e),
                // A tail that is no record: look before the window.
                _ => {}
            }
        }
        self.resolve_before(cx, p, buf)
    }

    /// The target's record starts before the window: read back far enough to
    /// find its start (a record is at most `MAX_ENC` long) and scan again.
    fn resolve_before(&self, cx: &Cx<'_>, p: &Parsed, buf: &mut [u8]) -> Result<()> {
        let Cx {
            file, gen, loc, ..
        } = *cx;
        let (t, end) = (loc.offset, loc.offset + buf.len() as u64);
        let a = sys::floor4k(end.saturating_sub(MAX_ENC)).max(B);
        if a >= p.base {
            return Err(corrupt("no valid record covers the location"));
        }
        let back = sys::read_aligned(&file.file, a, (p.base - a) as usize).map_err(io_err)?;
        self.completing_reads.fetch_add(1, Ordering::Relaxed);
        let mut comb = Vec::with_capacity(back.len() + p.raw.len());
        comb.extend_from_slice(back.as_slice());
        comb.extend_from_slice(p.raw.as_slice());
        let scan = scan_records(&comb, a, gen, a);
        let pick = scan
            .recs
            .iter()
            .find(|&&(q, enc)| q < t && covers(q, enc as u64, t, end));
        if let Some(&(q, enc)) = pick {
            let at = (q - a) as usize;
            let bytes: Arc<[u8]> = Arc::from(&comb[at..at + enc]);
            let i = (t - q) as usize;
            buf.copy_from_slice(&bytes[i..i + buf.len()]);
            self.insert_record(cx, q, bytes);
            return Ok(());
        }
        if scan.bad.iter().any(|&(q, enc)| q < t && end <= q + enc) {
            return Err(corrupt("record checksum mismatch"));
        }
        if let Some(q) = scan.tail.filter(|&q| q < t) {
            let head = &comb[(q - a) as usize..];
            let rec = self.complete(cx, a + comb.len() as u64, head, q)?;
            if covers(q, rec.len() as u64, t, end) {
                let i = (t - q) as usize;
                buf.copy_from_slice(&rec[i..i + buf.len()]);
                return Ok(());
            }
        }
        Err(corrupt("no valid record covers the location"))
    }

    /// Completes the record that starts at `q` and runs past `have_end`, with
    /// one more read, verifies it and offers it to the window. `head` is the
    /// bytes from `q` to `have_end`.
    fn complete(&self, cx: &Cx<'_>, have_end: u64, head: &[u8], q: u64) -> Result<Arc<[u8]>> {
        let Cx {
            rings, file, gen, ..
        } = *cx;
        let file_len = rings.file_len;
        let mut have = head.to_vec();
        let mut end = have_end;
        if have.len() < HDR {
            if end + B > file_len {
                return Err(corrupt("record header runs past the file"));
            }
            let more = sys::read_aligned(&file.file, end, BLOCK).map_err(io_err)?;
            self.completing_reads.fetch_add(1, Ordering::Relaxed);
            have.extend_from_slice(more.as_slice());
            end += B;
        }
        let Some((_, enc)) = peek(&have[..HDR], gen, q) else {
            return Err(corrupt("no valid record covers the location"));
        };
        if (have.len() as u64) < enc {
            let want = round4k(q + enc);
            if want > file_len {
                return Err(corrupt("record runs past the file"));
            }
            let more = sys::read_aligned(&file.file, end, (want - end) as usize).map_err(io_err)?;
            self.completing_reads.fetch_add(1, Ordering::Relaxed);
            have.extend_from_slice(more.as_slice());
        }
        let enc = enc as usize;
        match Record::parse(&have[..enc]) {
            Ok((_, used)) if used == enc => {}
            _ => return Err(corrupt("record checksum mismatch")),
        }
        have.truncate(enc);
        let bytes: Arc<[u8]> = Arc::from(have);
        self.insert_record(cx, q, Arc::clone(&bytes));
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lunix_format::journal::RecordHeader;
    use lunix_format::WriteSeq;

    fn encode(seq: u64, off: u64, gen: u64, blocks: u32, fill: u8) -> Vec<u8> {
        let data = vec![fill; blocks as usize * BLOCK];
        let rec = Record {
            header: RecordHeader {
                kind: RecordKind::Write,
                write_seq: WriteSeq(seq),
                epoch: 0,
                lba: 0,
                len: blocks,
                gen,
                off,
                flushed: WriteSeq(0),
            },
            data: &data,
        };
        let mut v = Vec::new();
        rec.encode(&mut v).expect("encode");
        v
    }

    #[test]
    fn scan_accepts_matching_records_and_skips_padding() {
        let gen = 0x55;
        let mut buf = vec![0u8; 5 * BLOCK];
        let r1 = encode(1, 4096, gen, 1, 7);
        buf[BLOCK..BLOCK + r1.len()].copy_from_slice(&r1);
        let at2 = BLOCK + r1.len();
        let r2 = encode(2, at2 as u64, gen, 1, 9);
        buf[at2..at2 + r2.len()].copy_from_slice(&r2);
        let s = scan_records(&buf, 0, gen, B);
        assert_eq!(
            s.recs,
            vec![(4096, r1.len()), (at2 as u64, r2.len())],
            "{:?}",
            s.bad
        );
        assert!(s.tail.is_none() && s.bad.is_empty());
    }

    #[test]
    fn scan_rejects_a_wrong_generation_a_wrong_offset_and_a_damaged_record() {
        let gen = 0x55;
        let mut buf = vec![0u8; 7 * BLOCK];
        let ok = encode(1, 4096, gen, 1, 7);
        buf[BLOCK..BLOCK + ok.len()].copy_from_slice(&ok);
        let stale = encode(2, 12288, gen + 1, 1, 7);
        buf[3 * BLOCK..3 * BLOCK + stale.len()].copy_from_slice(&stale);
        let mut dam = encode(3, 20480, gen, 1, 7);
        dam[100] ^= 1;
        buf[5 * BLOCK..5 * BLOCK + dam.len()].copy_from_slice(&dam);
        let s = scan_records(&buf, 0, gen, B);
        assert_eq!(s.recs, vec![(4096, ok.len())]);
        assert_eq!(s.bad, vec![(20480, dam.len() as u64)]);
    }

    #[test]
    fn scan_reports_a_record_cut_by_the_end_of_the_buffer() {
        let gen = 3;
        let mut buf = vec![0u8; 2 * BLOCK];
        let r = encode(1, 4096, gen, 2, 1);
        buf[BLOCK..].copy_from_slice(&r[..BLOCK]);
        let s = scan_records(&buf, 0, gen, B);
        assert!(s.recs.is_empty());
        assert_eq!(s.tail, Some(4096));
    }

    fn rec(n: usize) -> Arc<[u8]> {
        Arc::from(vec![1u8; n])
    }

    fn loc(ordinal: u32, offset: u64) -> JournalLoc {
        JournalLoc {
            lane: lunix_iface::journal::LaneId(0),
            ordinal,
            offset,
        }
    }

    #[test]
    fn a_read_that_began_before_a_clear_cannot_insert_after_it() {
        let w = Window::new(1, 1 << 20);
        let epoch = w.epoch.load(Ordering::Acquire);
        w.clear();
        w.insert_checked(0, 1, 4096, rec(4160), epoch, || true);
        assert_eq!(w.stats().resident_bytes, 0);
        assert!(w.find(loc(1, 4096), 8).is_none());
        // Under the new epoch the same insert is accepted.
        let epoch = w.epoch.load(Ordering::Acquire);
        w.insert_checked(0, 1, 4096, rec(4160), epoch, || true);
        assert_eq!(w.stats().resident_bytes, 4160);
        assert!(w.find(loc(1, 4096), 8).is_some());
    }

    #[test]
    fn a_batch_that_is_not_admitted_still_evicts_what_it_overlaps() {
        let w = Window::new(1, 1 << 20);
        let e = w.epoch.load(Ordering::Acquire);
        w.insert_checked(0, 1, 4096, rec(4160), e, || true);
        w.insert_checked(0, 1, 12288, rec(4160), e, || true);
        w.insert_checked(0, 1, 20480, rec(4160), e, || true);
        // A write over [9000, 16384) (not admitted) takes the middle one only.
        w.evict_range(0, 1, 9000, 16384);
        assert!(w.find(loc(1, 4096), 8).is_some());
        assert!(w.find(loc(1, 12288), 8).is_none());
        assert!(w.find(loc(1, 20480), 8).is_some());
        assert_eq!(w.stats().resident_bytes, 2 * 4160);
        // An entry that starts before the range and runs into it goes too.
        w.evict_range(0, 1, 6000, 6001);
        assert!(w.find(loc(1, 4096), 8).is_none());
        assert_eq!(w.stats().resident_bytes, 4160);
    }

    #[test]
    fn a_completed_batch_replaces_the_read_around_entries_it_overlaps() {
        let w = Window::new(1, 1 << 20);
        let e = w.epoch.load(Ordering::Acquire);
        w.insert_checked(0, 1, 4096, rec(4160), e, || true);
        w.insert_checked(0, 1, 8256, rec(4160), e, || true);
        w.insert_checked(0, 2, 4096, rec(100), e, || true);
        assert!(w.reserve_batch(9000));
        let batch: Arc<[u8]> = Arc::from(vec![2u8; 9000]);
        w.insert_batch(0, 1, 4096, batch);
        let (b, i) = w.find(loc(1, 8256), 8).expect("hit");
        assert_eq!((b.len(), i, b[i]), (9000, 4160, 2));
        // The other ordinal is untouched; the replaced entries' bytes went.
        assert!(w.find(loc(2, 4096), 8).is_some());
        assert_eq!(w.stats().resident_bytes, 9000 + 100);
    }

    #[test]
    fn a_record_is_not_inserted_over_an_entry_or_for_a_recycled_file() {
        let w = Window::new(1, 1 << 20);
        let e = w.epoch.load(Ordering::Acquire);
        w.insert_checked(0, 1, 4096, rec(4160), e, || true);
        w.insert_checked(0, 1, 6000, rec(100), e, || true);
        w.insert_checked(0, 3, 4096, rec(100), e, || false);
        assert_eq!(w.stats().resident_bytes, 4160);
    }

    #[test]
    fn scan_reports_a_header_cut_by_the_end_of_the_buffer() {
        let gen = 3;
        let mut buf = vec![0u8; 2 * BLOCK];
        let r = encode(1, 4096 + 4096 - 8, gen, 1, 1);
        buf[2 * BLOCK - 8..].copy_from_slice(&r[..8]);
        let s = scan_records(&buf, 4096, gen, 4096);
        assert!(s.recs.is_empty() && s.bad.is_empty());
        assert_eq!(s.tail, Some(4096 + 2 * 4096 - 8));
    }

    #[test]
    fn the_cap_is_never_passed() {
        let w = Window::new(1, 100);
        assert!(w.reserve_batch(60));
        assert!(!w.reserve_batch(60));
        assert!(w.reserve_batch(40));
        assert_eq!(w.stats().resident_bytes, 100);
        assert_eq!(w.stats().batches_skipped, 1);
        w.drop_reservation(60);
        assert!(w.reserve_batch(60));
    }
}
