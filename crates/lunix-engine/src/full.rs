//! The engine's side of the bounded full volume (ADR-32, steps T1 and T2 of
//! `docs/explanation/research/2026-11-18-write-backpressure.md`).
//!
//! * **Ring-full waits have a deadline** (path P6). A write refused by a full
//!   journal ring parks in its queue, not in the gate. It fails at once when
//!   the engine has stopped committing or the volume is read-only (`EIO`).
//!   Otherwise it fails after `full_timeout` without ring progress when a
//!   commit attempt failed since it arrived, and after `max(full_timeout,
//!   60 s)` without ring progress while commits succeed (ordinary journal
//!   back-pressure must not turn into `EIO` on a healthy volume with a short
//!   `full_timeout`): `ENOSPC` while the failed commits since the last success
//!   failed for space, else `EIO`. **Ring progress** is per lane (milestone
//!   jstall): a ring file freed on the write's lane, or a sign of life of the
//!   cut that is running (a segment appended or sealed, C5, C6, the reclaim).
//!   The time counts from the later of the write's arrival and the last
//!   progress, so a slow but moving ring never fails a write, a cut longer
//!   than the deadline that keeps working does not either, and a lane that
//!   gets no file fails its parked writes at their deadline whatever the
//!   other lanes do. The queues judge every parked write
//!   of a queue that waits for the ring (not only the head) at each wake and
//!   on the timer thread's signal ([`Inner::signal_ring_parked`]), which runs
//!   while a commit holds the coordinator.
//!   Once the volume began to close
//!   ([`Engine::begin_close`](crate::Engine::begin_close)) it answers as soon
//!   as a commit attempt failed for space or I/O after the close, and at the
//!   bound the daemon gives (two thirds of `quiesce_timeout`) otherwise, so a
//!   quiesce can drain. Only `NoSpace` and `Io` failures count anywhere: a
//!   benign one (writes in flight at the cut) neither shortens nor ends a
//!   ring wait, and neither does one of another kind (`MapFull`, `Corrupt`):
//!   with such a failure on every attempt a ring-parked write waits
//!   `max(full_timeout, 60 s)`, or the whole bound at quiesce, instead of
//!   `full_timeout` (bounded; a stopped engine still fails it at once). The
//!   close is one-way: after the bound every later ring-full park fails at
//!   once (a failed quiesce leaves the device refusing I/O).
//! * **A discard frees space on a full volume** (path P4, decision M6). Hole
//!   records are charged with `SpaceGate::charge` (the 64 bytes go into a
//!   preallocated ring file), never refused for space, and do not queue
//!   behind parked writes. While writes are refused or parked for space, the
//!   coordinator cuts hole records not yet cut (at most once a second) and
//!   then owes forced FULLs until `retain_last + 1` commits followed the last
//!   commit that took a hole record (the window turn), so the discarded
//!   extents leave every retained root. The debt is a function of state (the
//!   tip and that commit's number), not of the moment the hole was cut: it
//!   holds after a normal commit took the holes, after `commit_now`, and
//!   after a restart (the last such commit is then taken to be the tip).
//! * **`JournalReserved` is released per cut** (review M4): every record's
//!   charge is kept by `write_seq` and released at C9 for every record at or
//!   below the cut, whichever ring files the journal recycled. The ledger
//!   then equals what a restart charges (the records above the cut).
//!
//! "Refused or parked for space" is the gate's waiter count, its full latch
//! (`SpaceSnapshot::full`), or a queue-side flag (a write was refused or
//! expired and none admitted since) confirmed by the ledger being within
//! 1 MiB of the limit.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lunix_format::CommitSeq;
use lunix_iface::{ErrorKind, Result};

use crate::coord::Coord;
use crate::engine::{lock, CommitOutcome, Inner};

/// The shortest spacing of the cuts the full volume asks for.
const SPACE_CUT_SPACING: Duration = Duration::from_secs(1);

/// The shortest ring-full wait while commits succeed (review m2).
pub(crate) const RING_MIN_WAIT: Duration = Duration::from_secs(60);

