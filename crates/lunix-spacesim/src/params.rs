//! The scenario: workload, volume, journal, collector and device parameters,
//! the presets of the measured cells, and the flat TOML parameter files.

use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};

/// How the workload picks the blocks it overwrites.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Pattern {
    /// Uniform random over the working set (fio `randwrite`).
    Random,
    /// Cyclic sequential over the working set (fio `write` looping).
    Sequential,
}

/// The device the journal ring, the store and the collector write to.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Device {
    /// Name of the profile (for the report).
    pub name: &'static str,
    /// What the write path can append, in writes per second (the queue
    /// threads' ceiling on a RAM-backed directory; 0 means unbounded).
    pub engine_iops: f64,
    /// The journal ring's append bandwidth, bytes per second (0 = unbounded).
    pub journal_bw: f64,
    /// How fast a cut turns bytes into stored segments (hash, compress,
    /// seal), stored bytes per second of cut time.
    pub store_bw: f64,
    /// How fast compaction moves live bytes, per second of collector time.
    pub gc_bw: f64,
    /// One bandwidth shared by the journal appends, the cuts and the
    /// collector (a network volume); 0 = no sharing.
    pub shared_bw: f64,
    /// The fixed cost of a cut (C1 to C9 without the data), milliseconds.
    pub cut_fixed_ms: f64,
    /// Relative jitter of the service times, drawn from the seed.
    pub jitter: f64,
}

impl Device {
    /// A RAM-backed directory (tmpfs): CPU-bound, no device limit.
    pub fn ram() -> Self {
        Self {
            name: "ram",
            engine_iops: 110_000.0,
            journal_bw: 0.0,
            store_bw: 1.9e9,
            gc_bw: 0.8e9,
            shared_bw: 0.0,
            cut_fixed_ms: 12.0,
            jitter: 0.15,
        }
    }

    /// The measured network volume: one device of about 70 MB/s sequential
    /// write for everything (raw 1 MiB write of the T6 baseline).
    pub fn netvol() -> Self {
        Self {
            name: "netvol",
            engine_iops: 110_000.0,
            journal_bw: 0.0,
            store_bw: 1.9e9,
            gc_bw: 25e6,
            shared_bw: 70e6,
            cut_fixed_ms: 600.0,
            jitter: 0.15,
        }
    }
}

