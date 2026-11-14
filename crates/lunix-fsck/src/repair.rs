//! Step 8: `--repair` and its explicit companions.
//!
//! Only safe things happen without a dedicated flag: `*.tmp` and `*.open`
//! files go, the index is rebuilt when it is damaged, a damaged checkpoint is
//! rewritten. Orphans go only with `--delete-orphans`; the tip is dropped only
//! with `--truncate-tip`; the rollback floor is reset only with
//! `--reset-floor`. Every action is reported, and a refused one changes
//! nothing.

use lunix_format::commit::Commit;
use lunix_format::local::{CheckpointSlot, RollbackFloor};
use lunix_iface::store::{CheckpointStore, CommitStore, PackStore, SegmentStore, VolumeDir};
use lunix_iface::Result;

use crate::code;
use crate::facts::Facts;
use crate::gc::{self, GcContext};
use crate::indexcheck;
use crate::journal;
use crate::report::{Finding, RepairAction, Sink};
use crate::session::Session;
use crate::Options;

/// The one operation fsck needs and `lunix-iface` does not offer: lowering the
/// rollback floor (`VolumeDir::raise_rollback_floor` is raise-only and
/// `store_local_pins` keeps the floor).
///
/// **Open iface item (M14 report, proposal P1).** Until the lead approves the
/// proposed `VolumeDir::reset_rollback_floor(&RollbackFloor, reason)`, no
/// implementation is wired in: [`Options::floor_lowering`] is `None` and every
/// repair that would lower the floor is refused before it changes anything.
/// Tests install a stand-in that rewrites `state/pins` with the format types.
pub trait FloorLowering: Send + Sync {
    /// Sets the floor of `vol` to `new`, lower than the one recorded or not.
    /// `reason` is `"truncate_tip"` or `"reset_floor"`, for the audit line.
    ///
    /// # Errors
    /// Whatever the implementation refuses with; nothing may have changed.
    fn set_floor(
        &self,
        vol: &dyn VolumeDir,
        new: &RollbackFloor,
        reason: &'static str,
    ) -> Result<()>;
}

fn refuse(sink: &mut Sink, what: &str, why: impl Into<String>) {
    sink.push(Finding::new(
        code::REPAIR_REFUSED,
        what.to_owned(),
        why.into(),
    ));
}

const NEEDS_IFACE: &str = "lowering the rollback floor needs the iface method VolumeDir::reset_rollback_floor, which is not wired in yet (M14 report, proposal P1)";

/// Lowers (or sets) the floor through the installed hook.
fn lower_floor(
    s: &Session,
    opts: &Options,
    new: &RollbackFloor,
    reason: &'static str,
) -> std::result::Result<(), String> {
    match &opts.floor_lowering {
        None => Err(NEEDS_IFACE.to_owned()),
        Some(h) => h
            .set_floor(&s.vol, new, reason)
            .map_err(|e| format!("lowering the floor failed: {e}")),
    }
}

