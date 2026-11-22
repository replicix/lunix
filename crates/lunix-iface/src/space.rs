//! The space ledger and the space ladder (DESIGN section 2 "Space policy",
//! section 4.3; ADR-14).
//!
//! # Threads and blocking
//!
//! [`SpaceGate::reserve`] is called from queue threads and is **non-blocking**:
//! it either charges the ledger, parks the request behind a [`Waiter`], or
//! refuses. Every other method is also non-blocking (atomic counters and short
//! locks). A [`LadderStep`] runs on the gate's own background thread, never on
//! a queue thread, and may block (the compact step is GC's urgent pass).
//!
//! # Accounting walk-through
//!
//! The ledger is `used = JournalReserved + Segments + Cache + Map + Index +
//! Commits`; `Headroom` is a pseudo-kind holding the *bounds* charged ahead of
//! time for commit-time objects. It is conservative between C2 and C9 (journal
//! bytes and segment bytes are both counted) and exact outside that window.
//! M8, M9, M11 and M17 cite these steps and test them:
//!
//! 1. *Admission* of a write: `reserve(JournalReserved, journal::record_bytes(kind,
//!    blocks))`, the encoded record length, before `JournalLane::try_reserve`
//!    (released again if that returns `Full` or `NoSpace`). May park or
//!    refuse; this is the only place a guest write waits for space, and
//!    inside the soft band below the limit the place where it is paced
//!    (ADR-32: [`ReserveOutcome::Paced`]).
//! 2. *C2* (cut taken): `reserve_headroom(Segments, ulen_of_nonzero_chunks)` as
//!    an upper bound for the segments the cut will write, and
//!    `reserve_headroom(Headroom, map_bound + commit_bound + index_bound)` for
//!    the objects C5, C6 and C8 will create. Neither parks or consults the free
//!    space limit; they draw on the reserved headroom region.
//! 3. *C4* (segments sealed): `transfer(Segments, Segments, bound, actual)`.
//! 4. *C5, C6, C8* (map pack, commit, index run durable):
//!    `transfer(Headroom, Map, bound, actual)`, `transfer(Headroom, Commits,
//!    bound, actual)`, `transfer(Headroom, Index, bound, actual)`.
//! 5. *C9* (the cut is durable): `release(JournalReserved, charged)` with the
//!    sum of the charges of every record at or below the cut (the same unit,
//!    encoded record length), whichever ring files the journal recycled
//!    (ADR-32; `ReclaimReport::bytes` is not the amount), and
//!    `release(Headroom, remainder)` for any bound left unused.
//! 6. *GC delete*: `release(kind, bytes)` for each file removed, after the
//!    delete is durable.
//! 7. *Compaction output*: charged from headroom like a cut's segments
//!    (`reserve_headroom(Segments, bound)`, then `transfer` at seal).
//!
//! `transfer` is atomic: no observer sees the bytes in both kinds or in
//! neither. It never parks and never fails for lack of space (the headroom is
//! what pays for the difference); the ledger may exceed the limit by at most
//! the headroom while a cut is in flight.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

use lunix_format::local::SpaceSettings;
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::wake::WakeFd;

/// What a ledger entry pays for.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum SpaceKind {
    /// Journal bytes admitted and not yet reclaimed.
    JournalReserved,
    /// Sealed segments (and bounds for ones being written).
    Segments,
    /// Cached copies of remote data (none in phase 1).
    Cache,
    /// Map packs.
    Map,
    /// Index runs.
    Index,
    /// Commit objects.
    Commits,
    /// The headroom pseudo-kind: bounds held for commit-time objects.
    Headroom,
}

impl SpaceKind {
    /// Every kind, in ledger order.
    pub const ALL: [SpaceKind; 7] = [
        Self::JournalReserved,
        Self::Segments,
        Self::Cache,
        Self::Map,
        Self::Index,
        Self::Commits,
        Self::Headroom,
    ];

    /// The position in [`SpaceSnapshot::by_kind`].
    pub fn index(self) -> usize {
        self as usize
    }
}

/// The state of a parked request.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WaiterState {
    /// Still waiting.
    Pending,
    /// Space was found and **is already charged**; proceed.
    Granted,
    /// `full_timeout` expired; nothing is charged; fail with `NoSpace`.
    TimedOut,
    /// The requester withdrew; nothing is charged.
    Cancelled,
}

