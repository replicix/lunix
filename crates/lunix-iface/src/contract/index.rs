//! Index contract tests. See the [`ChunkIndex`] docs.

use lunix_format::{ChunkId, Hint, IdLen, IdPrefix, ObjId, SegId};

use crate::index::{ChunkIndex, Confirm};
use crate::store::SegmentFooters;

/// An index under test, with a footer source for `catch_up` and `rebuild`.
pub trait IndexFixture {
    /// The index.
    fn index(&self) -> &dyn ChunkIndex;

    /// The footers the index may read.
    fn footers(&self) -> &dyn SegmentFooters;

    /// Registers sealed segment `seg` holding these extents, so that its
    /// footer exists. Does not touch the index.
    fn add_segment(&mut self, seg: SegId, entries: &[(ChunkId, Hint)]);

    /// Simulates a crash and restart: everything not persisted is lost, the
    /// persisted runs and manifest are reopened, footers stay.
    fn reopen(&mut self);
}

fn seg(n: u64) -> SegId {
    ObjId::new(0, n).expect("id")
}

fn id(n: u8) -> ChunkId {
    let mut b = [0u8; 32];
    b[0] = n;
    b[1] = n.wrapping_mul(7);
    b[31] = n;
    ChunkId(b)
}

fn hint(s: u64, off: u32) -> Hint {
    Hint {
        seg: seg(s),
        off,
        clen: 500,
    }
}

/// An inserted chunk is found for dedup, marked verified.
pub fn insert_then_dedup_hit_is_verified<F: IndexFixture>(f: &mut F) {
    let ix = f.index();
    assert_eq!(ix.get_for_dedup(&id(1)).expect("dedup"), None);
    ix.insert(id(1), hint(1, 0)).expect("insert");
    let hit = ix.get_for_dedup(&id(1)).expect("dedup").expect("hit");
    assert_eq!(hit.hint, hint(1, 0));
    assert_eq!(hit.confirm, Confirm::Verified);
    let got = ix.get(&id(1)).expect("get").expect("entry");
    assert_eq!(
        (got.id, got.hint, got.condemned),
        (id(1), hint(1, 0), false)
    );
}

/// A prefix lookup returns candidates with their full ids.
pub fn prefix_lookup_returns_full_ids<F: IndexFixture>(f: &mut F) {
    let ix = f.index();
    ix.insert(id(1), hint(1, 0)).expect("insert");
    ix.insert(id(2), hint(1, 600)).expect("insert");
    let p = IdPrefix::of(&id(1), IdLen::Sixteen);
    let found = ix.get_by_prefix(&p).expect("prefix");
    assert!(found.iter().any(|c| c.id == id(1) && c.hint == hint(1, 0)));
    assert!(found.iter().all(|c| p.matches(&c.id)));
}

/// A condemned segment's entries are never returned for dedup, also after a
/// reopen, but stay readable through `get`.
pub fn condemned_entries_never_dedup<F: IndexFixture>(f: &mut F) {
    f.index().insert(id(1), hint(1, 0)).expect("insert");
    f.index().persist(seg(1)).expect("persist");
    f.index().mark_condemned(seg(1)).expect("condemn");
    assert_eq!(f.index().get_for_dedup(&id(1)).expect("dedup"), None);
    let got = f.index().get(&id(1)).expect("get").expect("entry");
    assert!(got.condemned);
    assert_eq!(f.index().condemned().expect("list"), vec![seg(1)]);
    f.reopen();
    assert_eq!(f.index().get_for_dedup(&id(1)).expect("dedup"), None);
    f.index().unmark_condemned(seg(1)).expect("unmark");
    assert!(f.index().get_for_dedup(&id(1)).expect("dedup").is_some());
}

/// Relocation applies when `old` is current and reports false when it is not.
pub fn relocate_follows_newest_wins<F: IndexFixture>(f: &mut F) {
    let ix = f.index();
    ix.insert(id(1), hint(1, 0)).expect("insert");
    assert!(ix
        .relocate(&id(1), &hint(1, 0), &hint(5, 64))
        .expect("relocate"));
    assert_eq!(ix.get(&id(1)).expect("get").expect("e").hint, hint(5, 64));
    assert!(!ix
        .relocate(&id(1), &hint(1, 0), &hint(6, 0))
        .expect("relocate"));
    assert_eq!(ix.get(&id(1)).expect("get").expect("e").hint, hint(5, 64));
}

