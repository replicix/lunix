//! The write-ahead journal: one lane per ublk queue, a group flush across
//! lanes, floor-based recovery (D7, D12, D17; DESIGN section 4.1).
//!
//! # Threads and blocking
//!
//! * [`JournalLane`] is owned by one queue thread and is **non-blocking**:
//!   `try_append`, `submit_read`, `poll` and `completed_watermark` never wait.
//! * [`Journal::submit_group_flush`] is non-blocking for the caller; the wait
//!   and the `fdatasync`s happen on the journal's flush-coordinator thread.
//!   Lane threads never wait for the coordinator.
//! * [`Journal::read_at`], [`Journal::recover`], [`Journal::tail_after`] and
//!   [`Journal::reclaim`] **block**; destage workers, the engine's startup and
//!   its cut coordinator call them, never a queue thread.
//!
//! # The `write_seq` contract (for the engine)
//!
//! `write_seq` is global and comes from the caller. Recovery stops at the
//! first gap in the merged `write_seq` order, so a `write_seq` that is handed
//! out but never reaches a lane is a hole that discards every later record,
//! including ones already acknowledged to the guest after a FLUSH. The engine
//! therefore must (1) retry a parked append (`Full`) with the **same**
//! `write_seq`, never skipping it, and (2) pass to `submit_group_flush` only an
//! `upto` such that every write with `write_seq <= upto` has **completed** (the
//! contiguous completed prefix), not merely the highest completed `write_seq`.

use std::fmt;

use lunix_format::journal::RecordKind;
use lunix_format::local::CheckpointSlot;
use lunix_format::WriteSeq;

use crate::block::IoStatus;
use crate::error::Result;
use crate::wake::WakeFd;

/// A journal lane, equal to the queue number that owns it.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct LaneId(pub u16);

/// An opaque id echoed back in a completion (a read request, a flush).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Token(pub u64);

/// The position of a record's **data**: lane, file ordinal within the lane's
/// ring, byte offset of the first data byte in that file. A record's data is
/// contiguous inside one file, so a sub-range is addressed with
/// [`JournalLoc::advance`].
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct JournalLoc {
    /// The lane.
    pub lane: LaneId,
    /// The ring file ordinal.
    pub ordinal: u32,
    /// Byte offset of the first data byte.
    pub offset: u64,
}

impl JournalLoc {
    /// The location `bytes` further into the same record's data.
    ///
    /// # Errors
    /// `Invalid` on overflow.
    pub fn advance(self, bytes: u64) -> Result<Self> {
        let offset = self
            .offset
            .checked_add(bytes)
            .ok_or(crate::Error::invalid("journal offset overflow"))?;
        Ok(Self { offset, ..self })
    }
}

/// What the caller supplies for a record. `epoch` is 0 in phase 1 and the
/// file `gen` and the record's own `off` are the journal's business, which is
/// why this is not `lunix_format::journal::RecordHeader`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct AppendRecord {
    /// Write (with data) or hole (DISCARD / WRITE_ZEROES, no data).
    pub kind: RecordKind,
    /// The global sequence number, strictly rising within a lane.
    pub write_seq: WriteSeq,
    /// First 4 KiB block.
    pub lba: u64,
    /// Length in 4 KiB blocks (at most 256 for a write).
    pub blocks: u32,
}

/// The outcome of a non-blocking append that did not fail.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AppendOutcome {
    /// The record was copied into the lane's buffer and queued. The caller's
    /// buffer is free again. Durability is reported by a later
    /// [`JournalCompletion::Append`].
    Appended {
        /// Where the data now lives (valid for reads from this moment).
        loc: JournalLoc,
    },
    /// No free ring file, or the lane's bounded buffer pool is exhausted.
    /// Nothing was consumed; retry with the same `write_seq` after a
    /// completion or a reclaim.
    Full,
    /// The host file system is out of space for the lane. Nothing consumed.
    NoSpace,
}