pub(crate) fn run(
    s: &Session,
    facts: &Facts,
    opts: &Options,
    found: &Sink,
    sink: &mut Sink,
    actions: &mut Vec<RepairAction>,
) {
    if opts.truncate_tip {
        if !opts.repair {
            refuse(sink, "--truncate-tip", "it is a repair: add --repair");
        } else if !truncate_tip(s, facts, opts, sink, actions) {
            // Nothing else runs on a volume whose tip is in question.
            return;
        }
    }
    if opts.reset_floor {
        if !opts.repair {
            refuse(sink, "--reset-floor", "it is a repair: add --repair");
        } else {
            reset_floor(s, facts, opts, sink, actions);
        }
    }
    if opts.repair {
        match s.vol.remove_leftovers() {
            Ok(0) => {}
            Ok(n) => actions.push(RepairAction {
                action: "remove_leftovers",
                detail: format!("{n} *.tmp and *.open files removed"),
            }),
            Err(e) => refuse(sink, "leftovers", format!("{e}")),
        }
    }
    if opts.repair && opts.delete_orphans {
        if opts.truncate_tip {
            refuse(
                sink,
                "--delete-orphans",
                "not together with --truncate-tip: the checkpoint's id_high is zeroed with it and the ids of deleted objects could be reissued; run it after the next mount",
            );
        } else {
            delete_orphans(s, facts, sink, actions);
        }
    } else if opts.delete_orphans {
        refuse(sink, "--delete-orphans", "it is a repair: add --repair");
    }
    let index_damaged = [
        code::INDEX_RUN,
        code::INDEX_MISSING,
        code::INDEX_STALE,
        code::INDEX_EXTRA,
        code::INDEX_FILE,
    ]
    .iter()
    .any(|c| found.has(c));
    if opts.rebuild_index || (opts.repair && index_damaged) {
        if facts.tip.is_none() || !facts.chain_ok {
            refuse(
                sink,
                "index",
                "the retained chain does not verify, so the segments in use are unknown; repair the chain first",
            );
        } else {
            match indexcheck::rebuild(s, facts) {
                Ok(n) => actions.push(RepairAction {
                    action: "rebuild_index",
                    detail: format!("rebuilt from the footers: {n} entries"),
                }),
                Err(e) => refuse(sink, "index", format!("{e}")),
            }
        }
    }
    if opts.repair && found.has(code::CHECKPOINT) && !opts.truncate_tip {
        rewrite_checkpoint(s, facts, sink, actions);
    }
    if opts.gc && !opts.repair {
        refuse(sink, "--gc", "it is a repair: add --repair");
    }
    if opts.repair && opts.gc {
        let ctx = GcContext {
            session: s,
            facts,
            repair: true,
        };
        for f in gc::check_gc(&ctx) {
            sink.push(f);
        }
        actions.push(RepairAction {
            action: "gc",
            detail: "the GC repair of this build has no pending work to finish".to_owned(),
        });
    }
}

/// Returns false when the repair was refused.
fn truncate_tip(
    s: &Session,
    facts: &Facts,
    opts: &Options,
    sink: &mut Sink,
    actions: &mut Vec<RepairAction>,
) -> bool {
    let what = "--truncate-tip";
    let list = match CommitStore::list(&s.vol) {
        Ok(l) => l,
        Err(e) => {
            refuse(sink, what, format!("{e}"));
            return false;
        }
    };
    let [.., prev, tip] = list.as_slice() else {
        refuse(sink, what, "there is no previous commit to make the tip");
        return false;
    };
    if prev.seq.0 + 1 != tip.seq.0 {
        refuse(sink, what, "the previous commit is absent");
        return false;
    }
    let prev_bytes = match s.vol.get(prev.seq) {
        Ok(b) => b,
        Err(e) => {
            refuse(
                sink,
                what,
                format!("the previous commit cannot be read: {e}"),
            );
            return false;
        }
    };
    let prev_commit = match s.commit_codec().open_commit(prev.seq, &prev_bytes) {
        Ok(Commit::Full(f)) => f,
        Ok(_) => {
            refuse(sink, what, "the previous commit is not a FULL commit");
            return false;
        }
        Err(_) => {
            refuse(
                sink,
                what,
                "the previous commit does not authenticate; the volume cannot be repaired by dropping the tip",
            );
            return false;
        }
    };
    if s.commit_codec()
        .verify_chain(&s.vol, prev.seq, prev_commit.chain_floor, &s.volume_hash)
        .is_err()
    {
        refuse(
            sink,
            what,
            "the chain below the previous commit does not verify",
        );
        return false;
    }
    let prev_hash = s.commit_codec().object_hash(&prev_bytes);
    let new_floor = RollbackFloor {
        max_commit_seq: prev.seq,
        tip_hash: prev_hash,
    };
    // The floor first: a crash after it leaves the old tip with a lower
    // floor, and a rerun does the same again.
    let mut lowered = None;
    if let Some(f) = facts.floor {
        if f.max_commit_seq > prev.seq {
            if let Err(why) = lower_floor(s, opts, &new_floor, "truncate_tip") {
                refuse(sink, what, why);
                return false;
            }
            lowered = Some(f);
        } else if f.max_commit_seq == prev.seq && f.tip_hash != prev_hash {
            refuse(
                sink,
                what,
                "the previous commit is not the commit the floor recorded at its height: the chain forks; decide with --reset-floor",
            );
            return false;
        }
    }
    // What the older floor can no longer cover.
    let mut jn = Sink::default();
    let sc = journal::scan(s, &mut jn);
    let cut = prev_commit.cut_seq.0;
    let first_above = sc.seqs.iter().copied().find(|q| *q > cut);
    let lost = match first_above {
        Some(q) if q > cut + 1 => format!(
            "journal records {}..={} were reclaimed by the damaged tip's cut and are lost",
            cut + 1,
            q - 1
        ),
        Some(_) => "the journal still holds every record above the new cut".to_owned(),
        None => "the journal holds no record above the new cut; writes between the new tip's cut and the old one, if there were any, are lost".to_owned(),
    };
    if let Err(e) = s.vol.set_aside(tip.seq) {
        refuse(sink, what, format!("{e}"));
        return false;
    }
    if let Err(e) = s.vol.zero() {
        refuse(
            sink,
            "checkpoint",
            format!("the tip was set aside but the checkpoint could not be zeroed: {e}"),
        );
        return false;
    }
    actions.push(RepairAction {
        action: "truncate_tip",
        detail: format!(
            "commit {} moved aside as {:016x}.damaged, commit {} is the tip, both checkpoint slots zeroed; {lost}",
            tip.seq.0, tip.seq.0, prev.seq.0
        ),
    });
    if let Some(old) = lowered {
        actions.push(RepairAction {
            action: "lower_floor",
            detail: format!(
                "rollback floor lowered from commit {} to commit {}",
                old.max_commit_seq.0, prev.seq.0
            ),
        });
    }
    true
}

