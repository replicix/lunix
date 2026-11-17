//! Step 2 and rule R4: the commit chain from the tip down to `chain_floor`,
//! the rollback floor and the fork walk.

use lunix_format::commit::{check_child, check_genesis, Commit};
use lunix_format::local::RollbackFloor;
use lunix_format::CommitSeq;
use lunix_iface::store::CommitStore;
use lunix_iface::ErrorKind;

use crate::code;
use crate::facts::{Facts, Retained, Tip};
use crate::report::{Finding, Sink};
use crate::session::Session;

fn name(seq: CommitSeq) -> String {
    format!("state/commits/{:016x}", seq.0)
}

fn auth_word(kind: ErrorKind) -> &'static str {
    if kind == ErrorKind::Auth {
        "does not authenticate"
    } else {
        "does not decode"
    }
}

pub(crate) fn check(s: &Session, sink: &mut Sink, facts: &mut Facts) {
    let list = match s.vol.list() {
        Ok(l) => l,
        Err(e) => {
            sink.push(Finding::new(code::IO, "state/commits", format!("{e}")));
            return;
        }
    };
    let Some(last) = list.last() else {
        sink.push(Finding::new(
            code::NO_COMMIT,
            "state/commits",
            "the directory holds no commit",
        ));
        return;
    };
    let tip_seq = last.seq;
    let tip_bytes = match s.vol.get(tip_seq) {
        Ok(b) => b,
        Err(e) => {
            sink.push(Finding::new(code::TIP_AUTH, name(tip_seq), format!("{e}")));
            return;
        }
    };
    let tip_commit = match s.commit_codec().open_commit(tip_seq, &tip_bytes) {
        Ok(c) => c,
        Err(e) => {
            sink.push(Finding::new(
                code::TIP_AUTH,
                name(tip_seq),
                format!(
                    "the tip {}; mount refuses and never falls back (repair: --repair --truncate-tip)",
                    auth_word(e.kind())
                ),
            ));
            return;
        }
    };
    facts.stats.commits += 1;
    let Commit::Full(tip_full) = tip_commit else {
        sink.push(Finding::new(
            code::TIP_NOT_FULL,
            name(tip_seq),
            "the tip is not a FULL commit",
        ));
        return;
    };
    let tip_hash = s.commit_codec().object_hash(&tip_bytes);
    let floor_seq = tip_full.chain_floor;
    if floor_seq > tip_seq {
        sink.push(Finding::new(
            code::CHAIN_FLOOR_ABOVE_TIP,
            name(tip_seq),
            format!("chain_floor {} is above the tip {}", floor_seq.0, tip_seq.0),
        ));
        return;
    }

    // The chain, tip first.
    let mut chain = vec![Retained {
        seq: tip_seq,
        commit: Commit::Full(tip_full.clone()),
        hash: tip_hash,
    }];
    let mut complete = true;
    let mut child_common = tip_full.common;
    let mut created_decrease: Option<u64> = None;
    let mut seq = tip_seq;
    while seq > floor_seq {
        let parent_seq = CommitSeq(seq.0 - 1);
        let bytes = match s.vol.get(parent_seq) {
            Ok(b) => b,
            Err(e) => {
                sink.push(Finding::new(
                    code::COMMIT_MISSING,
                    name(parent_seq),
                    format!(
                        "commit {} is at or above chain_floor {} and cannot be read: {e}",
                        parent_seq.0, floor_seq.0
                    ),
                ));
                complete = false;
                break;
            }
        };
        let parent = match s.commit_codec().open_commit(parent_seq, &bytes) {
            Ok(c) => c,
            Err(e) => {
                sink.push(Finding::new(
                    code::COMMIT_AUTH,
                    name(parent_seq),
                    format!("a retained commit {}", auth_word(e.kind())),
                ));
                complete = false;
                break;
            }
        };
        facts.stats.commits += 1;
        let hash = s.commit_codec().object_hash(&bytes);
        if let Err(e) = check_child(parent.common(), &hash, &child_common) {
            sink.push(Finding::new(
                code::CHAIN_BROKEN,
                name(parent_seq),
                format!("the link to commit {} is wrong: {e}", seq.0),
            ));
            complete = false;
            break;
        }
        if parent.common().created_at > child_common.created_at && created_decrease.is_none() {
            created_decrease = Some(seq.0);
        }
        child_common = *parent.common();
        chain.push(Retained {
            seq: parent_seq,
            commit: parent,
            hash,
        });
        seq = parent_seq;
    }
    if complete && floor_seq == CommitSeq(0) {
        if let Err(e) = check_genesis(&child_common, &s.volume_hash) {
            sink.push(Finding::new(
                code::CHAIN_BROKEN,
                name(CommitSeq(0)),
                format!("the genesis commit does not name this volume: {e}"),
            ));
            complete = false;
        }
    }
    if let Some(at) = created_decrease {
        sink.push(Finding::new(
            code::CREATED_AT,
            name(CommitSeq(at)),
            format!("created_at of commit {at} is below its parent's"),
        ));
    }
    chain.reverse();
    facts.chain_ok = complete;
    facts.retained = chain;
    facts.tip = Some(Tip {
        seq: tip_seq,
        hash: tip_hash,
        full: tip_full.clone(),
    });

    // chain_floor against retain_last.
    if let Some(json) = &facts.json {
        let keep = u64::from(json.retain_last.max(1));
        let expected = tip_seq.0.saturating_sub(keep - 1);
        if floor_seq.0 != expected {
            sink.push(Finding::new(
                code::CHAIN_FLOOR,
                name(tip_seq),
                format!(
                    "chain_floor is {}, the older of {} retained commits would be {}",
                    floor_seq.0, keep, expected
                ),
            ));
        }
    }

    // The size must be a whole number of chunks.
    if !tip_full.size.is_multiple_of(u64::from(s.desc.chunk_size)) {
        sink.push(Finding::new(
            code::SIZE,
            name(tip_seq),
            format!(
                "size {} is not a multiple of the chunk size {}",
                tip_full.size, s.desc.chunk_size
            ),
        ));
    }

    // Commit files below the floor are legal only while condemned; one that
    // no longer authenticates is a warning (it matters only to the R4 walk).
    let mut below: std::collections::BTreeMap<CommitSeq, Retained> =
        std::collections::BTreeMap::new();
    for info in list.iter().filter(|i| i.seq < floor_seq) {
        let opened = s.vol.get(info.seq).ok().and_then(|b| {
            let c = s.commit_codec().open_commit(info.seq, &b).ok()?;
            Some((c, s.commit_codec().object_hash(&b)))
        });
        match opened {
            Some((c, hash)) => {
                facts.stats.commits += 1;
                // Still on disk until GC deletes it: what it names is not an
                // orphan, only garbage that GC has not collected yet.
                if let Commit::Full(f) = &c {
                    facts.named_segments.extend(f.new_segments.iter().copied());
                    facts.named_packs.extend(f.map_packs.iter().copied());
                    facts.named_packs.extend(f.map_root.pack);
                }
                below.insert(
                    info.seq,
                    Retained {
                        seq: info.seq,
                        commit: c,
                        hash,
                    },
                );
            }
            None => sink.push(Finding::new(
                code::COMMIT_BELOW_FLOOR,
                name(info.seq),
                "a commit below chain_floor does not authenticate",
            )),
        }
    }

    // Names, for the orphan check.
    for r in &facts.retained {
        if let Commit::Full(f) = &r.commit {
            facts.named_segments.extend(f.new_segments.iter().copied());
            facts.named_packs.extend(f.map_packs.iter().copied());
            facts.named_packs.extend(f.map_root.pack);
            facts.high_water = facts.high_water.max(
                f.new_segments
                    .iter()
                    .chain(&f.map_packs)
                    .chain(&f.index_packs)
                    .chain(f.map_root.pack.iter())
                    .map(|i| i.raw())
                    .max()
                    .unwrap_or(0),
            );
        }
    }

    gc_window(sink, facts, floor_seq, below);
    rollback(s, sink, facts, tip_seq, &tip_hash);
}