/// How long a volume must admit writes again before the ladder's back-off
/// starts over (review m3: not on every flap).
const BACKOFF_RESET_AFTER_MS: u64 = 5_000;

/// The shortest spacing of the "writes are refused" log line.
const REFUSED_LOG_SPACING_MS: u64 = 5_000;

/// How close to the limit the ledger must be for the queue-side flag to
/// count as "refusing" (review m3).
const NEAR_LIMIT: u64 = 1 << 20;

/// The shortest spacing of the "journal ring full" log line.
const RING_LOG_SPACING_MS: u64 = 5_000;

const EIO: i32 = 5;
const ENOSPC: i32 = 28;

/// The full-volume state the queues and the coordinator share.
#[derive(Debug)]
pub(crate) struct FullState {
    /// The `full_timeout` in milliseconds.
    full_timeout_ms: AtomicU64,
    /// A write reservation was refused or expired for space, and no write
    /// was admitted since.
    blocked: AtomicBool,
    /// The failed commit attempts since the last success include one that
    /// failed for space.
    commit_nospace: AtomicBool,
    /// Commit attempts that failed for space or I/O so far.
    commit_failures: AtomicU64,
    /// `clock.monotonic()` in ms + 1 of the last commit attempt that failed
    /// for space or I/O (0: none).
    last_failure_ms: AtomicU64,
    /// `clock.monotonic()` in ms + 1 when writes were admitted again after
    /// a refusal (0: refusing, or never refused).
    admitted_since_ms: AtomicU64,
    /// Per lane: the files `Journal::freed_files_by_lane` last showed, and
    /// `clock.monotonic()` in ms + 1 when that count last rose (0: never):
    /// the lane's ring progress (milestone jstall).
    lane_freed_seen: Vec<AtomicU64>,
    lane_progress_ms: Vec<AtomicU64>,
    /// A cut is running (between its start and its end, success or not).
    cut_running: AtomicBool,
    /// `clock.monotonic()` in ms + 1 of the last sign of life of the running
    /// cut: its start, a segment append or seal, C5, C6, the reclaim.
    cut_alive_ms: Arc<AtomicU64>,
    /// `clock.monotonic()` in ms + 1 of the last "refused" log line.
    refused_logged: AtomicU64,
    /// The ring bound after `begin_close`, in ms.
    close_bound_ms: AtomicU64,
    /// The `write_seq` of the newest hole record appended (0: none).
    last_hole: AtomicU64,
    /// The `commit_seq` of the last commit that took a hole record (at
    /// open: the tip, the conservative reading).
    hole_commit: AtomicU64,
    /// `clock.monotonic()` in ms of the last ring-full log line + 1.
    ring_logged: AtomicU64,
    /// The pause between two urgent passes the space ladder asks for, in ms
    /// (`Inner::run_urgent`, ADR-32 P5).
    ladder_backoff_ms: AtomicU64,
    /// `begin_close` was called.
    closing: AtomicBool,
    /// `clock.monotonic()` in ms at `begin_close`.
    close_at_ms: AtomicU64,
    /// `commit_failures` at `begin_close`.
    close_failures: AtomicU64,
}

impl FullState {
    /// `last_hole`: the highest replayed hole record (its cut is to come).
    pub(crate) fn new(
        full_timeout: Duration,
        tip_commit: u64,
        last_hole: u64,
        lanes: usize,
    ) -> Self {
        Self {
            full_timeout_ms: AtomicU64::new(ms(full_timeout).max(1)),
            blocked: AtomicBool::new(false),
            commit_nospace: AtomicBool::new(false),
            commit_failures: AtomicU64::new(0),
            last_failure_ms: AtomicU64::new(0),
            admitted_since_ms: AtomicU64::new(0),
            lane_freed_seen: (0..lanes).map(|_| AtomicU64::new(0)).collect(),
            lane_progress_ms: (0..lanes).map(|_| AtomicU64::new(0)).collect(),
            cut_running: AtomicBool::new(false),
            cut_alive_ms: Arc::new(AtomicU64::new(0)),
            refused_logged: AtomicU64::new(0),
            close_bound_ms: AtomicU64::new(0),
            last_hole: AtomicU64::new(last_hole),
            hole_commit: AtomicU64::new(tip_commit),
            ring_logged: AtomicU64::new(0),
            ladder_backoff_ms: AtomicU64::new(ms(crate::gc::SPACE_URGENT_MIN)),
            closing: AtomicBool::new(false),
            close_at_ms: AtomicU64::new(0),
            close_failures: AtomicU64::new(0),
        }
    }

