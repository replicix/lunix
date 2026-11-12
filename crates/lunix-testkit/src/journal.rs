//! [`MemJournal`]: the write-ahead journal in memory, with a crash model.
//!
//! It follows the frozen [`Journal`] contract to the letter:
//!
//! * the journal assigns `write_seq` in [`JournalLane::try_reserve`], registers
//!   it as in flight in the same step, and refuses (`Full`, `NoSpace`) without
//!   consuming a number;
//! * the group flush is keyed on `upto`: a coordinator thread waits until no
//!   reserved slot and no unfinished append with `write_seq <= upto` is in
//!   flight, then makes every completed record durable and calls `done` on
//!   the coordinator thread;
//! * a failed append, a failed sync, a dropped slot or a foreign slot
//!   **poisons** the journal: flushes with `upto >=` the failed number end
//!   with `Err(Io)` and every lane returns `Io` from then on;
//! * `recover` is floor based, merges the lanes, stops at the first gap and
//!   reports what lies beyond it as ignored; it never changes what it reads
//!   (so it is idempotent), and stale records at or above the new `next_seq`
//!   are retired when the first lane is opened.
//!
//! # Crash model
//!
//! [`MemJournal::crash`] freezes the journal and returns a [`JournalImage`]:
//! records made durable by a completed group flush survive; every other record
//! (written but unflushed, or never completed) is independently **kept**,
//! **dropped** or **torn** (kept as damage the resync rule skips), chosen by a
//! seeded generator. Records can therefore be lost from the middle of a lane,
//! which is what produces the gaps recovery must handle. [`MemJournal::from_image`]
//! opens the image as a new journal; the caller runs `recover` as the engine does.
//!
//! # Test controls
//!
//! Appends complete immediately by default. [`MemJournal::set_manual_completions`]
//! holds completions until [`MemJournal::complete_appends`], to exercise the
//! in-flight rules. `fail_next_append`, `fail_next_completion`,
//! `fail_next_flush` and `set_nospace` inject failures.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;

use lunix_format::journal::RecordKind;
use lunix_format::local::CheckpointSlot;
use lunix_format::WriteSeq;
use lunix_iface::block::IoStatus;
use lunix_iface::journal::{
    record_bytes, Appended, DamagedRange, FlushDone, Journal, JournalCompletion, JournalFill,
    JournalLane, JournalLoc, JournalTail, LaneId, ReclaimReport, RecordInfo, Recovered,
    RecoveredRecord, Slot, SlotOutcome, Token, WriteSeqRange,
};
use lunix_iface::wake::WakeFd;
use lunix_iface::{Error, Result};

const BLOCK: usize = 4096;
const HEADER: u64 = 64;
/// The smallest legal ring file: one maximal write record.
pub const MIN_FILE_BYTES: u64 = HEADER + 256 * BLOCK as u64;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// Sizes of a [`MemJournal`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct MemJournalConfig {
    /// Lanes (one per queue).
    pub lanes: u16,
    /// Bytes per ring file, at least [`MIN_FILE_BYTES`].
    pub file_bytes: u64,
    /// Ring files in total; `try_reserve` answers `Full` when a lane needs a
    /// new file and none is free.
    pub max_files: u32,
    /// Appends a lane may have unfinished (the bounded buffer pool); `Full`
    /// beyond it. Only reachable with manual completions.
    pub pool_depth: u32,
}

impl Default for MemJournalConfig {
    fn default() -> Self {
        Self {
            lanes: 2,
            file_bytes: 2 << 20,
            max_files: 64,
            pool_depth: 64,
        }
    }
}

impl MemJournalConfig {
    /// The defaults with `lanes` lanes.
    pub fn with_lanes(lanes: u16) -> Self {
        Self {
            lanes,
            ..Self::default()
        }
    }
}

