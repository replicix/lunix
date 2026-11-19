//! State shared by lanes, the flush coordinator and the journal handle.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::Instant;

use lunix_iface::journal::FlushDone;
use lunix_iface::wake::WakeFd;
use lunix_iface::Error;

use crate::aligned::AlignedBuf;
use crate::config::JournalConfig;
use crate::ring::{lock, Rings};
use crate::scan::Plan;
use crate::window::Window;

/// `poison_at` value meaning "not poisoned".
pub(crate) const NOT_POISONED: u64 = u64::MAX;

/// What the coordinator needs to know about one lane.
#[derive(Debug)]
pub(crate) struct LaneFlight {
    /// `write_seq`s reserved or appended whose write has not been reaped.
    pub pending: BTreeSet<u64>,
    /// Ring files (slot indexes) with reaped writes not yet `fdatasync`ed.
    pub dirty: BTreeSet<usize>,
    /// Lowest `write_seq` reaped since the last successful sync.
    pub unsynced_min: u64,
}

impl LaneFlight {
    pub(crate) fn new() -> Self {
        Self {
            pending: BTreeSet::new(),
            dirty: BTreeSet::new(),
            unsynced_min: u64::MAX,
        }
    }
}

/// A mark the coordinator owes: the watermark `flushed` became durable when
/// the counter stood at `counter_at`; unless a later group flush with `upto
/// > counter_at` completes first (it makes a record carrying the watermark
/// durable), a mark is written at `at`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct MarkDue {
    pub flushed: u64,
    pub counter_at: u64,
    pub at: Instant,
}

pub(crate) struct CoordQ {
    pub queue: Vec<(u64, FlushDone)>,
    pub shutdown: bool,
    pub mark_due: Option<MarkDue>,
}

/// The flush coordinator's mailbox.
pub(crate) struct Coord {
    pub q: Mutex<CoordQ>,
    pub cv: Condvar,
    pub epoch: AtomicU64,
    pub waiting: AtomicBool,
}

impl Coord {
    pub(crate) fn new() -> Self {
        Self {
            q: Mutex::new(CoordQ {
                queue: Vec::new(),
                shutdown: false,
                mark_due: None,
            }),
            cv: Condvar::new(),
            epoch: AtomicU64::new(0),
            waiting: AtomicBool::new(false),
        }
    }

    /// Tells a waiting coordinator that a lane or the poison state changed.
    /// Cheap when nobody waits.
    pub(crate) fn notify(&self) {
        self.epoch.fetch_add(1, Ordering::SeqCst);
        if self.waiting.load(Ordering::SeqCst) {
            let _g = lock(&self.q);
            self.cv.notify_all();
        }
    }
}

/// Recovery and lane-handout state.
pub(crate) struct State {
    pub plan: Option<Plan>,
    pub prepared: bool,
    pub lane_open: Vec<bool>,
    pub floor: u64,
}

pub(crate) struct Shared {
    pub cfg: JournalConfig,
    pub rings: Rings,
    /// The highest `write_seq` handed out.
    pub counter: AtomicU64,
    /// The flushed watermark: the highest `upto` of a group flush that
    /// completed (every `fdatasync` done, nothing poisoned at or below it),
    /// or the recovered records once `prepare` made them durable. Written
    /// into every record appended (on-disk-formats §5.2).
    pub flushed: AtomicU64,
    /// The highest watermark a durable record or mark is known to carry.
    pub witnessed: AtomicU64,
    /// Marks written since `open` (a metric).
    pub marks_written: AtomicU64,
    /// Watermarks a mark could not be written for because the mark ring had
    /// no free file (a metric).
    pub marks_skipped: AtomicU64,
    /// The last watermark counted in `marks_skipped`.
    pub mark_skipped_at: AtomicU64,
    /// The mark ring was full at the last attempt (the warning is logged
    /// once per episode).
    pub mark_ring_full: AtomicBool,
    /// The `write_seq` of the last mark written.
    pub last_mark: AtomicU64,
    /// Bytes the journal holds locked (`JournalConfig::lock_memory`).
    pub locked: AtomicU64,
    /// The lowest sequence number of a failed append, dropped slot or failed
    /// sync; `NOT_POISONED` when healthy.
    pub poison_at: AtomicU64,
    pub flights: Vec<Mutex<LaneFlight>>,
    pub coord: Coord,
    pub st: Mutex<State>,
    pub closed: AtomicBool,
    /// The highest cut a caller asked a tail for (initially the floor).
    pub max_cut_asked: AtomicU64,
    pub zero: AlignedBuf,
    /// The resident journal window (ADR-33).
    pub window: Window,
    /// Lanes whose next append fails (test hook).
    pub fail_next: Mutex<BTreeSet<u16>>,
    pub fail_any: AtomicBool,
    /// The next group's `fdatasync` fails (test hook).
    pub fail_sync: AtomicBool,
    /// The next batch write completes as failed (test hook).
    pub fail_write: AtomicBool,
    /// Each lane's wake fd, kept from `open_lane`: reclaim signals a lane as
    /// soon as one of its files is free (ADR-33), not at the end of the
    /// whole reclaim.
    pub wakes: Vec<Mutex<Option<WakeFd>>>,
    /// Called after each queue file reclaim freed, with the lane and the
    /// number of files freed so far by that reclaim (test hook).
    pub reclaim_hook: Mutex<Option<ReclaimHook>>,
}

/// A test hook of reclaim: see [`Shared::reclaim_hook`].
pub(crate) type ReclaimHook = std::sync::Arc<dyn Fn(u16, u32) + Send + Sync>;

impl Shared {
    pub(crate) fn poisoned_at(&self) -> Option<u64> {
        let p = self.poison_at.load(Ordering::SeqCst);
        (p != NOT_POISONED).then_some(p)
    }

    /// Poisons the journal at `seq` (keeping the lowest) and wakes the
    /// coordinator so waiting flushes fail.
    pub(crate) fn poison(&self, seq: u64) {
        self.poison_at.fetch_min(seq, Ordering::SeqCst);
        self.coord.notify();
    }

    /// Signals lane `lane`'s queue: a ring file of it is free, so a write
    /// parked for the ring may fit now.
    pub(crate) fn wake_lane(&self, lane: u16) {
        if let Some(w) = self.wakes.get(usize::from(lane)) {
            if let Some(w) = lock(w).as_ref() {
                w.signal();
            }
        }
    }

    pub(crate) fn poisoned_error() -> Error {
        Error::Io(std::io::Error::other("journal poisoned"))
    }

    /// True when no lane has a reserved or appending `write_seq <= upto`.
    pub(crate) fn lanes_clear(&self, upto: u64) -> bool {
        self.flights
            .iter()
            .all(|f| lock(f).pending.first().is_none_or(|&m| m > upto))
    }
}
