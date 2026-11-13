//! Heap retention after a workload (maintainer, 2026-11-13: the memory gate is
//! measured after a workload).
//!
//! glibc keeps freed memory: its mmap threshold grows after the first large
//! free, so later 64 KiB-1 MiB chunk buffers come from the heap and stay there,
//! and every thread gets an arena. Under `mlockall` the pages stay resident and
//! `madvise(MADV_DONTNEED)` on locked pages does not release them, so
//! `malloc_trim` alone cannot give the memory back. Two measures:
//!
//! * [`tune_allocator`] at start, before any thread: a fixed mmap threshold
//!   (allocations of 128 KiB and more are mmapped and unmapped on free, which
//!   also unlocks them) and a bounded arena count.
//! * [`trim`] from the daemon's idle thread (never a queue thread), once every
//!   volume has been idle for `idle_trim_ms`: `malloc_trim(0)`, which returns
//!   the top of each heap (`brk`/heap shrink) and what `madvise` can release.
//!
//! Other allocators: both are no-ops outside glibc.
#![allow(unsafe_code)]

/// Allocations at least this large are served by `mmap` (glibc's dynamic
/// threshold is switched off by setting it).
pub const MMAP_THRESHOLD: i32 = 128 << 10;

/// Arenas glibc may create (the default is 8 × cores).
pub const ARENA_MAX: i32 = 4;

/// Sets the allocator options. Call once, before other threads exist.
pub fn tune_allocator() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        // SAFETY: mallopt takes two integers and only changes allocator
        // parameters; it is called before any other thread allocates.
        unsafe {
            libc::mallopt(libc::M_MMAP_THRESHOLD, MMAP_THRESHOLD);
            libc::mallopt(libc::M_ARENA_MAX, ARENA_MAX);
        }
    }
}

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
        tune_allocator();
        let big: Vec<Vec<u8>> = (0..64).map(|i| vec![i as u8; 256 << 10]).collect();
        drop(big);
        trim();
        assert!(rss_bytes().is_some_and(|r| r > 0));
    }
}
