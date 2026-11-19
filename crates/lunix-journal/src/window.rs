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
//!   with the recovery reader's rules: a record is accepted only if its
//!   header carries the file's generation and its own offset, and its crc32c
//!   over header and data verifies. Only accepted records become resident
//!   (within the cap) or are served; a damaged record fails the read with
//!   [`Error::Corrupt`] and nothing of it is copied. A record cut by the edge
//!   of the window is completed with one more read (forward for a record that
//!   starts inside the window, backward for one that starts before it).
//!
//! Nothing is read ahead of need and nothing is read back after a write.
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

    /// Indexes the copy of a batch whose write completed. Any entry it
    /// overlaps (a record a read-around made resident while the write was in
    /// flight) is replaced.
    pub(crate) fn insert_batch(&self, lane: u16, ordinal: u32, start: u64, bytes: Arc<[u8]>) {
        let Some(m) = self.lanes.get(usize::from(lane)) else {
            self.release(bytes.len() as u64);
            return;
        };
        let end = start + bytes.len() as u64;
        let mut map = lock(m);
        let lo = match map.range(..(ordinal, start)).next_back() {
            Some((&(o, s), b)) if o == ordinal && s + b.len() as u64 > start => (o, s),
            _ => (ordinal, start),
        };
        let keys: Vec<(u32, u64)> = map.range(lo..(ordinal, end)).map(|(k, _)| *k).collect();
        for k in keys {
            if let Some(b) = map.remove(&k) {
                self.release(b.len() as u64);
            }
        }
        map.insert((ordinal, start), bytes);
    }

    /// Makes one verified record resident when it fits the cap, does not
    /// overlap an entry and its file is still the one it was read from.
    fn insert_record(
        &self,
        rings: &Rings,
        lane: u16,
        ordinal: u32,
        gen: u64,
        start: u64,
        b: Arc<[u8]>,
    ) {
        let Some(m) = self.lanes.get(usize::from(lane)) else {
            return;
        };
        let n = b.len() as u64;
        let end = start + n;
        let mut map = lock(m);
        let before = map
            .range(..=(ordinal, start))
            .next_back()
            .is_some_and(|(&(o, s), e)| o == ordinal && s + e.len() as u64 > start);
        if before || map.range((ordinal, start)..(ordinal, end)).next().is_some() {
            return;
        }
        // Under the lane's lock: reclaim takes the file out of use before it
        // releases the file's entries (which needs this lock), so an entry
        // inserted here is released by it.
        if rings
            .find_in_use_gen(lane, ordinal)
            .is_none_or(|(_, g)| g != gen)
        {
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
        let mut map = lock(m);
        let keys: Vec<(u32, u64)> = map
            .range((ordinal, 0)..=(ordinal, u64::MAX))
            .map(|(k, _)| *k)
            .collect();
        let mut freed = 0u64;
        for k in keys {
            if let Some(b) = map.remove(&k) {
                freed += b.len() as u64;
            }
        }
        drop(map);
        self.release(freed);
    }

    /// Drops everything (recovery and the wipe re-establish the files).
    pub(crate) fn clear(&self) {
        let mut freed = 0u64;
        for m in &self.lanes {
            let mut map = lock(m);
            freed += map.values().map(|b| b.len() as u64).sum::<u64>();
            map.clear();
        }
        self.release(freed);
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
        let key = (loc.lane.0, loc.ordinal, loc.offset / READ_AROUND);
        let cx = Cx {
            rings,
            file,
            gen,
            loc,
        };
        loop {
            match self.claim(key) {
                Claim::Follow(f) => {
                    if let Some(p) = f.wait() {
                        return self.resolve(&cx, &p, buf);
                    }
                    // The leader failed: try again, possibly as the leader.
                }
                Claim::Lead(mut lead) => {
                    // The window may have become resident since the caller
                    // looked.
                    if self.copy_hit(loc, buf) {
                        return Ok(());
                    }
                    let parsed = Arc::new(self.fill(&cx)?);
                    lead.end(Some(Arc::clone(&parsed)));
                    return self.resolve(&cx, &parsed, buf);
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
            self.insert_record(rings, loc.lane.0, loc.ordinal, gen, q, Arc::clone(&bytes));
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
        let loc = cx.loc;
        let (t, end) = (loc.offset, loc.offset + buf.len() as u64);
        if let Some((s, b)) = p.find(t, end) {
            let i = (t - s) as usize;
            buf.copy_from_slice(&b[i..i + buf.len()]);
            return Ok(());
        }
        if p.bad_covers(t, end) {
            return Err(self.fail("record checksum mismatch"));
        }
        let have_end = p.base + p.raw.len() as u64;
        if let Some(q) = p.tail.filter(|&q| q < t) {
            let head = &p.raw.as_slice()[(q - p.base) as usize..];
            let rec = self
                .complete(cx, have_end, head, q)
                .inspect_err(|e| self.note(e))?;
            if covers(q, rec.len() as u64, t, end) {
                let i = (t - q) as usize;
                buf.copy_from_slice(&rec[i..i + buf.len()]);
                return Ok(());
            }
        }
        self.resolve_before(cx, p, buf)
    }

    /// The target's record starts before the window: read back far enough to
    /// find its start (a record is at most `MAX_ENC` long) and scan again.
    fn resolve_before(&self, cx: &Cx<'_>, p: &Parsed, buf: &mut [u8]) -> Result<()> {
        let Cx {
            rings,
            file,
            gen,
            loc,
        } = *cx;
        let (t, end) = (loc.offset, loc.offset + buf.len() as u64);
        let a = sys::floor4k(end.saturating_sub(MAX_ENC)).max(B);
        if a >= p.base {
            return Err(self.fail("no valid record covers the location"));
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
            self.insert_record(rings, loc.lane.0, loc.ordinal, gen, q, bytes);
            return Ok(());
        }
        if scan.bad.iter().any(|&(q, enc)| q < t && end <= q + enc) {
            return Err(self.fail("record checksum mismatch"));
        }
        if let Some(q) = scan.tail.filter(|&q| q < t) {
            let head = &comb[(q - a) as usize..];
            let rec = self
                .complete(cx, a + comb.len() as u64, head, q)
                .inspect_err(|e| self.note(e))?;
            if covers(q, rec.len() as u64, t, end) {
                let i = (t - q) as usize;
                buf.copy_from_slice(&rec[i..i + buf.len()]);
                return Ok(());
            }
        }
        Err(self.fail("no valid record covers the location"))
    }

    /// Completes the record that starts at `q` and runs past `have_end`, with
    /// one more read, verifies it and offers it to the window. `head` is the
    /// bytes from `q` to `have_end`.
    fn complete(&self, cx: &Cx<'_>, have_end: u64, head: &[u8], q: u64) -> Result<Arc<[u8]>> {
        let Cx {
            rings,
            file,
            gen,
            loc,
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
        self.insert_record(rings, loc.lane.0, loc.ordinal, gen, q, Arc::clone(&bytes));
        Ok(bytes)
    }

    /// Counts a failed target read (a record that does not verify).
    fn note(&self, e: &Error) {
        if matches!(e, Error::Corrupt { .. }) {
            self.check_failures.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn fail(&self, detail: &'static str) -> Error {
        self.check_failures.fetch_add(1, Ordering::Relaxed);
        corrupt(detail)
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