    pub(crate) fn set_full_timeout(&self, d: Duration) {
        self.full_timeout_ms.store(ms(d).max(1), Ordering::Relaxed);
    }

    pub(crate) fn full_timeout(&self) -> Duration {
        Duration::from_millis(self.full_timeout_ms.load(Ordering::Relaxed))
    }

    pub(crate) fn ladder_backoff(&self) -> Duration {
        Duration::from_millis(self.ladder_backoff_ms.load(Ordering::Relaxed))
    }

    pub(crate) fn set_ladder_backoff(&self, d: Duration) {
        self.ladder_backoff_ms.store(ms(d), Ordering::Relaxed);
    }

    /// Records the outcome of a write's space reservation at `now`. Stores
    /// only on a change: the admitted path stays a load. Returns true when
    /// this call changed it.
    pub(crate) fn set_blocked(&self, blocked: bool, now: Duration) -> bool {
        if self.blocked.load(Ordering::Relaxed) == blocked {
            return false;
        }
        self.blocked.store(blocked, Ordering::Relaxed);
        if blocked {
            // A new episode after writes were admitted for a while starts
            // with the shortest pause (final review m-A): the "admitted"
            // fact is about to be zeroed.
            let since = self.admitted_since_ms.load(Ordering::Relaxed);
            if since > 0
                && ms(now).saturating_add(1).saturating_sub(since) >= BACKOFF_RESET_AFTER_MS
            {
                self.set_ladder_backoff(crate::gc::SPACE_URGENT_MIN);
            }
        }
        let since = if blocked {
            0
        } else {
            ms(now).saturating_add(1)
        };
        self.admitted_since_ms.store(since, Ordering::Relaxed);
        true
    }

    /// The ladder's back-off at `now`: back to the minimum once writes have
    /// been admitted again for a few seconds (review m6, m3).
    pub(crate) fn ladder_backoff_at(&self, now: Duration) -> Duration {
        let since = self.admitted_since_ms.load(Ordering::Relaxed);
        if since > 0 && ms(now).saturating_add(1).saturating_sub(since) >= BACKOFF_RESET_AFTER_MS {
            self.set_ladder_backoff(crate::gc::SPACE_URGENT_MIN);
        }
        self.ladder_backoff()
    }

    /// The journal's freed-files counts (by lane) as of `now`: a lane whose
    /// count rose got a file freed, which is that lane's ring progress.
    pub(crate) fn note_freed_files(&self, counts: &[u64], now: Duration) {
        for ((seen, at), &n) in self
            .lane_freed_seen
            .iter()
            .zip(&self.lane_progress_ms)
            .zip(counts)
        {
            if seen.swap(n, Ordering::Relaxed) < n {
                at.fetch_max(ms(now).saturating_add(1), Ordering::Relaxed);
            }
        }
    }

    /// Stamps the lanes flagged in `left` as of `now`.
    pub(crate) fn note_lanes_pending(&self, left: &[bool], now: Duration) {
        for (at, _) in self.lane_progress_ms.iter().zip(left).filter(|(_, l)| **l) {
            at.fetch_max(ms(now).saturating_add(1), Ordering::Relaxed);
        }
    }

    /// A lane's last ring progress, `None` before the first.
    pub(crate) fn lane_progress(&self, lane: usize) -> Option<Duration> {
        match self.lane_progress_ms.get(lane)?.load(Ordering::Relaxed) {
            0 => None,
            p => Some(Duration::from_millis(p - 1)),
        }
    }

    /// A cut began at `now`.
    pub(crate) fn cut_begin(&self, now: Duration) {
        self.cut_alive_ms
            .store(ms(now).saturating_add(1), Ordering::Relaxed);
        self.cut_running.store(true, Ordering::Release);
    }

