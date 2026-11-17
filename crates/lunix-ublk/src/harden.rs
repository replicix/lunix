//! Process settings of the daemon (DESIGN §3, W7): the memory-lock check,
//! `PR_SET_IO_FLUSHER` and a private mount namespace (libublk issue 50), and
//! the helpers that lock the I/O path's memory.
//!
//! The daemon does not `mlockall` (maintainer, 2026-11-15, M12a memory gate):
//! what must never be paged is locked region by region — every queue's I/O
//! buffers and its thread stack (here), the journal's batch buffers and flush
//! thread stack (`lunix-journal`), the key arena (`lunix-crypto`). The rest of
//! the heap may be paged under memory pressure, which costs latency, not a
//! deadlock: swap never lives on a device the daemon serves (W7).

use crate::error::Result;

/// Which settings to apply.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Harden {
    /// Check that this process may lock what the I/O path needs
    /// (`CAP_IPC_LOCK`, or an unlimited `RLIMIT_MEMLOCK`); the regions
    /// themselves are locked when they are created.
    pub mlock: bool,
    /// `prctl(PR_SET_IO_FLUSHER)` on the calling thread (queue threads set it
    /// themselves).
    pub io_flusher: bool,
    /// `unshare(CLONE_NEWNS)`, before any thread exists. Mount propagation is
    /// left as the host has it, so an `umount` elsewhere reaches this
    /// namespace too.
    pub private_mount_ns: bool,
    /// `RLIMIT_CORE` 0 and `PR_SET_DUMPABLE` 0: a core dump of a process with
    /// locked buffers keeps `/dev/ublkcN` open for long, so `abort()` (the
    /// recovery path) must not dump.
    pub no_core_dumps: bool,
}

impl Default for Harden {
    fn default() -> Self {
        Self {
            mlock: true,
            io_flusher: true,
            private_mount_ns: true,
            no_core_dumps: true,
        }
    }
}