/// A finished journal operation.
#[derive(Debug)]
pub enum JournalCompletion {
    /// The write of the record with `write_seq` reached the file (not yet
    /// flushed: durability is the group flush).
    Append {
        /// The record.
        write_seq: WriteSeq,
        /// Success or `-errno`.
        status: IoStatus,
    },
    /// A [`JournalLane::submit_read`] finished; `buf` is the buffer that was
    /// passed in, filled on success.
    Read {
        /// The request's token.
        token: Token,
        /// Success (`bytes == buf.len()`) or `-errno`.
        status: IoStatus,
        /// The caller's buffer, returned.
        buf: Vec<u8>,
    },
}

/// One lane. Owned by a single queue thread; not `Sync`.
pub trait JournalLane: Send {
    /// This lane's id.
    fn lane(&self) -> LaneId;

    /// Copies the record into a buffer from the lane's bounded pool of aligned
    /// buffers and queues the write. Non-blocking. `data` is
    /// `blocks * 4096` bytes for a write and empty for a hole.
    ///
    /// # Errors
    /// `Invalid` for a malformed record or a non-rising `write_seq`;
    /// `Io` for a fatal ring error.
    fn try_append(&mut self, rec: &AppendRecord, data: &[u8]) -> Result<AppendOutcome>;

    /// Queues a read of `buf.len()` bytes of record data at `loc`. Non-blocking.
    /// Completes through `poll` with the buffer handed back. A location the
    /// lane has not written or has reclaimed completes with an error status.
    ///
    /// # Errors
    /// `Invalid` for a location on another lane.
    fn submit_read(&mut self, loc: JournalLoc, buf: Vec<u8>, token: Token) -> Result<()>;

    /// Moves finished operations into `out`. Non-blocking; call after the wake
    /// fd fires. Drains everything ready.
    fn poll(&mut self, out: &mut Vec<JournalCompletion>);

    /// The highest `s` such that every append with `write_seq <= s` accepted on
    /// this lane has completed its write. With nothing in flight it is the
    /// highest `write_seq` the lane accepted (0 if none). Non-blocking.
    fn completed_watermark(&self) -> WriteSeq;
}

/// Called once when a group flush finishes. May run on the journal's
/// coordinator thread or inline in `submit_group_flush`; must not block or take
/// a lock the submitter holds. Typically pushes to a list and signals wake fds.
pub type FlushDone = Box<dyn FnOnce(Result<()>) + Send + 'static>;

/// An inclusive range of write sequence numbers.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct WriteSeqRange {
    /// First.
    pub first: WriteSeq,
    /// Last (inclusive).
    pub last: WriteSeq,
}

/// A byte range in a ring file that no valid record covers.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DamagedRange {
    /// The lane.
    pub lane: LaneId,
    /// The ring file ordinal.
    pub ordinal: u32,
    /// First damaged byte.
    pub start: u64,
    /// One past the last damaged byte.
    pub end: u64,
}

/// A record found by recovery, with where its data lives.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct RecoveredRecord {
    /// The record as appended.
    pub record: AppendRecord,
    /// Its data location (holes carry a location but no data).
    pub loc: JournalLoc,
}

/// The result of [`Journal::recover`].
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Recovered {
    /// Records with `write_seq > floor`, merged across lanes in `write_seq`
    /// order, ending at the first gap in contiguity.
    pub records: Vec<RecoveredRecord>,
    /// The next free `write_seq`: one above the last record kept, or
    /// `floor + 1` when none was kept.
    pub next_seq: WriteSeq,
    /// The `write_seq` range of valid records found beyond the first gap.
    /// They are **ignored**, not an error: nothing on disk records the last
    /// flushed `write_seq`. `lunix fsck` reports them as a warning (N4).
    pub ignored_after_gap: Option<WriteSeqRange>,
    /// Damage the resync rule (D17) skipped, for fsck.
    pub damaged: Vec<DamagedRange>,
}

/// Per lane, the lowest ring file ordinal that may still hold a record above a
/// cut. Same shape as the checkpoint's `lane_tail_ordinals`.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct JournalTail {
    /// One entry per lane, indexed by lane number.
    pub lane_ordinals: Vec<u32>,
}

/// What a reclaim freed.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct ReclaimReport {
    /// Ring files recycled or deleted.
    pub files: u32,
    /// Bytes of the lanes' reserved capacity that became free (the amount for
    /// `SpaceGate::release(JournalReserved, ..)`).
    pub bytes: u64,
}

