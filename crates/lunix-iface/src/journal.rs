//! The write-ahead journal: one lane per ublk queue, a group flush across
//! lanes, floor-based recovery (D7, D12, D17; DESIGN section 4.1).
//!
//! # Threads and blocking
//!
//! * [`JournalLane`] is owned by one queue thread and is **non-blocking**:
//!   `try_reserve`, `append`, `submit_read`, `poll` and `completed_watermark` never wait.
//! * [`Journal::submit_group_flush`] is non-blocking for the caller; the wait
//!   and the `fdatasync`s happen on the journal's flush-coordinator thread.
//!   Lane threads never wait for the coordinator.
//! * [`Journal::read_at`], [`Journal::recover`], [`Journal::tail_after`] and
//!   [`Journal::reclaim`] **block**; destage workers, the engine's startup and
//!   its cut coordinator call them, never a queue thread.
//!
//! # Who assigns `write_seq`
//!
//! The **journal** does (Fix A of the M1.B1 review; D7's "assigned by the
//! caller" is read as "taken from one global counter by the writer's lane").
//! Recovery stops at the first gap in the merged `write_seq` order, so a
//! sequence number that is issued but never reaches a lane would discard every
//! later record, including acknowledged ones. To make that impossible,
//! [`JournalLane::try_reserve`] either fails (`Full`, `NoSpace`) **without
//! consuming a sequence number**, or returns a [`Slot`] holding a freshly
//! allocated global `write_seq` that the lane has already registered as
//! in-flight. [`JournalLane::append`] then consumes the slot and cannot fail
//! except on a fatal I/O error.
//!
//! # Failure poisons the journal
//!
//! An `append` that returns `Io`, an [`JournalCompletion::Append`] with an
//! error status, or a failed `fdatasync` **poisons the journal**: every pending
//! and every later group flush with `upto >=` the failed sequence number
//! completes with `Err(Io)`, and `try_reserve` and `append` return `Io` on every
//! lane from then on. No FLUSH ever completes past a missing sequence number,
//! so the gap is at the tail and nothing acknowledged after it is promised. A
//! flush whose `upto` is below the failed sequence number is unaffected. The
//! engine also calls `SpaceEvents::fatal_io` so the volume goes read-only.
//!
//! A [`Slot`] must be appended before the next call on its lane. This is
//! checked: `try_reserve` while a slot is outstanding, or `append` with a slot
//! this lane did not issue, returns `Invalid` **and poisons the journal** (the
//! outstanding sequence number counts as the failed one). A dropped slot would
//! otherwise leave its sequence number in flight and hang every later flush
//! silently; `Slot` is `#[must_use]` and asserts in `Drop` in debug builds.
//!
//! # Group flush
//!
//! [`Journal::last_issued`] is the highest sequence number handed out. A FLUSH
//! covers **every write acknowledged before its flush group formed**: the
//! engine reads `last_issued()` when the group forms and passes it as `upto`.
//! Every acknowledged write has a sequence number `<= upto`; the coordinator
//! waits until no slot or append with `write_seq <= upto` is in flight on any
//! lane (a reserved but not yet appended slot counts), then syncs. A write
//! whose slot is outstanding at that moment is waited for, never skipped. A
//! FUA write passes its own sequence number as `upto`, which may be below
//! `last_issued()`; it waits for every lower sequence number, not for higher ones.
//!
//! # The flushed watermark
//!
//! When a group flush completes (every `fdatasync` done, nothing poisoned at
//! or below `upto`), every record at or below its `upto` is durable on every
//! lane. The journal writes the highest such `upto` into every record it
//! appends afterwards (on-disk-formats §5.2), and [`Journal::recover`]
//! reports the highest value any surviving record carries as
//! [`Recovered::flushed`]. A gap or damage at or below it is loss of durable
//! data; above it, recovery cannot tell crash residue from medium damage,
//! because a flush is witnessed only by a record appended after it completed
//! (the unwitnessed window, ADR-4). `upto` is clamped to `last_issued()` at
//! submission, so the watermark never names a number not yet written.
//!
//! # `JournalReserved` units
//!
//! Admission charges, and reclaim releases, the **encoded record length**
//! ([`record_bytes`], equal to `lunix_format::journal::Record::encoded_len`:
//! 64-byte header plus data, 8-byte aligned). Ring files are preallocated, so
//! file capacity is a different quantity and is never used for the ledger.

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

/// A record as the journal knows it: what was appended or recovered. The file
/// `gen` and the record's own offset are the journal's business, so this is not
/// `lunix_format::journal::RecordHeader`. `epoch` is 0 in phase 1.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct RecordInfo {
    /// Write (with data) or hole (DISCARD / WRITE_ZEROES, no data).
    pub kind: RecordKind,
    /// The global sequence number.
    pub write_seq: WriteSeq,
    /// First 4 KiB block.
    pub lba: u64,
    /// Length in 4 KiB blocks (at most 256 for a write).
    pub blocks: u32,
}

