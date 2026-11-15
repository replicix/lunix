//! The key arena is locked memory (DESIGN §3, W7: the daemon locks region by
//! region; it no longer runs `mlockall`). One 64 KiB chunk fits an
//! unprivileged `RLIMIT_MEMLOCK`.

use lunix_crypto::{arena_stats, require_locked, Secret};

/// The `Locked:` figure, in KiB, of the mapping that contains `addr`.
fn locked_kib_at(addr: usize) -> Option<u64> {
    let smaps = std::fs::read_to_string("/proc/self/smaps").ok()?;
    let mut inside = false;
    for line in smaps.lines() {
        if let Some((range, _)) = line.split_once(' ') {
            if let Some((a, b)) = range.split_once('-') {
                if let (Ok(a), Ok(b)) = (usize::from_str_radix(a, 16), usize::from_str_radix(b, 16))
                {
                    inside = (a..b).contains(&addr);
                    continue;
                }
            }
        }
        if inside {
            if let Some(v) = line.strip_prefix("Locked:") {
                return v.split_whitespace().next()?.parse().ok();
            }
        }
    }
    None
}

#[test]
fn every_key_byte_is_in_locked_memory() {
    let st = require_locked().expect("the arena locks under the test's RLIMIT_MEMLOCK");
    assert!(st.all_locked() && st.locked_bytes() >= 64 * 1024, "{st:?}");
    let s = Secret::new(&[7u8; 32]);
    assert!(s.is_arena_backed());
    let locked = locked_kib_at(s.slot_address() as usize).expect("the slot's mapping");
    assert!(
        locked >= 64,
        "the key's chunk is locked: Locked: {locked} kB"
    );
    assert!(arena_stats().all_locked());
}