/// How unflushed records fare in a crash: relative weights.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CrashModel {
    /// Weight of "the record reached the file".
    pub keep: u32,
    /// Weight of "the record is lost".
    pub drop: u32,
    /// Weight of "the record is torn (damage)".
    pub tear: u32,
}

impl Default for CrashModel {
    fn default() -> Self {
        Self {
            keep: 1,
            drop: 1,
            tear: 1,
        }
    }
}

impl CrashModel {
    /// Every unflushed record is lost.
    pub fn lose_all() -> Self {
        Self {
            keep: 0,
            drop: 1,
            tear: 0,
        }
    }

    /// Every unflushed record survives (as if flushed).
    pub fn keep_all() -> Self {
        Self {
            keep: 1,
            drop: 0,
            tear: 0,
        }
    }
}

#[derive(Clone, Debug)]
struct Rec {
    seq: u64,
    kind: RecordKind,
    lba: u64,
    blocks: u32,
    data: Vec<u8>,
    off: u64,
    completed: bool,
    durable: bool,
    torn: bool,
}

impl Rec {
    fn len(&self) -> u64 {
        record_bytes(self.kind, self.blocks)
    }
}

#[derive(Clone, Debug, Default)]
struct RingFile {
    recs: BTreeMap<u64, Rec>,
    len: u64,
}

/// What survives a crash; the input of [`MemJournal::from_image`].
#[derive(Clone, Debug)]
pub struct JournalImage {
    cfg: MemJournalConfig,
    lanes: Vec<BTreeMap<u32, RingFile>>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Fail {
    Append,
    Completion,
}

#[derive(Debug)]
struct LaneState {
    files: BTreeMap<u32, RingFile>,
    cur: u32,
    opened: bool,
    outstanding: Option<(u64, u32, u64)>,
    pending_complete: VecDeque<(u64, u32, u64)>,
    completions: VecDeque<JournalCompletion>,
    wake: Option<WakeFd>,
    accepted_max: u64,
    fail_next: Option<Fail>,
}

struct State {
    cfg: MemJournalConfig,
    lanes: Vec<LaneState>,
    next_seq: u64,
    last_issued: u64,
    in_flight: BTreeSet<u64>,
    poisoned: Option<u64>,
    pending: VecDeque<(u64, FlushDone)>,
    closed: bool,
    manual: bool,
    fail_flush: bool,
    nospace: bool,
    retire_from: Option<u64>,
}

struct Inner {
    state: Mutex<State>,
    cv: Condvar,
}

fn token_of(lane: u16, seq: u64) -> u64 {
    (u64::from(lane) << 48) | (seq & ((1 << 48) - 1))
}

fn poisoned_err() -> Error {
    Error::Io(std::io::Error::other("journal is poisoned"))
}

impl State {
    fn poison(&mut self, seq: u64) {
        self.poisoned = Some(self.poisoned.map_or(seq, |p| p.min(seq)));
    }

    fn total_files(&self) -> usize {
        self.lanes.iter().map(|l| l.files.len()).sum()
    }

    fn live(&self) -> Result<()> {
        if self.closed {
            Err(Error::Closed)
        } else {
            Ok(())
        }
    }

