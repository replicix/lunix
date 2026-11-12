//! Plan M6 performance gate (ignored by default; it builds 10 million entries):
//!
//! ```text
//! tools/remote.sh cargo test --release -p lunix-index --test perf -- --ignored --nocapture
//! ```
//!
//! * at least 1 M lookups/s on 10 M entries, single thread, memtable hits and
//!   run hits mixed 50/50;
//! * at most 16 bytes of RAM per indexed chunk for the filters and block
//!   directories, measured as live heap bytes with a counting allocator around
//!   `open`.

mod common;

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use lunix_format::ChunkId;
use lunix_iface::index::{ChunkIndex, Confirm};

use common::{cid, hint, open, seg, Ids};

static LIVE: AtomicUsize = AtomicUsize::new(0);

struct Counting;

// SAFETY: forwards every operation to `System` and only adds to an atomic.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        LIVE.fetch_add(l.size(), Ordering::Relaxed);
        // SAFETY: forwarded unchanged.
        unsafe { System.alloc(l) }
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        LIVE.fetch_add(l.size(), Ordering::Relaxed);
        // SAFETY: forwarded unchanged.
        unsafe { System.alloc_zeroed(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        LIVE.fetch_sub(l.size(), Ordering::Relaxed);
        // SAFETY: forwarded unchanged.
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        LIVE.fetch_add(new, Ordering::Relaxed);
        LIVE.fetch_sub(l.size(), Ordering::Relaxed);
        // SAFETY: forwarded unchanged.
        unsafe { System.realloc(p, l, new) }
    }
}

#[global_allocator]
static COUNTING: Counting = Counting;

const PER_RUN: u64 = 1_250_000;
const IN_RUNS: u64 = 4 * PER_RUN;
const IN_MEM: u64 = 5_000_000;

fn xorshift(s: &mut u64) -> u64 {
    *s ^= *s << 13;
    *s ^= *s >> 7;
    *s ^= *s << 17;
    *s
}

fn dir() -> tempfile::TempDir {
    match std::env::var_os("LUNIX_PERF_DIR") {
        Some(d) => tempfile::tempdir_in(d).expect("tempdir"),
        None => tempfile::tempdir().expect("tempdir"),
    }
}

#[test]
#[ignore = "builds 10 M entries; run with --release -- --ignored"]
fn ten_million_entries_lookup_rate_and_ram() {
    let tmp = dir();
    let ids = Ids::new();
    let ix = open(tmp.path(), &ids);
    for r in 0..4u64 {
        for n in r * PER_RUN..(r + 1) * PER_RUN {
            ix.insert(cid(n), hint(r + 1, (n % 1_000_000) as u32))
                .expect("insert");
        }
        ix.persist(seg(r + 1)).expect("persist");
    }
    assert_eq!(ix.runs().expect("runs").len(), 4);
    drop(ix);

    // RAM of the persisted part, measured around `open`.
    let before = LIVE.load(Ordering::Relaxed);
    let ix = open(tmp.path(), &ids);
    let held = LIVE.load(Ordering::Relaxed) - before;
    let per_chunk = held as f64 / IN_RUNS as f64;
    println!("open: {held} B live for {IN_RUNS} entries = {per_chunk:.2} B/chunk");
    assert!(per_chunk <= 16.0, "{per_chunk} B per chunk");
    assert!(ix.filter_ram_bytes() as f64 / IN_RUNS as f64 <= 16.0);

    // Another 5 M entries stay in the memtable.
    for n in IN_RUNS..IN_RUNS + IN_MEM {
        ix.insert(cid(n), hint(5, (n % 1_000_000) as u32))
            .expect("insert");
    }
    let total = IN_RUNS + IN_MEM;
    // Pre-draw the probe ids: half from runs, half from the memtable.
    let mut s = 0x9e37_79b9_7f4a_7c15u64;
    let probes: Vec<ChunkId> = (0..2_000_000u64)
        .map(|i| {
            let r = xorshift(&mut s);
            if i % 2 == 0 {
                cid(r % IN_RUNS)
            } else {
                cid(IN_RUNS + r % IN_MEM)
            }
        })
        .collect();
    let mut hinted = 0u64;
    let mut verified = 0u64;
    let t = Instant::now();
    for id in &probes {
        match ix.get_for_dedup(id).expect("lookup") {
            Some(h) if h.confirm == Confirm::Hinted => hinted += 1,
            Some(_) => verified += 1,
            None => panic!("a present id was not found"),
        }
    }
    let secs = t.elapsed().as_secs_f64();
    let rate = probes.len() as f64 / secs;
    println!("{total} entries: {rate:.0} lookups/s ({hinted} run hits, {verified} memtable hits)");
    for (name, par) in [("run hits only", 0usize), ("memtable hits only", 1)] {
        let t = Instant::now();
        let mut n = 0u64;
        for id in probes.iter().skip(par).step_by(2) {
            assert!(ix.get_for_dedup(id).expect("lookup").is_some());
            n += 1;
        }
        println!(
            "  {name}: {:.0} lookups/s",
            n as f64 / t.elapsed().as_secs_f64()
        );
    }
    assert!(rate >= 1_000_000.0, "{rate:.0} lookups/s");

    // Misses must be cheap too (the filter answers them).
    let t = Instant::now();
    let mut found = 0u64;
    for n in 0..1_000_000u64 {
        if ix.get_for_dedup(&cid(total + n)).expect("lookup").is_some() {
            found += 1;
        }
    }
    println!(
        "misses: {:.0} lookups/s ({found} false hits)",
        1e6 / t.elapsed().as_secs_f64()
    );

    // All 10 M in runs: the budget again at full size.
    ix.persist(seg(5)).expect("persist");
    drop(ix);
    let before = LIVE.load(Ordering::Relaxed);
    let ix = open(tmp.path(), &ids);
    let held = LIVE.load(Ordering::Relaxed) - before;
    let per_chunk = held as f64 / total as f64;
    println!("open: {held} B live for {total} entries = {per_chunk:.2} B/chunk");
    assert!(per_chunk <= 16.0, "{per_chunk} B per chunk");
    drop(ix);
}
