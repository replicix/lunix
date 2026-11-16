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
use lunix_iface::store::{
    CheckpointStore, CommitStore, FloorReset, PackStore, SegmentStore, VolumeDir,
};

use crate::code;
use crate::facts::{Facts, Retained, Tip};
use crate::gc::{self, GcContext};
use crate::indexcheck;
use crate::journal;
use crate::mapwalk;
use crate::report::{Finding, RepairAction, Severity, Sink};
use crate::resolve;
use crate::session::Session;
use crate::Options;

fn refuse(sink: &mut Sink, what: &str, why: impl Into<String>) {
    sink.push(Finding::new(
        code::REPAIR_REFUSED,
        what.to_owned(),
        why.into(),
    ));
}

/// Runs the checks again; later repairs must use the state after the earlier ones.
pub(crate) type Recheck<'a> = &'a dyn Fn(&mut Sink) -> Facts;

fn refresh(recheck: Recheck<'_>, facts: &mut Facts, found: &mut Sink) {
    let mut sink = Sink::default();
    *facts = recheck(&mut sink);
    *found = sink;
}

pub(crate) fn run(
    s: &Session,
    opts: &Options,
    facts: &mut Facts,
    found: &mut Sink,
    refusals: &mut Sink,
    actions: &mut Vec<RepairAction>,
    recheck: Recheck<'_>,
) {
    if opts.truncate_tip {
        if !opts.repair {
            refuse(refusals, "--truncate-tip", "it is a repair: add --repair");
        } else {
            let before = actions.len();
            let ok = truncate_tip(s, facts, refusals, actions);
            if actions.len() > before {
                refresh(recheck, facts, found);
            }
            if !ok {
                // Nothing else runs on a volume whose tip is in question.
                return;
            }
        }
    }
    if opts.repair_pins {
        if !opts.repair {
            refuse(refusals, "--repair-pins", "it is a repair: add --repair");
        } else if repair_pins(s, facts, refusals, actions) {
            refresh(recheck, facts, found);
        }
    }
    if opts.reset_floor {
        if !opts.repair {
            refuse(refusals, "--reset-floor", "it is a repair: add --repair");
        } else if reset_floor(s, facts, refusals, actions) {
            refresh(recheck, facts, found);
        }
    }
    if opts.gc && !opts.repair {
        refuse(refusals, "--gc", "it is a repair: add --repair");
    }
    if opts.delete_orphans && !opts.repair {
        refuse(refusals, "--delete-orphans", "it is a repair: add --repair");
    }
    if opts.repair {
        match s.vol.remove_leftovers() {
            Ok(0) => {}
            Ok(n) => {
                actions.push(RepairAction {
                    action: "remove_leftovers",
                    detail: format!("{n} *.tmp and *.open files removed"),
                });
                refresh(recheck, facts, found);
            }
            Err(e) => refuse(refusals, "leftovers", format!("{e}")),
        }
    }
    // The index first: a stale hint is resolved only through it, so what is an
    // orphan can be said only after it is whole again.
    let index_damaged = [
        code::INDEX_RUN,
        code::INDEX_MISSING,
        code::INDEX_LOST,
        code::INDEX_STALE,
        code::INDEX_EXTRA,
        code::MAP_ENTRY_UNRESOLVED,
    ]
    .iter()
    .any(|c| found.has(c));
    if opts.rebuild_index || (opts.repair && index_damaged) {
        if facts.tip.is_none() || !facts.chain_ok {
            refuse(
                refusals,
                "index",
                "the retained chain does not verify, so the segments in use are unknown; repair the chain first",
            );
        } else {
            match indexcheck::rebuild(s, facts) {
                Ok(n) => {
                    actions.push(RepairAction {
                        action: "rebuild_index",
                        detail: format!(
                            "rebuilt from every good segment at or below the high-water mark: {n} entries"
                        ),
                    });
                    refresh(recheck, facts, found);
                }
                Err(e) => refuse(refusals, "index", format!("{e}")),
            }
        }
    }
    if opts.repair && opts.delete_orphans {
        if opts.truncate_tip {
            refuse(
                refusals,
                "--delete-orphans",
                "not together with --truncate-tip: the checkpoint's id_high is zeroed with it and the ids of deleted objects could be reissued; run it after the next mount",
            );
        } else if delete_orphans(s, facts, found, refusals, actions) {
            // The index still names what was deleted: rebuild it from what is left.
            refresh(recheck, facts, found);
            if let Err(e) = indexcheck::rebuild(s, facts) {
                refuse(refusals, "index", format!("after the delete: {e}"));
            }
            refresh(recheck, facts, found);
        }
    }
    if opts.repair && found.has(code::CHECKPOINT) && !opts.truncate_tip {
        rewrite_checkpoint(s, facts, refusals, actions);
    }
    if opts.repair && opts.gc {
        let ctx = GcContext {
            session: s,
            facts,
            repair: true,
        };
        let fs = gc::check_gc(&ctx);
        if !fs.is_empty() {
            for f in fs {
                refusals.push(f);
            }
            actions.push(RepairAction {
                action: "gc",
                detail: "the GC repair ran".to_owned(),
            });
        }
    }
}