/// How full the ring is.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct JournalFill {
    /// Bytes in files that hold unreclaimed records.
    pub used_bytes: u64,
    /// Total ring capacity (`journal_max`).
    pub capacity_bytes: u64,
}

impl JournalFill {
    /// `used / capacity` in `0.0 ..= 1.0` (0 for an empty capacity).
    pub fn ratio(&self) -> f64 {
        if self.capacity_bytes == 0 {
            0.0
        } else {
            (self.used_bytes as f64 / self.capacity_bytes as f64).min(1.0)
        }
    }
}

/// The journal. `Send + Sync`; shared by the engine, destage workers and the
/// flush coordinator.
pub trait Journal: Send + Sync {
    /// The number of lanes (one per queue).
    fn lanes(&self) -> u16;

    /// Hands out lane `lane` to its queue thread. `wake` is the queue's wake
    /// fd: the lane arranges for it to be signalled when a completion is ready
    /// (an `IORING_REGISTER_EVENTFD` on the lane's ring). Call once per lane,
    /// after [`recover`](Self::recover). Control plane; may block.
    ///
    /// # Errors
    /// `Invalid` for a bad lane or a lane already open.
    fn open_lane(&self, lane: LaneId, wake: WakeFd) -> Result<Box<dyn JournalLane>>;

    /// Group flush. Non-blocking for the caller. On the coordinator thread:
    /// wait until every lane has no in-flight append with `write_seq <= upto`
    /// (equivalently each lane's `completed_watermark() >= upto`, or the lane
    /// has nothing at or below `upto`), then issue one `fdatasync` per lane
    /// that has unflushed appends, in parallel, and call `done` when all have
    /// finished. At most one group is in flight; a second submission queues
    /// behind it. A flush covers writes **completed before the group formed**;
    /// `upto` must satisfy the contiguity rule in the module docs.
    ///
    /// # Errors
    /// `Closed` after shutdown. A later failure is passed to `done`.
    fn submit_group_flush(&self, upto: WriteSeq, done: FlushDone) -> Result<()>;

    /// Blocking read of record data (destage workers, recovery replay).
    ///
    /// # Errors
    /// `NotFound` for a reclaimed location; `Io`.
    fn read_at(&self, loc: JournalLoc, buf: &mut [u8]) -> Result<()>;

    /// Scans every file of every lane (blocking) and returns the records with
    /// `write_seq > floor`, merged in `write_seq` order up to the first gap.
    /// `floor` is the tip FULL's `cut_seq` (0 at genesis). `hint` only says
    /// which files can be skipped; a lost, stale or wrong hint changes speed,
    /// never the result, and a hint whose `last_commit_seq` is above the tip's
    /// is ignored by the caller before it gets here (D17). Corruption is never
    /// a hard error: damage at or below the floor costs nothing and damage
    /// above it ends the scan (the rest is `ignored_after_gap`). Idempotent.
    /// Call before any lane is opened.
    ///
    /// # Errors
    /// `Io` only.
    fn recover(&self, hint: Option<&CheckpointSlot>, floor: WriteSeq) -> Result<Recovered>;

    /// The tail after cut `cut`: per lane, the lowest ordinal that holds a
    /// record with `write_seq > cut` (or the lane's current file when none).
    /// Goes into the checkpoint hint and into [`reclaim`](Self::reclaim).
    ///
    /// # Errors
    /// `Io`.
    fn tail_after(&self, cut: WriteSeq) -> Result<JournalTail>;

    /// Recycles or deletes ring files wholly below `upto`. Called only after
    /// the commit covering them is durable (C9). Never touches a file holding
    /// a record above the tail. Blocking.
    ///
    /// # Errors
    /// `Io`; `Invalid` for a tail with the wrong lane count.
    fn reclaim(&self, upto: &JournalTail) -> Result<ReclaimReport>;

    /// The ring's fill. Non-blocking.
    fn fill(&self) -> JournalFill;
}

impl fmt::Display for LaneId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "lane{}", self.0)
    }
}
