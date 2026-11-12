//! Clock and random-number traits so that time and chance are injectable.
//!
//! Anything that waits, expires or timestamps (the space gate's waiters, GC's
//! grace, the commit cadence, `created_at`) reads a [`Clock`]; tests drive a
//! [`ManualClock`]. [`Rng`] is for tests and non-secret choices only: it must
//! never produce a salt, nonce or key (ADR-11; the seal functions draw their
//! own salt from the OS).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use lunix_format::TimeMs;

/// A source of wall-clock and monotonic time. Thread: any. Never blocks.
pub trait Clock: Send + Sync {
    /// Wall-clock time in milliseconds since the Unix epoch (for `created_at`).
    fn now(&self) -> TimeMs;
    /// Monotonic time since an arbitrary origin (for deadlines and grace).
    fn monotonic(&self) -> Duration;
}

/// The real clock.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> TimeMs {
        let ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        TimeMs(u64::try_from(ms).unwrap_or(u64::MAX))
    }

    fn monotonic(&self) -> Duration {
        static START: OnceLock<Instant> = OnceLock::new();
        START.get_or_init(Instant::now).elapsed()
    }
}

/// A clock that only moves when told to; wall and monotonic advance together.
#[derive(Debug)]
pub struct ManualClock {
    wall_ms: AtomicU64,
    mono_ns: AtomicU64,
}

impl ManualClock {
    /// A clock showing `wall` and monotonic zero.
    pub fn new(wall: TimeMs) -> Self {
        Self {
            wall_ms: AtomicU64::new(wall.0),
            mono_ns: AtomicU64::new(0),
        }
    }

    /// Moves both clocks forward.
    pub fn advance(&self, by: Duration) {
        let ms = u64::try_from(by.as_millis()).unwrap_or(u64::MAX);
        let ns = u64::try_from(by.as_nanos()).unwrap_or(u64::MAX);
        self.wall_ms.fetch_add(ms, Ordering::SeqCst);
        self.mono_ns.fetch_add(ns, Ordering::SeqCst);
    }

    /// Sets the wall clock only (to model a clock step backwards, D17).
    pub fn set_wall(&self, wall: TimeMs) {
        self.wall_ms.store(wall.0, Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn now(&self) -> TimeMs {
        TimeMs(self.wall_ms.load(Ordering::SeqCst))
    }

    fn monotonic(&self) -> Duration {
        Duration::from_nanos(self.mono_ns.load(Ordering::SeqCst))
    }
}

/// A source of non-secret random bytes. Thread: any.
pub trait Rng: Send + Sync {
    /// Fills `buf`.
    fn fill(&self, buf: &mut [u8]);

    /// The next 64 random bits.
    fn next_u64(&self) -> u64 {
        let mut b = [0u8; 8];
        self.fill(&mut b);
        u64::from_le_bytes(b)
    }
}

/// A deterministic generator (splitmix64) for reproducible tests.
#[derive(Debug)]
pub struct SeededRng(Mutex<u64>);

impl SeededRng {
    /// A generator with the given seed.
    pub fn new(seed: u64) -> Self {
        Self(Mutex::new(seed))
    }
}

impl Rng for SeededRng {
    fn fill(&self, buf: &mut [u8]) {
        let mut state = self.0.lock().unwrap_or_else(|p| p.into_inner());
        for chunk in buf.chunks_mut(8) {
            *state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut z = *state;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            z ^= z >> 31;
            let bytes = z.to_le_bytes();
            chunk.copy_from_slice(&bytes[..chunk.len()]);
        }
    }
}