/// Returns false when the repair was refused.
fn truncate_tip(
    s: &Session,
    facts: &Facts,
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
    // The previous commit must still be a root: what only it references may
    // already be collected (the rollback floor keeps its commit file, not its
    // data).
    if let Some(t) = &facts.tip {
        if prev.seq < t.full.chain_floor {
            refuse(
                sink,
                what,
                format!(
                    "the previous commit {} is below the tip's chain_floor {}: GC may have collected what only it references",
                    prev.seq.0, t.full.chain_floor.0
                ),
            );
            return false;
        }
    }
    // With retain_last below 2 and a tip that does not authenticate, the
    // previous commit was not a retained root: the walk below proves whether
    // its data is still there (M14 re-review R-2), and the action says so.
    let not_retained =
        facts.tip.is_none() && facts.json.as_ref().is_some_and(|j| j.retain_last < 2);
    let prev_hash = s.commit_codec().object_hash(&prev_bytes);
    if let Err(why) = previous_root_resolves(s, facts, &prev_commit, prev.seq, prev_hash) {
        refuse(
            sink,
            what,
            format!("the previous commit's map does not resolve completely ({why}); nothing was changed"),
        );
        return false;
    }
    let new_floor = RollbackFloor {
        max_commit_seq: prev.seq,
        tip_hash: prev_hash,
    };
    // The floor first: a crash after it leaves the old tip with a lower
    // floor, and a rerun does the same again.
    if let Some(f) = facts.floor {
        if f.max_commit_seq > prev.seq {
            if let Err(e) = s
                .vol
                .reset_rollback_floor(&new_floor, FloorReset::TruncateTip)
            {
                refuse(sink, what, format!("lowering the floor failed: {e}"));
                return false;
            }
            actions.push(RepairAction {
                action: "lower_floor",
                detail: format!(
                    "rollback floor lowered from commit {} to commit {}",
                    f.max_commit_seq.0, prev.seq.0
                ),
            });
        } else if f.max_commit_seq == prev.seq && f.tip_hash != prev_hash {
            refuse(
                sink,
                what,
                "the previous commit is not the commit the floor recorded at its height: the chain forks; decide with --reset-floor",
            );
            return false;
        }
    }
    // What the older cut can no longer cover.
    let mut jn = Sink::default();
    let sc = journal::scan(s, &mut jn);
    let cut = prev_commit.cut_seq.0;
    let first_above = sc.seqs.iter().copied().find(|q| *q > cut);
    let lost = match first_above {
        Some(q) if q > cut + 1 => format!(
            "journal records {}..={} were reclaimed by the damaged tip's cut and are lost; recovery stops at that gap and ignores every record above it (ignored_after_gap)",
            cut + 1,
            q - 1
        ),
        Some(_) => "the journal still holds every record above the new cut".to_owned(),
        None => "the journal holds no record above the new cut; writes between the new tip's cut and the old one, if there were any, are lost".to_owned(),
    };
    if let Err(e) = s.vol.set_aside(tip.seq) {
        // The floor may already be lowered (an action says so): nothing else
        // runs on a volume whose tip is in question.
        refuse(sink, what, format!("{e}"));
        return false;
    }
    if let Err(e) = s.vol.zero() {
        refuse(
            sink,
            "checkpoint",
            format!("the tip was set aside but the checkpoint could not be zeroed: {e}"),
        );
        actions.push(RepairAction {
            action: "truncate_tip",
            detail: format!(
                "commit {} moved aside; the checkpoint is NOT zeroed",
                tip.seq.0
            ),
        });
        return false;
    }
    actions.push(RepairAction {
        action: "truncate_tip",
        detail: format!(
            "commit {} moved aside as {:016x}.damaged, commit {} is the tip, both checkpoint slots zeroed; {lost}{}",
            tip.seq.0,
            tip.seq.0,
            prev.seq.0,
            if not_retained {
                "; retain_last is below 2, so the previous commit was not a retained root: its map was walked and resolves completely"
            } else {
                ""
            }
        ),
    });
    true
}

