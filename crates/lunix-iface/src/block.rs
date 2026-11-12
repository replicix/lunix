//! Block device <-> engine (`lunix-ublk` calls, `lunix-engine` implements).
//!
//! The ublk layer owns tags, buffers and the io_uring; the engine owns I/O
//! semantics. One [`QueueHandler`] exists per queue and lives on that queue's
//! thread for the life of the device.
//!
//! # Threading rule
//!
//! libublk 0.4.8 commits a request only from the queue thread. Therefore
//! [`Completer::complete`] is called **only** from [`QueueHandler::on_request`]
//! or [`QueueHandler::poll`], on the queue's own thread. Workers (journal
//! completions, read helpers, space waiters) never complete a tag; they
//! signal the queue's [`WakeFd`], and `poll` completes whatever finished.
//! `poll` is the only path for deferred completions.
//!
//! # Blocking class
//!
//! Everything on [`QueueHandler`] is **non-blocking**: no lock held across I/O,
//! no wait, no tokio (ADR-18). [`BlockEngine`] methods are control-plane and
//! may block.
//!
//! # Buffers
//!
//! `buf` in `on_request` is the tag's I/O buffer: `req.len` bytes for READ and
//! WRITE (the engine fills it for READ, reads it for WRITE) and empty for the
//! other ops. For a deferred READ the data is copied into
//! [`Completer::tag_buf`] from `poll`; the buffer is valid from `on_request`
//! until the tag is completed, and workers fill their own memory, never the
//! tag buffer.

use std::os::fd::BorrowedFd;
use std::sync::Arc;

use crate::error::Result;
use crate::stats::EngineStats;

/// A ublk request tag, unique per queue while the request is outstanding.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Tag(pub u16);

/// A queue number, `0..nr_queues`; also the journal lane number.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct QueueId(pub u16);

/// The operation of a request.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum IoOp {
    /// Read `len` bytes at `offset_bytes`.
    Read,
    /// Write `len` bytes at `offset_bytes`.
    Write,
    /// Make every earlier completed write durable.
    Flush,
    /// Deallocate the range (reads as zeros afterwards).
    Discard,
    /// Zero the range.
    WriteZeroes,
}

/// A request as the ublk layer hands it over.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct IoRequest {
    /// The tag to complete.
    pub tag: Tag,
    /// The operation.
    pub op: IoOp,
    /// Force unit access: durable before completion (WRITE only).
    pub fua: bool,
    /// Byte offset on the device. The engine rejects values that are not
    /// multiples of 4096 or that reach past the size with `EINVAL`.
    pub offset_bytes: u64,
    /// Length in bytes (0 for FLUSH). A single DISCARD or WRITE_ZEROES is
    /// capped at `i32::MAX` bytes: the ublk layer sets `max_discard_sectors`
    /// and `max_write_zeroes_sectors` to at most `i32::MAX / 512`.
    pub len: u32,
}

/// A completion result in the negative-errno convention: `>= 0` is the byte
/// count transferred, `< 0` is `-errno`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct IoStatus(i32);

impl IoStatus {
    /// Success with `bytes` transferred (FLUSH, DISCARD: the request length).
    pub fn ok(bytes: u32) -> Self {
        Self(i32::try_from(bytes).unwrap_or(i32::MAX))
    }

    /// Failure with a positive `errno`.
    pub fn err(errno: i32) -> Self {
        Self(-errno.abs().max(1))
    }

    /// The raw value for `UBLK_IO_RES`.
    pub fn raw(self) -> i32 {
        self.0
    }

    /// True for success.
    pub fn is_ok(self) -> bool {
        self.0 >= 0
    }

    /// The positive errno, if this is a failure.
    pub fn errno(self) -> Option<i32> {
        (self.0 < 0).then_some(-self.0)
    }
}

/// Completes tags on the queue thread. Implemented by the ublk layer.
pub trait Completer {
    /// Completes `tag`. Thread: the queue thread, from `on_request` or `poll`
    /// only. Completing a tag twice or an unknown tag is a bug the ublk layer
    /// may turn into an abort.
    fn complete(&mut self, tag: Tag, status: IoStatus);

    /// The I/O buffer of an outstanding `tag`, `max_io_buf_bytes` long.
    fn tag_buf(&mut self, tag: Tag) -> &mut [u8];
}

/// What the engine needs to know to build one queue's handler.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct QueueConfig {
    /// This queue (and journal lane).
    pub queue: QueueId,
    /// Total queues; the journal has this many lanes.
    pub nr_queues: u16,
    /// Requests in flight per queue.
    pub depth: u16,
    /// Size of each tag buffer (D14).
    pub max_io_buf_bytes: u32,
}

/// One queue's request handler. Built on, and never leaves, the queue thread.
pub trait QueueHandler {
    /// Handles one request. Non-blocking. Either completes it through `done`
    /// before returning or keeps the tag and completes it from a later `poll`.
    /// A request with a bad offset or length is completed with `EINVAL`
    /// here, never panicked on.
    fn on_request(&mut self, req: IoRequest, buf: &mut [u8], done: &mut dyn Completer);

    /// Completes everything that has finished since the last call. Called
    /// after the wake fd fires and after any ring event the layer chooses.
    /// Non-blocking; must drain all ready completions (signals coalesce).
    fn poll(&mut self, done: &mut dyn Completer);

    /// The eventfd the layer waits on for this queue. Stable for the handler's
    /// life. Workers signal it; `poll` drains it.
    fn wake_fd(&self) -> BorrowedFd<'_>;
}

/// Told after a grow is durable, so the ublk layer can issue `UPDATE_SIZE`.
pub trait ResizeHook: Send + Sync {
    /// Called once per grow, after the FULL commit with the new size is
    /// durable. May block. An error is reported to the caller of `grow`.
    fn resized(&self, new_size_bytes: u64) -> Result<()>;
}

/// The engine as the ublk layer and the daemon see it. `Send + Sync`.
pub trait BlockEngine: Send + Sync {
    /// The current device size in bytes (a multiple of 4096). Non-blocking.
    fn size_bytes(&self) -> u64;

    /// Builds the handler of queue `cfg.queue`. Call once per queue, on that
    /// queue's thread, before it serves I/O. Control plane; may block (it
    /// opens the journal lane).
    ///
    /// # Errors
    /// `Invalid` for a bad queue number or a second handler for one queue;
    /// `Io` when the lane cannot be opened.
    fn queue_handler(&self, cfg: &QueueConfig) -> Result<Box<dyn QueueHandler>>;

    /// Grows the device (shrink and unaligned sizes are `Unsupported`/`Invalid`).
    /// Blocks until the new size is committed, then calls the resize hook.
    /// Thread: a control thread, never a queue thread.
    fn grow(&self, new_size_bytes: u64) -> Result<()>;

    /// Installs the hook called by `grow` (replaces any earlier one).
    fn set_on_resize(&self, hook: Arc<dyn ResizeHook>);

    /// Called after the ublk layer stopped delivering requests (detach, or
    /// before a quiesce handover): performs the final cut and commit. Dropping
    /// the engine afterwards loses nothing. Idempotent. Control thread, may
    /// block. There is no `detach` here: stopping and deleting the device
    /// belongs to the ublk layer (W1, W2).
    ///
    /// # Errors
    /// Whatever the final commit reports.
    fn quiesce(&self) -> Result<()> {
        Ok(())
    }

    /// A snapshot of the counters. Non-blocking.
    fn stats(&self) -> EngineStats;
}
