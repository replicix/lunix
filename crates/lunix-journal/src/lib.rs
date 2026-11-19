//! Journal lanes, group commit, recovery scan and reclaim.
//!
//! [`FileJournal`] implements [`lunix_iface::journal::Journal`] over
//! preallocated, zero-filled 64 MiB ring files opened `O_DIRECT`, one ring per
//! ublk queue ("lane"), written through `io_uring`, plus a small mark ring the
//! flush coordinator writes witnesses of the flushed watermark into. The
//! `lunix-iface` journal
//! module documentation is the contract; this crate's own decisions are
//! recorded in the crate README.
//!
//! The journal also keeps what it wrote in memory until the ring file is
//! reclaimed (the *resident window*, ADR-33), serves reads from it, and on a
//! miss reads and checks a whole 1 MiB window of the file; see the `window`
//! module's documentation and [`FileJournal::window_stats`].

#![deny(unsafe_op_in_unsafe_fn)]

mod aligned;
mod config;
mod coord;
mod journal;
mod lane;
mod ring;
mod scan;
mod shared;
mod sys;
mod window;

pub use config::{
    JournalConfig, TraceEvent, TraceFn, DEFAULT_JOURNAL_MAX, DEFAULT_MARK_IDLE, MIN_JOURNAL_MAX,
};
pub use journal::FileJournal;
pub use window::WindowStats;

use lunix_format::local::CheckpointSlot;
use lunix_format::CommitSeq;

/// The checkpoint-tail guard (D17, DESIGN section 4.1): a checkpoint's lane
/// tails may be used as the `hint` of [`lunix_iface::journal::Journal::recover`]
/// only if its `last_commit_seq` is at most the tip's `commit_seq`. A checkpoint
/// written for a commit above the tip (the tip was truncated by
/// `fsck --repair --truncate-tip`) names files whose records the new tip does
/// not cover; it must be ignored and every file scanned.
pub fn usable_hint(
    hint: Option<&CheckpointSlot>,
    tip_commit_seq: CommitSeq,
) -> Option<&CheckpointSlot> {
    hint.filter(|h| h.last_commit_seq <= tip_commit_seq.0)
}