    /// The record data at `loc` (`len` bytes), if it can be read.
    fn data_at(&self, loc: JournalLoc, len: usize) -> Result<&[u8]> {
        let lane = self
            .lanes
            .get(usize::from(loc.lane.0))
            .ok_or(Error::invalid("lane out of range"))?;
        let file = lane
            .files
            .get(&loc.ordinal)
            .ok_or(Error::not_found("journal file"))?;
        let start = loc
            .offset
            .checked_sub(HEADER)
            .ok_or(Error::invalid("offset inside a record header"))?;
        let (_, rec) = file
            .recs
            .range(..=start)
            .next_back()
            .ok_or(Error::not_found("journal record"))?;
        if rec.torn {
            return Err(Error::not_found("journal record"));
        }
        let begin =
            usize::try_from(loc.offset - rec.off - HEADER).map_err(|_| Error::invalid("offset"))?;
        let end = begin
            .checked_add(len)
            .ok_or(Error::invalid("length overflow"))?;
        if rec.kind == RecordKind::Hole || end > rec.data.len() {
            return Err(Error::invalid("read outside the record data"));
        }
        if !rec.completed {
            return Err(Error::invalid("read before the append completed"));
        }
        Ok(&rec.data[begin..end])
    }
}

/// The in-memory journal. `Send + Sync`; dropping it stops the coordinator.
pub struct MemJournal {
    inner: Arc<Inner>,
    coordinator: Mutex<Option<JoinHandle<()>>>,
}

impl std::fmt::Debug for MemJournal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let st = lock(&self.inner.state);
        f.debug_struct("MemJournal")
            .field("lanes", &st.cfg.lanes)
            .field("last_issued", &st.last_issued)
            .field("poisoned", &st.poisoned)
            .finish()
    }
}

impl MemJournal {
    /// A fresh, empty journal.
    ///
    /// # Panics
    /// If `cfg.lanes` is zero or `cfg.file_bytes` is below [`MIN_FILE_BYTES`].
    pub fn new(cfg: MemJournalConfig) -> Self {
        let lanes = (0..cfg.lanes).map(|_| BTreeMap::new()).collect();
        Self::from_image(&JournalImage { cfg, lanes })
    }

    /// Opens a crash image as a new journal. The caller runs `recover`.
    pub fn from_image(image: &JournalImage) -> Self {
        let cfg = image.cfg;
        assert!(cfg.lanes > 0, "a journal needs a lane");
        assert!(cfg.file_bytes >= MIN_FILE_BYTES, "ring file too small");
        let lanes = image
            .lanes
            .iter()
            .map(|files| {
                let mut files = files.clone();
                for f in files.values_mut() {
                    for r in f.recs.values_mut() {
                        r.completed = true;
                        r.durable = true;
                    }
                }
                // New records never share a file with a possibly torn tail.
                let cur = files.keys().next_back().map_or(0, |o| o + 1);
                LaneState {
                    files,
                    cur,
                    opened: false,
                    outstanding: None,
                    pending_complete: VecDeque::new(),
                    completions: VecDeque::new(),
                    wake: None,
                    accepted_max: 0,
                    fail_next: None,
                }
            })
            .collect();
        let inner = Arc::new(Inner {
            state: Mutex::new(State {
                cfg,
                lanes,
                next_seq: 1,
                last_issued: 0,
                in_flight: BTreeSet::new(),
                poisoned: None,
                pending: VecDeque::new(),
                closed: false,
                manual: false,
                fail_flush: false,
                nospace: false,
                retire_from: None,
            }),
            cv: Condvar::new(),
        });
        let worker = Arc::clone(&inner);
        let handle = std::thread::Builder::new()
            .name("mem-journal-flush".into())
            .spawn(move || coordinator(&worker))
            .expect("spawn the flush coordinator");
        Self {
            inner,
            coordinator: Mutex::new(Some(handle)),
        }
    }

    /// The sizes this journal was built with.
    pub fn config(&self) -> MemJournalConfig {
        lock(&self.inner.state).cfg
    }

    /// Holds append completions until [`complete_appends`](Self::complete_appends).
    pub fn set_manual_completions(&self, manual: bool) {
        lock(&self.inner.state).manual = manual;
    }

    /// Completes up to `n` held appends of `lane`, oldest first; returns how
    /// many were completed.
    pub fn complete_appends(&self, lane: LaneId, n: usize) -> usize {
        let mut st = lock(&self.inner.state);
        let mut done = 0;
        for _ in 0..n {
            let Some(l) = st.lanes.get_mut(usize::from(lane.0)) else {
                break;
            };
            let Some((seq, ordinal, off)) = l.pending_complete.pop_front() else {
                break;
            };
            if let Some(r) = l.files.get_mut(&ordinal).and_then(|f| f.recs.get_mut(&off)) {
                r.completed = true;
            }
            l.completions.push_back(JournalCompletion::Append {
                write_seq: WriteSeq(seq),
                status: IoStatus::ok(0),
            });
            if let Some(w) = &l.wake {
                w.signal();
            }
            st.in_flight.remove(&seq);
            done += 1;
        }
        drop(st);
        self.inner.cv.notify_all();
        done
    }

