//! Process settings of the daemon (DESIGN §3, W7): `mlockall`,
//! `PR_SET_IO_FLUSHER` and a private mount namespace (libublk issue 50).

use crate::error::Result;

/// Which settings to apply.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Harden {
    /// `mlockall(MCL_CURRENT | MCL_FUTURE)`: the daemon's own memory is never
    /// reclaimed through a device it serves.
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
        // SAFETY: mlockall with constant flags.
        if unsafe { libc::mlockall(libc::MCL_CURRENT | libc::MCL_FUTURE) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
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

#[cfg(test)]
mod tests {
    use super::*;

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
