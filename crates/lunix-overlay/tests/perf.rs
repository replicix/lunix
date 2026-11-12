//! Informational perf: `cargo test --release -p lunix-overlay --test perf -- --ignored --nocapture`.

use std::time::Instant;

use lunix_format::WriteSeq;
use lunix_iface::journal::{JournalLoc, LaneId};
use lunix_iface::overlay::{BlockRange, Overlay};
use lunix_overlay::ShardedOverlay;

#[test]
#[ignore = "informational, run by hand on a quiet host"]
fn lookup_rate_on_a_million_entries() {
    let o = ShardedOverlay::new(16);
    let n = 1_000_000u64;
    for i in 0..n {
        o.insert_write(
            BlockRange {
                lba: i * 2,
                blocks: 1,
            },
            JournalLoc {
                lane: LaneId(0),
                ordinal: 1,
                offset: 4096 + i * 8192,
            },
            WriteSeq(i + 1),
        );
    }
    let mut out = Vec::new();
    let mut x = 88172645463325252u64;
    let lookups = 5_000_000u64;
    let t = Instant::now();
    let mut hits = 0u64;
    for _ in 0..lookups {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        o.lookup(
            BlockRange {
                lba: x % (2 * n),
                blocks: 1,
            },
            &mut out,
        );
        hits += out.len() as u64;
    }
    let rate = lookups as f64 / t.elapsed().as_secs_f64();
    println!("lookup: {:.2} M/s ({hits} pieces)", rate / 1e6);
}
