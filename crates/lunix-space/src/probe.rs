//! Where the free-space figures come from: [`FsProbe`], the `statvfs(2)`
//! implementation and a settable one for tests and simulations.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, Weak};

use lunix_iface::space::SpaceGate;

use crate::LunixSpace;

/// What a probe reports about the host file system holding the volume.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct FsStat {
    /// Size of the file system in bytes (for a percentage `free_floor`).
    pub total: u64,
    /// Bytes available to this process (`f_bavail`), not `f_bfree`.
    pub free: u64,
}

/// A source of [`FsStat`]. Called from the background thread every second and
/// from a queue thread when the ledger is within 5 % of its limit, so it must
/// be a cheap, non-blocking-in-practice call.
pub trait FsProbe: Send + Sync {
    /// Reads the figures now.
    ///
    /// # Errors
    /// The underlying I/O error; an `EIO` makes the gate go read-only.
    fn stat(&self) -> io::Result<FsStat>;
}

/// `statvfs(2)` on a directory of the host file system.
#[derive(Clone, Debug)]
pub struct StatvfsProbe {
    dir: PathBuf,
}

impl StatvfsProbe {
    /// A probe for the file system that holds `dir`.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// The directory probed.
    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

impl FsProbe for StatvfsProbe {
    fn stat(&self) -> io::Result<FsStat> {
        let v = rustix::fs::statvfs(&self.dir)?;
        let unit = if v.f_frsize == 0 {
            v.f_bsize
        } else {
            v.f_frsize
        };
        Ok(FsStat {
            total: v.f_blocks.saturating_mul(unit),
            free: v.f_bavail.saturating_mul(unit),
        })
    }
}

/// A probe whose figures are set by hand: for tests and simulations.
#[derive(Debug)]
pub struct ManualFs {
    total: AtomicU64,
    free: AtomicU64,
    fail: AtomicBool,
    calls: AtomicU64,
}

impl ManualFs {
    /// A file system of `total` bytes with `free` available.
    pub fn new(total: u64, free: u64) -> Self {
        Self {
            total: AtomicU64::new(total),
            free: AtomicU64::new(free),
            fail: AtomicBool::new(false),
            calls: AtomicU64::new(0),
        }
    }

    /// Sets the free bytes the next `stat` reports.
    pub fn set_free(&self, free: u64) {
        self.free.store(free, Ordering::SeqCst);
    }

    /// Sets the total bytes the next `stat` reports.
    pub fn set_total(&self, total: u64) {
        self.total.store(total, Ordering::SeqCst);
    }

    /// Makes `stat` fail with `EIO` (or work again).
    pub fn set_failing(&self, failing: bool) {
        self.fail.store(failing, Ordering::SeqCst);
    }

    /// How many times `stat` was called.
    pub fn calls(&self) -> u64 {
        self.calls.load(Ordering::SeqCst)
    }
}

impl FsProbe for ManualFs {
    fn stat(&self) -> io::Result<FsStat> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.fail.load(Ordering::SeqCst) {
            return Err(io::Error::from_raw_os_error(5));
        }
        Ok(FsStat {
            total: self.total.load(Ordering::SeqCst),
            free: self.free.load(Ordering::SeqCst),
        })
    }
}

/// A simulated host file system that agrees with the ledger: every byte the
/// gate holds counts as used on disk, plus `external` bytes of someone else.
/// `free = total - external - used`. For tests and simulations; call
/// [`attach`](Self::attach) once the gate exists.
#[derive(Debug)]
pub struct SimFs {
    total: u64,
    external: AtomicU64,
    calls: AtomicU64,
    gate: OnceLock<Weak<LunixSpace>>,
}

impl SimFs {
    /// A file system of `total` bytes with `external` bytes in use by others.
    pub fn new(total: u64, external: u64) -> Self {
        Self {
            total,
            external: AtomicU64::new(external),
            calls: AtomicU64::new(0),
            gate: OnceLock::new(),
        }
    }

    /// Links the file system to the gate whose ledger it mirrors.
    pub fn attach(&self, gate: &Arc<LunixSpace>) {
        let _ = self.gate.set(Arc::downgrade(gate));
    }

    /// Sets the bytes in use by others.
    pub fn set_external(&self, external: u64) {
        self.external.store(external, Ordering::SeqCst);
    }

    /// How many times `stat` was called.
    pub fn calls(&self) -> u64 {
        self.calls.load(Ordering::SeqCst)
    }
}

impl FsProbe for SimFs {
    fn stat(&self) -> io::Result<FsStat> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let used = self
            .gate
            .get()
            .and_then(Weak::upgrade)
            .map_or(0, |g| g.snapshot().used());
        let taken = used.saturating_add(self.external.load(Ordering::SeqCst));
        Ok(FsStat {
            total: self.total,
            free: self.total.saturating_sub(taken),
        })
    }
}
