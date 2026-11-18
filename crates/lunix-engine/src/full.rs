//! The engine's side of the bounded full volume (ADR-32, steps T1 and T2 of
//! `docs/explanation/research/2026-11-18-write-backpressure.md`).
//!
//! * **Ring-full waits have a deadline** (path P6). A write refused by a full
//!   journal ring parks in its queue, not in the gate. It fails at once when
//!   the engine has stopped committing (`EIO`), and after `full_timeout` from
//!   its arrival otherwise: `ENOSPC` if the last commit failed for space (the
//!   host file system is full), else `EIO`. The background coordinator wakes
//!   the queues every period while the ring is full, so the deadline is seen
//!   without new I/O.
//! * **A discard frees space on a full volume** (path P4, decision M6).
//!   Discards and write-zeroes are admitted from the reserved region and do
//!   not queue behind parked writes. While writes are refused or parked for
//!   space, the coordinator cuts as soon as hole records are journaled (at
//!   most once a second) and then writes `retain_last + 1` further FULLs, one
//!   a second (the window turn), so the discarded extents leave every
//!   retained root and the urgent pass can delete them after the floor grace.
//!
//! "Refused or parked for space" is read from the queues' own outcomes (a
//! reservation parked, refused or expired, and none admitted since) and from
//! the gate's waiter count, so the iface `SpaceSnapshot` needs no new field.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::Duration;

use lunix_iface::{ErrorKind, Result};

use crate::coord::Coord;
use crate::engine::{CommitOutcome, Inner};

/// The shortest spacing of the cuts the full volume asks for.
const SPACE_CUT_SPACING: Duration = Duration::from_secs(1);

/// The shortest spacing of the "ring full for full_timeout" log line.
const RING_LOG_SPACING_MS: u64 = 5_000;

const EIO: i32 = 5;
const ENOSPC: i32 = 28;

/// The full-volume state the queues and the coordinator share.
#[derive(Debug)]
pub(crate) struct FullState {
    /// The ring-full deadline, in milliseconds.
    full_timeout_ms: AtomicU64,
    /// A write reservation was parked, refused or expired for space, and no
    /// write was admitted since: the gate is in its stop or full state.
    blocked: AtomicBool,
    /// The last commit attempt failed for space.
    commit_nospace: AtomicBool,
    /// The `write_seq` of the newest hole record appended (0: none).
    last_hole: AtomicU64,
    /// FULLs the window turn still owes.
    turn: AtomicU32,
    /// `clock.monotonic()` in ms of the last ring-full expiry log line + 1.
    ring_logged: AtomicU64,
    /// The pause between two urgent passes the space ladder asks for, in ms
    /// (`Inner::run_urgent`, ADR-32 P5).
    ladder_backoff_ms: AtomicU64,
}

impl FullState {
    pub(crate) fn new(full_timeout: Duration) -> Self {
        Self {
            full_timeout_ms: AtomicU64::new(ms(full_timeout)),
            blocked: AtomicBool::new(false),
            commit_nospace: AtomicBool::new(false),
            last_hole: AtomicU64::new(0),
            turn: AtomicU32::new(0),
            ring_logged: AtomicU64::new(0),
            ladder_backoff_ms: AtomicU64::new(ms(crate::gc::SPACE_URGENT_MIN)),
        }
    }

    pub(crate) fn ladder_backoff(&self) -> Duration {
        Duration::from_millis(self.ladder_backoff_ms.load(Ordering::Relaxed))
    }

    pub(crate) fn set_ladder_backoff(&self, d: Duration) {
        self.ladder_backoff_ms.store(ms(d), Ordering::Relaxed);
    }

    pub(crate) fn set_full_timeout(&self, d: Duration) {
        self.full_timeout_ms.store(ms(d).max(1), Ordering::Relaxed);
    }

    fn full_timeout(&self) -> Duration {
        Duration::from_millis(self.full_timeout_ms.load(Ordering::Relaxed))
    }

