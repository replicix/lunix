//! The space ladder's compact step (DESIGN §4.3, §11; `docs/reference/gc.md`):
//! GC's urgent pass, run on the engine's coordinator through
//! [`GcControl::run_urgent`].
//!
//! The gate runs the step on its own thread and the step waits for the
//! coordinator; the coordinator never waits for the gate's thread (it never
//! parks in the gate), so the two cannot deadlock. The step holds the engine
//! weakly: the gate never keeps a closed volume's engine alive.

use std::sync::Weak;

use lunix_engine::Engine;
use lunix_iface::gc::GcControl;
use lunix_iface::space::{Freed, LadderStep, Urgency};
use lunix_iface::{Error, Result};

/// [`LadderStep`] for `LadderStepKind::Compact`.
pub struct CompactStep(pub Weak<Engine>);

impl LadderStep for CompactStep {
    fn run(&mut self, want_bytes: u64, urgency: Urgency) -> Result<Freed> {
        let Some(engine) = self.0.upgrade() else {
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
