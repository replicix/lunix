//! `GcControl`: how the daemon reaches the collector (plan M12 step 12,
//! DESIGN §11, `docs/reference/gc.md` "Where it runs").
//!
//! The coordinator is the collector's only caller: every entry here takes the
//! coordinator mutex, so a requested pass, a dry run or the space ladder's
//! urgent pass never runs during a cut, and always with the in-memory map at
//! the tip (the hook's precondition). The ladder waits for the coordinator,
//! never the reverse: the coordinator never parks in the space gate
//! (`reserve_headroom` does not wait), and a write parks in the gate before it
//! takes a `write_seq`, so a cut never waits for a parked write.
//!
//! A requested pass releases the mutex between steps: cuts and the background
//! steps go on while it waits out its grace. It gives up (`Closed`) once the
//! engine is quiesced, so a detach never waits for a grace. Moves of any pass
//! go to the next cut, as for a background step.

use std::sync::atomic::Ordering;
use std::time::Duration;

use lunix_iface::gc::{Collector, GcPassSummary, GcState, GcView, TipInfo};
use lunix_iface::{Error, Result};

use crate::coord::Coord;
use crate::engine::{lock, Inner};
use crate::recover::high_water;

/// How long a requested pass sleeps between steps while it waits out the
/// grace (other phases only yield the coordinator).
const GRACE_POLL: Duration = Duration::from_millis(100);

/// The shortest time between two urgent passes run because a cut failed for
/// space (each is a full mark).
const SPACE_URGENT_INTERVAL: Duration = Duration::from_secs(2);

/// How often a requested pass is asked for again when the collector went
/// idle without finishing one (another caller's step abandoned it after an
/// error) before the request fails.
const MAX_ASKS: u32 = 3;

fn no_collector() -> Error {
    Error::Unsupported {
        what: "gc on a volume without a collector",
    }
}

impl Inner {
    fn check_alive(&self) -> Result<()> {
        if self.is_failed() {
            return Err(Error::Io(std::io::Error::other(
                "the engine stopped committing after a failure",
            )));
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
        if co.collector.is_none() {
            return Err(no_collector());
        }
        if !self.map_at_tip(co) {
            return Err(Error::Busy {
                what: "the map is ahead of the tip",
            });
        }
        let tip = TipInfo {
            commit_seq: co.tip.common.commit_seq,
            map_root: co.tip.map_root,
            chain_floor: co.tip.chain_floor,
            high_water_mark: high_water(&co.retained),
        };
        let retained = co.retained.clone();
        let view = GcView {
            retained: &retained,
            tip,
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
        let Some(collector) = co.collector.as_mut() else {
            return Err(no_collector());
        };
        let r = f(&mut **collector, &view);
        *lock(&self.gc_status) = collector.status();
        r
    }

    /// `GcControl::run_pass`. A dry run is one call on the coordinator. A
    /// pass finishes the pass in progress, or starts one now (whether or not
    /// one is due) and steps it to its end, releasing the coordinator between
    /// steps; it returns the summary of the first pass that ends after the
    /// call began.
    pub(crate) fn run_pass(&self, dry_run: bool) -> Result<GcPassSummary> {
        let Some(budget) = self.cfg.gc_budget else {
            return Err(no_collector());
        };
        if dry_run {
            let mut co = lock(&self.coord);
            self.check_alive()?;
            return self.with_collector(&mut co, |c, v| c.dry_run(v));
        }
        let before = {
            let co = lock(&self.coord);
            co.collector.as_ref().ok_or_else(no_collector)?.stats().passes
        };
        let finished = |co: &Coord| -> Option<GcPassSummary> {
            let c = co.collector.as_ref()?;
            (c.stats().passes > before).then(|| c.status().last_pass.unwrap_or_default())
        };
        let mut asks = 0u32;
        loop {
            if self.closing.load(Ordering::Acquire) {
                return Err(Error::Closed);
            }
            let state = {
                let mut co = lock(&self.coord);
                self.check_alive()?;
                if let Some(s) = finished(&co) {
                    return Ok(s);
                }
                let c = co.collector.as_mut().ok_or_else(no_collector)?;
                if c.status().state.is_none() {
                    if asks == MAX_ASKS {
                        return Err(Error::Refused {
                            why: "the requested gc pass was abandoned (see the log)",
                        });
                    }
                    asks += 1;
                    c.request_pass();
                }
                let p = self.with_collector(&mut co, |c, v| c.step(v, budget))?;
                co.moved.extend(p.moved);
                if let Some(s) = finished(&co) {
                    return Ok(s);
                }
                p.state
            };
            if state == GcState::Grace {
                std::thread::sleep(GRACE_POLL);
            } else {
                std::thread::yield_now();
            }
        }
    }

    /// A cut failed for space (the host file system is full, a destage
    /// `AfterSpace`): the volume cannot commit, so no background step runs and
    /// the gate's ladder may never be asked (its reservations still fit).
    /// Runs GC's urgent pass here, on the coordinator, wanting the dirty
    /// bytes: it deletes what a pass condemned once the floor grace ran,
    /// condemns what is dead and compacts if still short. At most once per
    /// [`SPACE_URGENT_INTERVAL`]; the next trigger retries the cut.
    pub(crate) fn space_short(&self, co: &mut Coord) {
        if self.cfg.gc_budget.is_none()
            || co.collector.is_none()
            || self.is_failed()
            || self.closing.load(Ordering::Acquire)
            || !self.map_at_tip(co)
        {
            return;
        }
        let now = self.clock.monotonic();
        if co
            .last_space_urgent
            .is_some_and(|t| now < t.saturating_add(SPACE_URGENT_INTERVAL))
        {
            return;
        }
        co.last_space_urgent = Some(now);
        let want = self
            .overlay
            .dirty_bytes()
            .max(u64::from(self.cfg.chunk_size));
        match self.with_collector(co, |c, v| c.urgent(v, want)) {
            Ok(p) => {
                tracing::info!(want, freed = p.freed, "a cut failed for space: gc urgent pass");
                co.moved.extend(p.moved);
            }
            Err(e) => tracing::warn!(error = %e, "a cut failed for space; the gc urgent pass failed"),
        }
    }

    /// `GcControl::run_urgent`: the urgent pass on the coordinator; its moves
    /// go to the next cut. Returns the bytes freed.
    pub(crate) fn run_urgent(&self, want_bytes: u64) -> Result<u64> {
        if self.closing.load(Ordering::Acquire) {
            return Err(Error::Closed);
        }
        let mut co = lock(&self.coord);
        self.check_alive()?;
        let p = self.with_collector(&mut co, |c, v| c.urgent(v, want_bytes))?;
        co.moved.extend(p.moved);
        Ok(p.freed)
    }
}
