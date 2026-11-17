//! `GcControl`: how the daemon reaches the collector (plan M12 step 12,
//! DESIGN §11, `docs/reference/gc.md` "Where it runs").
//!
//! The coordinator mutex makes every caller the collector's single caller:
//! each entry here runs the collector on the calling thread (a control-call
//! thread, the space gate's thread) under that mutex, so a requested pass, a
//! dry run or the ladder's urgent pass never runs during a cut, and always
//! with the in-memory map at the tip (the hook's precondition). The ladder
//! waits for the coordinator, never the reverse: the coordinator never parks
//! in the space gate (`reserve_headroom` does not wait), and a write parks in
//! the gate before it takes a `write_seq`, so a cut never waits for a parked
//! write.
//!
//! A requested pass releases the mutex between steps and pauses there (a
//! condvar wait: a few ms after a step that worked, longer in the grace), so
//! cuts, the background steps, a quiesce and the ladder get the mutex between
//! two steps. Every entry checks `closing` under the mutex: once a quiesce
//! began, no collector call starts, and a waiting requested pass wakes and
//! ends `Closed`. Moves of any pass go to the next cut, as for a background
//! step.

use std::sync::atomic::Ordering;
use std::time::Duration;

use lunix_iface::gc::{Collector, GcPassSummary, GcProgress, GcState, GcStats, GcView, TipInfo};
use lunix_iface::{Error, ErrorKind, Result};

use crate::coord::Coord;
use crate::engine::{lock, stopped, Inner};
use crate::recover::high_water;

/// The pause of a requested pass between two steps that did work: the other
/// users of the coordinator mutex get it in between.
const STEP_PAUSE: Duration = Duration::from_millis(5);

/// The pause while the pass waits out its grace, or after a step that could
/// do nothing yet (compaction at its rate cap).
const IDLE_PAUSE: Duration = Duration::from_millis(100);

/// The minimum pause between two urgent passes run because commits fail for
/// space (each is a full mark); it doubles while they free nothing.
pub(crate) const SPACE_URGENT_MIN: Duration = Duration::from_secs(2);

/// The longest such pause.
const SPACE_URGENT_MAX: Duration = Duration::from_secs(60);

/// How often a requested pass is asked for again when the collector went
/// idle without finishing it (another caller's step abandoned it after an
/// error, or the pass in progress ended first) before the request fails.
const MAX_ASKS: u32 = 4;

/// How many steps of a requested pass may find the map ahead of the tip (a
/// cut failed after its re-hints) before the request fails `Busy`.
const MAX_BUSY: u32 = 50;

fn no_collector() -> Error {
    Error::Unsupported {
        what: "gc on a volume without a collector",
    }
}

impl Inner {
    /// Not failed and not quiesced. Called under the coordinator mutex.
    fn check_open(&self) -> Result<()> {
        if self.is_failed() {
            return Err(stopped());
        }
        if self.closing.load(Ordering::Acquire) {
            return Err(Error::Closed);
        }
        Ok(())
    }

    /// True when the hook's precondition holds: between cuts, the in-memory
    /// map equal to the tip's root, no dirty leaf.
    pub(crate) fn map_at_tip(&self, co: &Coord) -> bool {
        self.map.root() == co.tip.map_root && self.map.dirty_leaf_count() == 0
    }

