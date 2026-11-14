//! Crash points: the `crashpoint!` of `lunix-iface` (a `SIGKILL` in the
//! `crashpoints` build) plus an in-process stop for the tests over the fakes,
//! through [`GcHooks::crash_at`].

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use lunix_iface::{Error, Result};

use crate::config::GcHooks;

/// Every crash point of the collector (plan M17 rule 10), in pass order.
pub const CRASH_POINTS: &[&str] = &[
    "gc.mark.end",
    "gc.condemn.after_list_write",
    "gc.delete.before_unlink",
    "gc.delete.after_unlink",
    "gc.delete.after_dirsync",
    "gc.commit_delete.before_unlink",
    "gc.compact.after_segment_seal",
    "gc.compact.after_relocate",
    "gc.evidence.after_put",
];

/// Marks a durable step: `point!(points, "gc.x.y")`. Returns from the
/// enclosing function with `Io` when a test hook stops the collector there.
macro_rules! point {
    ($points:expr, $name:literal) => {{
        ::lunix_iface::crashpoint!($name);
        $points.hit($name)?;
    }};
}
pub(crate) use point;

/// The error of a collector stopped at a crash point.
pub(crate) fn crashed() -> Error {
    Error::Io(std::io::Error::other("gc stopped at a crash point"))
}

/// The collector's view of its crash points.
pub(crate) struct Points {
    hooks: Arc<dyn GcHooks>,
    dead: AtomicBool,
}

impl std::fmt::Debug for Points {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Points")
            .field("dead", &self.dead.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl Points {
    pub(crate) fn new(hooks: Arc<dyn GcHooks>) -> Self {
        Self {
            hooks,
            dead: AtomicBool::new(false),
        }
    }

    /// Passes a crash point.
    pub(crate) fn hit(&self, name: &'static str) -> Result<()> {
        self.alive()?;
        if self.hooks.crash_at(name) {
            self.dead.store(true, Ordering::Release);
            return Err(crashed());
        }
        Ok(())
    }

    /// `Io` once a hook stopped the collector.
    pub(crate) fn alive(&self) -> Result<()> {
        if self.dead.load(Ordering::Acquire) {
            Err(crashed())
        } else {
            Ok(())
        }
    }
}