/// After persist and a restart, a hit comes from a run and is `Hinted`.
pub fn persisted_hits_are_hinted<F: IndexFixture>(f: &mut F) {
    f.index().insert(id(1), hint(1, 0)).expect("insert");
    let runs = f.index().persist(seg(1)).expect("persist");
    assert!(!runs.is_empty());
    assert_eq!(f.index().runs().expect("runs").len(), runs.len());
    f.reopen();
    let hit = f
        .index()
        .get_for_dedup(&id(1))
        .expect("dedup")
        .expect("hit");
    assert_eq!(hit.confirm, Confirm::Hinted);
    assert_eq!(hit.hint, hint(1, 0));
}

/// A relocated entry is not written to a run until `persist` is told its new
/// segment is named by a durable commit; a crash before that keeps the old one.
pub fn relocated_entries_wait_for_their_segment<F: IndexFixture>(f: &mut F) {
    f.index().insert(id(1), hint(1, 0)).expect("insert");
    f.index().persist(seg(1)).expect("persist");
    assert!(f
        .index()
        .relocate(&id(1), &hint(1, 0), &hint(5, 64))
        .expect("relocate"));
    f.index().persist(seg(3)).expect("persist");
    f.reopen();
    assert_eq!(
        f.index().get(&id(1)).expect("get").expect("e").hint,
        hint(1, 0),
        "the old entry is still current after a crash"
    );
    assert!(f
        .index()
        .relocate(&id(1), &hint(1, 0), &hint(5, 64))
        .expect("relocate"));
    f.index().persist(seg(5)).expect("persist");
    f.reopen();
    assert_eq!(
        f.index().get(&id(1)).expect("get").expect("e").hint,
        hint(5, 64)
    );
}

/// Dropping a segment removes its entries from lookups at once.
pub fn drop_segment_removes_entries<F: IndexFixture>(f: &mut F) {
    let ix = f.index();
    ix.insert(id(1), hint(1, 0)).expect("insert");
    ix.insert(id(2), hint(2, 0)).expect("insert");
    ix.drop_segment(seg(1)).expect("drop");
    assert_eq!(ix.get(&id(1)).expect("get"), None);
    assert_eq!(ix.get_for_dedup(&id(1)).expect("dedup"), None);
    assert!(ix.get(&id(2)).expect("get").is_some());
}