/// Applies the settings. Call once, from the main thread, before creating any
/// other thread (the mount namespace and the I/O-flusher flag are inherited).
///
/// # Errors
/// The failing system call. Needs `CAP_SYS_ADMIN` (namespace, I/O flusher)
/// and an unlimited `RLIMIT_MEMLOCK` (or `CAP_IPC_LOCK`).
pub fn harden_process(h: Harden) -> Result<()> {
    if h.private_mount_ns {
        // SAFETY: unshare with a constant flag.
        if unsafe { libc::unshare(libc::CLONE_NEWNS) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    if h.no_core_dumps {
        let zero = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: setrlimit reads one initialised `rlimit`; prctl takes integers.
        if unsafe { libc::setrlimit(libc::RLIMIT_CORE, &zero) } != 0
            || unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) } != 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    if h.mlock {
        check_memlock()?;
    }
    if h.io_flusher {
        const PR_SET_IO_FLUSHER: libc::c_int = 57;
        // SAFETY: prctl with integer arguments only.
        if unsafe { libc::prctl(PR_SET_IO_FLUSHER, 1, 0, 0, 0) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    Ok(())
}

/// Whether this process may lock memory without a byte limit: it has
/// `CAP_IPC_LOCK` (root has it) or `RLIMIT_MEMLOCK` is unlimited.
///
/// # Errors
/// [`crate::UblkError::Io`] naming `RLIMIT_MEMLOCK` and the current limit.
pub fn check_memlock() -> Result<()> {
    let cap_ipc_lock = std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            let hex = s
                .lines()
                .find(|l| l.starts_with("CapEff:"))?
                .split_whitespace()
                .nth(1)?
                .to_owned();
            u64::from_str_radix(&hex, 16).ok()
        })
        .is_some_and(|caps| caps & (1 << 14) != 0);
    let mut lim = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: getrlimit writes one `rlimit`.
    let got = unsafe { libc::getrlimit(libc::RLIMIT_MEMLOCK, &mut lim) } == 0;
    if cap_ipc_lock || (got && lim.rlim_cur == libc::RLIM_INFINITY) {
        return Ok(());
    }
    Err(std::io::Error::other(format!(
        "cannot lock the I/O path's memory: no CAP_IPC_LOCK and RLIMIT_MEMLOCK is {} bytes (run as root, or raise RLIMIT_MEMLOCK to unlimited)",
        lim.rlim_cur
    ))
    .into())
}

/// Memory a thread of the I/O path holds locked: regions (populated) and,
/// optionally, the thread's own stack (locked as touched, `MLOCK_ONFAULT`).
/// Dropping it unlocks everything, on the thread that locked its stack:
/// a freed region the allocator keeps, or a stack the C library caches,
/// would otherwise stay locked.
#[derive(Debug, Default)]
pub struct LockedMemory {
    regions: Vec<(usize, usize)>,
    bytes: u64,
}

impl LockedMemory {
    /// Locks `len` bytes at `ptr`.
    ///
    /// # Errors
    /// `InvalidInput` for an unaligned region (unlocking it later would
    /// unlock a page it shares with a neighbour); otherwise the `mlock`
    /// error, with a hint at `RLIMIT_MEMLOCK`.
    ///
    /// # Safety
    /// `ptr .. ptr + len` must lie inside one live mapping of this process
    /// that outlives `self`, and `ptr` and `len` must be page-aligned (checked).
    pub unsafe fn lock_region(&mut self, ptr: *const u8, len: usize) -> std::io::Result<()> {
        // SAFETY: sysconf has no preconditions.
        let page = usize::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) }).unwrap_or(4096);
        if !(ptr as usize).is_multiple_of(page) || !len.is_multiple_of(page) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("lock_region: {ptr:p}+{len} is not page-aligned"),
            ));
        }
        // SAFETY: the caller's contract.
        if unsafe { libc::mlock(ptr.cast(), len) } != 0 {
            let e = std::io::Error::last_os_error();
            return Err(std::io::Error::new(
                e.kind(),
                format!("mlock of {len} bytes failed: {e} (RLIMIT_MEMLOCK, CAP_IPC_LOCK)"),
            ));
        }
        self.regions.push((ptr as usize, len));
        self.bytes += len as u64;
        Ok(())
    }

    /// Locks the calling thread's stack as it is touched: a thread of the
    /// I/O path never waits for a stack page to come back from swap.
    ///
    /// # Errors
    /// The failing call.
    pub fn lock_own_stack(&mut self) -> std::io::Result<()> {
        // SAFETY: a zeroed attr is filled by pthread_getattr_np and destroyed
        // after use; the range it reports is this thread's own stack.
        unsafe {
            let mut attr: libc::pthread_attr_t = std::mem::zeroed();
            if libc::pthread_getattr_np(libc::pthread_self(), &mut attr) != 0 {
                return Err(std::io::Error::other("pthread_getattr_np failed"));
            }
            let mut addr: *mut libc::c_void = std::ptr::null_mut();
            let mut size: libc::size_t = 0;
            let r = libc::pthread_attr_getstack(&attr, &mut addr, &mut size);
            libc::pthread_attr_destroy(&mut attr);
            if r != 0 {
                return Err(std::io::Error::other("pthread_attr_getstack failed"));
            }
            if libc::mlock2(addr, size, libc::MLOCK_ONFAULT) != 0 {
                let e = std::io::Error::last_os_error();
                return Err(std::io::Error::new(
                    e.kind(),
                    format!("mlock2 of the thread stack ({size} bytes) failed: {e} (RLIMIT_MEMLOCK, CAP_IPC_LOCK)"),
                ));
            }
            self.regions.push((addr as usize, size));
            self.bytes += size as u64;
        }
        Ok(())
    }

    /// Bytes locked (a stack counts with its full size).
    pub fn bytes(&self) -> u64 {
        self.bytes
    }
}

impl Drop for LockedMemory {
    fn drop(&mut self) {
        for &(a, l) in &self.regions {
            // SAFETY: munlock of a range this value locked; munlock never
            // touches the memory itself.
            unsafe { libc::munlock(a as *const libc::c_void, l) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unaligned_region_is_refused_and_not_recorded() {
        let mut l = LockedMemory::default();
        let buf = vec![0u8; 3 * 4096];
        let p = buf.as_ptr();
        // SAFETY: the region is refused before any call; `buf` is live.
        let e = unsafe { l.lock_region(p.wrapping_add(1), 4096) }.expect_err("unaligned ptr");
        assert_eq!(e.kind(), std::io::ErrorKind::InvalidInput);
        // SAFETY: as above.
        let e = unsafe { l.lock_region(p, 100) }.expect_err("unaligned len");
        assert_eq!(e.kind(), std::io::ErrorKind::InvalidInput);
        assert_eq!(l.bytes(), 0);
    }

    #[test]
    fn nothing_selected_does_nothing() {
        let none = Harden {
            mlock: false,
            io_flusher: false,
            private_mount_ns: false,
            no_core_dumps: false,
        };
        harden_process(none).unwrap();
    }

    #[test]
    fn the_default_selects_everything() {
        let d = Harden::default();
        assert!(d.mlock && d.io_flusher && d.private_mount_ns && d.no_core_dumps);
    }
}
