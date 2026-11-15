//! Memory after a workload, without a device (M12a root runs of 2026-11-15:
//! the gate read 748 MiB after the smoke against 601 MiB fresh). Drives the
//! real stack through its queue handlers like the smoke's fio job (1 GiB
//! written with flushes, then read back), and prints the heap figures at each
//! step. Runs only with `LUNIXD_MEMDIAG=1`; it asserts nothing about sizes.
//! (The test driver itself keeps every completed read buffer until it is
//! dropped; the daemon's ublk buffers do not grow.)
//!
//! `the_read_cache_is_released_when_idle` always runs: what the daemon's idle
//! thread calls empties the cache, and reads refill it.
#![allow(unsafe_code)]

mod common;

use std::sync::Arc;

use common::{pattern, scratch, Driver};
use lunix_crypto::Argon2Cost;
use lunix_format::volume::CodecPolicy;
use lunix_format::{CryptoMode, IdLen};
use lunix_iface::block::BlockEngine;
use lunixd::stack::{create_volume, open_volume, CreateParams, OpenParams};

const MIB: u64 = 1 << 20;

struct Heap {
    rss: u64,
    in_use: u64,
    free: u64,
    mmapped: u64,
}

fn heap() -> Heap {
    // SAFETY: mallinfo2 takes no arguments and only reads allocator state.
    let m = unsafe { libc::mallinfo2() };
    Heap {
        rss: lunixd::trim::rss_bytes().unwrap_or(0),
        in_use: m.uordblks as u64,
        free: m.fordblks as u64,
        mmapped: m.hblkhd as u64,
    }
}

fn show(step: &str) {
    let h = heap();
    eprintln!(
        "memdiag {step:<28} rss {:>5} MiB  heap in use {:>5} MiB  free in arenas {:>5} MiB  mmapped {:>5} MiB",
        h.rss / MIB,
        h.in_use / MIB,
        h.free / MIB,
        h.mmapped / MIB
    );
}

#[test]
fn memory_after_a_workload() {
    if std::env::var_os("LUNIXD_MEMDIAG").is_none() {
        return;
    }
    let Some(t) = scratch("memdiag") else { return };
    let read_cache: u64 = std::env::var("MEMDIAG_READ_CACHE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(128 * MIB);
    let map_cache: u64 = std::env::var("MEMDIAG_MAP_CACHE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(256 * MIB);
    show("start");
    let tweak = move |j: &mut lunix_format::local::VolumeJson| {
        j.journal_max = 512 << 20;
        j.read_cache_max = read_cache;
        j.map_cache_max = map_cache;
    };
    let v = create_volume(&CreateParams {
        name: "m",
        parent: t.path(),
        size_bytes: 8 << 30,
        chunk_size: 64 << 10,
        mode: CryptoMode::Integrity,
        policy: CodecPolicy::Auto,
        level: 1,
        id_len: IdLen::Sixteen,
        map_fanout: 512,
        key_file: None,
        passphrase: None,
        argon2: Argon2Cost {
            m_kib: 19_456,
            t: 2,
            p: 1,
        },
        json: Some(&tweak),
    })
    .expect("create");
    let mut p = OpenParams::new(v.voldir.clone(), 4);
    p.tweak = Some(Arc::new(|c| c.read_helpers = 4));
    let s = open_volume(p).expect("open");
    let mut d = Driver::new(&s.engine, 4, 1 << 20);
    show("open");
    let total = 1u64 << 30;
    let mut off = 0;
    while off < total {
        let data = pattern(off / MIB, MIB as usize);
        assert!(d.write(off, &data, false).is_ok());
        off += MIB;
        if off % (64 * MIB) == 0 {
            assert!(d.flush().is_ok());
        }
    }
    assert!(d.flush().is_ok());
    show("written 1 GiB");
    s.engine.commit_now(true).expect("commit");
    show("committed");
    let mut off = 0;
    while off < total {
        let got = d.read(off, MIB as u32);
        assert_eq!(got.len(), MIB as usize);
        off += MIB;
    }
    show("read back 1 GiB");
    s.engine.commit_now(true).expect("commit");
    lunixd::trim::trim();
    show("idle, trimmed");
    drop(d);
    lunixd::trim::trim();
    show("driver dropped, trimmed");
    let released = s.engine.release_read_cache();
    lunixd::trim::trim();
    show(&format!("read cache released ({} MiB)", released / MIB));
    s.engine.quiesce().expect("quiesce");
    drop(s);
    lunixd::trim::trim();
    show("stack dropped, trimmed");
}

#[test]
fn the_read_cache_is_released_when_idle() {
    let Some(t) = scratch("rcache") else { return };
    let v = create_volume(&CreateParams {
        name: "r",
        parent: t.path(),
        size_bytes: 256 << 20,
        chunk_size: 64 << 10,
        mode: CryptoMode::Integrity,
        policy: CodecPolicy::Auto,
        level: 1,
        id_len: IdLen::Sixteen,
        map_fanout: 512,
        key_file: None,
        passphrase: None,
        argon2: Argon2Cost {
            m_kib: 19_456,
            t: 2,
            p: 1,
        },
        json: Some(&|j: &mut lunix_format::local::VolumeJson| j.journal_max = 256 << 20),
    })
    .expect("create");
    let mut p = OpenParams::new(v.voldir.clone(), 2);
    p.tweak = Some(Arc::new(|c| c.background = false));
    let s = open_volume(p).expect("open");
    let mut d = Driver::new(&s.engine, 2, 1 << 20);
    for i in 0..16u64 {
        assert!(d.write(i * MIB, &pattern(i, MIB as usize), false).is_ok());
    }
    s.engine.commit_now(true).expect("commit");
    for i in 0..16u64 {
        assert_eq!(d.read(i * MIB, MIB as u32), pattern(i, MIB as usize));
    }
    let held = s.engine.release_read_cache();
    assert!(held >= 8 * MIB, "the reads filled the cache: {held}");
    assert_eq!(s.engine.release_read_cache(), 0, "empty after the release");
    assert_eq!(d.read(3 * MIB, MIB as u32), pattern(3, MIB as usize));
    assert!(s.engine.release_read_cache() > 0, "reads refill it");
    drop(d);
    s.engine.quiesce().expect("quiesce");
}
