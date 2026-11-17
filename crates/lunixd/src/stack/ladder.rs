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
    /// Incremented after every run that did not fail (`VolumeStack::compact_runs`).
    pub runs: Arc<AtomicU64>,
    /// The volume's borrow gate (`VolumeStack::borrows`).
    pub borrows: Arc<Borrows>,
}

impl CompactStep {
    fn urgent(&self, want_bytes: u64, urgency: Urgency) -> Result<Freed> {
        // The borrow first: a detach that closed the gate has the engine to
        // itself (declared first, it ends after the engine handle).
        let Some(_borrow) = self.borrows.enter() else {
            return Ok(Freed(0));
        };
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
        // Counted after the engine handle is gone; a failed run is counted by
        // the gate (`LunixSpace::step_errors`).
        if r.is_ok() {
            self.runs.fetch_add(1, Ordering::Release);
        }
        r
    }
}

/// The calls that borrow a volume's engine outside its owner (a `Gc` call,
/// the ladder's compact step), counted so a detach can wait for them and
/// then refuse new ones: once [`close_and_wait`](Self::close_and_wait)
/// returned `true`, nobody but the stack holds the engine.
#[derive(Debug, Default)]
pub struct Borrows {
    st: std::sync::Mutex<(u64, bool)>,
    cv: std::sync::Condvar,
}

/// One borrow; ends when dropped.
#[derive(Debug)]
pub struct Borrow<'a>(&'a Borrows);

impl Borrows {
    /// A borrow, or `None` once the detach closed the gate.
    pub fn enter(&self) -> Option<Borrow<'_>> {
        let mut st = crate::lock(&self.st);
        if st.1 {
            return None;
        }
        st.0 += 1;
        Some(Borrow(self))
    }

    /// Closes the gate and waits up to `limit` for the borrows to end.
    /// `false`: a borrow still runs after `limit` (the engine and `LOCK`
    /// outlive the detach until it ends).
    pub fn close_and_wait(&self, limit: std::time::Duration) -> bool {
        let start = std::time::Instant::now();
        let mut st = crate::lock(&self.st);
        st.1 = true;
        while st.0 > 0 {
            let left = limit.saturating_sub(start.elapsed());
            if left.is_zero() {
                return false;
            }
            st = self
                .cv
                .wait_timeout(st, left)
                .unwrap_or_else(|p| p.into_inner())
                .0;
        }
        true
    }
}

impl Drop for Borrow<'_> {
    fn drop(&mut self) {
        let mut st = crate::lock(&self.0.st);
        st.0 -= 1;
        self.0.cv.notify_all();
    }
}
