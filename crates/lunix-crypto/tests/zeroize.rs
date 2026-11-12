//! A dropped key buffer is zeroized (read through a retained raw pointer).
//!
//! This file is the one place besides the key arena that uses `unsafe`: the
//! slot of a dropped `Secret` stays mapped (the arena never unmaps), so a
//! pointer kept past the drop can be read to see what the drop left behind.
//! It is the only test in this binary, so nothing else allocates the slot
//! between the drop and the read.
#![allow(unsafe_code)]

use lunix_crypto::{arena_stats, Kmk};

#[test]
fn a_dropped_key_is_zeroed_in_the_arena() {
    let bytes = [0xA5u8; 32];
    let key = Kmk::from_bytes(&bytes);
    let secret = key.secret();
    if !secret.is_arena_backed() {
        eprintln!("SKIP: the arena could not map memory here; heap fallback is zeroized by Drop");
        return;
    }
    let ptr = secret.slot_address();
    // SAFETY: the slot is live and 32 bytes long.
    let before = unsafe { std::slice::from_raw_parts(ptr, 32) }.to_vec();
    assert_eq!(before, bytes, "the key is stored as given");
    drop(key);
    // SAFETY: arena chunks are never unmapped, so `ptr` still points into a
    // mapped page; the slot is free but nothing re-allocated it.
    let after = unsafe { std::slice::from_raw_parts(ptr, 32) }.to_vec();
    assert_eq!(after, [0u8; 32], "Drop must overwrite the key with zeros");

    // What the kernel allowed (mlock may fail under a small RLIMIT_MEMLOCK;
    // the counters say so instead of the daemon refusing to start). Run in
    // this test so nothing else touches the arena meanwhile.
    let s = arena_stats();
    assert!(s.chunks >= 1);
    assert!(s.locked_chunks <= s.chunks);
    assert!(s.dump_excluded_chunks <= s.chunks);
    eprintln!("arena: {s:?}");
}