    /// Appends of `lane` accepted but not yet completed (manual mode).
    pub fn held_appends(&self, lane: LaneId) -> usize {
        lock(&self.inner.state)
            .lanes
            .get(usize::from(lane.0))
            .map_or(0, |l| l.pending_complete.len())
    }

    /// Makes the next `append` on `lane` fail with `Io` (the slot is consumed
    /// and the journal is poisoned).
    pub fn fail_next_append(&self, lane: LaneId) {
        if let Some(l) = lock(&self.inner.state).lanes.get_mut(usize::from(lane.0)) {
            l.fail_next = Some(Fail::Append);
        }
    }

    /// Makes the next `append` on `lane` be accepted but complete with an
    /// error status (which poisons the journal).
    pub fn fail_next_completion(&self, lane: LaneId) {
        if let Some(l) = lock(&self.inner.state).lanes.get_mut(usize::from(lane.0)) {
            l.fail_next = Some(Fail::Completion);
        }
    }

    /// Makes the next group flush fail its `fdatasync` (and poison).
    pub fn fail_next_flush(&self) {
        lock(&self.inner.state).fail_flush = true;
    }

    /// While set, `try_reserve` answers `NoSpace` (nothing consumed).
    pub fn set_nospace(&self, on: bool) {
        lock(&self.inner.state).nospace = on;
    }

    /// True once the journal is poisoned.
    pub fn is_poisoned(&self) -> bool {
        lock(&self.inner.state).poisoned.is_some()
    }

    /// Records appended and not yet made durable by a flush.
    pub fn unflushed_records(&self) -> usize {
        lock(&self.inner.state)
            .lanes
            .iter()
            .flat_map(|l| l.files.values())
            .flat_map(|f| f.recs.values())
            .filter(|r| !r.durable)
            .count()
    }

    /// Crashes with the default model. See [`crash_with`](Self::crash_with).
    pub fn crash(&self, seed: u64) -> JournalImage {
        self.crash_with(seed, CrashModel::default())
    }

    /// Freezes the journal (every later call fails `Closed`, pending flushes
    /// end `Err(Closed)`) and returns what the "disk" holds: durable records
    /// plus a random subset of the rest, some torn.
    pub fn crash_with(&self, seed: u64, model: CrashModel) -> JournalImage {
        let mut st = lock(&self.inner.state);
        st.closed = true;
        let mut rng = Splitmix(seed);
        let total = u64::from(model.keep) + u64::from(model.drop) + u64::from(model.tear);
        let mut lanes = Vec::new();
        for l in &st.lanes {
            let mut files = BTreeMap::new();
            for (&ordinal, f) in &l.files {
                let mut out = RingFile {
                    recs: BTreeMap::new(),
                    len: f.len,
                };
                for (&off, r) in &f.recs {
                    let mut r = r.clone();
                    if !r.durable {
                        let pick = if total == 0 {
                            0
                        } else {
                            rng.next_u64() % total
                        };
                        if pick < u64::from(model.keep) {
                            // reached the file
                        } else if pick < u64::from(model.keep) + u64::from(model.drop) {
                            continue;
                        } else {
                            r.torn = true;
                            r.data = Vec::new();
                        }
                    }
                    out.recs.insert(off, r);
                }
                files.insert(ordinal, out);
            }
            lanes.push(files);
        }
        let cfg = st.cfg;
        drop(st);
        self.inner.cv.notify_all();
        JournalImage { cfg, lanes }
    }
}

