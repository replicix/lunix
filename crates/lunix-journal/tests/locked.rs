//! With `lock_memory` the journal's I/O memory is locked (DESIGN §3, W7):
//! the lanes' batch buffers, the zero-fill buffer, the flush thread's stack
//! and its mark buffer. Sized for an unprivileged `RLIMIT_MEMLOCK` of 8 MiB:
//! one lane, two pool buffers.

mod common;

use common::*;

fn vmlck_kib() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("VmLck:"))?
                .split_whitespace()
                .nth(1)?
                .parse()
                .ok()
        })
        .unwrap_or(0)
}

#[test]
fn the_journal_locks_its_buffers_and_unlocks_them_with_the_lane() {
    let t = scratch();
    let mut cfg = small_cfg(t.path().join("journal"), 1, 2);
    cfg.pool_buffers = 2;
    cfg.fixed_buffers = Some(false);
    cfg.lock_memory = true;
    let base = vmlck_kib();
    let j = open_journal(&cfg);
    let opened = j.locked_bytes();
    // The zero buffer (1 MiB), the mark buffer (4 KiB) and the flush thread's
    // stack.
    assert!(opened > (1 << 20) + 4096, "{opened}");
    assert!(
        vmlck_kib() >= base + opened / 1024,
        "VmLck covers what the journal locked"
    );
    recover_all(&j);
    let l = open_lane(&j, 0);
    let pool = 2 * ((256 << 12) + 4096);
    assert_eq!(j.locked_bytes(), opened + pool, "the lane's batch buffers");
    assert!(vmlck_kib() >= base + j.locked_bytes() / 1024);
    drop(l);
    assert_eq!(j.locked_bytes(), opened, "unlocked with the lane");
    drop(j);
    assert!(
        vmlck_kib() <= base + 8,
        "everything is released with the journal"
    );
}

#[test]
fn without_lock_memory_nothing_is_locked() {
    let t = scratch();
    let cfg = small_cfg(t.path().join("journal"), 1, 2);
    let j = open_journal(&cfg);
    recover_all(&j);
    let _l = open_lane(&j, 0);
    assert_eq!(j.locked_bytes(), 0);
}
