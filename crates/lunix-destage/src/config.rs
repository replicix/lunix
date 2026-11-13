//! Volume parameters and pool tuning.

use lunix_format::volume::{CodecPolicy, VolumeDescriptor};
use lunix_format::IdLen;
use lunix_iface::{Error, Result};

use crate::compress::Pressure;

/// 4 KiB: the overlay's block size.
pub(crate) const BLOCK: u64 = 4096;

/// What the pipeline needs of the volume descriptor.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DestageParams {
    /// Chunk size in bytes: a power of two from 16 KiB to 1 MiB (ADR-5).
    pub chunk_size: u32,
    /// Length of the id prefix the map stores per leaf entry.
    pub id_len: IdLen,
    /// Compression policy (ADR-12).
    pub policy: CodecPolicy,
    /// Policy level: the zstd level for `auto` and `zstd`, 0 otherwise.
    pub level: i8,
}

impl DestageParams {
    /// Takes the parameters from a verified descriptor.
    pub fn from_descriptor(d: &VolumeDescriptor) -> Self {
        Self {
            chunk_size: d.chunk_size,
            id_len: d.id_len,
            policy: d.codec_policy,
            level: d.codec_level,
        }
    }

    /// Checks the ranges the format promises.
    ///
    /// # Errors
    /// `Invalid` for a chunk size outside 16 KiB ..= 1 MiB or not a power of
    /// two, or a level the policy does not allow.
    pub fn validate(&self) -> Result<()> {
        let ok = self.chunk_size.is_power_of_two()
            && (16 << 10..=1 << 20).contains(&self.chunk_size)
            && self.policy.level_ok(self.level);
        if ok {
            Ok(())
        } else {
            Err(Error::invalid("destage parameters out of range"))
        }
    }

    /// 4 KiB blocks per chunk.
    pub(crate) fn chunk_blocks(&self) -> u64 {
        u64::from(self.chunk_size) / BLOCK
    }
}

/// Pool and policy tuning. The defaults follow DESIGN section 5 and the plan.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DestageConfig {
    /// Worker threads. Default `max(2, cpus / 2)`.
    pub workers: usize,
    /// Most chunks in one job (default 16).
    pub batch_chunks: usize,
    /// Most bytes in one job (default 1 MiB). A job holds
    /// `min(batch_chunks, batch_bytes / chunk_size)` chunks, at least one.
    pub batch_bytes: usize,
    /// How many jobs may be finished and waiting for the in-order packer
    /// before workers hold back (bounds the memory of sealed extents). At
    /// least `workers`. Default `4 * workers`.
    pub result_window: usize,
    /// Backpressure the coordinator asks for (journal fill, dirty bytes, the
    /// ladder's "throttle destage" rung). One level for the whole cut, so the
    /// stored codec never depends on thread timing.
    pub pressure: Pressure,
    /// A cut of more than this many bytes (chunks times chunk size) is
    /// compressed with LZ4 instead of the policy's codec, whatever `pressure`
    /// says (a large backlog is itself pressure). `None` switches it off.
    pub lz4_cut_bytes: Option<u64>,
    /// A cut of more than this many bytes is stored raw. Must not be below
    /// `lz4_cut_bytes`.
    pub raw_cut_bytes: Option<u64>,
    /// Bytes the `auto` probe compresses (default 4096).
    pub probe_len: usize,
    /// The probe must save at least this percentage of its input for `auto`
    /// to go on to zstd (default 6, ADR-12).
    pub probe_min_saving_percent: u32,
}

impl Default for DestageConfig {
    fn default() -> Self {
        let cpus = std::thread::available_parallelism().map_or(2, |n| n.get());
        let workers = (cpus / 2).max(2);
        Self {
            workers,
            batch_chunks: 16,
            batch_bytes: 1 << 20,
            result_window: workers * 4,
            pressure: Pressure::None,
            lz4_cut_bytes: Some(4 << 30),
            raw_cut_bytes: Some(16 << 30),
            probe_len: 4096,
            probe_min_saving_percent: 6,
        }
    }
}

impl DestageConfig {
    /// Checks the tuning.
    ///
    /// # Errors
    /// `Invalid` for a zero worker count or batch, a result window below the worker count, a probe length of
    /// zero or a saving above 100, or `raw_cut_bytes` below `lz4_cut_bytes`.
    pub fn validate(&self) -> Result<()> {
        let depths_ok = match (self.lz4_cut_bytes, self.raw_cut_bytes) {
            (Some(l), Some(r)) => l <= r,
            _ => true,
        };
        let ok = self.workers >= 1
            && self.batch_chunks >= 1
            && self.batch_bytes >= 1
            && self.result_window >= self.workers
            && self.probe_len >= 1
            && self.probe_min_saving_percent <= 100
            && depths_ok;
        if ok {
            Ok(())
        } else {
            Err(Error::invalid("destage configuration out of range"))
        }
    }

    /// Chunks per job for a chunk size.
    pub(crate) fn chunks_per_job(&self, chunk_size: u32) -> usize {
        let by_bytes = self.batch_bytes / chunk_size as usize;
        self.batch_chunks.min(by_bytes).max(1)
    }
}

impl DestageConfig {
    /// The pressure for a cut of `bytes` bytes: the larger of the requested
    /// level and the one the cut's size implies.
    pub(crate) fn pressure_for(&self, bytes: u64) -> Pressure {
        let by_size = if self.raw_cut_bytes.is_some_and(|t| bytes > t) {
            Pressure::Raw
        } else if self.lz4_cut_bytes.is_some_and(|t| bytes > t) {
            Pressure::Lz4
        } else {
            Pressure::None
        };
        self.pressure.max(by_size)
    }
}
