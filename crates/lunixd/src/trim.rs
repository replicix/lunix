//! Heap retention after a workload (maintainer, 2026-11-13: the memory gate is
//! measured after a workload).
//!
//! [`trim`] runs from the daemon's idle thread (never a queue thread), once
//! every volume has been idle for `idle_trim_ms`: `malloc_trim(0)` returns the
//! top of each heap and what `madvise` can release.
//!
//! The allocator keeps glibc's defaults (M12a re-review R6): a fixed mmap
//! threshold would turn every chunk-size allocation into an `mmap`, which under
//! `mlockall` is populated and locked at once and whose `munmap` sends TLB
//! shootdowns to the queue threads, and fewer arenas would make the queue
//! threads share them. Whether more than the idle trim is needed is decided by
//! the memory gate after a workload (root, old host), and the throughput effect
//! of any further measure by M16. Note that under `mlockall` the kernel ignores
//! `MADV_DONTNEED` on locked pages, so the trim may release less than it would
//! without it; the gate's reading says how much.
//!
//! Outside glibc the trim is a no-op.
#![allow(unsafe_code)]

/// Returns free heap memory to the kernel where the allocator can.
pub fn trim() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        // SAFETY: malloc_trim takes an integer and is thread-safe; it only
        // releases memory the allocator holds free.
        unsafe {
            libc::malloc_trim(0);
        }
    }
}

/// The resident set of this process in bytes (`VmRSS`), for logs and tests.
pub fn rss_bytes() -> Option<u64> {
    let s = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = s.lines().find(|l| l.starts_with("VmRSS:"))?;
    let kib: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kib * 1024)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trim_runs_and_rss_is_read() {
        let big: Vec<Vec<u8>> = (0..64).map(|i| vec![i as u8; 256 << 10]).collect();
        drop(big);
        trim();
        assert!(rss_bytes().is_some_and(|r| r > 0));
    }
}