    /// The running cut did something (appended, sealed, committed).
    pub(crate) fn cut_alive(&self, now: Duration) {
        self.cut_alive_ms
            .fetch_max(ms(now).saturating_add(1), Ordering::Relaxed);
    }

    /// The stamp the segment wrapper of the running cut writes to.
    pub(crate) fn cut_alive_handle(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.cut_alive_ms)
    }

    /// The cut ended, in success or failure: its life signs stop counting.
    pub(crate) fn cut_end(&self) {
        self.cut_running.store(false, Ordering::Release);
    }

    /// The running cut's last sign of life (`None` when no cut runs).
    fn cut_life(&self) -> Option<Duration> {
        if !self.cut_running.load(Ordering::Acquire) {
            return None;
        }
        match self.cut_alive_ms.load(Ordering::Relaxed) {
            0 => None,
            p => Some(Duration::from_millis(p - 1)),
        }
    }

    /// A hole record with this number was appended.
    pub(crate) fn note_hole(&self, write_seq: u64) {
        self.last_hole.fetch_max(write_seq, Ordering::AcqRel);
    }

    /// The commit `commit_seq` is durable and cut `(prev, cut]`.
    pub(crate) fn note_cut(&self, prev: u64, cut: u64, commit_seq: u64) {
        let hole = self.last_hole.load(Ordering::Acquire);
        if hole > prev && hole <= cut {
            self.hole_commit.store(commit_seq, Ordering::Release);
        }
    }

    /// Records a commit attempt's outcome at `now`. Only failures for space
    /// or I/O count (review N3): `Busy` (writes in flight at the cut) and the
    /// like say nothing about the ring.
    pub(crate) fn note_commit<T>(&self, res: &Result<T>, now: Duration) {
        match res {
            Ok(_) => self.commit_nospace.store(false, Ordering::Relaxed),
            Err(e) => {
                let kind = e.kind();
                if kind == ErrorKind::NoSpace {
                    self.commit_nospace.store(true, Ordering::Relaxed);
                }
                if matches!(kind, ErrorKind::NoSpace | ErrorKind::Io) {
                    self.last_failure_ms
                        .store(ms(now).saturating_add(1), Ordering::Relaxed);
                    self.commit_failures.fetch_add(1, Ordering::AcqRel);
                }
            }
        }
    }

    /// `begin_close` at `now`, ring waits answered within `bound`.
    pub(crate) fn begin_close(&self, now: Duration, bound: Duration) {
        self.close_bound_ms.store(ms(bound), Ordering::Relaxed);
        self.close_at_ms.store(ms(now), Ordering::Relaxed);
        self.close_failures.store(
            self.commit_failures.load(Ordering::Acquire),
            Ordering::Relaxed,
        );
        self.closing.store(true, Ordering::Release);
    }
}

