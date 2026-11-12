//! Journal configuration and the test trace hook.

use std::os::fd::RawFd;
use std::path::PathBuf;
use std::sync::Arc;

use lunix_format::journal::{BLOCK, FILE_LEN, MAX_LANES, MAX_WRITE_BLOCKS};
use lunix_format::VolumeUuid;
use lunix_iface::{Error, Result};

/// Smallest `journal_max` a production geometry accepts (DESIGN section 4.1).
pub const MIN_JOURNAL_MAX: u64 = 256 << 20;
/// Default `journal_max`.
pub const DEFAULT_JOURNAL_MAX: u64 = 2 << 30;

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
        if self.pool_buffers < 2 || self.pool_buffers > 1024 {
            return Err(Error::invalid("pool_buffers must be 2..=1024"));
        }
        if self.max_inflight == 0 {
            return Err(Error::invalid("max_inflight must be at least 1"));
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
            .finish_non_exhaustive()
    }
}