/// The encoded length of a record, which is what `JournalReserved` is charged
/// and released in: 64 + `blocks * 4096` for a write, 64 for a hole.
pub fn record_bytes(kind: RecordKind, blocks: u32) -> u64 {
    match kind {
        RecordKind::Write => 64 + u64::from(blocks) * 4096,
        RecordKind::Hole => 64,
    }
}

/// A reserved place in a lane with its sequence number. Obtained from
/// [`JournalLane::try_reserve`], consumed by [`JournalLane::append`]. Dropping
/// one unconsumed leaves its sequence number in flight (see the module docs).
#[derive(Debug)]
#[must_use = "a slot must be passed to JournalLane::append"]
#[non_exhaustive]
pub struct Slot {
    /// The sequence number, already registered as in-flight.
    pub write_seq: WriteSeq,
    /// Write or hole.
    pub kind: RecordKind,
    /// Length in blocks.
    pub blocks: u32,
    /// Implementation-private.
    #[doc(hidden)]
    pub token: u64,
    consumed: bool,
}

/// The contents of a consumed [`Slot`], for journal implementations.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SlotParts {
    /// The sequence number.
    pub write_seq: WriteSeq,
    /// Write or hole.
    pub kind: RecordKind,
    /// Length in blocks.
    pub blocks: u32,
    /// The implementation's token.
    pub token: u64,
}

impl Slot {
    /// For journal implementations.
    pub fn new(write_seq: WriteSeq, kind: RecordKind, blocks: u32, token: u64) -> Self {
        Self {
            write_seq,
            kind,
            blocks,
            token,
            consumed: false,
        }
    }

    /// Consumes the slot (marking it used) and returns its parts. Journal
    /// implementations call this in `append`, also on the error paths.
    pub fn take(mut self) -> SlotParts {
        self.consumed = true;
        SlotParts {
            write_seq: self.write_seq,
            kind: self.kind,
            blocks: self.blocks,
            token: self.token,
        }
    }

    /// The `JournalReserved` charge for this record.
    pub fn record_bytes(&self) -> u64 {
        record_bytes(self.kind, self.blocks)
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        debug_assert!(
            self.consumed || std::thread::panicking(),
            "journal Slot {} dropped without append",
            self.write_seq
        );
    }
}

/// The outcome of a non-blocking reservation that did not fail.
#[derive(Debug)]
#[must_use]
pub enum SlotOutcome {
    /// A slot with a fresh `write_seq`.
    Reserved(Slot),
    /// No free ring file, or the lane's bounded buffer pool is exhausted.
    /// Nothing was consumed, not even a sequence number; retry after a
    /// completion or a reclaim. The queue parks the write until then.
    Full,
    /// The host file system is out of space for the lane. Nothing consumed.
    NoSpace,
}

/// What `append` returns.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Appended {
    /// The record's sequence number (the slot's).
    pub write_seq: WriteSeq,
    /// Where the data now lives. It is readable once the matching
    /// [`JournalCompletion::Append`] has been delivered.
    pub loc: JournalLoc,
}