const PENDING: u8 = 0;
const GRANTED: u8 = 1;
const TIMED_OUT: u8 = 2;
const CANCELLED: u8 = 3;

#[derive(Debug)]
struct WaiterShared {
    state: AtomicU8,
    wake: Option<WakeFd>,
}

/// A handle to a parked reservation. The requester polls [`state`](Self::state)
/// from `QueueHandler::poll`; the gate decides the outcome and signals the
/// requester's wake fd. The first transition wins; later ones are no-ops.
#[derive(Clone, Debug)]
pub struct Waiter(Arc<WaiterShared>);

impl Waiter {
    /// A pending waiter that signals `wake` (if any) on a transition. For
    /// gate implementations.
    pub fn new(wake: Option<WakeFd>) -> Self {
        Self(Arc::new(WaiterShared {
            state: AtomicU8::new(PENDING),
            wake,
        }))
    }

    /// The current state.
    pub fn state(&self) -> WaiterState {
        match self.0.state.load(Ordering::Acquire) {
            PENDING => WaiterState::Pending,
            GRANTED => WaiterState::Granted,
            TIMED_OUT => WaiterState::TimedOut,
            _ => WaiterState::Cancelled,
        }
    }

    fn transition(&self, to: u8) -> bool {
        let won = self
            .0
            .state
            .compare_exchange(PENDING, to, Ordering::AcqRel, Ordering::Acquire)
            .is_ok();
        if won {
            if let Some(w) = &self.0.wake {
                w.signal();
            }
        }
        won
    }

    /// Gate side: marks the reservation granted (after charging it). Returns
    /// false if the waiter already left the pending state.
    pub fn grant(&self) -> bool {
        self.transition(GRANTED)
    }

    /// Gate side: marks the wait expired.
    pub fn expire(&self) -> bool {
        self.transition(TIMED_OUT)
    }

    /// Requester side: withdraws. If this returns false because the state was
    /// already `Granted`, the bytes are charged and the requester must
    /// `release` them.
    pub fn cancel(&self) -> bool {
        self.0
            .state
            .compare_exchange(PENDING, CANCELLED, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }
}

/// What a reservation is for, which decides whether the paced band slows it
/// (ADR-32).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ReserveClass {
    /// An ordinary write: paced inside the band.
    #[default]
    Bulk,
    /// The write an `fsync` waits on (a FUA write of at most
    /// [`SYNC_MAX_BYTES`] of data): admitted at once inside the band and
    /// charged to its bucket. At the hard mark it waits like any write.
    Sync,
    /// The retry of a write the journal ring refused after the gate admitted
    /// it (milestone jstall): it paid its pace and its tokens at the first
    /// pass and takes none again, neither from the bucket nor from the delay
    /// cap's overdraw, and it does not make the requests behind it due. It is
    /// admitted at once when it fits and no request that waits for space
    /// stands before it; else it waits for space like any request.
    Retry,
}

/// The largest FUA write (data bytes) that is a [`ReserveClass::Sync`]
/// reservation: the journal commit block of ext4, the log write of xfs.
pub const SYNC_MAX_BYTES: u64 = 256 << 10;

/// The answer to [`SpaceGate::reserve`].
#[derive(Debug)]
pub enum ReserveOutcome {
    /// Charged; proceed.
    Reserved,
    /// Not enough space now (`on_full = wait`): the request is parked FIFO
    /// behind this handle and charged when it is granted.
    Parked(Waiter),
    /// The request fits but the band paces it (ADR-32): parked FIFO behind
    /// this handle, charged when the token bucket has its bytes or the delay
    /// cap passes. Not a refusal for space: the volume is slow, not full.
    /// A judgment at reserve time: the request can become a space waiter
    /// later (a commit's charge or a FUA write took the room) and the caller
    /// is not told; it reads [`ThrottleFigures::space_waiters`] for that.
    /// Under `on_full = error` such a request is refused, never latched, at
    /// the gate's next grant pass (a release, a transfer or its timer: within
    /// a second), not at the instant it stops fitting. A gate that does not pace never returns it.
    Paced(Waiter),
    /// Refused (`on_full = error`, or the wait would exceed `full_timeout`
    /// at once): nothing charged.
    NoSpace,
}

/// One store byte per journal byte, in the thousandths of the store factor:
/// its lower clamp.
pub const FACTOR_ONE: u64 = 1000;

