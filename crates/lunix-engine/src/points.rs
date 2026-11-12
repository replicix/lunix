//! Crash points: the `crashpoint!` of `lunix-iface` (a `SIGKILL` in the
//! `crashpoints` build) plus an in-process stop for tests, through
//! [`EngineHooks::crash_at`](crate::EngineHooks::crash_at).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use lunix_iface::{Error, Result};

use crate::deps::EngineHooks;

/// Marks a durable step: `point!(points, "engine.cN.what")`. Returns from the
/// enclosing function with `Io` when a test hook stops the engine there.
macro_rules! point {
    ($points:expr, $name:literal) => {{
        ::lunix_iface::crashpoint!($name);
        $points.hit($name)?;
    }};
}

/// The engine's view of its crash points.
pub(crate) struct Points {
    hooks: Arc<dyn EngineHooks>,
    dead: AtomicBool,
}

/// The error of an engine stopped at a crash point.
pub(crate) fn crashed() -> Error {
    Error::Io(std::io::Error::other("engine stopped at a crash point"))
}

impl Points {
    pub(crate) fn new(hooks: Arc<dyn EngineHooks>) -> Self {
        Self {
            hooks,
            dead: AtomicBool::new(false),
        }
    }

    /// Passes a crash point.
    pub(crate) fn hit(&self, name: &'static str) -> Result<()> {
        if self.dead.load(Ordering::Acquire) {
            return Err(crashed());
        }
        if self.hooks.crash_at(name) {
            self.dead.store(true, Ordering::Release);
            return Err(crashed());
        }
        Ok(())
    }

    /// True once a hook stopped the engine.
    pub(crate) fn is_dead(&self) -> bool {
        self.dead.load(Ordering::Acquire)
    }

    pub(crate) fn hooks(&self) -> &dyn EngineHooks {
        &*self.hooks
    }
}
