//! Memory gate: 1 000 000 distinct, unmergeable 4 KiB entries cost at most
//! 96 bytes each, measured with a counting allocator. Own test binary because
//! of the global allocator.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use lunix_format::WriteSeq;
use lunix_iface::journal::{JournalLoc, LaneId};
use lunix_iface::overlay::{BlockRange, Overlay};
use lunix_overlay::ShardedOverlay;

struct Counting;
static LIVE: AtomicUsize = AtomicUsize::new(0);

// SAFETY: forwards every call to `System` unchanged and only counts sizes.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        LIVE.fetch_add(l.size(), Ordering::Relaxed);
        // SAFETY: same contract as ours.
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        LIVE.fetch_sub(l.size(), Ordering::Relaxed);
        // SAFETY: `p` came from `System.alloc` with `l` through us.
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        LIVE.fetch_add(l.size(), Ordering::Relaxed);
        // SAFETY: same contract as ours.
        unsafe { System.alloc_zeroed(l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        LIVE.fetch_add(n, Ordering::Relaxed);
        LIVE.fetch_sub(l.size(), Ordering::Relaxed);
        // SAFETY: `p` and `l` are from our `alloc`; `n` is the caller's.
        unsafe { System.realloc(p, l, n) }
    }
}

#[global_allocator]
static A: Counting = Counting;

const N: u64 = 1_000_000;

fn fill(o: &ShardedOverlay, order: impl Iterator<Item = u64>) {
    for (n, i) in order.enumerate() {
        // Every other block, so that no two entries are adjacent; distinct
        // sequence numbers and locations.
        o.insert_write(
            BlockRange {
                lba: i * 2,
                blocks: 1,
            },
            JournalLoc {
                lane: LaneId((n % 4) as u16),
                ordinal: 1,
                offset: 4096 + n as u64 * 8192,
            },
            WriteSeq(n as u64 + 1),
        );
    }
}

fn per_entry(order: impl Iterator<Item = u64>) -> f64 {
    let before = LIVE.load(Ordering::Relaxed);
    let o = ShardedOverlay::new(16);
    fill(&o, order);
    let used = LIVE.load(Ordering::Relaxed) - before;
    assert_eq!(o.entries() as u64, N);
    used as f64 / N as f64
}

#[test]
fn a_million_entries_cost_at_most_96_bytes_each() {
    let seq = per_entry(0..N);
    // A pseudo-random permutation of 0..N (multiplier coprime to N).
    let random = per_entry((0..N).map(|i| i * 618_033 % N));
    eprintln!("bytes per entry: sequential {seq:.1}, random order {random:.1}");
    assert!(seq <= 96.0, "{seq}");
    assert!(random <= 96.0, "{random}");
}
