//! Probe for the memory gate (M12a root runs, second round): can `malloc_trim`
//! return freed heap pages that are locked, as every page is under the
//! daemon's `mlockall`? Runs only with `LUNIXD_MLOCK_PROBE=1`. An
//! unprivileged `RLIMIT_MEMLOCK` (8 MiB on the build host) does not allow
//! `mlockall` of a test process, so the probe `mlock`s the heap pages it
//! allocates, which is what `mlockall` does to every page: the kernel then
//! refuses `MADV_DONTNEED` on them (madvise(2)), and glibc's trim releases
//! freed heap pages with exactly that. It prints the figures.
#![allow(unsafe_code)]

const KIB: usize = 1024;
const CHUNK: usize = 64 * KIB;

fn rss_kib() -> u64 {
    lunixd::trim::rss_bytes().unwrap_or(0) / 1024
}

/// The page-aligned interior of a buffer.
fn pages(v: &[u8]) -> (usize, usize) {
    let start = (v.as_ptr() as usize + 4095) & !4095;
    let end = (v.as_ptr() as usize + v.len()) & !4095;
    (start, end.saturating_sub(start))
}

/// Allocates 40 chunks of 64 KiB (the read cache's chunk size) and touches
/// them, keeps a small allocation after them (the arena top cannot shrink
/// past it), optionally locks the chunks' pages, frees the chunks and trims.
/// With `unlock_before_trim` the pages are unlocked first (what an unlock
/// around the idle trim would do). Returns RSS before and after.
fn probe(lock: bool, unlock_before_trim: bool) -> (u64, u64) {
    let chunks: Vec<Vec<u8>> = (0..40).map(|i| vec![i as u8 | 1; CHUNK]).collect();
    let pin = vec![1u8; 512];
    let ranges: Vec<(usize, usize)> = chunks.iter().map(|c| pages(c)).collect();
    if lock {
        for &(a, l) in &ranges {
            // SAFETY: the range lies inside a live allocation of ours.
            let r = unsafe { libc::mlock(a as *const libc::c_void, l) };
            assert_eq!(r, 0, "mlock: {}", std::io::Error::last_os_error());
        }
    }
    let before = rss_kib();
    drop(chunks);
    if lock && unlock_before_trim {
        for &(a, l) in &ranges {
            // SAFETY: munlock of pages of this process (now free heap).
            unsafe { libc::munlock(a as *const libc::c_void, l) };
        }
    }
    lunixd::trim::trim();
    let after = rss_kib();
    if lock && !unlock_before_trim {
        for &(a, l) in &ranges {
            // SAFETY: as above; leaves the process unlocked.
            unsafe { libc::munlock(a as *const libc::c_void, l) };
        }
    }
    drop(pin);
    (before, after)
}

#[test]
fn malloc_trim_of_locked_free_heap() {
    if std::env::var_os("LUNIXD_MLOCK_PROBE").is_none() {
        return;
    }
    for (name, lock, unlock) in [
        ("not locked", false, false),
        ("locked (as under mlockall)", true, false),
        ("locked, unlocked before the trim", true, true),
    ] {
        let (b, a) = probe(lock, unlock);
        eprintln!(
            "mlockprobe {name:<34} rss {b:>6} KiB -> {a:>6} KiB after free and trim (returned {} KiB of 2 560)",
            b.saturating_sub(a)
        );
    }
}