/// Sixteen store bytes per journal byte (a 64 KiB chunk written 4 KiB at a
/// time, incompressible): the upper clamp of the engine's `A_hat`.
pub const FACTOR_MAX: u64 = 16_000;

/// The factor before the engine has measured a cut: 2.0 (the measured write
/// amplification of the first cuts is 2.4 to 2.8), so a volume that starts
/// full of journal does not start at one.
pub const FACTOR_INITIAL: u64 = 2000;

/// The gate's write-throttle state (ADR-32).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThrottleState {
    /// The headroom is above the soft mark (or the band is off): writes are
    /// untouched.
    #[default]
    Open,
    /// Inside the band: admission is paced.
    Slowdown,
    /// A write waits for space.
    Stop,
    /// Latched full: writes fail at once.
    Full,
}

impl ThrottleState {
    /// The state's name as the status prints it.
    pub fn name(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Slowdown => "slowdown",
            Self::Stop => "stop",
            Self::Full => "full",
        }
    }
}

/// The throttle's figures (ADR-32, research note section 7). The counters
/// live in the gate and start at zero when it is built (the attach); the
/// checkpoint holds none of them.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct ThrottleFigures {
    /// The state now.
    pub state: ThrottleState,
    /// When the state was entered, in milliseconds of the gate's clock.
    pub state_since_ms: u64,
    /// `limit - used`, bytes.
    pub headroom: u64,
    /// The soft mark: pacing starts below this headroom (0 with the band off).
    pub soft: u64,
    /// The resume mark of the full latch.
    pub resume: u64,
    /// The bucket's rate now, bytes per second; `None` outside the band.
    pub rate: Option<u64>,
    /// Requests waiting for tokens.
    pub paced_waiters: u64,
    /// Requests waiting for space.
    pub space_waiters: u64,
    /// Requests that waited in the gate, for tokens or for space, or stood
    /// in their queue while the pace was active, and were admitted. Their
    /// wait counts from their arrival at the queue. A wait in a queue while
    /// nothing paces (behind a request parked for the journal ring, a slow
    /// append) is not counted here.
    pub delayed_writes: u64,
    /// The summed wait of those, in milliseconds.
    pub delayed_ms: u64,
    /// The longest wait of one request (admitted or failed) from its arrival
    /// at its queue, ms: the time it stood behind others included.
    pub delay_max_ms: u64,
    /// Requests admitted by the delay cap.
    pub delay_capped: u64,
    /// FUA requests admitted at once inside the band.
    pub bypassed_fua: u64,
    /// Reservations that failed for space (a refusal, an expiry, the latch).
    pub enospc: u64,
    /// Times the gate entered `Stop`.
    pub stops: u64,
    /// Times the volume latched full.
    pub fulls: u64,
    /// Time spent outside `Open`, in milliseconds.
    pub throttled_ms: u64,
    /// Bytes admitted per second, a 10 s average.
    pub ingest_rate: u64,
    /// Bytes of store freed per second, a 10 s average.
    pub reclaim_rate: u64,
    /// Requests the ring pace (not the space curve) delayed and the gate
    /// admitted (milestone jstall). Kept apart from `delayed_writes` and
    /// the figures around it, which are the space throttle's.
    pub ring_paced_writes: u64,
    /// Their summed wait, milliseconds.
    pub ring_paced_ms: u64,
    /// The longest wait of one of them, milliseconds.
    pub ring_pace_max_ms: u64,
    /// Requests the ring pace admitted by the delay cap.
    pub ring_pace_capped: u64,
    /// FUA requests the ring pace let through at once.
    pub ring_pace_bypassed_fua: u64,
    /// The ring pace the gate applies now, bytes per second (0: none,
    /// also with the band off or closing).
    pub ring_pace_rate: u64,
    /// The store factor the gate holds now, thousandths of a store byte per
    /// journal byte (the engine's `A_hat`; [`FACTOR_INITIAL`] before its
    /// first measured cut). Held whether or not the gate paces by it.
    pub store_factor_milli: u64,
    /// The factor the curve reads: `store_factor_milli` times the margin
    /// (`throttle_pace_store_margin_percent`).
    pub store_factor_biased_milli: u64,
    /// The journal bytes reserved and not yet in a cut (`J`), the cut in
    /// flight left out.
    pub journal_uncut: u64,
    /// The projected headroom `headroom - (A_biased - 1) * J`, bytes. What
    /// the band's curve reads when it paces by store bytes; computed from the
    /// held figures whether or not it does.
    pub store_h_proj: u64,
    /// With the store pace on: the pace applies now although the raw
    /// headroom is at or above the soft mark (the state is `open`).
    pub store_paced_above_soft: bool,
    /// Time the store pace applied above the soft mark, milliseconds.
    pub store_above_soft_ms: u64,
    /// Requests admitted after a wait while the raw headroom was at or above
    /// the soft mark (only the store pace could delay them); also counted in
    /// `delayed_writes`.
    pub store_delayed_writes: u64,
    /// Their summed wait, milliseconds.
    pub store_delayed_ms: u64,
}