impl Drop for MemJournal {
    fn drop(&mut self) {
        lock(&self.inner.state).closed = true;
        self.inner.cv.notify_all();
        if let Some(h) = lock(&self.coordinator).take() {
            let _ = h.join();
        }
    }
}

struct Splitmix(u64);

impl Splitmix {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
}

/// The flush coordinator: one group at a time, in submission order.
fn coordinator(inner: &Inner) {
    let mut st = lock(&inner.state);
    loop {
        if st.closed {
            let drained: Vec<_> = st.pending.drain(..).collect();
            drop(st);
            for (_, done) in drained {
                done(Err(Error::Closed));
            }
            return;
        }
        let Some(&(upto, _)) = st.pending.front() else {
            st = inner.cv.wait(st).unwrap_or_else(|p| p.into_inner());
            continue;
        };
        let result = if st.poisoned.is_some_and(|f| upto >= f) {
            Err(poisoned_err())
        } else if st.in_flight.range(..=upto).next().is_some() {
            st = inner.cv.wait(st).unwrap_or_else(|p| p.into_inner());
            continue;
        } else if st.fail_flush {
            st.fail_flush = false;
            let lowest = st
                .lanes
                .iter()
                .flat_map(|l| l.files.values())
                .flat_map(|f| f.recs.values())
                .filter(|r| !r.durable && r.completed)
                .map(|r| r.seq)
                .min()
                .unwrap_or(upto);
            st.poison(lowest);
            Err(Error::Io(std::io::Error::other("fdatasync failed")))
        } else {
            for l in &mut st.lanes {
                for f in l.files.values_mut() {
                    for r in f.recs.values_mut().filter(|r| r.completed) {
                        r.durable = true;
                    }
                }
            }
            Ok(())
        };
        let (_, done) = st.pending.pop_front().expect("front checked above");
        drop(st);
        done(result);
        st = lock(&inner.state);
    }
}

impl Journal for MemJournal {
    fn lanes(&self) -> u16 {
        lock(&self.inner.state).cfg.lanes
    }

    fn open_lane(&self, lane: LaneId, wake: WakeFd) -> Result<Box<dyn JournalLane>> {
        let mut st = lock(&self.inner.state);
        st.live()?;
        let idx = usize::from(lane.0);
        if idx >= st.lanes.len() {
            return Err(Error::invalid("no such lane"));
        }
        if st.lanes[idx].opened {
            return Err(Error::invalid("lane already open"));
        }
        if let Some(from) = st.retire_from.take() {
            for l in &mut st.lanes {
                for f in l.files.values_mut() {
                    f.recs.retain(|_, r| r.seq < from);
                }
            }
        }
        let l = &mut st.lanes[idx];
        l.opened = true;
        l.wake = Some(wake);
        drop(st);
        Ok(Box::new(MemLane {
            inner: Arc::clone(&self.inner),
            id: lane,
        }))
    }

    fn last_issued(&self) -> WriteSeq {
        WriteSeq(lock(&self.inner.state).last_issued)
    }

    fn submit_group_flush(&self, upto: WriteSeq, done: FlushDone) -> Result<()> {
        let mut st = lock(&self.inner.state);
        st.live()?;
        if st.poisoned.is_some_and(|f| upto.0 >= f) {
            drop(st);
            done(Err(poisoned_err()));
            return Ok(());
        }
        st.pending.push_back((upto.0, done));
        drop(st);
        self.inner.cv.notify_all();
        Ok(())
    }

    fn read_at(&self, loc: JournalLoc, buf: &mut [u8]) -> Result<()> {
        let st = lock(&self.inner.state);
        st.live()?;
        buf.copy_from_slice(st.data_at(loc, buf.len())?);
        Ok(())
    }