/// GC's retained set reaches below `chain_floor` (N24): the FULLs from the
/// lowest `chain_floor` a retained FULL records, linked down from the floor
/// commit (a missing or unlinked one ends the walk, as in `lunix-gc`). They
/// are roots for the walk and the orphan check. Commits further down that the
/// rollback floor keeps are reported once as a note (the window of ADR-31).
fn gc_window(
    sink: &mut Sink,
    facts: &mut Facts,
    floor_seq: CommitSeq,
    mut below: std::collections::BTreeMap<CommitSeq, Retained>,
) {
    if !facts.chain_ok {
        return;
    }
    let gc_floor = facts
        .retained
        .iter()
        .filter_map(|r| match &r.commit {
            Commit::Full(f) => Some(f.chain_floor),
            _ => None,
        })
        .min()
        .unwrap_or(floor_seq);
    facts.gc_floor = gc_floor.min(floor_seq);
    let mut child = facts.retained.first().map(|r| *r.commit.common());
    let mut seq = floor_seq;
    let mut extra = Vec::new();
    while seq > gc_floor {
        seq = CommitSeq(seq.0 - 1);
        let Some(r) = below.remove(&seq) else {
            break;
        };
        if child.is_some_and(|c| check_child(r.commit.common(), &r.hash, &c).is_err()) {
            break;
        }
        child = Some(*r.commit.common());
        extra.push(r);
    }
    extra.reverse();
    let lowest_kept = extra.first().map_or(floor_seq, |r| r.seq);
    facts.gc_extra = extra;
    let window: Vec<CommitSeq> = below
        .keys()
        .copied()
        .filter(|&q| q < lowest_kept && facts.floor.is_some_and(|f| q >= f.max_commit_seq))
        .collect();
    if let (Some(lo), Some(hi)) = (window.first(), window.last()) {
        sink.push(Finding::new(
            code::FLOOR_WINDOW,
            "state/commits",
            format!(
                "commits {}..={} lie between the rollback floor and GC's retained set: kept as chain links only, what they name is not checked",
                lo.0, hi.0
            ),
        ));
    }
}

