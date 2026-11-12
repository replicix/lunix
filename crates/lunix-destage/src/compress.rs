//! Compression policy (ADR-12): `none`, `lz4`, `zstd:N` and `auto`, with the
//! two degrade steps of backpressure.
//!
//! The decision table, by policy and pressure (one level per cut):
//!
//! | policy | no pressure | `Lz4` pressure | `Raw` pressure |
//! |---|---|---|---|
//! | `none` | raw | raw | raw |
//! | `lz4` | LZ4 | LZ4 | raw |
//! | `zstd:N` | zstd N | LZ4 | raw |
//! | `auto` | probe, then zstd | LZ4 | raw |
//!
//! `auto` compresses the first `probe_len` bytes with LZ4. If that saves less
//! than `probe_min_saving_percent` of the probe (`saved * 100 < percent *
//! probe_len`) the chunk is stored raw without ever calling zstd; the probe
//! looks only at the head, so a chunk with an incompressible head and a
//! compressible tail is stored raw by design. Otherwise the chunk goes
//! through zstd at the policy level, held in a per-worker context.
//!
//! Whatever codec ran, an output that is not smaller than the input is
//! replaced by the raw chunk, so `clen < ulen` for every compressed extent and
//! `clen == ulen` for raw ones.

use lunix_format::volume::CodecPolicy;
use lunix_format::{Codec, Level};
use lunix_iface::{Error, Result};

use crate::stats::DestageStats;

/// How hard destage is pressed: the compression it may afford (ADR-12).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub enum Pressure {
    /// Normal operation: the policy's codec.
    #[default]
    None,
    /// Use LZ4 whatever the policy.
    Lz4,
    /// Store raw.
    Raw,
}

/// What the policy decided for one chunk.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Choice {
    /// The codec of the extent.
    pub codec: Codec,
    /// The level to write in the header.
    pub level: Level,
}

const RAW: Choice = Choice {
    codec: Codec::Raw,
    level: Level(0),
};

/// One worker's compression state.
pub(crate) struct Compressor {
    policy: CodecPolicy,
    level: i8,
    probe_len: usize,
    min_saving_percent: u32,
    zstd: Option<zstd::bulk::Compressor<'static>>,
}

fn lz4_into(src: &[u8], out: &mut Vec<u8>) -> Result<usize> {
    out.resize(lz4_flex::block::get_maximum_output_size(src.len()), 0);
    let n = lz4_flex::block::compress_into(src, out)
        .map_err(|_| Error::invalid("lz4 output buffer too small"))?;
    out.truncate(n);
    Ok(n)
}

impl Compressor {
    pub(crate) fn new(
        policy: CodecPolicy,
        level: i8,
        probe_len: usize,
        min_saving_percent: u32,
    ) -> Self {
        Self {
            policy,
            level,
            probe_len,
            min_saving_percent,
            zstd: None,
        }
    }

    /// Compresses `data` into `out` (cleared first) unless the choice is raw,
    /// in which case `out` is left empty and the caller stores `data` itself.
    pub(crate) fn compress(
        &mut self,
        data: &[u8],
        pressure: Pressure,
        out: &mut Vec<u8>,
        stats: &mut DestageStats,
    ) -> Result<Choice> {
        out.clear();
        let effective = match (self.policy, pressure) {
            (CodecPolicy::None, _) | (_, Pressure::Raw) => Step::Raw,
            (CodecPolicy::Lz4, _) | (_, Pressure::Lz4) => Step::Lz4,
            (CodecPolicy::Auto, Pressure::None) => Step::Auto,
            (CodecPolicy::Zstd, Pressure::None) => Step::Zstd,
        };
        // Pressure is reported only where it changed the outcome.
        match (self.policy, pressure) {
            (CodecPolicy::None, _) => {}
            (_, Pressure::Raw) => stats.degraded_raw += 1,
            (CodecPolicy::Zstd | CodecPolicy::Auto, Pressure::Lz4) => stats.degraded_lz4 += 1,
            _ => {}
        }
        match effective {
            Step::Raw => Ok(RAW),
            Step::Lz4 => self.lz4(data, out, stats),
            Step::Zstd => self.zstd(data, out, stats),
            Step::Auto => {
                let probe = &data[..self.probe_len.min(data.len())];
                stats.probe_calls += 1;
                let n = lz4_into(probe, out)?;
                out.clear();
                let saved = probe.len().saturating_sub(n) as u128;
                if saved * 100 < u128::from(self.min_saving_percent) * probe.len() as u128 {
                    stats.probe_raw += 1;
                    return Ok(RAW);
                }
                self.zstd(data, out, stats)
            }
        }
    }

    fn lz4(&mut self, data: &[u8], out: &mut Vec<u8>, stats: &mut DestageStats) -> Result<Choice> {
        stats.lz4_calls += 1;
        let n = lz4_into(data, out)?;
        if n >= data.len() {
            out.clear();
            return Ok(RAW);
        }
        Ok(Choice {
            codec: Codec::Lz4,
            level: Level(0),
        })
    }

    fn zstd(&mut self, data: &[u8], out: &mut Vec<u8>, stats: &mut DestageStats) -> Result<Choice> {
        stats.zstd_calls += 1;
        if self.zstd.is_none() {
            self.zstd = Some(zstd::bulk::Compressor::new(i32::from(self.level))?);
        }
        let Some(ctx) = self.zstd.as_mut() else {
            return Err(Error::invalid("zstd context missing"));
        };
        out.resize(zstd::zstd_safe::compress_bound(data.len()), 0);
        let n = ctx.compress_to_buffer(data, &mut out[..])?;
        if n >= data.len() {
            out.clear();
            return Ok(RAW);
        }
        out.truncate(n);
        Ok(Choice {
            codec: Codec::Zstd,
            level: Level(self.level),
        })
    }
}

#[derive(Clone, Copy)]
enum Step {
    Raw,
    Lz4,
    Zstd,
    Auto,
}
