//! Minimal in-memory implementations of the iface traits, just good enough to
//! run the contract suites. They are not the testkit (M1.B2) and prove only that
//! the macros compile, run and are satisfiable.
#![allow(dead_code, clippy::type_complexity)]

pub mod index;
pub mod journal;
pub mod map;
pub mod overlay;
pub mod space;
pub mod store;

use std::sync::{Mutex, MutexGuard};

/// Locks, ignoring poisoning (a failed test elsewhere must not cascade).
pub fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// A deterministic 32-byte digest (four FNV-1a lanes). Not cryptographic.
pub fn hash32(data: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    for (lane, chunk) in out.chunks_mut(8).enumerate() {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325 ^ (lane as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15);
        for &b in data {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        chunk.copy_from_slice(&h.to_le_bytes());
    }
    out
}