fn ms(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

type Charges = Mutex<VecDeque<(u64, u64)>>;

/// The `JournalReserved` charge of every record not yet at or below a cut,
/// by `write_seq`: one FIFO per lane (a lane appends in increasing numbers)
/// and one for the records recovery replayed.
#[derive(Debug)]
pub(crate) struct JournalCharges {
    lanes: Vec<Charges>,
    recovered: Charges,
}

impl JournalCharges {
    pub(crate) fn new(lanes: usize, mut recovered: Vec<(u64, u64)>) -> Self {
        recovered.sort_unstable();
        Self {
            lanes: (0..lanes).map(|_| Mutex::new(VecDeque::new())).collect(),
            recovered: Mutex::new(recovered.into()),
        }
    }

    /// Lane `lane`'s record `write_seq` charged `bytes`. 16 bytes per record
    /// above the last cut: at worst a quarter of the ring size, for a storm
    /// of discards while cuts fail.
    pub(crate) fn push(&self, lane: usize, write_seq: u64, bytes: u64) {
        debug_assert!(lane < self.lanes.len(), "lane {lane} out of range");
        if let Some(q) = self.lanes.get(lane) {
            lock(q).push_back((write_seq, bytes));
        }
    }

    /// The cut `c'` in `(last cut, c]` that a cut bounded by journal bytes
    /// takes (ADR-33): the `write_seq` of the first record, in `write_seq`
    /// order, at which the charges since the last cut reach `budget` and
    /// `write_seq >= floor`; `c` when they do not reach it by `c`. Every
    /// record at or below the answer is in the cut, whichever lane holds it.
    /// Reads only: the charges go at C9 (`take_upto`).
    pub(crate) fn cut_for(&self, budget: u64, floor: u64, c: u64) -> u64 {
        // The coordinator holds every stream at once, in a fixed order; a
        // queue thread holds one lane's at a time, so this cannot deadlock.
        let guards: Vec<_> = self
            .lanes
            .iter()
            .chain(std::iter::once(&self.recovered))
            .map(lock)
            .collect();
        let mut at = vec![0usize; guards.len()];
        let mut sum = 0u64;
        loop {
            let next = guards
                .iter()
                .zip(&at)
                .enumerate()
                .filter_map(|(k, (g, &i))| g.get(i).map(|e| (e.0, k)))
                .min();
            let Some((seq, k)) = next else {
                return c;
            };
            if seq > c {
                return c;
            }
            sum = sum.saturating_add(guards[k][at[k]].1);
            at[k] += 1;
            if sum >= budget && seq >= floor {
                return seq;
            }
        }
    }

    /// Takes the charges of every record at or below `cut`; returns their
    /// sum and, per lane, whether the lane still holds charges above the cut
    /// (the replayed records, while any are left, count for every lane): the
    /// cuts have not reached that lane's oldest records yet.
    pub(crate) fn take_upto(&self, cut: u64) -> (u64, Vec<bool>) {
        // The lane's queue thread may be appending: hold its mutex only to
        // split the backlog off, and sum it outside.
        let take = |q: &Charges| {
            let (taken, left): (Vec<(u64, u64)>, bool) = {
                let mut q = lock(q);
                let n = q.partition_point(|(s, _)| *s <= cut);
                let t = q.drain(..n).collect();
                (t, !q.is_empty())
            };
            (
                taken.iter().fold(0u64, |a, (_, b)| a.saturating_add(*b)),
                left,
            )
        };
        let mut sum = 0u64;
        let mut left: Vec<bool> = Vec::with_capacity(self.lanes.len());
        for q in &self.lanes {
            let (s, l) = take(q);
            sum = sum.saturating_add(s);
            left.push(l);
        }
        let (s, rec_left) = take(&self.recovered);
        if rec_left {
            left.iter_mut().for_each(|l| *l = true);
        }
        (sum.saturating_add(s), left)
    }
}

impl Inner {
    /// Reads the journal's per-lane freed-file counts: a lane whose count rose
    /// got ring progress (milestone jstall).
    pub(crate) fn note_freed_files(&self) {
        let counts = self.journal.freed_files_by_lane();
        self.full.note_freed_files(&counts, self.clock.monotonic());
        self.full.cut_alive(self.clock.monotonic());
    }

    /// A cut released journal bytes: a lane that still holds records above
    /// the new cut is waiting for the cuts to reach its files, not stuck, so
    /// it counts as progress for that lane. A lane the cuts have passed holds
    /// nothing above the cut and gets no stamp: its parked writes fail at
    /// their deadline.
    pub(crate) fn note_lane_backlog(&self, left: &[bool]) {
        self.full.note_lanes_pending(left, self.clock.monotonic());
    }

    /// The verdict on a write parked for a full journal ring that arrived at
    /// `arrived` (`clock.monotonic()`): `None` keeps it waiting, else the
    /// errno it fails with. The deadline counts from the later of the arrival
    /// and the ring's last progress.
    pub(crate) fn ring_wait_verdict(&self, arrived: Duration, lane: usize) -> Option<i32> {
        if self.read_only() {
            return Some(EIO);
        }
        let f = &self.full;
        let now = self.clock.monotonic();
        let waited = now.saturating_sub(arrived);
        // The later of the arrival, the lane's last progress (a file freed on
        // it, or a cut that released bytes while the lane still held records
        // above it) and the life of the cut that is running (a long cut that
        // keeps sealing is not a stuck ring).
        let since = [f.lane_progress(lane), f.cut_life()]
            .into_iter()
            .flatten()
            .fold(arrived, Duration::max);
        let idle = now.saturating_sub(since);
        let timeout = f.full_timeout();
        // The ADR-32 rule: a commit that failed (for space or I/O) after this
        // write arrived or last progressed shortens the limit to
        // `full_timeout`. A cut that ran long and then failed is no progress
        // once it has ended: the write is judged at the next tick.
        let failed_since = f.last_failure_ms.load(Ordering::Relaxed) > ms(since);
        let normal = if failed_since {
            idle >= timeout
        } else {
            idle >= timeout.max(RING_MIN_WAIT)
        };
        let closing = f.closing.load(Ordering::Acquire);
        let close_due = closing && {
            let close_at = Duration::from_millis(f.close_at_ms.load(Ordering::Relaxed));
            let bound = Duration::from_millis(f.close_bound_ms.load(Ordering::Relaxed));
            f.commit_failures.load(Ordering::Acquire) > f.close_failures.load(Ordering::Relaxed)
                || now.saturating_sub(close_at) >= bound
        };
        if !(normal || close_due) {
            return None;
        }
        let nospace = f.commit_nospace.load(Ordering::Relaxed);
        let now_ms = ms(now).saturating_add(1);
        let last = f.ring_logged.load(Ordering::Relaxed);
        if last == 0 || now_ms.saturating_sub(last) >= RING_LOG_SPACING_MS {
            f.ring_logged.store(now_ms, Ordering::Relaxed);
            tracing::warn!(
                full_timeout_ms = ms(timeout),
                waited_ms = ms(waited),
                no_progress_ms = ms(idle),
                last_commit_error = if nospace { "no space" } else { "other" },
                closing,
                "write failed: journal ring full"
            );
        }
        Some(if nospace { ENOSPC } else { EIO })
    }

    /// True while writes are refused or parked for space: the gate has
    /// waiters or is latched full (the latch holds while the headroom is
    /// below the resume mark, review N1), or a queue saw a refusal and the
    /// ledger is within 1 MiB of the limit.
    pub(crate) fn space_refusing(&self) -> bool {
        let snap = self.space.snapshot();
        // Requests parked for pacing are not refused (ADR-32).
        snap.waiters > snap.throttle.paced_waiters
            || snap.full
            || (self.full.blocked.load(Ordering::Relaxed)
                && snap.used().saturating_add(NEAR_LIMIT) > snap.limit)
    }

    /// FULLs the window turn owes: `retain_last + 1` less the commits that
    /// followed the last one that took a hole record.
    fn turn_owed(&self, tip: CommitSeq) -> u64 {
        let since = tip
            .0
            .saturating_sub(self.full.hole_commit.load(Ordering::Acquire));
        u64::from(self.cfg.retain_last)
            .saturating_add(1)
            .saturating_sub(since)
    }

    /// FULLs the window turn owes while the volume refuses writes (0
    /// otherwise).
    pub(crate) fn window_turn_left(&self, tip: CommitSeq) -> u64 {
        if self.space_refusing() {
            self.turn_owed(tip)
        } else {
            0
        }
    }

    /// Whether the full volume wants a cut now (`Some(force)`): while writes
    /// are refused or parked for space, once a second, for hole records not
    /// yet cut (`Some(false)`) or for the FULLs the window turn owes
    /// (`Some(true)`, an empty FULL on an idle volume).
    pub(crate) fn space_cut(&self, co: &Coord) -> Option<bool> {
        if !self.space_refusing() {
            return None;
        }
        let now = self.clock.monotonic();
        if now.saturating_sub(co.last_commit_at) < SPACE_CUT_SPACING {
            return None;
        }
        if self.full.last_hole.load(Ordering::Acquire) > co.tip.cut_seq.0 {
            return Some(false);
        }
        (self.turn_owed(co.tip.common.commit_seq) > 0).then_some(true)
    }

    /// After a tick's forced FULL of the window turn: one log line.
    pub(crate) fn after_space_cut(
        &self,
        co: &Coord,
        wanted: Option<bool>,
        res: &Result<CommitOutcome>,
    ) {
        if wanted == Some(true) && matches!(res, Ok(CommitOutcome::Committed { .. })) {
            tracing::info!(
                commit = co.tip.common.commit_seq.0,
                owed = self.turn_owed(co.tip.common.commit_seq),
                "volume full: an empty FULL turns the retained window after a discard"
            );
        }
    }

    /// Signals the queues that hold a request parked for the ring.
    pub(crate) fn signal_ring_parked(&self) {
        for q in lock(&self.queues).iter().flatten() {
            if q.ring_parked.load(Ordering::Relaxed) {
                q.signal();
            }
        }
    }

    /// A write has waited `waited` (from its arrival) for a free ring file so
    /// far (ADR-33): the longest wait since the attach and in the last 60 s.
    pub(crate) fn note_ring_wait(&self, waited: Duration) {
        let w = u64::try_from(waited.as_millis()).unwrap_or(u64::MAX);
        crate::stats::max(&self.cuts.ring_wait_max_ms, w);
        self.cuts.ring_wait_recent.note(self.clock.monotonic(), w);
    }

    /// A write that waited for the ring, since `arrived`, is no longer
    /// waiting: it was admitted, failed or its queue went away.
    pub(crate) fn end_ring_wait(&self, arrived: Duration) {
        self.note_ring_wait(self.clock.monotonic().saturating_sub(arrived));
        let _ = self
            .cuts
            .ring_waiters
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_sub(1));
    }

    /// A write was admitted (reserved or granted). A load on the hot path.
    pub(crate) fn note_admitted(&self) {
        if self.full.blocked.load(Ordering::Relaxed) {
            let _ = self.full.set_blocked(false, self.clock.monotonic());
        }
    }

    /// A write was refused or expired for space: the first one of an
    /// episode is logged with the ledger (at most one line per 5 s), so the
    /// operator sees why (review m1; the gate's own transition lines are
    /// T5's).
    pub(crate) fn note_refused(&self) {
        let now = self.clock.monotonic();
        if !self.full.set_blocked(true, now) {
            return;
        }
        let now_ms = ms(now).saturating_add(1);
        let last = self.full.refused_logged.load(Ordering::Relaxed);
        if last == 0 || now_ms.saturating_sub(last) >= REFUSED_LOG_SPACING_MS {
            self.full.refused_logged.store(now_ms, Ordering::Relaxed);
            let snap = self.space.snapshot();
            tracing::warn!(
                used = snap.used(),
                limit = snap.limit,
                "writes are refused for space (ENOSPC): discard, run gc, or raise cache_max"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lunix_iface::Error;

    #[test]
    fn only_space_and_io_failures_count_for_a_ring_wait() {
        let f = FullState::new(Duration::from_secs(60), 0, 0, 1);
        let busy: Result<()> = Err(Error::Busy {
            what: "writes in flight at the cut",
        });
        f.note_commit(&busy, Duration::from_secs(5));
        assert_eq!(f.commit_failures.load(Ordering::Relaxed), 0);
        assert_eq!(f.last_failure_ms.load(Ordering::Relaxed), 0);
        let io: Result<()> = Err(Error::Io(std::io::Error::other("disk")));
        f.note_commit(&io, Duration::from_secs(6));
        assert_eq!(f.commit_failures.load(Ordering::Relaxed), 1);
        assert!(!f.commit_nospace.load(Ordering::Relaxed));
        let full: Result<()> = Err(Error::NoSpace);
        f.note_commit(&full, Duration::from_secs(7));
        assert!(f.commit_nospace.load(Ordering::Relaxed));
        // A benign failure keeps the space answer; a success clears it.
        f.note_commit(&busy, Duration::from_secs(8));
        assert!(f.commit_nospace.load(Ordering::Relaxed));
        f.note_commit(&Ok(()), Duration::from_secs(9));
        assert!(!f.commit_nospace.load(Ordering::Relaxed));
    }

    #[test]
    fn the_ladder_back_off_starts_over_only_after_writes_were_admitted_a_while() {
        let f = FullState::new(Duration::from_secs(60), 0, 0, 1);
        f.set_ladder_backoff(Duration::from_secs(32));
        assert!(f.set_blocked(true, Duration::from_secs(1)));
        assert!(f.set_blocked(false, Duration::from_secs(2)));
        // A flap: refused again a second later; nothing reset.
        assert_eq!(
            f.ladder_backoff_at(Duration::from_secs(3)),
            Duration::from_secs(32)
        );
        assert!(f.set_blocked(true, Duration::from_secs(3)));
        assert_eq!(
            f.ladder_backoff_at(Duration::from_secs(30)),
            Duration::from_secs(32)
        );
        // Admitted for 5 s: back to the minimum.
        assert!(f.set_blocked(false, Duration::from_secs(31)));
        assert_eq!(
            f.ladder_backoff_at(Duration::from_secs(36)),
            crate::gc::SPACE_URGENT_MIN
        );
    }

    #[test]
    fn a_new_episode_after_a_long_admitted_stretch_starts_at_the_minimum() {
        // Final review m-A: the next refusal comes before any ladder pass
        // asked for the back-off; the long admitted stretch still counts.
        let f = FullState::new(Duration::from_secs(60), 0, 0, 1);
        f.set_ladder_backoff(Duration::from_secs(60));
        assert!(f.set_blocked(true, Duration::from_secs(1)));
        assert!(f.set_blocked(false, Duration::from_secs(10)));
        assert!(f.set_blocked(true, Duration::from_secs(86_400)));
        assert_eq!(
            f.ladder_backoff_at(Duration::from_secs(86_401)),
            crate::gc::SPACE_URGENT_MIN
        );
        // A short admitted stretch keeps it.
        f.set_ladder_backoff(Duration::from_secs(16));
        assert!(f.set_blocked(false, Duration::from_secs(86_402)));
        assert!(f.set_blocked(true, Duration::from_secs(86_403)));
        assert_eq!(
            f.ladder_backoff_at(Duration::from_secs(86_404)),
            Duration::from_secs(16)
        );
    }
}

#[cfg(test)]
mod cut_for_tests {
    use super::*;

    /// Lane 0 holds the odd numbers, lane 1 the even ones, 10 bytes each.
    fn charges() -> JournalCharges {
        let c = JournalCharges::new(2, Vec::new());
        for s in 1..=6u64 {
            c.push(usize::from(s % 2 == 0), s, 10);
        }
        c
    }

    #[test]
    fn the_cut_is_the_record_where_the_sum_reaches_the_budget_across_lanes() {
        let c = charges();
        assert_eq!(c.cut_for(25, 0, 6), 3, "10, 20, 30 at seq 3");
        assert_eq!(c.cut_for(60, 0, 6), 6);
        assert_eq!(
            c.cut_for(0, 0, 6),
            1,
            "a budget of 0 takes the first record"
        );
        assert_eq!(c.cut_for(1000, 0, 6), 6, "unreachable: everything up to c");
    }

    #[test]
    fn records_above_c_are_not_taken_and_the_floor_raises_the_cut() {
        let c = charges();
        assert_eq!(c.cut_for(1000, 0, 4), 4);
        assert_eq!(c.cut_for(10, 5, 6), 5, "the floor is past the budget point");
        assert_eq!(c.cut_for(10, 100, 6), 6, "a floor above c is c");
        assert_eq!(
            c.cut_for(10, 1, 6),
            1,
            "a floor at the first record is no floor"
        );
    }

    #[test]
    fn the_replayed_records_come_first_in_write_seq_order() {
        let c = JournalCharges::new(2, vec![(2, 10), (1, 10)]);
        c.push(0, 3, 10);
        c.push(1, 4, 10);
        assert_eq!(c.cut_for(30, 0, 4), 3);
        assert_eq!(c.cut_for(40, 0, 4), 4);
        assert_eq!(c.take_upto(2).0, 20, "what the cut released");
        assert_eq!(c.cut_for(10, 0, 4), 3);
    }
}
