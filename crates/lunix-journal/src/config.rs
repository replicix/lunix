//! Journal configuration and the test trace hook.

use std::os::fd::RawFd;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use lunix_format::journal::{BLOCK, FILE_LEN, MAX_LANES, MAX_WRITE_BLOCKS};
use lunix_format::VolumeUuid;
use lunix_iface::{Error, Result};

/// Smallest `journal_max` a production geometry accepts (DESIGN section 4.1).
pub const MIN_JOURNAL_MAX: u64 = 256 << 20;
/// Default `journal_max`.
pub const DEFAULT_JOURNAL_MAX: u64 = 2 << 30;
/// Default [`JournalConfig::mark_idle`].
pub const DEFAULT_MARK_IDLE: Duration = Duration::from_millis(50);

/// Bytes of one lane batch buffer: the largest record (64-byte header plus
/// 256 blocks) rounded up to a block.
pub(crate) const BATCH_CAP: usize = BLOCK + MAX_WRITE_BLOCKS as usize * BLOCK;

/// Something the journal did, for tests (alignment, group flush ordering).
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum TraceEvent {
    /// A batch write was queued.
    Write {
        /// The lane.
        lane: u16,
        /// The ring file's descriptor.
        fd: RawFd,
        /// File offset.
        offset: u64,
        /// Length in bytes.
        len: usize,
        /// Buffer address.
        addr: usize,
    },
    /// A flush group formed and its watermark wait finished.
    GroupStart {
        /// The group's `upto`.
        upto: u64,
    },
    /// A lane was opened.
    LaneOpened {
        /// The lane.
        lane: u16,
        /// True when its batch buffers are registered (`WriteFixed`).
        fixed: bool,
    },
    /// An `fdatasync` was issued for one ring file.
    Fdatasync {
        /// The lane.
        lane: u16,
        /// The ring file's descriptor.
        fd: RawFd,
        /// The group's `upto`.
        upto: u64,
        /// The lowest `write_seq` still reserved or appending on the lane at
        /// that moment (must be above `upto`).
        lane_pending_min: Option<u64>,
    },
    /// The group finished (all `fdatasync`s done).
    GroupDone {
        /// The group's `upto`.
        upto: u64,
        /// False when a sync failed.
        ok: bool,
    },
    /// A mark (a witness of the flushed watermark) is durable in the mark
    /// ring.
    Mark {
        /// The mark's `write_seq`.
        write_seq: u64,
        /// The watermark it carries.
        flushed: u64,
    },
}

/// The trace callback type.
pub type TraceFn = Arc<dyn Fn(&TraceEvent) + Send + Sync>;

/// How a [`crate::FileJournal`] is laid out and tuned.
#[derive(Clone)]
pub struct JournalConfig {
    /// The `journal/` directory of the volume.
    pub dir: PathBuf,
    /// The volume uuid written into every file header.
    pub volume_uuid: VolumeUuid,
    /// Lanes, one per ublk queue.
    pub lanes: u16,
    /// Ring size for the whole volume, split evenly across lanes.
    pub journal_max: u64,
    /// Length of one ring file. The format fixes 64 MiB; smaller values exist
    /// so tests can wrap a ring cheaply. Must be a multiple of 4096 and at
    /// least one block plus one maximal record.
    pub file_len: u64,
    /// Aligned batch buffers per lane (at least 2). Running out is `Full`.
    pub pool_buffers: usize,
    /// Batches a lane keeps in flight before it lets records accumulate in
    /// the open batch (at least 1).
    pub max_inflight: usize,
    /// Open ring files with `O_DIRECT` (always on in production; off only for
    /// a test file system that refuses it).
    pub direct: bool,
    /// Fixed (registered) batch buffers: `None` registers them when
    /// `RLIMIT_MEMLOCK` allows and otherwise writes from plain buffers;
    /// `Some(true)` makes `open_lane` fail if they cannot be registered;
    /// `Some(false)` never registers. Registering needs about
    /// `lanes x pool_buffers x (1 MiB + 4 KiB)` of locked memory.
    pub fixed_buffers: Option<bool>,
    /// How long the flush coordinator waits after a group flush that raised
    /// the flushed watermark before it writes a mark witnessing it, when no
    /// later group flush made a witness durable in the meantime (on-disk-
    /// formats section 5.3). `None` writes no marks.
    pub mark_idle: Option<Duration>,
    /// Lock the journal's I/O memory (DESIGN §3, W7): every lane's batch
    /// buffers (populated), the zero-fill buffer, the flush thread's stack
    /// and its mark buffer. A failed lock fails `open` or `open_lane`. The
    /// daemon sets it; tests under a small `RLIMIT_MEMLOCK` leave it off.
    pub lock_memory: bool,
    /// Cap on the resident journal window (ADR-33): the bytes the journal
    /// keeps in memory of what it wrote, until the ring file is reclaimed,
    /// plus the records a read-around made resident. `None` is
    /// `journal_max`; a larger value is clamped to it. The memory is not
    /// locked. At least one maximal batch. Below the backlog a cut reads, a
    /// miss costs a 1 MiB read and check per window that was not made
    /// resident (see the `window` module): leave it at the default unless
    /// memory is tight.
    pub resident_max: Option<u64>,
    /// Zero a recycled ring file's old records with `fallocate`
    /// (`FALLOC_FL_ZERO_RANGE`) where the file system supports it, instead of
    /// rewriting them with zeros (milestone jstall; the reader rule of
    /// on-disk-formats §5.1 holds either way: the file reads as zeros past its
    /// last write). A file system that refuses it is detected at the first
    /// reclaim and the rewrite is used from then on. `false` always rewrites.
    pub zero_range: bool,
    /// Test hook, see [`TraceEvent`].
    pub trace: Option<TraceFn>,
}