/// Everything a run needs. Defaults are the RAM 64 KiB w4k cell.
#[derive(Clone, Debug)]
pub struct Scenario {
    /// Name of the preset this came from.
    pub name: String,
    // --- workload ---
    /// Outstanding requests in total (fio iodepth x jobs).
    pub in_flight: u32,
    /// Writes simulated as one gate request (see the report: the gate sees
    /// bytes, so a request of `agg` blocks is one reservation).
    pub agg: u32,
    /// Block size of a write, bytes.
    pub block: u64,
    /// The overwrite pattern.
    pub pattern: Pattern,
    /// Working set, bytes.
    pub ws_bytes: u64,
    // --- run ---
    /// Number of cells (fio runs) back to back on one attached volume.
    pub cells: u32,
    /// Length of a cell, seconds.
    pub cell_secs: f64,
    /// Idle time between two cells, seconds.
    pub gap_secs: f64,
    /// Time step, milliseconds.
    pub step_ms: f64,
    /// The seed of the jitter.
    pub seed: u64,
    /// Seconds from the end of the collector's last (empty) pass to the
    /// start of the overwrite: the phase of its 30 s idle cadence. None: drawn
    /// from the seed.
    pub gc_phase_s: Option<f64>,
    // --- volume and space ---
    /// The volume's chunk size, bytes.
    pub chunk: u64,
    /// Stored bytes per chunk byte (the inverse of the compression ratio).
    pub stored_ratio: f64,
    /// Size of the host file system, bytes.
    pub fs_total: u64,
    /// Bytes of the file system used by others (ring files, scratch).
    pub fs_external: u64,
    /// Reclaimed ring files are zero-rewritten and stay allocated (a file
    /// system without `FALLOC_FL_ZERO_RANGE`, tmpfs): the space they hold
    /// is not in the ledger and lowers the limit.
    pub zero_alloc: bool,
    /// The volume's cap (`cache_max`), bytes; none = the file system decides.
    pub cache_max: Option<u64>,
    /// The reserved headroom region, bytes.
    pub headroom: u64,
    /// Segment size, bytes.
    pub segment: u64,
    /// The gate's `full_timeout`, seconds.
    pub full_timeout_s: f64,
    // --- journal and cuts ---
    /// `journal_max`, bytes (the fill ratio's denominator).
    pub journal_max: u64,
    /// Record bytes the ring holds before a write parks.
    pub ring_park: u64,
    /// One ring file, bytes.
    pub ring_file: u64,
    /// `cut_target`, seconds.
    pub cut_target_s: f64,
    /// The commit interval `T`, seconds.
    pub commit_interval_s: f64,
    /// A cut starts when the ring holds at least this share of `ring_park`.
    pub cut_trigger: f64,
    /// Bytes of map, commit and index objects a cut adds.
    pub meta_per_cut: u64,
    // --- collector ---
    /// `gc_rate`, bytes per second.
    pub gc_rate: u64,
    /// `gc_grace_ms`, seconds.
    pub gc_grace_s: f64,
    /// `read_timeout`, seconds.
    pub read_timeout_s: f64,
    /// `gc_live_threshold`.
    pub live_threshold: f64,
    /// `compact_per_pass`, live bytes.
    pub compact_per_pass: u64,
    /// `GcConfig::busy_interval`, seconds.
    pub busy_interval_s: f64,
    /// `GcConfig::pass_interval`, seconds.
    pub pass_interval_s: f64,
    /// The mark's fixed cost, seconds.
    pub mark_s: f64,
    /// The mark's cost per chunk of the working set, microseconds
    /// (gcrate: 0.2-0.65 s per 65 000 chunks).
    pub mark_per_chunk_us: f64,
    /// Index work to condemn one segment, milliseconds (gcrate: 5.5).
    pub condemn_ms: f64,
    /// Index work to delete one segment, milliseconds (gcrate: 13).
    pub delete_ms: f64,
    /// FULL commits that keep a dead segment out of reach (the retained
    /// set is the tip, its predecessor and one more: 3).
    pub retain_cuts: u64,
    /// G1: the per-object index work is done once per pass (a tenth).
    pub batch_index: bool,
    /// G3: the mark is incremental (a tenth of its cost per pass).
    pub incremental_mark: bool,
    /// G4: mark and copy run on `gc_workers` threads off the coordinator.
    pub gc_workers: u32,
    /// `gc_step_budget_ms`.
    pub step_budget_ms: u64,
    /// `gc_pressure_rate_factor`.
    pub rate_factor: u64,
    // --- gate ---
    /// Throttle on or off.
    pub throttle: bool,
    /// `throttle_soft_percent`.
    pub soft_percent: u8,
    /// `throttle_soft_min_bytes`.
    pub soft_min: u64,
    /// `throttle_soft_max_bytes`.
    pub soft_max: u64,
    /// `throttle_min_rate`.
    pub min_rate: u64,
    /// `throttle_max_delay_ms`, seconds.
    pub max_delay_s: f64,
    // --- device ---
    /// The device profile.
    pub device: Device,
}

impl Default for Scenario {
    fn default() -> Self {
        Self::ram_64k()
    }
}

const GIB: u64 = 1 << 30;
const MIB: u64 = 1 << 20;

/// The presets, by name.
pub const SCENARIO_NAMES: &[&str] = &["ram-64k", "ram-16k", "netvol-64k", "netvol-16k", "m15-8g"];

impl Scenario {
    /// A preset by name.
    ///
    /// # Errors
    /// Unknown name.
    pub fn preset(name: &str) -> Result<Self> {
        Ok(match name {
            "ram-64k" => Self::ram_64k(),
            "ram-16k" => Self::ram_16k(),
            "netvol-64k" => Self::netvol(65_536),
            "netvol-16k" => Self::netvol(16_384),
            "m15-8g" => Self::m15_8g(),
            _ => bail!(
                "unknown scenario {name:?}; known: {}",
                SCENARIO_NAMES.join(", ")
            ),
        })
    }