    /// Calls `f` with the collector and a view of the durable tip, then
    /// publishes the collector's status. The caller holds the coordinator
    /// mutex (`co`).
    ///
    /// # Errors
    /// `Unsupported` without a collector, `Busy` when the map is not at the
    /// tip, else what `f` returns.
    pub(crate) fn with_collector<R>(
        &self,
        co: &mut Coord,
        f: impl FnOnce(&mut dyn Collector, &GcView<'_>) -> Result<R>,
    ) -> Result<R> {
        let Some(params) = self.cfg.gc_params else {
            return Err(no_collector());
        };
        if !self.map_at_tip(co) {
            return Err(Error::Busy {
                what: "the map is ahead of the tip",
            });
        }
        let Coord {
            tip,
            retained,
            collector,
            ..
        } = co;
        let Some(collector) = collector.as_mut() else {
            return Err(no_collector());
        };
        let view = GcView {
            retained: retained.as_slice(),
            tip: TipInfo {
                commit_seq: tip.common.commit_seq,
                map_root: tip.map_root,
                chain_floor: tip.chain_floor,
                high_water_mark: high_water(retained),
            },
            pins: &*self.pins,
            map: &*self.map,
            index: &*self.index,
            segments: &*self.segments,
            packs: &*self.packs,
            commits: &*self.commits,
            commit_codec: &*self.commit_codec,
            extents: &*self.extents,
            state: &*self.gc_state,
            space: &*self.space,
            clock: &*self.clock,
            params,
        };
        let r = f(&mut **collector, &view);
        let mut st = collector.status();
        st.requests = self.gc_requests.load(Ordering::Acquire);
        *lock(&self.gc_status) = st;
        r
    }

    /// Waits `d` between two steps of a requested pass, or less when a
    /// quiesce begins.
    fn pause(&self, d: Duration) {
        let g = lock(&self.gc_wake.0);
        if self.closing.load(Ordering::Acquire) {
            return;
        }
        let _ = self
            .gc_wake
            .1
            .wait_timeout(g, d)
            .unwrap_or_else(|p| p.into_inner());
    }

    /// `GcControl::run_pass`. A dry run is one call on the coordinator.
    ///
    /// A pass asks for one now (whether or not one is due) and steps it to
    /// its end, pausing between steps without the coordinator mutex. The
    /// call returns once a pass that started after it ended: the next pass
    /// to end when the collector was idle (the requested one, or an urgent
    /// pass, whose mark began after the call), else the one after the pass in
    /// progress (its view predates the call; an urgent pass that aborts it
    /// counts as that pass). The answer covers every pass that ended while
    /// the call waited: counts summed, `bytes_freed` and
    /// `segments_compacted` from the collector's counters, `pass` the newest
    /// evidence number. So a call that found a pass in its grace reports what
    /// that pass freed too.
    pub(crate) fn run_pass(&self, dry_run: bool) -> Result<GcPassSummary> {
        let Some(budget) = self.cfg.gc_budget else {
            return Err(no_collector());
        };
        let mut co = lock(&self.coord);
        self.check_open()?;
        if dry_run {
            return self.with_collector(&mut co, |c, v| c.dry_run(v));
        }
        let c = co.collector.as_mut().ok_or_else(no_collector)?;
        let start = c.stats();
        let need = start.passes
            + match c.status().state {
                None => 1,
                Some(_) => 2,
            };
        ask(&mut **c)?;
        drop(co);
        let _waiting = Waiting::new(self);
        let mut acc = Answer::new(start);
        let mut asks = 1u32;
        let mut busy = 0u32;
        loop {
            let pause = {
                let mut co = lock(&self.coord);
                self.check_open()?;
                let c = co.collector.as_mut().ok_or_else(no_collector)?;
                acc.observe(&**c);
                if acc.passes() >= need {
                    return Ok(acc.summary(&**c));
                }
                if c.status().state.is_none() {
                    if asks == MAX_ASKS {
                        return Err(Error::Refused {
                            why: "the requested gc pass was abandoned (see the log)",
                        });
                    }
                    asks += 1;
                    ask(&mut **c)?;
                }
                let read = c.stats().compaction_read;
                match self.with_collector(&mut co, |c, v| c.step(v, budget)) {
                    Ok(p) => {
                        co.moved.extend(p.moved.iter().copied());
                        let c = co.collector.as_ref().ok_or_else(no_collector)?;
                        acc.observe(&**c);
                        if acc.passes() >= need {
                            return Ok(acc.summary(&**c));
                        }
                        step_pause(&p, c.stats().compaction_read > read)
                    }
                    // A cut failed after it applied re-hints: wait for the
                    // next one (bounded).
                    Err(e) if e.kind() == ErrorKind::Busy && busy < MAX_BUSY => {
                        busy += 1;
                        IDLE_PAUSE
                    }
                    Err(e) => return Err(e),
                }
            };
            self.pause(pause);
        }
    }

    /// A commit failed for space (the host file system is full: a destage
    /// `AfterSpace`, an index run that cannot be written): the volume cannot
    /// commit, and the gate's ladder may never be asked (its reservations
    /// still fit). Runs GC's urgent pass here, on the coordinator, wanting the
    /// dirty bytes: it deletes what a pass condemned once the floor grace ran,
    /// condemns what is dead and compacts if still short. Not again within
    /// `space_backoff` of the end of the last urgent pass (this one or the
    /// ladder's); the backoff doubles while they free nothing.
    pub(crate) fn space_short(&self, co: &mut Coord) {
        if self.cfg.gc_budget.is_none()
            || co.collector.is_none()
            || self.check_open().is_err()
            || !self.map_at_tip(co)
        {
            return;
        }
        let now = self.clock.monotonic();
        if co
            .last_space_urgent
            .is_some_and(|t| now < t.saturating_add(co.space_backoff))
        {
            return;
        }
        let want = self
            .overlay
            .dirty_bytes()
            .max(u64::from(self.cfg.chunk_size));
        let res = self.with_collector(co, |c, v| c.urgent(v, want));
        co.last_space_urgent = Some(self.clock.monotonic());
        match res {
            Ok(p) => {
                tracing::info!(
                    want,
                    freed = p.freed,
                    "a commit failed for space: gc urgent pass"
                );
                let condemned = co
                    .collector
                    .as_ref()
                    .is_some_and(|c| c.status().condemned_pending > 0);
                let doubled = co.space_backoff.saturating_mul(2).min(SPACE_URGENT_MAX);
                co.space_backoff = if p.freed > 0 {
                    SPACE_URGENT_MIN
                } else if condemned {
                    // What it condemned can go once the floor grace ran: the
                    // next pass is useful then, not later.
                    doubled.min(self.floor_grace()).max(SPACE_URGENT_MIN)
                } else {
                    doubled
                };
                co.moved.extend(p.moved);
            }
            Err(e) => {
                co.space_backoff = co.space_backoff.saturating_mul(2).min(SPACE_URGENT_MAX);
                tracing::warn!(error = %e, "a commit failed for space; the gc urgent pass failed");
            }
        }
    }

    /// `GcControl::run_urgent`: the urgent pass on the coordinator; its moves
    /// go to the next cut. Returns the bytes freed. Counts as the last urgent
    /// pass for `space_short`.
    pub(crate) fn run_urgent(&self, want_bytes: u64) -> Result<u64> {
        let mut co = lock(&self.coord);
        self.check_open()?;
        let res = self.with_collector(&mut co, |c, v| c.urgent(v, want_bytes));
        // Only a pass that ran counts for `space_short`'s pause.
        if !matches!(&res, Err(e) if matches!(e.kind(), ErrorKind::Busy | ErrorKind::Unsupported)) {
            co.last_space_urgent = Some(self.clock.monotonic());
        }
        let p = res?;
        co.moved.extend(p.moved);
        Ok(p.freed)
    }

    /// The urgent pass's grace: 10 s plus twice `read_timeout` (the
    /// collector's `grace_floor`; the engine names no collector crate).
    fn floor_grace(&self) -> Duration {
        self.cfg.gc_params.map_or(SPACE_URGENT_MAX, |p| {
            Duration::from_secs(10).saturating_add(p.read_timeout.saturating_mul(2))
        })
    }
}

/// Asks `c` for a pass now.
fn ask(c: &mut dyn Collector) -> Result<()> {
    if c.request_pass() {
        Ok(())
    } else {
        Err(Error::Unsupported {
            what: "a gc pass on request (this collector keeps its own schedule)",
        })
    }
}

/// The pause after a step: longer when the pass waits out its grace or the
/// step could do nothing (compaction at its rate cap: it read nothing).
fn step_pause(p: &GcProgress, compaction_read: bool) -> Duration {
    let idle = match p.state {
        GcState::Grace => true,
        GcState::Compacting => !compaction_read && p.moved.is_empty() && p.freed == 0,
        _ => false,
    };
    if idle {
        IDLE_PAUSE
    } else {
        STEP_PAUSE
    }
}

/// Counts a waiting `run_pass` in the published status
/// (`GcStatusSnapshot::requests`) while it lives.
struct Waiting<'a>(&'a Inner);

