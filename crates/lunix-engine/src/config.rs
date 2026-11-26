//! Engine settings: the volume parameters and the commit cadence.

use std::time::Duration;

use lunix_format::local::VolumeJson;
use lunix_format::IdLen;
use lunix_iface::gc::GcBudget;

/// What `Engine::open` does when journal recovery found damage or valid
/// records beyond the first gap (DESIGN §4.1, M12 note): the first lane open
/// wipes them irreversibly.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum OnJournalDamage {
    /// Log and continue; the first `open_lane` wipes (availability first).
    #[default]
    Wipe,
    /// Refuse to open, so `fsck` can read the journal untouched, when a gap
    /// or damage lies at or below the flushed watermark (a durable record is
    /// gone or damaged). Everything above the watermark (a torn last record,
    /// a gap across lanes, a torn batch followed by a later one) is crash
    /// residue of writes no acknowledged flush covered; it is logged and
    /// wiped as under `Wipe` (ADR-4, maintainer decision of 2026-11-14).
    Refuse,
}

/// What the engine's per-segment reference table is for (the daemon's
/// `gc_condemn_on_cut`).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum CondemnOnCut {
    /// No table: nothing is counted or compared.
    #[default]
    Off,
    /// The table is maintained and compared with every mark, and what it
    /// would offer for condemnation is counted and compared with what the
    /// marks condemn. Nothing is condemned, listed or deleted because of it.
    /// Opening the engine switches the collector's counting of the tip
    /// root on (`Collector::set_tip_refs`); there is no second switch.
    Shadow,
    /// As `Shadow`, and a segment the table finds dead is offered to the
    /// collector (`Collector::propose_dead`) at the first step that finds
    /// the collector idle, `2 x (retain_last - 1)` cuts after its last entry
    /// went: the collector lists, flags and starts the grace of what its own
    /// rules accept, and the recheck mark that began after the grace ended
    /// is, as ever, the only judge of a delete. Nothing is offered while a
    /// pin exists.
    On,
}