/// A point-in-time view of the ledger.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct SpaceSnapshot {
    /// Bytes charged per kind, indexed by [`SpaceKind::index`].
    pub by_kind: [u64; 7],
    /// `min(cache_max, fs_free - free_floor)` as of the last refresh.
    pub limit: u64,
    /// Free bytes on the host file system as of the last refresh.
    pub fs_free: u64,
    /// The reserved headroom region (default 256 MiB).
    pub headroom_total: u64,
    /// Requests parked.
    pub waiters: u64,
    /// True after a fatal host I/O error.
    pub read_only: bool,
    /// Latched full (ADR-32): a wait for space expired, and writes fail at
    /// once until the headroom reaches the resume mark. Nothing is parked
    /// while it holds.
    pub full: bool,
    /// The write throttle (ADR-32). `waiters` counts both kinds of parked
    /// request; `throttle.space_waiters` is the part that waits for space.
    pub throttle: ThrottleFigures,
}

impl SpaceSnapshot {
    /// Bytes counted against the limit: every kind but `Headroom`.
    pub fn used(&self) -> u64 {
        SpaceKind::ALL
            .iter()
            .filter(|k| **k != SpaceKind::Headroom)
            .map(|k| self.by_kind[k.index()])
            .sum()
    }
}

/// Which ladder step a callback is.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum LadderStepKind {
    /// Evict clean cache.
    EvictCache,
    /// Evict cold segments that are synced to a backend.
    EvictSynced,
    /// Compact (GC's urgent pass in phase 1).
    Compact,
    /// Throttle destage.
    ThrottleDestage,
}

/// How hard a step should try.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Urgency {
    /// Background pressure.
    Normal,
    /// A write is parked.
    Urgent,
}

/// Bytes a step freed; 0 means "nothing".
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Freed(pub u64);

/// One rung of the ladder.
pub trait LadderStep: Send {
    /// Tries to free about `want_bytes` and reports what it freed. Runs on the
    /// gate's thread and may block; must not call back into `reserve`.
    fn run(&mut self, want_bytes: u64, urgency: Urgency) -> Result<Freed>;
}

/// Registration of the ladder's callbacks.
pub trait Ladder {
    /// Installs `step` for `kind`, replacing an earlier one. The gate runs the
    /// registered steps in the order `EvictCache`, `EvictSynced`, `Compact`,
    /// `ThrottleDestage` until enough is freed, then applies `on_full`.
    fn register(&self, kind: LadderStepKind, step: Box<dyn LadderStep>);
}

/// Host I/O error reporting.
pub trait SpaceEvents {
    /// A host `EIO` (or equivalent) was seen: the volume becomes read-only
    /// (writes fail, reads continue). `what` names the operation, for the log.
    fn fatal_io(&self, what: &str);

    /// True once `fatal_io` was called.
    fn is_read_only(&self) -> bool;
}

/// The ledger. `Send + Sync`; see the module docs for the walk-through.
pub trait SpaceGate: Ladder + SpaceEvents + Send + Sync {
    /// Charges `bytes` to `kind` if the ledger allows, else parks or refuses
    /// per `on_full`. `wake` is signalled when a parked request resolves.
    /// Reads never call this. A bulk write; see
    /// [`reserve_since`](Self::reserve_since) for the class.
    ///
    /// # Errors
    /// `ReadOnly` after `fatal_io`.
    fn reserve(&self, kind: SpaceKind, bytes: u64, wake: Option<&WakeFd>)
        -> Result<ReserveOutcome>;

