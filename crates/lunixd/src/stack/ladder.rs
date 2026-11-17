//! The space ladder's compact step (DESIGN §4.3, §11; `docs/reference/gc.md`):
//! GC's urgent pass, run on the engine's coordinator through
//! [`GcControl::run_urgent`].
//!
//! The gate runs the step on its own thread and the step waits for the
//! coordinator; the coordinator never waits for the gate's thread (it never
//! parks in the gate), so the two cannot deadlock. The step holds the engine
//! weakly: the gate never keeps a closed volume's engine alive, and on a
//! quiesced engine the step frees nothing and lets go at once.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};

use lunix_engine::Engine;
use lunix_iface::gc::GcControl;
use lunix_iface::space::{Freed, LadderStep, Urgency};
use lunix_iface::{Error, Result};

/// [`LadderStep`] for `LadderStepKind::Compact`.
pub struct CompactStep {
    /// The volume's engine.
    pub engine: Weak<Engine>,
    /// Incremented after every run (`VolumeStack::compact_runs`).
    pub runs: Arc<AtomicU64>,
}

impl CompactStep {
    fn urgent(&self, want_bytes: u64, urgency: Urgency) -> Result<Freed> {
        let Some(engine) = self.engine.upgrade() else {
            return Ok(Freed(0));
        };
        match engine.run_urgent(want_bytes) {
            Ok(freed) => {
                tracing::info!(want_bytes, freed, ?urgency, "space ladder: gc urgent pass");
                Ok(Freed(freed))
            }
            // The volume is closing: nothing to free here.
            Err(Error::Closed) => Ok(Freed(0)),
            Err(e) => {
                tracing::warn!(want_bytes, error = %e, "space ladder: gc urgent pass failed");
                Err(e)
            }
        }
    }
}

impl LadderStep for CompactStep {
    fn run(&mut self, want_bytes: u64, urgency: Urgency) -> Result<Freed> {
        let r = self.urgent(want_bytes, urgency);
        self.runs.fetch_add(1, Ordering::Release);
        r
    }
}
