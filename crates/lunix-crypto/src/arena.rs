//! The key arena: page-locked, dump-excluded storage for 32-byte secrets.
//!
//! This module and one test are the only `unsafe` in the crate. Long-lived
//! keys (roots, DEKs, metadata keys, MAC keys) live in 32-byte slots of
//! anonymous private mappings that are `mlock`ed and marked `MADV_DONTDUMP`,
//! both best effort: a failure is counted in [`arena_stats`] and the mapping
//! is used anyway (a daemon without `CAP_IPC_LOCK` and a small `RLIMIT_MEMLOCK`
//! still works, it just may swap). If a mapping cannot be made at all the
//! secret falls back to a zeroized heap box.
//!
//! Chunks are never unmapped, so a slot pointer stays valid for the life of
//! the process; a freed slot is overwritten with zeros before it goes back on
//! the free list.

#![allow(unsafe_code)]

use std::ptr::NonNull;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock, PoisonError};

use rustix::mm::{madvise, mlock, mmap_anonymous, Advice, MapFlags, ProtFlags};
use zeroize::Zeroize;

/// Length of every secret.
pub const KEY_LEN: usize = 32;
const CHUNK_BYTES: usize = 64 * 1024;
const SLOTS_PER_CHUNK: usize = CHUNK_BYTES / KEY_LEN;

#[derive(Clone, Copy)]
struct SlotPtr(NonNull<[u8; KEY_LEN]>);

// SAFETY: a `SlotPtr` names a slot of a chunk that stays mapped for the life
// of the process. Ownership of a slot moves between the free list (behind its
// mutex) and exactly one `Secret`, so no two threads write the same slot, and
// readers only get `&[u8; 32]` through the owning `Secret`.
unsafe impl Send for SlotPtr {}
// SAFETY: see above; a shared `SlotPtr` is never dereferenced without owning it.
unsafe impl Sync for SlotPtr {}

struct Arena {
    free: Mutex<Vec<SlotPtr>>,
    chunks: AtomicU64,
    locked_chunks: AtomicU64,
    dump_excluded_chunks: AtomicU64,
    heap_fallbacks: AtomicU64,
}

/// What the arena managed to do, for the daemon's start-up log.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ArenaStats {
    /// Chunks mapped.
    pub chunks: u64,
    /// Chunks whose `mlock` succeeded.
    pub locked_chunks: u64,
    /// Chunks whose `MADV_DONTDUMP` succeeded.
    pub dump_excluded_chunks: u64,
    /// Secrets that fell back to a heap box because no chunk could be mapped.
    pub heap_fallbacks: u64,
}

/// A snapshot of the arena's counters.
pub fn arena_stats() -> ArenaStats {
    let a = arena();
    ArenaStats {
        chunks: a.chunks.load(Ordering::Relaxed),
        locked_chunks: a.locked_chunks.load(Ordering::Relaxed),
        dump_excluded_chunks: a.dump_excluded_chunks.load(Ordering::Relaxed),
        heap_fallbacks: a.heap_fallbacks.load(Ordering::Relaxed),
    }
}

fn arena() -> &'static Arena {
    static ARENA: OnceLock<Arena> = OnceLock::new();
    ARENA.get_or_init(|| Arena {
        free: Mutex::new(Vec::new()),
        chunks: AtomicU64::new(0),
        locked_chunks: AtomicU64::new(0),
        dump_excluded_chunks: AtomicU64::new(0),
        heap_fallbacks: AtomicU64::new(0),
    })
}

impl Arena {
    fn take(&self) -> Option<SlotPtr> {
        let mut free = self.free.lock().unwrap_or_else(PoisonError::into_inner);
        if free.is_empty() {
            self.grow(&mut free);
        }
        free.pop()
    }

    fn give(&self, p: SlotPtr) {
        self.free
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(p);
    }

    fn grow(&self, free: &mut Vec<SlotPtr>) {
        // SAFETY: a null hint with MAP_PRIVATE (anonymous) maps fresh zeroed
        // pages and cannot replace an existing mapping.
        let mapped = unsafe {
            mmap_anonymous(
                std::ptr::null_mut(),
                CHUNK_BYTES,
                ProtFlags::READ | ProtFlags::WRITE,
                MapFlags::PRIVATE,
            )
        };
        let Ok(base) = mapped else {
            return;
        };
        self.chunks.fetch_add(1, Ordering::Relaxed);
        // SAFETY: `base` is the start of a mapping of `CHUNK_BYTES` bytes.
        if unsafe { mlock(base, CHUNK_BYTES) }.is_ok() {
            self.locked_chunks.fetch_add(1, Ordering::Relaxed);
        }
        // SAFETY: same mapping; MADV_DONTDUMP only changes core-dump policy.
        if unsafe { madvise(base, CHUNK_BYTES, Advice::LinuxDontDump) }.is_ok() {
            self.dump_excluded_chunks.fetch_add(1, Ordering::Relaxed);
        }
        let Some(first) = NonNull::new(base.cast::<[u8; KEY_LEN]>()) else {
            return;
        };
        // Pushed so that the lowest address is handed out first.
        for i in (0..SLOTS_PER_CHUNK).rev() {
            // SAFETY: `i < SLOTS_PER_CHUNK`, so the slot lies inside the mapping.
            free.push(SlotPtr(unsafe { first.add(i) }));
        }
    }
}

enum Slot {
    Arena(SlotPtr),
    Heap(Box<[u8; KEY_LEN]>),
}

/// A 32-byte secret in locked memory, zeroized on drop.
///
/// No `Debug`, `Clone`, `Copy` or `Display`: there is no way to print or
/// duplicate one by accident.
pub struct Secret {
    slot: Slot,
}

impl Secret {
    /// Copies `bytes` into the arena.
    pub fn new(bytes: &[u8; KEY_LEN]) -> Self {
        if let Some(mut p) = arena().take() {
            // SAFETY: `p` came off the free list, so this `Secret` owns the
            // slot exclusively and it is mapped read-write.
            unsafe { p.0.as_mut() }.copy_from_slice(bytes);
            return Self {
                slot: Slot::Arena(p),
            };
        }
        arena().heap_fallbacks.fetch_add(1, Ordering::Relaxed);
        Self {
            slot: Slot::Heap(Box::new(*bytes)),
        }
    }

    /// The secret bytes, for the code that uses them.
    pub(crate) fn expose(&self) -> &[u8; KEY_LEN] {
        match &self.slot {
            // SAFETY: the slot is owned by `self`, mapped for the life of the
            // process and not written while `self` is shared.
            Slot::Arena(p) => unsafe { p.0.as_ref() },
            Slot::Heap(b) => b,
        }
    }

    /// True when the secret lives in the arena rather than a heap box.
    #[cfg(feature = "test-rng")]
    pub fn is_arena_backed(&self) -> bool {
        matches!(self.slot, Slot::Arena(_))
    }

    /// The address of the slot, so a test can look at it after the drop. The
    /// mapping is never unmapped, which is what makes that read defined.
    #[cfg(feature = "test-rng")]
    pub fn slot_address(&self) -> *const u8 {
        self.expose().as_ptr()
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        match &mut self.slot {
            Slot::Arena(p) => {
                // SAFETY: owned slot, see `new`; zeroed before it can be reused.
                unsafe { p.0.as_mut() }.zeroize();
                arena().give(*p);
            }
            Slot::Heap(b) => b.zeroize(),
        }
    }
}