/// Rule R4 against the floor in `state/pins`.
fn rollback(s: &Session, sink: &mut Sink, facts: &Facts, tip_seq: CommitSeq, tip_hash: &[u8; 32]) {
    let Some(f) = facts.floor else {
        sink.push(Finding::new(
            code::NO_FLOOR,
            "state/pins",
            "no rollback floor is recorded on this host",
        ));
        return;
    };
    if tip_seq < f.max_commit_seq {
        sink.push(Finding::new(
            code::ROLLBACK,
            "state/pins",
            format!(
                "the tip is commit {} but the floor recorded commit {}; a floor above the tip is never repaired silently (--repair --truncate-tip or --reset-floor, after you have decided which copy is right)",
                tip_seq.0, f.max_commit_seq.0
            ),
        ));
        return;
    }
    if tip_seq == f.max_commit_seq {
        if *tip_hash != f.tip_hash {
            sink.push(Finding::new(
                code::FORK,
                "state/pins",
                format!(
                    "commit {} is not the commit the floor recorded at that height",
                    tip_seq.0
                ),
            ));
        }
        return;
    }
    fork_walk(s, sink, facts, tip_seq, &f);
}

/// The tip is above the floor: it must descend from the floor's commit.
fn fork_walk(s: &Session, sink: &mut Sink, facts: &Facts, tip_seq: CommitSeq, f: &RollbackFloor) {
    let unverifiable = |why: String| {
        Finding::new(
            code::FORK_UNVERIFIABLE,
            "state/pins",
            format!(
                "the walk from commit {} down to the floor's commit {} cannot be completed: {why}",
                tip_seq.0, f.max_commit_seq.0
            ),
        )
    };
    // Start from the verified chain.
    let Some(first) = facts.retained.first() else {
        return;
    };
    if !facts.chain_ok {
        // The chain failure is its own finding; the walk cannot add to it.
        return;
    }
    let hash_at_floor = if f.max_commit_seq >= first.seq {
        facts
            .retained
            .iter()
            .find(|r| r.seq == f.max_commit_seq)
            .map(|r| r.hash)
    } else {
        // Below chain_floor: keep walking, every link authenticated.
        let mut child = *first.commit.common();
        let mut seq = first.seq;
        let mut found = None;
        while seq > f.max_commit_seq {
            let parent_seq = CommitSeq(seq.0 - 1);
            let bytes = match s.vol.get(parent_seq) {
                Ok(b) => b,
                Err(e) => {
                    sink.push(unverifiable(format!(
                        "commit {} is missing ({e})",
                        parent_seq.0
                    )));
                    return;
                }
            };
            let parent = match s.commit_codec().open_commit(parent_seq, &bytes) {
                Ok(c) => c,
                Err(_) => {
                    sink.push(unverifiable(format!(
                        "commit {} does not authenticate",
                        parent_seq.0
                    )));
                    return;
                }
            };
            let hash = s.commit_codec().object_hash(&bytes);
            if check_child(parent.common(), &hash, &child).is_err() {
                sink.push(unverifiable(format!(
                    "the link below commit {} is wrong",
                    seq.0
                )));
                return;
            }
            child = *parent.common();
            seq = parent_seq;
            if seq == f.max_commit_seq {
                found = Some(hash);
            }
        }
        found
    };
    if hash_at_floor != Some(f.tip_hash) {
        sink.push(Finding::new(
            code::FORK,
            "state/pins",
            format!(
                "the chain does not pass through the commit the floor recorded at height {}",
                f.max_commit_seq.0
            ),
        ));
    }
}