/// The settings of one engine.
#[derive(Clone, Debug)]
pub struct EngineConfig {
    /// The volume chunk size in bytes (16 KiB ..= 1 MiB, a power of two).
    pub chunk_size: u32,
    /// The map id-prefix length of the volume.
    pub id_len: IdLen,
    /// The commit interval `T`; an idle volume cuts nothing.
    pub commit_interval: Duration,
    /// The dirty-bytes trigger `B` (overlay bytes).
    pub dirty_bytes: u64,
    /// The descriptor's `map_fanout` (256 or 512, D1): chunks per map leaf.
    pub map_fanout: u32,
    /// The most map leaves one cut may dirty (`chunk / map_fanout`), so a
    /// cut stays well under the map's dirty-leaf cap (default: half of
    /// `map_dirty_leaves_max`). DISCARD and WRITE_ZEROES records are split
    /// to half of it, so a cut can always stop between records.
    pub max_leaves_per_cut: u64,
    /// Cut when the journal ring is fuller than this (0.5).
    pub journal_fill_trigger: f64,
    /// The cut target (ADR-33, `cut_target_ms` of `lunixd.toml`): the
    /// coordinator cuts at most this long at the drain rate of its last cuts,
    /// in journal bytes (1 s by default). Never less than one ring file of
    /// journal bytes (`cut_min_bytes`) and than what frees a ring file of the
    /// fullest lane, never more than half the ring.
    pub cut_target: Duration,
    /// The least a bounded cut takes, in journal bytes (`None`: one ring file
    /// of the journal, the default). Tests that want cuts of a record or two
    /// set it small; the daemon never does.
    pub cut_min_bytes: Option<u64>,
    /// FULL commits retained (`last:N`), at least 1.
    pub retain_last: u32,
    /// Recompute chunk ids on every store read.
    pub verify_reads: bool,
    /// Bytes of the decoded-chunk read cache.
    pub read_cache_max: u64,
    /// Threads of the read helper pool (at least 1).
    pub read_helpers: usize,
    /// Run the coordinator on a background thread that checks the triggers.
    /// Tests turn it off and drive [`Engine::tick`](crate::Engine::tick).
    pub background: bool,
    /// How often the background coordinator checks its triggers.
    pub background_period: Duration,
    /// The largest request a queue accepts (D14).
    pub max_io_buf_bytes: u32,
    /// What `open` does about journal damage.
    pub on_journal_damage: OnJournalDamage,
    /// How long C1 waits for in-flight writes before giving up on this cut.
    pub inflight_timeout: Duration,
    /// What one GC step may spend (C10); `None` never calls the collector.
    pub gc_budget: Option<GcBudget>,
    /// GC settings handed to the collector.
    pub gc_params: Option<lunix_iface::gc::GcParams>,
    /// The compaction rate's multiple while writes are slowed (ADR-32,
    /// `gc_pressure_rate_factor`; 4 by default). Held by `Pressure::params`.
    pub gc_pressure_rate_factor: u64,
    /// Operator override (ADR-31, decision of 2026-11-13): open although the
    /// chain from the tip back to the rollback floor cannot be walked
    /// (`OpenError::ForkUnverifiable`). The caller audits it.
    pub allow_unverifiable_fork: bool,
    /// The longest a write waits for a full journal ring before it fails
    /// (ADR-32: `ENOSPC` if the last commit failed for space, else `EIO`):
    /// the volume's resolved `full_timeout` (60 s by default). A live change
    /// goes through [`Engine::set_full_timeout`](crate::Engine::set_full_timeout).
    /// Whoever builds the engine keeps it equal to the space gate's
    /// resolved `full_timeout` (`lunixd`'s `open_volume` copies it); a
    /// ring-full wait is at least 60 s while commits succeed.
    pub full_timeout: Duration,
    /// The per-segment reference table of the cut (default off).
    pub gc_condemn_on_cut: CondemnOnCut,
    /// The short grace of condemned segments while the GC pressure is raised
    /// or urgent (the daemon's `gc_pressure_grace_ms`; ADR-32 amendment,
    /// rule change F3). `None`: off (the default). The collector holds it to
    /// its own minimum.
    pub gc_pressure_grace: Option<Duration>,
}

impl EngineConfig {
    /// The defaults of `volume.json` for a volume of `chunk_size` with
    /// 16-byte id prefixes.
    pub fn new(chunk_size: u32) -> Self {
        Self::from_volume_json(&VolumeJson::default(), chunk_size, IdLen::Sixteen)
    }

    /// Settings from `volume.json` and the descriptor's chunk size and id length.
    pub fn from_volume_json(json: &VolumeJson, chunk_size: u32, id_len: IdLen) -> Self {
        Self {
            chunk_size,
            id_len,
            commit_interval: Duration::from_millis(json.commit.interval_ms),
            dirty_bytes: json.commit.dirty_bytes,
            map_fanout: 512,
            max_leaves_per_cut: (json.map_dirty_leaves_max / 2).max(2),
            journal_fill_trigger: 0.5,
            cut_target: Duration::from_secs(1),
            cut_min_bytes: None,
            retain_last: json.retain_last.max(1),
            verify_reads: json.verify_reads,
            read_cache_max: json.read_cache_max,
            read_helpers: 4,
            background: true,
            background_period: Duration::from_millis(100),
            max_io_buf_bytes: json.max_io_buf_bytes,
            on_journal_damage: OnJournalDamage::Wipe,
            inflight_timeout: Duration::from_secs(60),
            gc_budget: None,
            gc_params: None,
            gc_pressure_rate_factor: 4,
            allow_unverifiable_fork: false,
            full_timeout: Duration::from_secs(60),
            gc_condemn_on_cut: CondemnOnCut::Off,
            gc_pressure_grace: None,
        }
    }

    /// 4 KiB blocks per chunk.
    pub(crate) fn chunk_blocks(&self) -> u64 {
        u64::from(self.chunk_size) / 4096
    }
}