    /// Records the outcome of a write's space reservation. Stores only on a
    /// change: the admitted path stays a load.
    pub(crate) fn set_blocked(&self, blocked: bool) {
        if self.blocked.load(Ordering::Relaxed) != blocked {
            self.blocked.store(blocked, Ordering::Relaxed);
        }
    }

    pub(crate) fn is_blocked(&self) -> bool {
        self.blocked.load(Ordering::Relaxed)
    }

    /// A hole record with this number was appended.
    pub(crate) fn note_hole(&self, write_seq: u64) {
        self.last_hole.fetch_max(write_seq, Ordering::AcqRel);
    }

    /// Records whether a commit attempt failed for space.
    pub(crate) fn note_commit<T>(&self, res: &Result<T>) {
        let nospace = matches!(res, Err(e) if e.kind() == ErrorKind::NoSpace);
        self.commit_nospace.store(nospace, Ordering::Relaxed);
    }

    /// FULLs the window turn still owes (tests and diagnostics).
    pub(crate) fn turn_left(&self) -> u32 {
        self.turn.load(Ordering::Relaxed)
    }
}

fn ms(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

impl Inner {
    /// The verdict on a write parked for a full journal ring that arrived at
    /// `arrived` (`clock.monotonic()`): `None` keeps it waiting, else the
    /// errno it fails with.
    pub(crate) fn ring_wait_verdict(&self, arrived: Duration) -> Option<i32> {
        if self.read_only() {
            return Some(EIO);
        }
        let now = self.clock.monotonic();
        let timeout = self.full.full_timeout();
        if now.saturating_sub(arrived) < timeout {
            return None;
        }
        let nospace = self.full.commit_nospace.load(Ordering::Relaxed);
        let now_ms = ms(now).saturating_add(1);
        let last = self.full.ring_logged.load(Ordering::Relaxed);
        if last == 0 || now_ms.saturating_sub(last) >= RING_LOG_SPACING_MS {
            self.full.ring_logged.store(now_ms, Ordering::Relaxed);
            tracing::warn!(
                full_timeout_ms = ms(timeout),
                last_commit_error = if nospace { "no space" } else { "other" },
                "write failed: journal ring full for full_timeout"
            );
        }
        Some(if nospace { ENOSPC } else { EIO })
    }

    /// Whether the full volume wants a cut now (`Some(force)`): while writes
    /// are refused or parked for space, once a second, for hole records not
    /// yet cut (`Some(false)`) or for the FULLs the window turn still owes
    /// (`Some(true)`, an empty FULL on an idle volume).
    pub(crate) fn space_cut(&self, co: &Coord) -> Option<bool> {
        let blocked = self.full.is_blocked() || self.space.snapshot().waiters > 0;
        if !blocked {
            self.full.turn.store(0, Ordering::Relaxed);
            return None;
        }
        let now = self.clock.monotonic();
        if now.saturating_sub(co.last_commit_at) < SPACE_CUT_SPACING {
            return None;
        }
        if self.full.turn.load(Ordering::Relaxed) > 0 {
            return Some(true);
        }
        (self.full.last_hole.load(Ordering::Acquire) > co.tip.cut_seq.0).then_some(false)
    }

    /// After a tick's commit: a cut that took hole records while writes were
    /// refused starts the window turn; a turn FULL counts down.
    pub(crate) fn after_space_cut(
        &self,
        prev_cut: u64,
        wanted: Option<bool>,
        res: &Result<CommitOutcome>,
    ) {
        let Ok(CommitOutcome::Committed { cut_seq, .. }) = res else {
            return;
        };
        let hole = self.full.last_hole.load(Ordering::Acquire);
        let took_holes = hole > prev_cut && hole <= cut_seq.0;
        if took_holes && (self.full.is_blocked() || self.space.snapshot().waiters > 0) {
            let n = self.cfg.retain_last.saturating_add(1);
            self.full.turn.store(n, Ordering::Relaxed);
            tracing::info!(
                cut = cut_seq.0,
                fulls = n,
                "volume full: a cut took discards; turning the retained window"
            );
        } else if wanted == Some(true) {
            let _ = self
                .full
                .turn
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |t| t.checked_sub(1));
        }
    }
}