/// `mark_condemned_all` and `retire_segments` do what the single calls do,
/// for any number of segments, idempotently, and the result survives a
/// reopen: a retired segment's entries are gone and its flag is cleared; a
/// flagged one stays readable and out of dedup.
pub fn batch_updates_equal_the_single_calls<F: IndexFixture>(f: &mut F) {
    for n in 1..=3u8 {
        f.index()
            .insert(id(n), hint(u64::from(n), 0))
            .expect("insert");
    }
    f.index().persist(seg(3)).expect("persist");
    f.index().mark_condemned_all(&[]).expect("empty condemn");
    f.index().retire_segments(&[]).expect("empty retire");
    f.index()
        .mark_condemned_all(&[seg(1), seg(2)])
        .expect("condemn");
    f.index()
        .mark_condemned_all(&[seg(2), seg(1)])
        .expect("condemn again");
    for reopen in [false, true] {
        if reopen {
            f.reopen();
        }
        let ix = f.index();
        assert_eq!(ix.condemned().expect("list"), vec![seg(1), seg(2)]);
        assert_eq!(ix.get_for_dedup(&id(1)).expect("dedup"), None);
        assert_eq!(ix.get_for_dedup(&id(2)).expect("dedup"), None);
        assert!(ix.get_for_dedup(&id(3)).expect("dedup").is_some());
        assert!(ix.get(&id(2)).expect("get").expect("entry").condemned);
    }
    // `unmark_condemned_all`: an empty set, an unflagged segment and a
    // duplicate are harmless; the entries stay and dedup sees them again.
    f.index().unmark_condemned_all(&[]).expect("empty unmark");
    f.index()
        .unmark_condemned_all(&[seg(1), seg(3), seg(1)])
        .expect("unmark");
    for reopen in [false, true] {
        if reopen {
            f.reopen();
        }
        let ix = f.index();
        assert_eq!(ix.condemned().expect("list"), vec![seg(2)]);
        assert!(ix.get_for_dedup(&id(1)).expect("dedup").is_some());
        assert_eq!(ix.get_for_dedup(&id(2)).expect("dedup"), None);
    }
    f.index().mark_condemned_all(&[seg(1)]).expect("flag again");
    f.index().retire_segments(&[seg(1)]).expect("retire one");
    assert_eq!(f.index().get(&id(1)).expect("get"), None);
    assert_eq!(f.index().condemned().expect("list"), vec![seg(2)]);
    assert!(f.index().get(&id(2)).expect("get").is_some());
    // An unflagged segment retires too, and retiring twice is harmless.
    f.index()
        .retire_segments(&[seg(2), seg(3), seg(1)])
        .expect("retire rest");
    for reopen in [false, true] {
        if reopen {
            f.reopen();
        }
        let ix = f.index();
        assert!(ix.condemned().expect("list").is_empty());
        for n in 1..=3u8 {
            assert_eq!(ix.get(&id(n)).expect("get"), None, "entry {n} is gone");
        }
    }
}

/// `catch_up` ingests the footers of the named segments only, and drops
/// entries whose segment is gone.
pub fn catch_up_ingests_only_named_segments<F: IndexFixture>(f: &mut F) {
    f.add_segment(seg(2), &[(id(10), hint(2, 0)), (id(11), hint(2, 600))]);
    f.add_segment(seg(3), &[(id(12), hint(3, 0))]);
    let report = f
        .index()
        .catch_up(&[seg(2)], f.footers())
        .expect("catch_up");
    assert_eq!(report.inserted, 2);
    assert!(f.index().get(&id(10)).expect("get").is_some());
    assert!(f.index().get(&id(11)).expect("get").is_some());
    assert_eq!(
        f.index().get(&id(12)).expect("get"),
        None,
        "unnamed segments are not ingested"
    );

    f.index().insert(id(20), hint(9, 0)).expect("insert");
    let report = f
        .index()
        .catch_up(&[seg(2), seg(9)], f.footers())
        .expect("catch_up");
    assert_eq!(
        report.dropped, 1,
        "an entry of a missing segment is dropped"
    );
    assert_eq!(f.index().get(&id(20)).expect("get"), None);
}

/// A rebuild from footers equals what incremental inserts produced.
pub fn rebuild_equals_incremental<F: IndexFixture>(f: &mut F) {
    let entries = [
        (id(1), hint(1, 0)),
        (id(2), hint(1, 600)),
        (id(3), hint(2, 0)),
    ];
    f.add_segment(seg(1), &entries[..2]);
    f.add_segment(seg(2), &entries[2..]);
    for (i, h) in &entries {
        f.index().insert(*i, *h).expect("insert");
    }
    let mut before = Vec::new();
    f.index()
        .iter_entries(&mut |e| before.push((e.id, e.hint)))
        .expect("iter");
    f.index()
        .rebuild(&[seg(1), seg(2)], f.footers())
        .expect("rebuild");
    let mut after = Vec::new();
    f.index()
        .iter_entries(&mut |e| after.push((e.id, e.hint)))
        .expect("iter");
    before.sort_by_key(|e| e.0);
    after.sort_by_key(|e| e.0);
    assert_eq!(before, after);
    assert_eq!(after.len(), 3);
}