    /// T6 RAM cell: tmpfs 20 GiB, 16 GiB volume, 2 GiB working set, 64 KiB
    /// chunks, 128 x 8 4 KiB random writes, three 60 s cells.
    pub fn ram_64k() -> Self {
        Self {
            name: "ram-64k".into(),
            in_flight: 1024,
            agg: 16,
            block: 4096,
            pattern: Pattern::Random,
            ws_bytes: 2 * GIB,
            cells: 3,
            cell_secs: 60.0,
            gap_secs: 5.0,
            step_ms: 10.0,
            seed: 1,
            gc_phase_s: Some(5.0),
            chunk: 65_536,
            stored_ratio: 0.506,
            fs_total: 20 * GIB,
            fs_external: 640 * MIB,
            zero_alloc: true,
            cache_max: Some(32 * GIB),
            headroom: 256 * MIB,
            segment: 16 * MIB,
            full_timeout_s: 60.0,
            journal_max: GIB,
            ring_park: 384 * MIB,
            ring_file: 32 * MIB,
            cut_target_s: 1.0,
            commit_interval_s: 30.0,
            cut_trigger: 1.0,
            meta_per_cut: MIB,
            gc_rate: 64 * MIB,
            gc_grace_s: 60.0,
            read_timeout_s: 10.0,
            live_threshold: 0.5,
            compact_per_pass: 256 * MIB,
            busy_interval_s: 5.0,
            pass_interval_s: 30.0,
            mark_s: 0.05,
            mark_per_chunk_us: 6.0,
            condemn_ms: 5.5,
            delete_ms: 13.0,
            retain_cuts: 3,
            batch_index: false,
            incremental_mark: false,
            gc_workers: 0,
            step_budget_ms: 50,
            rate_factor: 4,
            throttle: true,
            soft_percent: 15,
            soft_min: GIB,
            soft_max: 64 * GIB,
            min_rate: 4 * MIB,
            max_delay_s: 10.0,
            device: Device::ram(),
        }
    }

    /// The same with 16 KiB chunks.
    pub fn ram_16k() -> Self {
        Self {
            name: "ram-16k".into(),
            chunk: 16_384,
            ..Self::ram_64k()
        }
    }

    /// T6 network-volume cell: 32 GiB volume, 4 GiB working set, the
    /// default cap of 200 % of the size, a large host file system.
    pub fn netvol(chunk: u64) -> Self {
        Self {
            name: format!("netvol-{}k", chunk / 1024),
            ws_bytes: 4 * GIB,
            chunk,
            fs_total: 400 * GIB,
            fs_external: 4 * GIB,
            zero_alloc: false,
            cache_max: Some(64 * GIB),
            device: Device::netvol(),
            ..Self::ram_64k()
        }
    }

    /// The M15 cell of the gate report: xfs, 16 KiB chunks, `cache_max` 8 GiB,
    /// 128 x 8, about 2.3 GiB live; one cell of 240 s. The workload shape
    /// (random 4 KiB overwrite of the live set) is an assumption.
    pub fn m15_8g() -> Self {
        Self {
            name: "m15-8g".into(),
            chunk: 16_384,
            ws_bytes: 2_469_606_195,
            fs_total: 200 * GIB,
            fs_external: 2 * GIB,
            cache_max: Some(8 * GIB),
            cells: 1,
            cell_secs: 240.0,
            ..Self::ram_64k()
        }
    }

    /// Total simulated seconds.
    pub fn total_secs(&self) -> f64 {
        f64::from(self.cells) * self.cell_secs
            + f64::from(self.cells.saturating_sub(1)) * self.gap_secs
    }

    /// The step as a duration.
    pub fn step(&self) -> Duration {
        Duration::from_secs_f64(self.step_ms / 1000.0)
    }

    /// Record bytes of one write.
    pub fn record(&self) -> u64 {
        64 + self.block
    }

    /// Chunks of the working set.
    pub fn ws_chunks(&self) -> f64 {
        (self.ws_bytes / self.chunk).max(1) as f64
    }

    /// Stored bytes of one chunk.
    pub fn chunk_stored(&self) -> f64 {
        self.chunk as f64 * self.stored_ratio + 128.0
    }

