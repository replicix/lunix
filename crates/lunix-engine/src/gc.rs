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

use lunix_iface::gc::{Collector, GcPassSummary, GcProgress, GcState, GcView, TipInfo};
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
        *lock(&self.gc_status) = collector.status();
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
    /// its end, pausing between steps without the coordinator mutex. When a
    /// pass is already marking, that pass answers; when one is past its mark
    /// (its view predates the call), the next one does. An urgent pass that
    /// ends meanwhile (the ladder's) also answers: its mark began after the
    /// call.
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
        let need = c.stats().passes
            + match c.status().state {
                Some(GcState::Marking) => 1,
                None => {
                    ask(&mut **c)?;
                    1
                }
                Some(_) => {
                    ask(&mut **c)?;
                    2
                }
            };
        drop(co);
        let mut asks = 1u32;
        let mut busy = 0u32;
        loop {
            let pause = {
                let mut co = lock(&self.coord);
                self.check_open()?;
                let c = co.collector.as_mut().ok_or_else(no_collector)?;
                if c.stats().passes >= need {
                    return Ok(c.status().last_pass.unwrap_or_default());
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
                match self.with_collector(&mut co, |c, v| c.step(v, budget)) {
                    Ok(p) => {
                        let pause = step_pause(&p);
                        co.moved.extend(p.moved);
                        if let Some(c) = co.collector.as_ref() {
                            if c.stats().passes >= need {
                                return Ok(c.status().last_pass.unwrap_or_default());
                            }
                        }
                        pause
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
                co.space_backoff = if p.freed > 0 {
                    SPACE_URGENT_MIN
                } else {
                    co.space_backoff.saturating_mul(2).min(SPACE_URGENT_MAX)
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
        co.last_space_urgent = Some(self.clock.monotonic());
        let p = res?;
        co.moved.extend(p.moved);
        Ok(p.freed)
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
/// step could do nothing (compaction at its rate cap moves and frees
/// nothing).
fn step_pause(p: &GcProgress) -> Duration {
    let idle = match p.state {
        GcState::Grace => true,
        GcState::Compacting => p.moved.is_empty() && p.freed == 0,
        _ => false,
    };
    if idle {
        IDLE_PAUSE
    } else {
        STEP_PAUSE
    }
}