/// `all_runs` lists every run file, a superset of the manifest's `runs()`, and a
/// deleted run disappears from both.
pub fn all_runs_lists_every_run_file<F: IndexFixture>(f: &mut F) {
    f.index().insert(id(1), hint(1, 0)).expect("insert");
    let made = f.index().persist(seg(1)).expect("persist");
    assert!(!made.is_empty());
    let all = f.index().all_runs().expect("all_runs");
    for r in f.index().runs().expect("runs") {
        assert!(
            all.iter().any(|a| a.id == r.id),
            "named run {:?} is listed",
            r.id
        );
    }
    f.index().delete_run(made[0]).expect("delete");
    assert!(!f
        .index()
        .all_runs()
        .expect("all_runs")
        .iter()
        .any(|a| a.id == made[0].id));
    assert!(!f
        .index()
        .runs()
        .expect("runs")
        .iter()
        .any(|a| a.id == made[0].id));
}

/// Deleting a run file never fails a lookup that is running at the time: a
/// reader sees the run's entries or the index without it, never an error and
/// never an entry it should not have. (The collector deletes runs no manifest
/// names while helper threads look chunks up.) A run that no manifest names is
/// deleted when there is one; otherwise a named one is, and then its entries
/// may disappear for lookups that start after the manifest changed.
pub fn delete_run_never_fails_a_lookup_in_flight<F: IndexFixture>(f: &mut F) {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    const ROUNDS: u8 = 6;
    const PER_ROUND: u8 = 12;
    let ix = f.index();
    let mut expect: Vec<(ChunkId, Hint)> = Vec::new();
    for round in 0..ROUNDS {
        let s = u64::from(round) + 1;
        for k in 0..PER_ROUND {
            let n = round * PER_ROUND + k + 1;
            let h = hint(s, u32::from(k) * 600);
            ix.insert(id(n), h).expect("insert");
            expect.push((id(n), h));
        }
        ix.persist(seg(s)).expect("persist");
    }
    let named: Vec<_> = ix.runs().expect("runs");
    let all = ix.all_runs().expect("all_runs");
    let dead = all
        .iter()
        .find(|r| !named.iter().any(|n| n.id == r.id))
        .copied();
    let victim = dead.or_else(|| named.first().copied()).expect("a run");
    let must_stay = dead.is_some();

    let stop = AtomicBool::new(false);
    let warm = AtomicUsize::new(0);
    std::thread::scope(|scope| {
        let readers: Vec<_> = (0..3)
            .map(|t| {
                let (expect, stop, warm) = (&expect, &stop, &warm);
                scope.spawn(move || {
                    let mut looked = 0u64;
                    let mut counted = false;
                    while !stop.load(Ordering::Acquire) {
                        // Point lookups.
                        for (cid, h) in expect {
                            match ix.get(cid).expect("a lookup never fails") {
                                Some(c) => assert_eq!(c.hint, *h, "the right entry"),
                                None => assert!(!must_stay, "an entry of a live run vanished"),
                            }
                            looked += 1;
                        }
                        // A scan holds the runs it cloned for its whole walk.
                        if t == 0 {
                            let mut seen = 0usize;
                            ix.iter_entries(&mut |c| {
                                seen += 1;
                                assert!(
                                    expect.iter().any(|(i, h)| *i == c.id && *h == c.hint),
                                    "an entry nobody inserted"
                                );
                            })
                            .expect("a scan never fails");
                            assert!(!must_stay || seen == expect.len(), "{seen} entries");
                        }
                        if !counted {
                            counted = true;
                            warm.fetch_add(1, Ordering::AcqRel);
                        }
                    }
                    looked
                })
            })
            .collect();
        // Delete while every reader is mid-loop.
        while warm.load(Ordering::Acquire) < 3 {
            std::thread::yield_now();
        }
        let freed = ix.delete_run(victim).expect("delete the run");
        assert!(freed > 0 || dead.is_none());
        // Let the readers run through the moment after the unlink as well.
        for _ in 0..50 {
            std::thread::yield_now();
        }
        stop.store(true, Ordering::Release);
        for r in readers {
            assert!(r.join().expect("a reader panicked") > 0);
        }
    });
    assert!(!ix
        .all_runs()
        .expect("all_runs")
        .iter()
        .any(|r| r.id == victim.id));
}
