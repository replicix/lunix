//! Startup recovery, `Engine::open` (plan M11 step 6, DESIGN §4.4).
//!
//! Order: verify the tip and the retained chain (no fallback); R4 against
//! the rollback floor, before anything is changed; the orphan sweep and the
//! allocator floor (D17); the index catch-up from the segments the chain
//! names (after the sweep, so dedup can never adopt an orphan); the map at
//! the tip's root; the checkpoint hint (ignored when above the tip); the
//! journal recovered from the tip's `cut_seq` and the overlay rebuilt; the
//! floor raised to the tip; the ledger charged with what exists. Recovery
//! writes only the sweep's deletions and the floor, both idempotent, so a
//! crash during it is another recovery. Lanes are opened later by the queue
//! handlers; the first `open_lane` wipes the stale journal tail, after the
//! damage found here was logged.

use std::collections::HashSet;
use std::sync::Arc;

use lunix_format::commit::{Commit, Full};
use lunix_format::journal::RecordKind;
use lunix_format::local::RollbackFloor;
use lunix_format::{CommitSeq, SegId, WriteSeq};
use lunix_iface::index::CatchUpReport;
use lunix_iface::journal::{record_bytes, DamagedRange, WriteSeqRange};
use lunix_iface::map::BlockMap;
use lunix_iface::overlay::BlockRange;
use lunix_iface::space::SpaceKind;
use lunix_iface::stats::EngineStats;
use lunix_iface::store::{SegmentFooters, SegmentStore, SweepReport, SweepRequest};
use lunix_iface::{ErrorKind, Result};

use crate::config::{EngineConfig, OnJournalDamage};
use crate::deps::EngineDeps;
use crate::error::OpenError;
use crate::points::Points;

/// What `open` found and did.
#[derive(Clone, Debug, Default)]
pub struct RecoveryReport {
    /// The verified tip.
    pub tip: CommitSeq,
    /// Its cut (the journal recovery floor).
    pub cut_seq: WriteSeq,
    /// The tip's `chain_floor`.
    pub chain_floor: CommitSeq,
    /// Journal records replayed into the overlay.
    pub replayed: usize,
    /// The next `write_seq`.
    pub next_seq: WriteSeq,
    /// Valid records beyond the first gap, which the first lane open wipes.
    pub ignored_after_gap: Option<WriteSeqRange>,
    /// Damaged journal ranges the resync skipped.
    pub damaged: Vec<DamagedRange>,
    /// The journal's flushed watermark: every record at or below it was
    /// durable before the crash; gaps and damage above it were crash residue.
    pub flushed: WriteSeq,
    /// No rollback floor existed: this host cannot detect a rollback of
    /// earlier history (ADR-31); the floor is set now.
    pub first_attach: bool,
    /// The orphan sweep.
    pub sweep: SweepReport,
    /// The index catch-up.
    pub catch_up: CatchUpReport,
    /// The checkpoint hint was used (written for a commit at or below the tip).
    pub checkpoint_used: bool,
}

/// What recovery hands to the engine.
pub(crate) struct Recovered {
    pub(crate) tip: Full,
    pub(crate) tip_hash: [u8; 32],
    pub(crate) retained: Vec<Commit>,
    pub(crate) map: Arc<dyn BlockMap>,
    pub(crate) stats: Option<EngineStats>,
    pub(crate) report: RecoveryReport,
    pub(crate) journal_charged: u64,
    pub(crate) index_charged: u64,
    pub(crate) upto_seg: Option<SegId>,
}

/// The raw value of the highest object id any of `commits` names.
pub(crate) fn high_water(commits: &[Commit]) -> u64 {
    commits
        .iter()
        .filter_map(|c| match c {
            Commit::Full(f) => Some(f),
            _ => None,
        })
        .flat_map(|f| {
            f.new_segments
                .iter()
                .chain(&f.map_packs)
                .chain(&f.index_packs)
                .chain(f.map_root.pack.iter())
                .map(|id| id.raw())
                .collect::<Vec<_>>()
        })
        .max()
        .unwrap_or(0)
}

struct Footers<'a>(&'a dyn SegmentStore);

impl SegmentFooters for Footers<'_> {
    fn segment_footer(&self, seg: SegId) -> Result<lunix_format::segment::FooterBody> {
        self.0.footer(seg)
    }
}

