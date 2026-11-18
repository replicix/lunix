//! The space ledger and the out-of-space ladder (DESIGN section 4.3, ADR-14).
//!
//! [`LunixSpace`] implements [`lunix_iface::space::SpaceGate`]. The
//! accounting walk-through in `lunix_iface::space` is the specification; this
//! crate adds the policy around it.
//!
//! # The ledger
//!
//! `used = JournalReserved + Segments + Cache + Map + Index + Commits`.
//! `Headroom` is a pseudo-kind holding bounds charged ahead of time; it is not
//! in `used`. All units are bytes. `JournalReserved` is charged and released
//! in **encoded record length** (`lunix_iface::journal::record_bytes`: 64 +
//! 4096 per block for a write, 64 for a hole), the same unit as
//! `ReclaimReport::bytes`.
//!
//! One mutex guards the seven counters and the wait queue, so every method is
//! atomic for observers: `transfer` is seen entirely or not at all, and
//! [`snapshot`](lunix_iface::space::SpaceGate::snapshot) is one consistent
//! reading.
//!
//! # The limit
//!
//! The file system is read through an [`FsProbe`] (`statvfs`, `f_bavail`)
//! once per second by the background thread, on demand when a reservation
//! would bring `used` within 5 % of the limit, and from
//! [`LunixSpace::refresh`]. A reading is anchored to the ledger level at the
//! moment it was started:
//!
//! ```text
//! fs_ceiling = used_at_reading + fs_free - free_floor
//! reserve    = 2 * segment_size + settings.headroom
//! limit      = min(cache_max, fs_ceiling - reserve)       (writes)
//! ```
//!
//! Counting the room from `used_at_reading` means bytes the ledger holds but
//! that are not on disk yet are never counted as free space twice, and files
//! already on disk are not counted against the room twice. The `free_floor`
//! is bytes or a percentage of the file system's size. `reserve` is carved
//! out of the file system's room, never out of `cache_max`.
//!
//! * [`reserve`](lunix_iface::space::SpaceGate::reserve) charges when
//!   `used + bytes <= limit`; otherwise `on_full = error` returns `NoSpace`
//!   with no state change and `wait` parks the request FIFO behind a
//!   [`Waiter`](lunix_iface::space::Waiter). Parked requests are charged at
//!   grant time, strictly in order; a request behind a larger one waits.
//! * `reserve_headroom` and `transfer` never park and never consult the
//!   limit. `reserve_headroom` fails with `NoSpace` only when `used` plus the
//!   `Headroom` bounds held plus the request would exceed `limit + reserve`;
//!   the `Headroom` pseudo-kind itself is also capped at `reserve`, so the
//!   objects it pays for can never push `used` more than `reserve` past the
//!   limit.
//!   Writes therefore never take the reserve; commits, map packs and the
//!   checkpoint do.
//! * Reads never call the gate.
//!
//! # The ladder
//!
//! Steps register per [`LadderStepKind`](lunix_iface::space::LadderStepKind)
//! and run on the gate's ladder thread (`lunix-space-ladder`) in the order
//! evict cache, evict synced, compact, throttle destage, until the waiting
//! requests fit. Their `want` is every waiting reservation plus the resume
//! mark, less the headroom. A request that does not fit starts a run (also
//! with `on_full = error`, and on a full volume, for the next write); while
//! requests stay parked the timer thread asks for another run each second
//! (the compact step, GC's urgent pass, backs off by itself: ADR-32). A step
//! that returns an error is skipped and counted ([`LunixSpace::step_errors`]).
//! `lunixd` registers no-ops for three of the four in phase 1 and GC's urgent
//! pass as compact.
//!
//! # The bounded stop (ADR-32)
//!
//! Every parked request has a deadline: its arrival plus `full_timeout`
//! (`space.full_timeout_ms`, or [`SpaceConfig::full_timeout`], 60 s by
//! default, when that is null). The first expiry fails every other request
//! waiting for space and **latches the volume full**: reservations fail at
//! once (`NoSpace`) until the headroom `limit - used` reaches the resume
//! mark (the larger of 4 segments and 1 % of the limit, at most half the
//! limit). `on_full = error` refuses at once and does not latch.
//! [`reconfigure`](lunix_iface::space::SpaceGate::reconfigure) re-judges
//! every parked request under the new policy. [`LunixSpace::begin_close`]
//! (the quiesce rule) fails the space waiters and parks nothing after it.
//!
//! Grants and deadlines never wait for the ladder: with `background_timers`
//! a timer thread (`lunix-space-timer`) takes only the gate's lock and the
//! probe, sleeps until the nearest deadline or the next refresh, and
//! [`tick`](lunix_iface::space::SpaceGate::tick) does the same for tests on a
//! manual clock.
//!
//! # Read-only
//!
//! `fatal_io` (or an `EIO` from the probe) makes `reserve` fail with
//! `ReadOnly` and expires every parked request.

mod gate;
mod ledger;
mod policy;
mod probe;

pub use gate::{LunixSpace, SpaceConfig, DEFAULT_FULL_TIMEOUT, REFRESH_INTERVAL};
pub use probe::{FsProbe, FsStat, ManualFs, SimFs, StatvfsProbe};