fn reset_floor(
    s: &Session,
    facts: &Facts,
    opts: &Options,
    sink: &mut Sink,
    actions: &mut Vec<RepairAction>,
) {
    let what = "--reset-floor";
    let Some(tip) = &facts.tip else {
        refuse(
            sink,
            what,
            "the tip does not authenticate; there is no verified tip to set the floor to",
        );
        return;
    };
    if !facts.chain_ok {
        refuse(sink, what, "the chain does not verify down to chain_floor");
        return;
    }
    let new = RollbackFloor {
        max_commit_seq: tip.seq,
        tip_hash: tip.hash,
    };
    let old = facts.floor;
    if old == Some(new) {
        actions.push(RepairAction {
            action: "reset_floor",
            detail: format!("the floor is already commit {}", tip.seq.0),
        });
        return;
    }
    let lowering = old.is_some_and(|f| {
        f.max_commit_seq > new.max_commit_seq
            || (f.max_commit_seq == new.max_commit_seq && f.tip_hash != new.tip_hash)
    });
    let r: std::result::Result<(), String> = if lowering {
        lower_floor(s, opts, &new, "reset_floor")
    } else {
        s.vol.raise_rollback_floor(&new).map_err(|e| format!("{e}"))
    };
    match r {
        Ok(()) => actions.push(RepairAction {
            action: "reset_floor",
            detail: format!(
                "floor was {}, now commit {} ({})",
                old.map_or_else(
                    || "absent".to_owned(),
                    |f| format!("commit {}", f.max_commit_seq.0)
                ),
                tip.seq.0,
                hex8(&tip.hash)
            ),
        }),
        Err(why) => refuse(sink, what, why),
    }
}

fn hex8(h: &[u8; 32]) -> String {
    h[..4]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>()
        + "..."
}

fn highest_id(s: &Session, facts: &Facts) -> u64 {
    let mut hi = 0;
    for id in facts.segments.keys().chain(facts.packs.keys()) {
        hi = hi.max(id.raw());
    }
    hi.max(s.vol.ids().high_water())
}

