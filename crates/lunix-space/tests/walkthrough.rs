//! The accounting walk-through of `lunix_iface::space`, replayed: admission,
//! C2 headroom, C4 true-up, C5/C6/C8 transfers, C9 release, GC delete. The
//! ledger must equal the sum of the surviving files.

mod common;

use std::sync::Arc;

use lunix_format::journal::RecordKind;
use lunix_format::local::OnFull;
use lunix_iface::journal::record_bytes;
use lunix_iface::space::{ReserveOutcome, SpaceGate, SpaceKind};
use lunix_space::LunixSpace;

use common::{clock, config, gate_cfg, settings};

const MIB: u64 = 1 << 20;

fn by(g: &LunixSpace, k: SpaceKind) -> u64 {
    g.snapshot().by_kind[k.index()]
}

/// What is on disk: the ledger must equal it once nothing is in flight.
#[derive(Default, Debug)]
struct Files {
    segments: Vec<u64>,
    map_packs: Vec<u64>,
    commits: Vec<u64>,
    index_runs: Vec<u64>,
    journal_records: Vec<u64>,
}

fn assert_ledger_is_files(g: &LunixSpace, f: &Files) {
    let snap = g.snapshot();
    assert_eq!(
        snap.by_kind[SpaceKind::Segments.index()],
        f.segments.iter().sum::<u64>()
    );
    assert_eq!(
        snap.by_kind[SpaceKind::Map.index()],
        f.map_packs.iter().sum::<u64>()
    );
    assert_eq!(
        snap.by_kind[SpaceKind::Commits.index()],
        f.commits.iter().sum::<u64>()
    );
    assert_eq!(
        snap.by_kind[SpaceKind::Index.index()],
        f.index_runs.iter().sum::<u64>()
    );
    assert_eq!(
        snap.by_kind[SpaceKind::JournalReserved.index()],
        f.journal_records.iter().sum::<u64>()
    );
    assert_eq!(snap.by_kind[SpaceKind::Headroom.index()], 0);
    assert_eq!(snap.by_kind[SpaceKind::Cache.index()], 0);
}

fn new_gate() -> Arc<LunixSpace> {
    let mut cfg = config(settings(64 * MIB, OnFull::Error, None));
    cfg.segment_size = 16 * MIB;
    gate_cfg(8 * 1024 * MIB, 0, cfg, clock()).0
}

/// One cut: `writes` admitted, destaged into segments, map pack, commit and
/// index run, journal reclaimed.
fn run_cut(g: &LunixSpace, f: &mut Files, writes: &[(RecordKind, u32)], seg_actual: &[u64]) {
    // 1. Admission, in encoded record length. A refusal by the lane is rolled back.
    let mut recs = vec![];
    for (kind, blocks) in writes {
        let b = record_bytes(*kind, *blocks);
        assert!(matches!(
            g.reserve(SpaceKind::JournalReserved, b, None).unwrap(),
            ReserveOutcome::Reserved
        ));
        recs.push(b);
    }
    let rolled_back = record_bytes(RecordKind::Write, 256);
    g.reserve(SpaceKind::JournalReserved, rolled_back, None)
        .unwrap();
    g.release(SpaceKind::JournalReserved, rolled_back); // lane said Full
    f.journal_records.extend(&recs);
    assert_ledger_is_files_journal(g, f);

    // 2. C2: bounds for the segments and for the commit-time objects.
    let seg_bound: u64 = seg_actual.iter().map(|a| a + a / 3 + 4096).sum();
    let (map_b, commit_b, index_b) = (3 * MIB, MIB, 2 * MIB);
    let before_used = g.snapshot().used();
    g.reserve_headroom(SpaceKind::Segments, seg_bound).unwrap();
    g.reserve_headroom(SpaceKind::Headroom, map_b + commit_b + index_b)
        .unwrap();
    assert_eq!(
        g.snapshot().used(),
        before_used + seg_bound,
        "bounds are conservative"
    );
    assert_eq!(by(g, SpaceKind::Headroom), map_b + commit_b + index_b);

    // 3. C4: each sealed segment trues its share of the bound up to its size.
    let mut bound_left = seg_bound;
    for a in seg_actual {
        let share = a + a / 3 + 4096;
        g.transfer(SpaceKind::Segments, SpaceKind::Segments, share, *a)
            .unwrap();
        bound_left -= share;
        f.segments.push(*a);
    }
    assert_eq!(bound_left, 0);

    // 4. C5, C6, C8.
    let (map_a, commit_a, index_a) = (2 * MIB + 17, 9_000, MIB + 5);
    g.transfer(SpaceKind::Headroom, SpaceKind::Map, map_b, map_a)
        .unwrap();
    f.map_packs.push(map_a);
    g.transfer(SpaceKind::Headroom, SpaceKind::Commits, commit_b, commit_a)
        .unwrap();
    f.commits.push(commit_a);
    g.transfer(SpaceKind::Headroom, SpaceKind::Index, index_b, index_a)
        .unwrap();
    f.index_runs.push(index_a);
    assert_eq!(by(g, SpaceKind::Headroom), 0);

    // 5. C9: the cut is durable; the charges of its records go (per cut).
    let reclaimed: u64 = recs.iter().sum();
    g.release(SpaceKind::JournalReserved, reclaimed);
    for r in &recs {
        let pos = f.journal_records.iter().position(|x| x == r).unwrap();
        f.journal_records.remove(pos);
    }
}