    /// Sets one parameter by name (`key=value`), as the command line and the
    /// parameter files do.
    ///
    /// # Errors
    /// Unknown key or a value that does not parse.
    pub fn set(&mut self, key: &str, val: &str) -> Result<()> {
        let b = || parse_bytes(val).with_context(|| format!("{key} = {val}"));
        let f = || parse_f64(val).with_context(|| format!("{key} = {val}"));
        match key {
            "in_flight" => self.in_flight = f()? as u32,
            "agg" => self.agg = (f()? as u32).max(1),
            "block" => self.block = b()?,
            "pattern" => {
                self.pattern = match val {
                    "random" => Pattern::Random,
                    "sequential" | "seq" => Pattern::Sequential,
                    _ => bail!("pattern: random or sequential"),
                }
            }
            "ws" | "ws_bytes" => self.ws_bytes = b()?,
            "cells" => self.cells = f()? as u32,
            "cell_secs" => self.cell_secs = f()?,
            "gap_secs" => self.gap_secs = f()?,
            "step_ms" => self.step_ms = f()?.max(1.0),
            "seed" => self.seed = f()? as u64,
            "gc_phase_s" => self.gc_phase_s = if val == "seed" { None } else { Some(f()?) },
            "chunk" => self.chunk = b()?,
            "stored_ratio" => self.stored_ratio = f()?,
            "fs_total" => self.fs_total = b()?,
            "fs_external" => self.fs_external = b()?,
            "zero_alloc" => self.zero_alloc = parse_bool(val)?,
            "cache_max" => self.cache_max = if val == "none" { None } else { Some(b()?) },
            "headroom" => self.headroom = b()?,
            "segment" => self.segment = b()?,
            "full_timeout_s" => self.full_timeout_s = f()?,
            "journal_max" => self.journal_max = b()?,
            "ring_park" => self.ring_park = b()?,
            "ring_file" => self.ring_file = b()?,
            "cut_target_s" => self.cut_target_s = f()?,
            "commit_interval_s" => self.commit_interval_s = f()?,
            "cut_trigger" => self.cut_trigger = f()?,
            "meta_per_cut" => self.meta_per_cut = b()?,
            "gc_rate" => self.gc_rate = b()?,
            "gc_grace_s" => self.gc_grace_s = f()?,
            "read_timeout_s" => self.read_timeout_s = f()?,
            "live_threshold" => self.live_threshold = f()?,
            "compact_per_pass" => self.compact_per_pass = b()?,
            "busy_interval_s" => self.busy_interval_s = f()?,
            "pass_interval_s" => self.pass_interval_s = f()?,
            "mark_s" => self.mark_s = f()?,
            "mark_per_chunk_us" => self.mark_per_chunk_us = f()?,
            "condemn_ms" => self.condemn_ms = f()?,
            "delete_ms" => self.delete_ms = f()?,
            "retain_cuts" => self.retain_cuts = f()? as u64,
            "batch_index" => self.batch_index = parse_bool(val)?,
            "incremental_mark" => self.incremental_mark = parse_bool(val)?,
            "gc_workers" => self.gc_workers = f()? as u32,
            "step_budget_ms" => self.step_budget_ms = f()? as u64,
            "rate_factor" => self.rate_factor = f()? as u64,
            "throttle" => self.throttle = parse_bool(val)?,
            "soft_percent" => self.soft_percent = f()? as u8,
            "soft_min" => self.soft_min = b()?,
            "soft_max" => self.soft_max = b()?,
            "min_rate" => self.min_rate = b()?,
            "max_delay_s" => self.max_delay_s = f()?,
            "device" => {
                self.device = match val {
                    "ram" => Device::ram(),
                    "netvol" => Device::netvol(),
                    _ => bail!("device: ram or netvol"),
                }
            }
            "engine_iops" => self.device.engine_iops = f()?,
            "journal_bw" => self.device.journal_bw = f()?,
            "store_bw" => self.device.store_bw = f()?,
            "gc_bw" => self.device.gc_bw = f()?,
            "shared_bw" => self.device.shared_bw = f()?,
            "cut_fixed_ms" => self.device.cut_fixed_ms = f()?,
            "jitter" => self.device.jitter = f()?,
            _ => bail!("unknown scenario key {key:?}"),
        }
        Ok(())
    }