fn checkpoint_slot(s: &Session, facts: &Facts) -> Option<CheckpointSlot> {
    let tip = facts.tip.as_ref()?;
    let old = s.vol.load().ok().flatten();
    let lanes = facts.journal_lanes.max(1);
    let named_hi = facts
        .named_segments
        .iter()
        .chain(&facts.named_packs)
        .map(|i| i.raw())
        .max()
        .unwrap_or(0);
    Some(CheckpointSlot {
        slot_seq: 1,
        volume_uuid: s.desc.volume_uuid,
        last_commit_seq: tip.seq.0,
        id_high: highest_id(s, facts)
            .max(named_hi)
            .max(old.as_ref().map_or(0, |o| o.id_high)),
        lane_tail_ordinals: vec![0; lanes],
        stats: old.map(|o| o.stats).unwrap_or_default(),
    })
}

fn rewrite_checkpoint(
    s: &Session,
    facts: &Facts,
    sink: &mut Sink,
    actions: &mut Vec<RepairAction>,
) {
    let Some(slot) = checkpoint_slot(s, facts) else {
        return;
    };
    // Both slots go: a damaged one must not survive next to the new one.
    let written = s.vol.zero().and_then(|()| s.vol.store(&slot));
    match written {
        Ok(()) => actions.push(RepairAction {
            action: "rewrite_checkpoint",
            detail: format!(
                "a fresh slot for commit {} with no lane tails (nothing is skipped at recovery)",
                slot.last_commit_seq
            ),
        }),
        Err(e) => refuse(sink, "checkpoint", format!("{e}")),
    }
}

fn delete_orphans(s: &Session, facts: &Facts, sink: &mut Sink, actions: &mut Vec<RepairAction>) {
    let what = "--delete-orphans";
    let walked = facts.walk.as_ref().is_some_and(|w| w.tip_complete);
    if facts.tip.is_none() || !facts.chain_ok || !walked {
        refuse(
            sink,
            what,
            "the retained roots could not be walked completely, so nothing can be called an orphan",
        );
        return;
    }
    let used_packs = facts
        .walk
        .as_ref()
        .map(|w| w.used_packs.clone())
        .unwrap_or_default();
    let segs: Vec<_> = facts
        .segments
        .keys()
        .filter(|id| {
            !facts.named_segments.contains(id)
                && !facts.referenced_segments.contains(id)
                && !facts.condemned_ids.contains(id)
        })
        .copied()
        .collect();
    let packs: Vec<_> = facts
        .packs
        .keys()
        .filter(|id| {
            !facts.named_packs.contains(id)
                && !used_packs.contains(id)
                && !facts.condemned_ids.contains(id)
        })
        .copied()
        .collect();
    if segs.is_empty() && packs.is_empty() {
        return;
    }
    // Ids are never reused: record the highest id on disk before any delete.
    let hi = highest_id(s, facts);
    let recorded = s.vol.load().ok().flatten().map_or(0, |c| c.id_high);
    if recorded < hi {
        if let Some(slot) = checkpoint_slot(s, facts) {
            if let Err(e) = s.vol.store(&slot) {
                refuse(sink, "checkpoint", format!("{e}"));
                return;
            }
        }
    }
    let (mut ns, mut np, mut bytes) = (0u64, 0u64, 0u64);
    for id in segs {
        match SegmentStore::delete(&s.vol, id) {
            Ok(b) => {
                ns += 1;
                bytes += b;
            }
            Err(e) => refuse(sink, what, format!("segment {id}: {e}")),
        }
    }
    for id in packs {
        match s.vol.delete_pack(id) {
            Ok(b) => {
                np += 1;
                bytes += b;
            }
            Err(e) => refuse(sink, what, format!("pack {id}: {e}")),
        }
    }
    actions.push(RepairAction {
        action: "delete_orphans",
        detail: format!("{ns} segments and {np} packs deleted, {bytes} bytes freed"),
    });
}