    fn recover(&self, _hint: Option<&CheckpointSlot>, floor: WriteSeq) -> Result<Recovered> {
        let mut st = lock(&self.inner.state);
        st.live()?;
        if st.lanes.iter().any(|l| l.opened) {
            return Err(Error::invalid("recover after a lane was opened"));
        }
        let mut valid: Vec<(u64, RecoveredRecord)> = Vec::new();
        let mut damaged = Vec::new();
        for (lane_no, l) in st.lanes.iter().enumerate() {
            let lane = LaneId(u16::try_from(lane_no).map_err(|_| Error::invalid("lanes"))?);
            let mut prev_seq = 0u64;
            // (ordinal, start, end, prev_seq) of a damage run waiting for its next record.
            let mut open: Vec<(u32, u64, u64, u64)> = Vec::new();
            let mut closed_runs: Vec<(u32, u64, u64, u64, Option<u64>)> = Vec::new();
            for (&ordinal, f) in &l.files {
                for r in f.recs.values() {
                    if r.torn {
                        match open.last_mut() {
                            Some(last) if last.0 == ordinal && last.2 == r.off => {
                                last.2 = r.off + r.len();
                            }
                            _ => open.push((ordinal, r.off, r.off + r.len(), prev_seq)),
                        }
                    } else {
                        for (o, s, e, p) in open.drain(..) {
                            closed_runs.push((o, s, e, p, Some(r.seq)));
                        }
                        prev_seq = r.seq;
                        if r.seq > floor.0 {
                            valid.push((
                                r.seq,
                                RecoveredRecord {
                                    record: RecordInfo {
                                        kind: r.kind,
                                        write_seq: WriteSeq(r.seq),
                                        lba: r.lba,
                                        blocks: r.blocks,
                                    },
                                    loc: JournalLoc {
                                        lane,
                                        ordinal,
                                        offset: r.off + HEADER,
                                    },
                                },
                            ));
                        }
                    }
                }
            }
            for (o, s, e, p) in open {
                closed_runs.push((o, s, e, p, None));
            }
            for (ordinal, start, end, prev, next) in closed_runs {
                damaged.push(DamagedRange {
                    lane,
                    ordinal,
                    start,
                    end,
                    prev_seq: WriteSeq(prev),
                    next_seq: next.map(WriteSeq),
                });
            }
        }
        valid.sort_by_key(|(seq, _)| *seq);
        valid.dedup_by_key(|(seq, _)| *seq);
        let mut expected = floor.0 + 1;
        let mut records = Vec::new();
        let mut rest = Vec::new();
        for (seq, rec) in valid {
            if rest.is_empty() && seq == expected {
                records.push(rec);
                expected += 1;
            } else {
                rest.push(seq);
            }
        }
        let (ignored_after_gap, gap_at) = match (rest.first(), rest.last()) {
            (Some(&first), Some(&last)) => (
                Some(WriteSeqRange {
                    first: WriteSeq(first),
                    last: WriteSeq(last),
                }),
                Some(WriteSeq(expected)),
            ),
            _ => (None, None),
        };
        st.next_seq = expected;
        st.last_issued = expected - 1;
        st.retire_from = Some(expected);
        Ok(Recovered {
            records,
            next_seq: WriteSeq(expected),
            ignored_after_gap,
            gap_at,
            damaged,
        })
    }

    fn tail_after(&self, cut: WriteSeq) -> Result<JournalTail> {
        let st = lock(&self.inner.state);
        st.live()?;
        let lane_ordinals = st
            .lanes
            .iter()
            .map(|l| {
                l.files
                    .iter()
                    .find(|(_, f)| f.recs.values().any(|r| !r.torn && r.seq > cut.0))
                    .map_or(l.cur, |(&o, _)| o)
            })
            .collect();
        Ok(JournalTail { lane_ordinals })
    }