    /// Checks the combination.
    ///
    /// # Errors
    /// A parameter that cannot work.
    pub fn validate(&self) -> Result<()> {
        if self.chunk == 0 || !self.chunk.is_power_of_two() || self.chunk < self.block {
            bail!("chunk must be a power of two of at least the block size");
        }
        if self.ws_bytes < self.chunk {
            bail!("the working set is smaller than a chunk");
        }
        if self.in_flight == 0 || self.agg == 0 || self.in_flight < self.agg {
            bail!("in_flight must be at least agg");
        }
        if self.ring_park < self.ring_file || self.ring_file == 0 {
            bail!("ring_park must hold at least one ring file");
        }
        if self.cells == 0 || self.cell_secs <= 0.0 {
            bail!("no cell to run");
        }
        Ok(())
    }
}

fn parse_bool(v: &str) -> Result<bool> {
    match v {
        "true" | "on" | "1" | "yes" => Ok(true),
        "false" | "off" | "0" | "no" => Ok(false),
        _ => Err(anyhow!("not a boolean: {v}")),
    }
}

/// A number, with an optional `s` / `ms` suffix removed.
pub fn parse_f64(v: &str) -> Result<f64> {
    let t = v.trim().trim_end_matches('s');
    let t = t.strip_suffix('m').map_or(t, |x| x); // "ms" -> "m" left after the s
    Ok(t.trim().parse::<f64>()?)
}

/// A byte count: a plain number, or one with a `k`, `m`, `g`, `t` suffix
/// (optionally `ib` / `b`; powers of 1024).
pub fn parse_bytes(v: &str) -> Result<u64> {
    let t = v.trim().to_ascii_lowercase();
    let t = t.trim_end_matches('b').trim_end_matches('i');
    let (num, mult) = match t.chars().last() {
        Some('k') => (&t[..t.len() - 1], 1u64 << 10),
        Some('m') => (&t[..t.len() - 1], 1u64 << 20),
        Some('g') => (&t[..t.len() - 1], 1u64 << 30),
        Some('t') => (&t[..t.len() - 1], 1u64 << 40),
        _ => (t, 1),
    };
    let x: f64 = num.trim().replace('_', "").parse()?;
    if x < 0.0 {
        bail!("negative size");
    }
    Ok((x * mult as f64) as u64)
}

/// Parses a flat TOML parameter set: `key = value` lines, `#` comments, and
/// `[section]` headers that prefix the keys that follow (`section.key`).
/// Values are bare words, numbers or quoted strings.
///
/// # Errors
/// A line that is neither.
pub fn parse_flat_toml(text: &str) -> Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    let mut section = String::new();
    for (i, raw) in text.lines().enumerate() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        if let Some(s) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = s.trim().to_string();
            continue;
        }
        let (k, v) = line
            .split_once('=')
            .ok_or_else(|| anyhow!("line {}: expected key = value", i + 1))?;
        let v = v.trim().trim_matches('"').to_string();
        let k = k.trim();
        let key = if section.is_empty() {
            k.to_string()
        } else {
            format!("{section}.{k}")
        };
        out.push((key, v));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_and_numbers_parse() {
        assert_eq!(parse_bytes("64k").unwrap(), 65_536);
        assert_eq!(parse_bytes("2GiB").unwrap(), 2 << 30);
        assert_eq!(parse_bytes("1.5g").unwrap(), 1_610_612_736);
        assert_eq!(parse_bytes("123").unwrap(), 123);
        assert!((parse_f64("30s").unwrap() - 30.0).abs() < 1e-9);
        assert!(parse_bytes("-1k").is_err());
    }

    #[test]
    fn flat_toml_has_sections_and_comments() {
        let kv = parse_flat_toml("a = 1 # c\n[p]\nb = \"x\"\n\n").unwrap();
        assert_eq!(
            kv,
            vec![("a".into(), "1".into()), ("p.b".into(), "x".into())]
        );
        assert!(parse_flat_toml("nonsense").is_err());
    }

    #[test]
    fn set_rejects_unknown_keys_and_applies_known_ones() {
        let mut s = Scenario::ram_64k();
        s.set("cache_max", "8G").unwrap();
        assert_eq!(s.cache_max, Some(8 << 30));
        s.set("pattern", "seq").unwrap();
        assert_eq!(s.pattern, Pattern::Sequential);
        assert!(s.set("nope", "1").is_err());
        assert!(Scenario::preset("nope").is_err());
        for n in SCENARIO_NAMES {
            Scenario::preset(n).unwrap().validate().unwrap();
        }
    }
}