/// A finished journal operation.
#[derive(Debug)]
pub enum JournalCompletion {
    /// The write of the record with `write_seq` reached the file (not yet
    /// flushed: durability is the group flush).
    Append {
        /// The record.
        write_seq: WriteSeq,
        /// Success or `-errno`; an error poisons the journal.
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

    /// Reserves room for one record and allocates its global `write_seq`.
    /// Non-blocking. The sequence number is registered as in-flight in the same
    /// step, so a group flush can never miss it. On `Full` or `NoSpace` no
    /// sequence number is consumed. The caller has already charged
    /// `JournalReserved` with [`record_bytes`] and releases it on a refusal.
    ///
    /// # Errors
    /// `Invalid` for a malformed request (`blocks` 0, or above 256 for a write)
    /// or while an earlier slot of this lane is still outstanding (this also
    /// poisons the journal); `Io` once the journal is poisoned.
    fn try_reserve(&mut self, kind: RecordKind, blocks: u32) -> Result<SlotOutcome>;

    /// Copies `data` (`blocks * 4096` bytes for a write, empty for a hole) into
    /// a buffer from the lane's bounded pool of aligned buffers and queues the
    /// write. Non-blocking, and infallible apart from a fatal `Io`, which
    /// poisons the volume. The caller's buffer is free on return. Durability is
    /// reported by a later [`JournalCompletion::Append`] and made permanent by
    /// the group flush.
    ///
    /// # Errors
    /// `Invalid` if `data` does not match the slot or the slot was not issued by
    /// this lane (a caller bug; poisons the journal); `Io` for a failed write or
    /// a poisoned journal (poisons it: see the module docs).
    fn append(&mut self, slot: Slot, lba: u64, data: &[u8]) -> Result<Appended>;

    /// Queues a read of `buf.len()` bytes of record data at `loc`. Non-blocking.
    /// `loc` may name **any lane**: overlay hits for an LBA were mostly written
    /// by other queues. Precondition: that append has **completed** (its
    /// `Append` completion was delivered; the engine inserts into the overlay
    /// on that completion). Record data is 8-byte packed, so `loc.offset` is not
    /// 4 KiB aligned: the journal reads the covering aligned range into its own
    /// `O_DIRECT` buffer and copies into `buf`, which has no alignment
    /// requirement and may be recycled by the caller. Completes through `poll`
    /// of **this** lane with the buffer handed back. A location that was
    /// reclaimed completes with an error status.
    ///
    /// # Errors
    /// `Invalid` for a location outside any lane's range.
    fn submit_read(&mut self, loc: JournalLoc, buf: Vec<u8>, token: Token) -> Result<()>;

    /// Moves finished operations into `out`. Non-blocking; call after the wake
    /// fd fires. Drains everything ready.
    fn poll(&mut self, out: &mut Vec<JournalCompletion>);

    /// The highest `s` such that every append accepted on this lane with
    /// `write_seq <= s` has completed its write; with nothing in flight, the
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
    /// `write_seq` of the last record accepted before the damage in that lane
    /// (0 if none): with `next_seq` and the floor, fsck classifies the damage
    /// as above the floor (an error, it ends recovery) or at or below (a warning).
    pub prev_seq: WriteSeq,
    /// `write_seq` of the first record accepted after the damage in that lane.
    pub next_seq: Option<WriteSeq>,
}

/// A record found by recovery, with where its data lives.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct RecoveredRecord {
    /// The record as appended.
    pub record: RecordInfo,
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
    /// They are **ignored**; whether the gap is real loss or crash residue
    /// follows from [`flushed`](Self::flushed). `lunix fsck` reports them
    /// as a warning (N4).
    pub ignored_after_gap: Option<WriteSeqRange>,
    /// The first missing `write_seq`, set exactly when `ignored_after_gap` is.
    pub gap_at: Option<WriteSeq>,
    /// Damage the resync rule (D17) skipped, for fsck.
    pub damaged: Vec<DamagedRange>,
    /// The flushed watermark: the highest `flushed` value of any record found
    /// (at or below it every record was durable on every lane before a later
    /// record was written). A gap or damage at or below it is real loss; a
    /// gap above it is crash residue (ADR-4, maintainer decision of
    /// 2026-11-14). 0 when no record carries one.
    pub flushed: WriteSeq,
    /// A `write_seq` found in two valid records. Recovery stops there like at
    /// a gap, but it is never crash residue (an earlier wipe did not happen):
    /// `on_journal_damage = refuse` refuses it at any watermark.
    pub duplicate: Option<WriteSeq>,
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
    /// The sum of the **encoded record lengths** ([`record_bytes`]) of the
    /// records in the recycled files: the amount for
    /// `SpaceGate::release(JournalReserved, ..)`.
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

    /// The highest `write_seq` handed out so far (the sequence number before
    /// the first one after `recover`). The engine reads it when a flush group
    /// forms (and at C1) and passes it as `upto`. Non-blocking.
    fn last_issued(&self) -> WriteSeq;

    /// Group flush. Non-blocking for the caller. On the coordinator thread:
    /// wait until no slot or append with `write_seq <= upto` is in flight on any
    /// lane, then issue one `fdatasync` per lane that has unflushed appends, in
    /// parallel, and call `done` when all have finished. At most one group is
    /// in flight; a second submission queues behind it. A flush covers every
    /// write **acknowledged before the group formed**: with
    /// `upto = last_issued()` read at formation, all of them have a sequence
    /// number `<= upto` (see the module docs). An `upto` above
    /// [`last_issued`](Self::last_issued) is clamped to it at submission: a
    /// flush never covers a number handed out after it was submitted, and the
    /// flushed watermark it raises (on-disk-formats §5.2) never names one.
    ///
    /// If the journal is poisoned and `upto >=` the failed sequence number, `done`
    /// receives `Err(Io)` (immediately or when the poisoning happens); a flush
    /// below the failed number completes normally.
    ///
    /// # Errors
    /// `Closed` after shutdown. A later failure is passed to `done`.
    fn submit_group_flush(&self, upto: WriteSeq, done: FlushDone) -> Result<()>;

    /// Blocking read of record data from any lane (destage workers, recovery
    /// replay). Same precondition as `submit_read`.
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
    /// Call before any lane is opened; it also sets the counter behind
    /// `last_issued` and the next sequence number to `next_seq`.
    ///
    /// `recover` itself never writes and returns `Invalid` once a lane is
    /// open. Records beyond the stop point stay on disk until the first
    /// [`open_lane`](Self::open_lane) or [`reclaim`](Self::reclaim) after
    /// `recover`, which durably invalidates them (zeroes each lane's tail)
    /// before any append. **That wipe is irreversible**: a caller that must
    /// preserve those records (`fsck`) calls neither. `floor` must be at
    /// least every cut whose tail was reclaimed.
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
