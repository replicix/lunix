//! The eventfd that workers signal to wake a queue thread.
//!
//! A queue thread never blocks on anything but its ublk ring; everything that
//! completes elsewhere (journal writes, read helpers, space waiters) signals
//! the queue's [`WakeFd`], the ublk layer sees it readable, and the thread
//! calls `QueueHandler::poll`. Signals coalesce: one wake may cover many
//! completions, so `poll` always drains everything.

use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd, RawFd};
use std::sync::Arc;

use rustix::event::{eventfd, EventfdFlags};

/// A non-blocking, close-on-exec eventfd, cheap to clone and share.
#[derive(Clone, Debug)]
pub struct WakeFd(Arc<OwnedFd>);

impl WakeFd {
    /// Creates a new eventfd with a zero counter.
    ///
    /// # Errors
    /// The `eventfd(2)` error.
    pub fn new() -> io::Result<Self> {
        let fd = eventfd(0, EventfdFlags::NONBLOCK | EventfdFlags::CLOEXEC)?;
        Ok(Self(Arc::new(fd)))
    }

    /// Makes the fd readable. Never blocks; an overflowing counter is already
    /// readable, so the error is ignored. Thread: any.
    pub fn signal(&self) {
        let _ = rustix::io::write(&*self.0, &1u64.to_ne_bytes());
    }

    /// Resets the counter and returns how many signals it held (0 if none).
    /// Thread: the waiting thread. Never blocks.
    pub fn drain(&self) -> u64 {
        let mut b = [0u8; 8];
        match rustix::io::read(&*self.0, &mut b) {
            Ok(8) => u64::from_ne_bytes(b),
            _ => 0,
        }
    }

    /// The raw fd, valid while any clone of this `WakeFd` lives (for
    /// `IORING_REGISTER_EVENTFD`).
    pub fn raw_fd(&self) -> RawFd {
        self.0.as_raw_fd()
    }
}

impl AsFd for WakeFd {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}
