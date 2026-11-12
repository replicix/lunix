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