    fn reclaim(&self, upto: &JournalTail) -> Result<ReclaimReport> {
        let mut st = lock(&self.inner.state);
        st.live()?;
        if upto.lane_ordinals.len() != st.lanes.len() {
            return Err(Error::invalid("tail has the wrong lane count"));
        }
        let mut report = ReclaimReport::default();
        for (l, &limit) in st.lanes.iter_mut().zip(&upto.lane_ordinals) {
            let limit = limit.min(l.cur);
            let gone: Vec<u32> = l.files.range(..limit).map(|(&o, _)| o).collect();
            for o in gone {
                if let Some(f) = l.files.remove(&o) {
                    report.files += 1;
                    report.bytes += f
                        .recs
                        .values()
                        .filter(|r| !r.torn)
                        .map(Rec::len)
                        .sum::<u64>();
                }
            }
        }
        drop(st);
        self.inner.cv.notify_all();
        Ok(report)
    }

    fn fill(&self) -> JournalFill {
        let st = lock(&self.inner.state);
        JournalFill {
            used_bytes: st.total_files() as u64 * st.cfg.file_bytes,
            capacity_bytes: u64::from(st.cfg.max_files) * st.cfg.file_bytes,
        }
    }
}

struct MemLane {
    inner: Arc<Inner>,
    id: LaneId,
}

impl MemLane {
    fn idx(&self) -> usize {
        usize::from(self.id.0)
    }
}

impl JournalLane for MemLane {
    fn lane(&self) -> LaneId {
        self.id
    }

    fn try_reserve(&mut self, kind: RecordKind, blocks: u32) -> Result<SlotOutcome> {
        let mut st = lock(&self.inner.state);
        st.live()?;
        if st.poisoned.is_some() {
            return Err(poisoned_err());
        }
        if let Some((seq, _, _)) = st.lanes[self.idx()].outstanding {
            st.poison(seq);
            drop(st);
            self.inner.cv.notify_all();
            return Err(Error::invalid(
                "an earlier slot of this lane is outstanding",
            ));
        }
        let malformed = blocks == 0 || (kind == RecordKind::Write && blocks > 256);
        if malformed {
            return Err(Error::invalid("malformed reservation"));
        }
        if st.nospace {
            return Ok(SlotOutcome::NoSpace);
        }
        let idx = self.idx();
        if st.lanes[idx].pending_complete.len() >= st.cfg.pool_depth as usize {
            return Ok(SlotOutcome::Full);
        }
        let len = record_bytes(kind, blocks);
        let (file_bytes, max_files) = (st.cfg.file_bytes, st.cfg.max_files as usize);
        let total = st.total_files();
        let lane = &mut st.lanes[idx];
        let fits = lane
            .files
            .get(&lane.cur)
            .is_some_and(|f| f.len + len <= file_bytes);
        let exists = lane.files.contains_key(&lane.cur);
        if !fits && total >= max_files {
            return Ok(SlotOutcome::Full);
        }
        if exists && !fits {
            lane.cur += 1;
        }
        let ordinal = lane.cur;
        let file = lane.files.entry(ordinal).or_default();
        let off = file.len;
        file.len += len;
        let seq = st.next_seq;
        st.next_seq += 1;
        st.last_issued = seq;
        st.in_flight.insert(seq);
        st.lanes[idx].outstanding = Some((seq, ordinal, off));
        Ok(SlotOutcome::Reserved(Slot::new(
            WriteSeq(seq),
            kind,
            blocks,
            token_of(self.id.0, seq),
        )))
    }