fn assert_ledger_is_files_journal(g: &LunixSpace, f: &Files) {
    assert_eq!(
        by(g, SpaceKind::JournalReserved),
        f.journal_records.iter().sum::<u64>()
    );
}

#[test]
fn the_walkthrough_leaves_the_ledger_equal_to_the_files() {
    let g = new_gate();
    let mut f = Files::default();
    run_cut(
        &g,
        &mut f,
        &[
            (RecordKind::Write, 1),
            (RecordKind::Write, 256),
            (RecordKind::Hole, 0),
            (RecordKind::Write, 37),
        ],
        &[5 * MIB + 3, 16 * MIB],
    );
    assert_ledger_is_files(&g, &f);
    run_cut(&g, &mut f, &[(RecordKind::Write, 64)], &[MIB / 2]);
    assert_ledger_is_files(&g, &f);

    // GC: a compaction output is charged from headroom like a cut's segments
    // and trued up at seal, then the inputs are deleted (release per file,
    // after the delete is durable).
    let out_bound = 4 * MIB;
    g.reserve_headroom(SpaceKind::Segments, out_bound).unwrap();
    g.transfer(
        SpaceKind::Segments,
        SpaceKind::Segments,
        out_bound,
        3 * MIB + 1,
    )
    .unwrap();
    f.segments.push(3 * MIB + 1);
    let gone_a = f.segments.remove(0);
    g.release(SpaceKind::Segments, gone_a);
    let gone_b = f.segments.remove(0);
    g.release(SpaceKind::Segments, gone_b);
    // An old index run and an old commit are deleted too.
    let r = f.index_runs.remove(0);
    g.release(SpaceKind::Index, r);
    let c = f.commits.remove(0);
    g.release(SpaceKind::Commits, c);
    assert_ledger_is_files(&g, &f);

    // Startup accounting of the same files gives the same ledger.
    let g2 = new_gate();
    for s in &f.segments {
        g2.charge(SpaceKind::Segments, *s).unwrap();
    }
    for s in &f.map_packs {
        g2.charge(SpaceKind::Map, *s).unwrap();
    }
    for s in &f.commits {
        g2.charge(SpaceKind::Commits, *s).unwrap();
    }
    for s in &f.index_runs {
        g2.charge(SpaceKind::Index, *s).unwrap();
    }
    assert_eq!(g2.snapshot().by_kind, g.snapshot().by_kind);
    assert_eq!(g2.snapshot().used(), g.snapshot().used());
}

#[test]
fn an_unused_bound_is_released_at_c9() {
    let g = new_gate();
    g.reserve_headroom(SpaceKind::Headroom, 10 * MIB).unwrap();
    g.transfer(SpaceKind::Headroom, SpaceKind::Map, 3 * MIB, MIB)
        .unwrap();
    // The commit was never created (an aborted cut): the remainder goes back.
    g.release(SpaceKind::Headroom, 7 * MIB);
    assert_eq!(by(&g, SpaceKind::Headroom), 0);
    assert_eq!(g.snapshot().used(), MIB);
}

#[test]
fn a_write_is_refused_at_the_limit_but_a_commit_still_fits() {
    let mut cfg = config(settings(10 * MIB, OnFull::Error, None));
    cfg.segment_size = 2 * MIB; // region = 14 MiB
    let g = gate_cfg(100 * MIB, 0, cfg, clock()).0;
    let limit = g.snapshot().limit;
    assert_eq!(limit, 86 * MIB);
    assert!(matches!(
        g.reserve(SpaceKind::JournalReserved, limit, None).unwrap(),
        ReserveOutcome::Reserved
    ));
    assert!(matches!(
        g.reserve(SpaceKind::JournalReserved, 4096, None).unwrap(),
        ReserveOutcome::NoSpace
    ));
    // C2 of the cut that will free the journal: segment bound + object bounds.
    g.reserve_headroom(SpaceKind::Segments, 4 * MIB).unwrap();
    g.reserve_headroom(SpaceKind::Headroom, 6 * MIB).unwrap();
    g.transfer(SpaceKind::Headroom, SpaceKind::Commits, MIB, 4096)
        .unwrap();
    assert!(
        g.snapshot().used() > limit,
        "the ledger exceeds the limit by <= region"
    );
    assert!(g.snapshot().used() <= limit + g.snapshot().headroom_total);
}