    /// [`reserve`](Self::reserve) for a request that arrived at `arrived`
    /// (`Clock::monotonic` of the gate's clock), earlier than this call: a
    /// write that waited in its queue behind a parked one, or for a full
    /// journal ring. A parked request's `full_timeout` counts from
    /// `arrived`, not from this call (ADR-32: a write waits at most
    /// `full_timeout` from its arrival). One that does not fit and is
    /// already past it is refused (`NoSpace`) without parking; that refusal
    /// does not latch the volume (it never waited in the gate). Required: a
    /// gate that parks must honour `arrived`. `class` tells a gate that
    /// paces whether the band may slow the request.
    ///
    /// # Errors
    /// As [`reserve`](Self::reserve).
    fn reserve_since(
        &self,
        kind: SpaceKind,
        bytes: u64,
        wake: Option<&WakeFd>,
        arrived: std::time::Duration,
        class: ReserveClass,
    ) -> Result<ReserveOutcome>;

    /// Charges `bytes` to `kind` from the reserved headroom: no limit check,
    /// never parks.
    ///
    /// # Errors
    /// `NoSpace` only if the headroom itself is exhausted.
    fn reserve_headroom(&self, kind: SpaceKind, bytes: u64) -> Result<()>;

    /// Returns `bytes` of `kind` (saturating at zero is a bug and is reported
    /// by debug assertions). Wakes parked requests that now fit, FIFO.
    fn release(&self, kind: SpaceKind, bytes: u64);

    /// Startup accounting of files that already exist: charges `bytes` to `kind`
    /// without a limit check; never parks, never fails for lack of space.
    ///
    /// # Errors
    /// Whatever `transfer` reports.
    fn charge(&self, kind: SpaceKind, bytes: u64) -> Result<()> {
        self.transfer(kind, kind, 0, bytes)
    }

    /// Atomically releases `from_bytes` of `from_kind` and charges `to_bytes` of
    /// `to_kind`; the kinds may be equal (a true-up).
    ///
    /// # Errors
    /// `Invalid` if `from_bytes` exceeds what `from_kind` holds.
    fn transfer(
        &self,
        from_kind: SpaceKind,
        to_kind: SpaceKind,
        from_bytes: u64,
        to_bytes: u64,
    ) -> Result<()>;

    /// Expires parked requests whose `full_timeout` has passed on the gate's
    /// clock. A real gate also ticks itself; tests with a manual clock call
    /// this after advancing it. Returns how many expired.
    fn tick(&self) -> usize;

    /// The ledger now.
    fn snapshot(&self) -> SpaceSnapshot;

    /// Live reconfiguration of the policy (`lunix space`). A gate knows no
    /// volume size: the caller resolves a `cache_max_percent` to
    /// `cache_max` bytes first (FREEZE F2).
    ///
    /// # Errors
    /// `Invalid` for a setting that fails `VolumeJson` validation, or one
    /// that still carries a `cache_max_percent`.
    fn reconfigure(&self, settings: &SpaceSettings) -> Result<()>;

    /// Paces the admission of journal reservations to `rate` bytes per
    /// second while the journal ring is nearly full (ADR-32's band, fed by the
    /// engine with the drain rate of its last cuts; milestone jstall), or
    /// ends the pace (`None`). It is a second input of the band, not a
    /// space state: the throttle state, the GC pressure and the headroom
    /// marks do not follow it. The band's own rules apply to it: a
    /// [`ReserveClass::Sync`] request is admitted at once and charged to
    /// the bucket, and no request is delayed longer than the delay cap
    /// (counted from its arrival), so a ring pace never fails a request.
    /// The rate is raised to the band's minimum rate. A gate without a band
    /// ignores it.
    fn set_ring_pace(&self, _rate: Option<u64>) {}

    /// Tells the gate the store factor and the cut in flight (P1': the band
    /// paces in store bytes). `factor_milli` is the engine's estimate of the
    /// store bytes one journal byte becomes at the cut, in thousandths (at
    /// least 1000); `in_flight` the journal bytes of the cut being committed
    /// (0 between cuts), which already hold their segment bound and are not
    /// counted again. The gate derives the journal bytes not yet cut from its
    /// own `JournalReserved` charge less `in_flight`, so the figure is as
    /// fresh as the ledger. A gate without a band, or with store-byte pacing
    /// off, only records it for the status.
    fn set_store_pace(&self, _factor_milli: u64, _in_flight: u64) {}

    /// True when the gate paces by store bytes (`throttle_pace_store_bytes`):
    /// the engine then pushes the factor and the cut in flight, and does
    /// nothing for it otherwise.
    fn store_pace_enabled(&self) -> bool {
        false
    }
}