/// R4 for a tip above the floor (ADR-31, maintainer decision of 2026-11-13):
/// the chain from the tip back to the floor's height must pass through the
/// commit the floor recorded. The links down to `chain_floor` are already
/// verified; below it the walk continues over commits that may still exist
/// (GC keeps them while a floor needs them, M17).
///
/// * Only a **missing** link (`NotFound`: commits below the tip deleted) is
///   `ForkUnverifiable`, which the operator override may accept.
/// * A commit that is present but does not authenticate, does not decode or
///   does not link (`parent_hash`, `commit_seq`, `epoch`) is positive
///   evidence of another history: `Fork`, never overridable (M12a re-review
///   R2).
/// * A host I/O error is reported as such.
fn check_floor_on_path(
    cfg: &EngineConfig,
    d: &EngineDeps,
    tip_seq: CommitSeq,
    f: &RollbackFloor,
    volume_hash: &[u8; 32],
) -> std::result::Result<(), OpenError> {
    let walked = d
        .commit_codec
        .verify_chain(&*d.commits, tip_seq, f.max_commit_seq, volume_hash)
        .and_then(|_| d.commits.get(f.max_commit_seq));
    let fork = || OpenError::Fork {
        tip: f.max_commit_seq,
    };
    match walked {
        Ok(bytes) if d.commit_codec.object_hash(&bytes) != f.tip_hash => Err(fork()),
        Ok(_) => Ok(()),
        Err(source) if source.kind() == ErrorKind::NotFound => {
            if cfg.allow_unverifiable_fork {
                tracing::warn!(
                    floor = f.max_commit_seq.0,
                    tip = tip_seq.0,
                    error = %source,
                    "operator override: a commit between the tip and the rollback floor is missing; opening anyway"
                );
                return Ok(());
            }
            Err(OpenError::ForkUnverifiable {
                floor: f.max_commit_seq,
                tip: tip_seq,
                source,
            })
        }
        Err(source) if source.kind() == ErrorKind::Io => Err(OpenError::Store(source)),
        Err(source) => {
            tracing::warn!(
                floor = f.max_commit_seq.0,
                tip = tip_seq.0,
                error = %source,
                "a commit between the tip and the rollback floor does not authenticate or link: fork"
            );
            Err(fork())
        }
    }
}

/// Whether journal damage is the kind `on_journal_damage = refuse` keeps the
/// volume offline for (maintainer decision of 2026-11-14, ADR-4): real loss,
/// as the flushed watermark shows it.
///
/// Every record carries the highest `write_seq` a completed group flush had
/// made durable on every lane when it was appended; `Recovered::flushed` is
/// the highest such value any surviving record carries. Every record at or
/// below it was on stable storage before that later record was written, so:
///
/// * a gap at or below the watermark (`gap_at <= flushed`) is a durable record
///   that is gone, and a damaged range whose next valid record in its lane is
///   at or below the watermark is damage inside durable history: refused;
/// * everything above the watermark was never covered by an acknowledged
///   flush, whatever its shape: a torn last record, a gap across lanes (one
///   lane's record still in its open batch at a kill), a torn batch followed
///   by a later batch of the same lane (two batches in flight). That is crash
///   residue a block device may lose: logged at warn and wiped.
///
/// What the watermark cannot see is damage to the newest flushed records when
/// no later record witnessed that flush; those read as residue.
fn refuse_worthy(rec: &lunix_iface::journal::Recovered) -> bool {
    let w = rec.flushed;
    rec.gap_at.is_some_and(|g| g <= w)
        || rec
            .damaged
            .iter()
            .any(|d| d.next_seq.is_some_and(|n| n <= w))
}