impl JournalConfig {
    /// Production geometry: 64 MiB files, 8 pooled buffers per lane, two
    /// batches in flight.
    pub fn new(dir: PathBuf, volume_uuid: VolumeUuid, lanes: u16, journal_max: u64) -> Self {
        Self {
            dir,
            volume_uuid,
            lanes,
            journal_max,
            file_len: FILE_LEN as u64,
            pool_buffers: 8,
            max_inflight: 2,
            direct: true,
            fixed_buffers: None,
            mark_idle: Some(DEFAULT_MARK_IDLE),
            lock_memory: false,
            resident_max: None,
            zero_range: true,
            trace: None,
        }
    }

    /// Ring files per lane: `journal_max` split across lanes, at least two (a
    /// lane needs a second file to roll into while the first is reclaimed).
    pub fn files_per_lane(&self) -> usize {
        let per_lane = self.journal_max / u64::from(self.lanes.max(1));
        usize::try_from(per_lane / self.file_len.max(1))
            .unwrap_or(usize::MAX)
            .max(2)
    }

    /// The effective cap of the resident window: `resident_max` (default
    /// `journal_max`), never above `journal_max`.
    #[must_use]
    pub fn resident_cap(&self) -> u64 {
        self.resident_max
            .unwrap_or(self.journal_max)
            .min(self.journal_max)
    }

    pub(crate) fn validate(&self) -> Result<()> {
        if self.lanes == 0 || self.lanes > MAX_LANES {
            return Err(Error::invalid("lane count must be 1..=255"));
        }
        let min_file = (BLOCK + BATCH_CAP) as u64;
        if self.file_len < min_file || !self.file_len.is_multiple_of(BLOCK as u64) {
            return Err(Error::invalid(
                "file_len must be a block multiple holding a header and a maximal record",
            ));
        }
        if self.file_len == FILE_LEN as u64 && self.journal_max < MIN_JOURNAL_MAX {
            return Err(Error::invalid("journal_max is below the 256 MiB minimum"));
        }
        let min_max = u64::from(self.lanes) * 2 * self.file_len;
        if self.journal_max < min_max {
            return Err(Error::invalid(
                "journal_max is below lanes x 2 x file_len (a lane needs two files)",
            ));
        }
        if self.pool_buffers < 2 || self.pool_buffers > 1024 {
            return Err(Error::invalid("pool_buffers must be 2..=1024"));
        }
        if self.max_inflight == 0 {
            return Err(Error::invalid("max_inflight must be at least 1"));
        }
        if self.resident_max.is_some_and(|m| m < BATCH_CAP as u64) {
            return Err(Error::invalid(
                "resident_max is below one maximal batch (1 MiB + 4 KiB)",
            ));
        }
        Ok(())
    }

    pub(crate) fn emit(&self, ev: impl FnOnce() -> TraceEvent) {
        if let Some(t) = &self.trace {
            t(&ev());
        }
    }
}

impl std::fmt::Debug for JournalConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JournalConfig")
            .field("dir", &self.dir)
            .field("lanes", &self.lanes)
            .field("journal_max", &self.journal_max)
            .field("file_len", &self.file_len)
            .field("pool_buffers", &self.pool_buffers)
            .field("max_inflight", &self.max_inflight)
            .field("direct", &self.direct)
            .field("mark_idle", &self.mark_idle)
            .field("lock_memory", &self.lock_memory)
            .field("resident_max", &self.resident_max)
            .field("zero_range", &self.zero_range)
            .finish_non_exhaustive()
    }
}