/// Walks the previous commit's map completely: nothing may be unresolved.
fn previous_root_resolves(
    s: &Session,
    facts: &Facts,
    prev: &lunix_format::commit::Full,
    seq: lunix_format::CommitSeq,
    hash: [u8; 32],
) -> std::result::Result<(), String> {
    let mut f = Facts {
        segments: facts.segments.clone(),
        packs: facts.packs.clone(),
        tip: Some(Tip {
            seq,
            hash,
            full: prev.clone(),
        }),
        retained: vec![Retained {
            seq,
            commit: Commit::Full(prev.clone()),
            hash,
        }],
        ..Facts::default()
    };
    let mut sink = Sink::default();
    mapwalk::walk(s, &mut sink, &mut f);
    let view = indexcheck::open_view(s, &mut sink);
    resolve::run(s, view.as_ref(), &mut sink, &mut f);
    let bad = sink.findings.iter().find(|x| x.severity == Severity::Error);
    match bad {
        Some(x) => Err(format!("{}: {}", x.code, x.message)),
        None if !f.walk.as_ref().is_some_and(|w| w.tip_complete) => {
            Err("the walk did not complete".to_owned())
        }
        None => Ok(()),
    }
}

/// `--repair-pins`: a rejected `state/pins` is replaced from the verified tip.
fn repair_pins(
    s: &Session,
    facts: &Facts,
    sink: &mut Sink,
    actions: &mut Vec<RepairAction>,
) -> bool {
    let what = "--repair-pins";
    if !facts.pins_rejected {
        refuse(sink, what, "state/pins is not rejected; nothing to repair");
        return false;
    }
    let (Some(tip), true) = (&facts.tip, facts.chain_ok) else {
        refuse(sink, what, "the tip or its chain does not verify");
        return false;
    };
    let new = RollbackFloor {
        max_commit_seq: tip.seq,
        tip_hash: tip.hash,
    };
    let why = FloorReset::RepairPins {
        max_keyring_seq: s.keyring_seq,
    };
    match s.vol.reset_rollback_floor(&new, why) {
        Ok(_) => {
            actions.push(RepairAction {
                action: "repair_pins",
                detail: format!(
                    "state/pins replaced; floor is commit {}; max_keyring_seq is {} (the keyring that unlocked the volume); max_epoch and the pin were reset (the next attach pins again)",
                    tip.seq.0, s.keyring_seq
                ),
            });
            true
        }
        Err(e) => {
            refuse(sink, what, format!("{e}"));
            false
        }
    }
}

/// Returns true when the floor changed.
fn reset_floor(
    s: &Session,
    facts: &Facts,
    sink: &mut Sink,
    actions: &mut Vec<RepairAction>,
) -> bool {
    let what = "--reset-floor";
    let Some(tip) = &facts.tip else {
        refuse(
            sink,
            what,
            "the tip does not authenticate; there is no verified tip to set the floor to",
        );
        return false;
    };
    if !facts.chain_ok {
        refuse(sink, what, "the chain does not verify down to chain_floor");
        return false;
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
        return false;
    }
    let r = s.vol.reset_rollback_floor(&new, FloorReset::OperatorReset);
    match r {
        Ok(_) => {
            actions.push(RepairAction {
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
            });
            true
        }
        Err(e) => {
            refuse(sink, what, format!("{e}"));
            false
        }
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
    // Two stores: each writes the older slot, so the damaged one is replaced
    // and, with a crash in between, a valid slot always remains.
    let written = s.vol.store(&slot).and_then(|()| s.vol.store(&slot));
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

/// Returns true when something was deleted.
fn delete_orphans(
    s: &Session,
    facts: &Facts,
    found: &Sink,
    sink: &mut Sink,
    actions: &mut Vec<RepairAction>,
) -> bool {
    let what = "--delete-orphans";
    // What is an orphan is said only from a complete resolution: any error
    // finding (an unresolved entry, a failed walk, a damaged container, ...),
    // a missing index view or a pin that is not understood stops it.
    let worst = found
        .findings
        .iter()
        .find(|f| f.severity == Severity::Error);
    if let Some(f) = worst {
        refuse(
            sink,
            what,
            format!(
                "the checks found an error ({}: {}); repair that first, a segment that a stale hint still reaches must never be deleted",
                f.code, f.message
            ),
        );
        return false;
    }
    let walked = facts.walk.as_ref().is_some_and(|w| w.tip_complete);
    if facts.tip.is_none() || !facts.chain_ok || !walked || !facts.index_ok {
        refuse(
            sink,
            what,
            "the retained roots could not be walked completely or the index could not be read, so nothing can be called an orphan",
        );
        return false;
    }
    if facts.pins_busy {
        refuse(
            sink,
            what,
            "there are snapshot, reader or hold pins (or they cannot be read): fsck does not walk pinned roots yet",
        );
        return false;
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
        return false;
    }
    // Ids are never reused: record the highest id on disk before any delete.
    let hi = highest_id(s, facts);
    let recorded = s.vol.load().ok().flatten().map_or(0, |c| c.id_high);
    if recorded < hi {
        if let Some(slot) = checkpoint_slot(s, facts) {
            if let Err(e) = s.vol.store(&slot) {
                refuse(sink, "checkpoint", format!("{e}"));
                return false;
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
    ns + np > 0
}