pub(crate) fn recover(
    cfg: &EngineConfig,
    d: &EngineDeps,
    points: &Points,
) -> std::result::Result<Recovered, OpenError> {
    let cs = u64::from(cfg.chunk_size);
    // 1. The tip and the retained chain, authenticated; no fallback.
    let volume_hash = d.commit_codec.object_hash(&d.dir.volume_bytes()?);
    let tip_seq = d.commits.tip()?.ok_or(OpenError::NoCommit)?;
    let tip_bytes = d.commits.get(tip_seq)?;
    let tip = d
        .commit_codec
        .open_commit(tip_seq, &tip_bytes)
        .map_err(|source| OpenError::TipUnauthenticated {
            tip: tip_seq,
            source,
        })?;
    let Commit::Full(tip) = tip else {
        return Err(OpenError::TipNotFull { tip: tip_seq });
    };
    if !tip.size.is_multiple_of(cs) {
        return Err(OpenError::Config(
            "volume size is not a multiple of the chunk size",
        ));
    }
    let tip_hash = d.commit_codec.object_hash(&tip_bytes);
    d.commit_codec
        .verify_chain(&*d.commits, tip_seq, tip.chain_floor, &volume_hash)
        .map_err(|source| OpenError::BrokenChain {
            tip: tip_seq,
            source,
        })?;
    points.hit("engine.recover.verified")?;
    lunix_iface::crashpoint!("engine.recover.verified");

    // 2. R4: the rollback floor, before anything changes.
    let floor = d.floor.load()?;
    if let Some(f) = &floor {
        if tip_seq < f.max_commit_seq {
            return Err(OpenError::Rollback {
                floor: f.max_commit_seq,
                tip: tip_seq,
            });
        }
        if tip_seq == f.max_commit_seq && f.tip_hash != tip_hash {
            return Err(OpenError::Fork { tip: tip_seq });
        }
        if tip_seq > f.max_commit_seq {
            check_floor_on_path(cfg, d, tip_seq, f, &volume_hash)?;
        }
    }

    // 3. What the retained chain names.
    let mut retained = Vec::new();
    for s in tip.chain_floor.0..=tip_seq.0 {
        let seq = CommitSeq(s);
        let bytes = d.commits.get(seq)?;
        retained.push(d.commit_codec.open_commit(seq, &bytes)?);
    }
    let mut named_segments: HashSet<SegId> = HashSet::new();
    let mut named_packs = HashSet::new();
    for c in &retained {
        if let Commit::Full(f) = c {
            named_segments.extend(f.new_segments.iter().copied());
            named_packs.extend(f.map_packs.iter().copied());
            named_packs.extend(f.map_root.pack);
        }
    }
    let mark = high_water(&retained);

    // 4. The orphan sweep (N2), then the allocator floor (D17).
    let checkpoint = d.checkpoint.load().unwrap_or_else(|e| {
        tracing::warn!(error = %e, "checkpoint unreadable; ignored");
        None
    });
    let sweep = d.dir.sweep_orphans(&SweepRequest {
        high_water_mark: mark,
        named_segments: &named_segments,
        named_packs: &named_packs,
    })?;
    let ids = d.dir.ids();
    ids.observe(mark);
    ids.observe(sweep.highest_id_before);
    if let Some(c) = &checkpoint {
        ids.observe(c.id_high);
    }
    if !sweep.deleted_segments.is_empty() || !sweep.deleted_packs.is_empty() {
        tracing::info!(
            segments = sweep.deleted_segments.len(),
            packs = sweep.deleted_packs.len(),
            "orphans above the tip swept"
        );
    }
    points.hit("engine.recover.swept")?;
    lunix_iface::crashpoint!("engine.recover.swept");

    // 5. The index catch-up from the segments the chain names.
    let mut segs: Vec<SegId> = named_segments.iter().copied().collect();
    segs.sort_unstable();
    let catch_up = d.index.catch_up(&segs, &Footers(&*d.segments))?;
    points.hit("engine.recover.caught_up")?;
    lunix_iface::crashpoint!("engine.recover.caught_up");

    // 6. The map, the checkpoint hint, the journal and the overlay.
    let map = d.maps.open(&tip.map_root, tip.size / cs)?;
    let hint = checkpoint.filter(|c| c.last_commit_seq <= tip_seq.0);
    let checkpoint_used = hint.is_some();
    let stats = hint.as_ref().map(|c| EngineStats::from_slots(&c.stats));
    let rec = d.journal.recover(hint.as_ref(), tip.cut_seq)?;
    if rec.ignored_after_gap.is_some() || !rec.damaged.is_empty() {
        // Logged before any lane opens: the first open_lane wipes them.
        tracing::warn!(
            ignored_after_gap = ?rec.ignored_after_gap,
            gap_at = ?rec.gap_at,
            damaged = ?rec.damaged,
            "journal recovery stopped at a gap or skipped damage"
        );
        if cfg.on_journal_damage == OnJournalDamage::Refuse && refuse_worthy(&rec) {
            return Err(OpenError::JournalDamage);
        }
        if !refuse_worthy(&rec) {
            tracing::warn!(
                flushed = rec.flushed.0,
                "the damage and gaps are above the flushed watermark: crash residue of writes no acknowledged flush covered; wiped"
            );
        }
    }
    let mut journal_charged = 0u64;
    for r in &rec.records {
        let range = BlockRange {
            lba: r.record.lba,
            blocks: u64::from(r.record.blocks),
        };
        match r.record.kind {
            RecordKind::Write => d.overlay.insert_write(range, r.loc, r.record.write_seq),
            RecordKind::Hole => d.overlay.insert_hole(range, r.record.write_seq),
        }
        journal_charged += record_bytes(r.record.kind, r.record.blocks);
    }
    points.hit("engine.recover.journal")?;
    lunix_iface::crashpoint!("engine.recover.journal");

    // 7. The floor follows the verified tip (raise-only).
    let first_attach = floor.is_none();
    if first_attach {
        tracing::info!("no rollback floor on this host: rollback of earlier history cannot be detected; recording one now");
    }
    if floor.is_none_or(|f| f.max_commit_seq < tip_seq) {
        d.floor.raise(&RollbackFloor {
            max_commit_seq: tip_seq,
            tip_hash,
        })?;
    }
    points.hit("engine.recover.floor")?;
    lunix_iface::crashpoint!("engine.recover.floor");

    // 8. The ledger: what exists on disk, and the journal records kept.
    let seg_bytes: u64 = d.segments.list()?.iter().map(|s| s.size_bytes).sum();
    let pack_bytes: u64 = d.packs.list_packs()?.iter().map(|p| p.size_bytes).sum();
    let commit_bytes: u64 = d.commits.list()?.iter().map(|c| c.size_bytes).sum();
    let index_charged: u64 = d.index.all_runs()?.iter().map(|r| r.size_bytes).sum();
    for (kind, bytes) in [
        (SpaceKind::Segments, seg_bytes),
        (SpaceKind::Map, pack_bytes),
        (SpaceKind::Commits, commit_bytes),
        (SpaceKind::Index, index_charged),
        (SpaceKind::JournalReserved, journal_charged),
    ] {
        if bytes > 0 {
            d.space.charge(kind, bytes)?;
        }
    }
    let upto_seg = named_segments.iter().copied().max();
    let report = RecoveryReport {
        tip: tip_seq,
        cut_seq: tip.cut_seq,
        chain_floor: tip.chain_floor,
        replayed: rec.records.len(),
        next_seq: rec.next_seq,
        ignored_after_gap: rec.ignored_after_gap,
        damaged: rec.damaged,
        flushed: rec.flushed,
        first_attach,
        sweep,
        catch_up,
        checkpoint_used,
    };
    Ok(Recovered {
        tip,
        tip_hash,
        retained,
        map,
        stats,
        report,
        journal_charged,
        index_charged,
        upto_seg,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use lunix_iface::journal::{LaneId, Recovered};

    fn rec(damaged: Vec<DamagedRange>, gap: bool, flushed: u64) -> Recovered {
        Recovered {
            records: Vec::new(),
            next_seq: WriteSeq(3),
            ignored_after_gap: gap.then_some(WriteSeqRange {
                first: WriteSeq(4),
                last: WriteSeq(4),
            }),
            gap_at: gap.then_some(WriteSeq(3)),
            damaged,
            flushed: WriteSeq(flushed),
        }
    }

    fn dmg(next: Option<u64>) -> DamagedRange {
        DamagedRange {
            lane: LaneId(0),
            ordinal: 1,
            start: 8192,
            end: 12288,
            prev_seq: WriteSeq(2),
            next_seq: next.map(WriteSeq),
        }
    }

    #[test]
    fn only_loss_at_or_below_the_watermark_is_refused() {
        // Above the watermark (2): residue, whatever the shape.
        assert!(!refuse_worthy(&rec(vec![dmg(None)], false, 2)), "torn tail");
        assert!(
            !refuse_worthy(&rec(vec![dmg(Some(4))], false, 2)),
            "two batches in flight"
        );
        assert!(
            !refuse_worthy(&rec(Vec::new(), true, 2)),
            "a gap across lanes"
        );
        assert!(!refuse_worthy(&rec(Vec::new(), false, 0)), "clean");
        // At or below it: real loss.
        assert!(
            refuse_worthy(&rec(Vec::new(), true, 3)),
            "gap at the watermark"
        );
        assert!(
            refuse_worthy(&rec(vec![dmg(Some(4))], false, 4)),
            "damage inside durable history"
        );
        assert!(
            refuse_worthy(&rec(vec![dmg(None)], true, 5)),
            "a durable record gone"
        );
    }
}