impl<'a> Waiting<'a> {
    fn new(inner: &'a Inner) -> Self {
        let n = inner.gc_requests.fetch_add(1, Ordering::AcqRel) + 1;
        lock(&inner.gc_status).requests = n;
        Self(inner)
    }
}

impl Drop for Waiting<'_> {
    fn drop(&mut self) {
        let n = self.0.gc_requests.fetch_sub(1, Ordering::AcqRel) - 1;
        lock(&self.0.gc_status).requests = n;
    }
}

/// What `run_pass` answers: the passes that ended while it waited.
struct Answer {
    start: GcStats,
    seen: u64,
    sum: GcPassSummary,
}

impl Answer {
    fn new(start: GcStats) -> Self {
        Self {
            start,
            seen: start.passes,
            sum: GcPassSummary::default(),
        }
    }

    fn passes(&self) -> u64 {
        self.seen
    }

    /// Adds the last pass's summary when a pass ended since the last look
    /// (a pass ends at most once between two looks in practice; the counters
    /// below are exact in any case).
    fn observe(&mut self, c: &dyn Collector) {
        let now = c.stats().passes;
        if now > self.seen {
            if let Some(p) = c.status().last_pass {
                let s = &mut self.sum;
                s.pass = s.pass.max(p.pass);
                s.segments_condemned += p.segments_condemned;
                s.segments_deleted += p.segments_deleted;
                s.packs_deleted += p.packs_deleted;
                s.runs_deleted += p.runs_deleted;
                s.commits_deleted += p.commits_deleted;
                s.bytes_moved += p.bytes_moved;
            }
            self.seen = now;
        }
    }

    fn summary(&self, c: &dyn Collector) -> GcPassSummary {
        let now = c.stats();
        GcPassSummary {
            bytes_freed: now.bytes_freed.saturating_sub(self.start.bytes_freed),
            segments_compacted: now
                .segments_compacted
                .saturating_sub(self.start.segments_compacted),
            ..self.sum.clone()
        }
    }
}