    fn append(&mut self, slot: Slot, lba: u64, data: &[u8]) -> Result<Appended> {
        let parts = slot.take();
        let seq = parts.write_seq.0;
        let mut st = lock(&self.inner.state);
        st.live()?;
        let idx = self.idx();
        let own = st.lanes[idx].outstanding;
        let Some((oseq, ordinal, off)) =
            own.filter(|&(s, _, _)| s == seq && parts.token == token_of(self.id.0, seq))
        else {
            st.poison(seq);
            drop(st);
            self.inner.cv.notify_all();
            return Err(Error::invalid("slot was not issued by this lane"));
        };
        debug_assert_eq!(oseq, seq);
        st.lanes[idx].outstanding = None;
        let want = match parts.kind {
            RecordKind::Write => parts.blocks as usize * BLOCK,
            RecordKind::Hole => 0,
        };
        let loc = JournalLoc {
            lane: self.id,
            ordinal,
            offset: off + HEADER,
        };
        let fail = |st: &mut State, e: Error| {
            st.in_flight.remove(&seq);
            st.poison(seq);
            e
        };
        if data.len() != want {
            let e = fail(&mut st, Error::invalid("data does not match the slot"));
            drop(st);
            self.inner.cv.notify_all();
            return Err(e);
        }
        if st.poisoned.is_some() {
            let e = fail(&mut st, poisoned_err());
            drop(st);
            self.inner.cv.notify_all();
            return Err(e);
        }
        let injected = st.lanes[idx].fail_next.take();
        if injected == Some(Fail::Append) {
            let e = fail(
                &mut st,
                Error::Io(std::io::Error::other("injected append failure")),
            );
            drop(st);
            self.inner.cv.notify_all();
            return Err(e);
        }
        let manual = st.manual;
        let completes_now = injected.is_none() && !manual;
        let rec = Rec {
            seq,
            kind: parts.kind,
            lba,
            blocks: parts.blocks,
            data: data.to_vec(),
            off,
            completed: completes_now,
            durable: false,
            torn: false,
        };
        let lane = &mut st.lanes[idx];
        lane.accepted_max = lane.accepted_max.max(seq);
        if injected == Some(Fail::Completion) {
            lane.completions.push_back(JournalCompletion::Append {
                write_seq: parts.write_seq,
                status: IoStatus::err(5),
            });
            if let Some(w) = &lane.wake {
                w.signal();
            }
            st.in_flight.remove(&seq);
            st.poison(seq);
        } else {
            if let Some(f) = lane.files.get_mut(&ordinal) {
                f.recs.insert(off, rec);
            }
            if completes_now {
                lane.completions.push_back(JournalCompletion::Append {
                    write_seq: parts.write_seq,
                    status: IoStatus::ok(0),
                });
                if let Some(w) = &lane.wake {
                    w.signal();
                }
                st.in_flight.remove(&seq);
            } else {
                lane.pending_complete.push_back((seq, ordinal, off));
            }
        }
        drop(st);
        self.inner.cv.notify_all();
        Ok(Appended {
            write_seq: parts.write_seq,
            loc,
        })
    }

    fn submit_read(&mut self, loc: JournalLoc, mut buf: Vec<u8>, token: Token) -> Result<()> {
        let mut st = lock(&self.inner.state);
        st.live()?;
        if usize::from(loc.lane.0) >= st.lanes.len() {
            return Err(Error::invalid("lane out of range"));
        }
        let status = match st.data_at(loc, buf.len()) {
            Ok(bytes) => {
                buf.copy_from_slice(bytes);
                IoStatus::ok(u32::try_from(buf.len()).unwrap_or(u32::MAX))
            }
            Err(e) if e.kind() == lunix_iface::ErrorKind::NotFound => IoStatus::err(2),
            Err(e) => return Err(e),
        };
        let idx = self.idx();
        let lane = &mut st.lanes[idx];
        lane.completions
            .push_back(JournalCompletion::Read { token, status, buf });
        if let Some(w) = &lane.wake {
            w.signal();
        }
        Ok(())
    }

    fn poll(&mut self, out: &mut Vec<JournalCompletion>) {
        let mut st = lock(&self.inner.state);
        let idx = self.idx();
        out.extend(st.lanes[idx].completions.drain(..));
    }

    fn completed_watermark(&self) -> WriteSeq {
        let st = lock(&self.inner.state);
        let l = &st.lanes[self.idx()];
        WriteSeq(
            l.pending_complete
                .front()
                .map_or(l.accepted_max, |&(s, _, _)| s - 1),
        )
    }
}
