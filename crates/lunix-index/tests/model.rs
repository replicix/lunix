//! Model test: random sequences against a map oracle, with crashes, damage to
//! the newest run or manifest, and `catch_up` from segment footers.
//!
//! What the oracle says after a crash is not "what was persisted": the engine
//! always runs `catch_up` with the segments the commit chain names, so the
//! index must then equal the fold of those segments' footers, highest segment
//! id winning, whatever the runs, the manifest or the damage were.
//!
//! The directory lives on tmpfs when `/dev/shm` exists: the sequences are
//! about logic, not about `fsync`, and 5 000 of them would take hours otherwise.

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::sync::Arc;

use lunix_format::{ChunkId, Hint, IdLen, IdPrefix};
use lunix_iface::index::ChunkIndex;
use lunix_index::LsmIndex;
use proptest::prelude::*;

use common::{cid, hint, open_with, seg, Footers, Ids};

#[derive(Clone, Debug)]
enum Damage {
    None,
    TruncateRun(u32),
    TruncateManifest(u32),
}

#[derive(Clone, Debug)]
enum Op {
    Seal(Vec<u8>),
    Relocate(usize),
    Commit(u8),
    Persist(u8),
    Condemn(usize),
    Uncondemn(usize),
    Drop(usize),
    Reopen(Damage),
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        4 => proptest::collection::vec(0u8..40, 1..12).prop_map(Op::Seal),
        2 => any::<usize>().prop_map(Op::Relocate),
        1 => (0u8..4).prop_map(Op::Commit),
        4 => (0u8..4).prop_map(Op::Persist),
        1 => any::<usize>().prop_map(Op::Condemn),
        1 => any::<usize>().prop_map(Op::Uncondemn),
        1 => any::<usize>().prop_map(Op::Drop),
        2 => prop_oneof![
            3 => Just(Damage::None),
            1 => any::<u32>().prop_map(Damage::TruncateRun),
            1 => any::<u32>().prop_map(Damage::TruncateManifest),
        ].prop_map(Op::Reopen),
    ]
}

struct Harness {
    _dir: tempfile::TempDir,
    root: std::path::PathBuf,
    ids: Arc<Ids>,
    max_runs: usize,
    ix: Option<LsmIndex>,
    footers: Footers,
    /// Sealed segments and their chunks.
    segs: BTreeMap<u64, Vec<(ChunkId, Hint)>>,
    next_seg: u64,
    named_upto: u64,
    oracle: BTreeMap<ChunkId, Hint>,
    condemned: BTreeSet<u64>,
}

fn tmp() -> tempfile::TempDir {
    if Path::new("/dev/shm").is_dir() {
        tempfile::tempdir_in("/dev/shm").expect("tempdir")
    } else {
        tempfile::tempdir().expect("tempdir")
    }
}

impl Harness {
    fn new(max_runs: usize) -> Self {
        let dir = tmp();
        let root = dir.path().to_owned();
        let ids = Ids::new();
        let ix = Some(open_with(&root, &ids, max_runs));
        Self {
            _dir: dir,
            root,
            ids,
            max_runs,
            ix,
            footers: Footers::default(),
            segs: BTreeMap::new(),
            next_seg: 1,
            named_upto: 0,
            oracle: BTreeMap::new(),
            condemned: BTreeSet::new(),
        }
    }

    fn ix(&self) -> &LsmIndex {
        self.ix.as_ref().expect("open")
    }

    fn target(&self, back: u8) -> u64 {
        (self.next_seg - 1).saturating_sub(u64::from(back))
    }

    fn pick_seg(&self, k: usize) -> Option<u64> {
        let keys: Vec<u64> = self.segs.keys().copied().collect();
        (!keys.is_empty()).then(|| keys[k % keys.len()])
    }

    fn apply(&mut self, op: &Op) {
        match op {
            Op::Seal(ids) => {
                let s = self.next_seg;
                self.next_seg += 1;
                let uniq: BTreeSet<u8> = ids.iter().copied().collect();
                let entries: Vec<(ChunkId, Hint)> = uniq
                    .iter()
                    .enumerate()
                    .map(|(i, x)| (cid(u64::from(*x)), hint(s, (i * 16) as u32)))
                    .collect();
                self.footers.add(seg(s), &entries);
                for (id, h) in &entries {
                    self.ix().insert(*id, *h).expect("insert");
                    self.oracle.insert(*id, *h);
                }
                self.segs.insert(s, entries);
            }
            Op::Relocate(k) => {
                if self.oracle.is_empty() {
                    return;
                }
                let (id, old) = self
                    .oracle
                    .iter()
                    .nth(k % self.oracle.len())
                    .map(|(a, b)| (*a, *b))
                    .expect("entry");
                let s = self.next_seg;
                self.next_seg += 1;
                let new = hint(s, 0);
                self.footers.add(seg(s), &[(id, new)]);
                self.segs.insert(s, vec![(id, new)]);
                assert!(self.ix().relocate(&id, &old, &new).expect("relocate"));
                self.oracle.insert(id, new);
            }
            Op::Commit(back) => {
                self.named_upto = self.named_upto.max(self.target(*back));
            }
            Op::Persist(back) => {
                let upto = self.target(*back);
                if upto == 0 {
                    return;
                }
                self.named_upto = self.named_upto.max(upto);
                self.ix().persist(seg(upto)).expect("persist");
            }
            Op::Condemn(k) => {
                if let Some(s) = self.pick_seg(*k) {
                    self.ix().mark_condemned(seg(s)).expect("condemn");
                    self.condemned.insert(s);
                }
            }
            Op::Uncondemn(k) => {
                if self.condemned.is_empty() {
                    return;
                }
                let s = *self
                    .condemned
                    .iter()
                    .nth(k % self.condemned.len())
                    .expect("one");
                self.ix().unmark_condemned(seg(s)).expect("uncondemn");
                self.condemned.remove(&s);
            }
            Op::Drop(k) => {
                let Some(s) = self.pick_seg(*k) else { return };
                // The GC's delete-time invariant: every chunk of the segment
                // has a current copy in another, named segment.
                let ok = s <= self.named_upto
                    && self.segs[&s].iter().all(|(id, _)| {
                        let cur = self.oracle[id];
                        cur.seg != seg(s) && cur.seg.counter() <= self.named_upto
                    });
                if !ok {
                    return;
                }
                self.footers.remove(seg(s));
                self.segs.remove(&s);
                self.ix().drop_segment(seg(s)).expect("drop");
            }
            Op::Reopen(damage) => self.reopen(damage),
        }
    }

    fn reopen(&mut self, damage: &Damage) {
        let newest_run = self.ix().runs().expect("runs").last().copied();
        self.ix = None;
        let mut manifest_damaged = false;
        match damage {
            Damage::None => {}
            Damage::TruncateRun(r) => {
                if let Some(run) = newest_run {
                    let p = self.root.join("index").join(run.id.to_string());
                    let cut = u64::from(*r) % run.size_bytes;
                    fs::OpenOptions::new()
                        .write(true)
                        .open(p)
                        .expect("open run")
                        .set_len(cut)
                        .expect("truncate");
                }
            }
            Damage::TruncateManifest(r) => {
                let p = self.root.join("index/manifest");
                let len = fs::metadata(&p).map_or(0, |m| m.len());
                if len > 0 {
                    fs::OpenOptions::new()
                        .write(true)
                        .open(p)
                        .expect("open manifest")
                        .set_len(u64::from(*r) % len)
                        .expect("truncate");
                    manifest_damaged = true;
                }
            }
        }
        self.ix = Some(open_with(&self.root, &self.ids, self.max_runs));
        // The orphan sweep: segments no commit names are gone.
        let orphans: Vec<u64> = self
            .segs
            .keys()
            .copied()
            .filter(|s| *s > self.named_upto)
            .collect();
        for s in orphans {
            self.segs.remove(&s);
            self.footers.remove(seg(s));
        }
        let named: Vec<_> = self.segs.keys().map(|s| seg(*s)).collect();
        self.ix().catch_up(&named, &self.footers).expect("catch_up");
        self.oracle.clear();
        for entries in self.segs.values() {
            for (id, h) in entries {
                self.oracle.insert(*id, *h);
            }
        }
        let flags: BTreeSet<u64> = self
            .ix()
            .condemned()
            .expect("condemned")
            .iter()
            .map(|s| s.counter())
            .collect();
        if manifest_damaged {
            // A damaged manifest falls back one generation: a flag can be lost
            // or come back after an unmark. The GC list is authoritative and
            // re-applies flags; a stale flag only blocks dedup.
            self.condemned = flags;
        } else {
            assert_eq!(flags, self.condemned);
        }
    }

    fn check(&self) {
        let ix = self.ix();
        let mut seen = BTreeMap::new();
        ix.iter_entries(&mut |e| {
            seen.insert(e.id, e.hint);
            assert_eq!(e.condemned, self.condemned.contains(&e.hint.seg.counter()));
        })
        .expect("iter");
        assert_eq!(seen, self.oracle);
        for x in 0..40u64 {
            let id = cid(x);
            let want = self.oracle.get(&id).copied();
            let got = ix.get(&id).expect("get").map(|c| c.hint);
            assert_eq!(got, want, "get {x}");
            let dedup = ix.get_for_dedup(&id).expect("dedup");
            let want_dedup = want.filter(|h| !self.condemned.contains(&h.seg.counter()));
            assert_eq!(dedup.map(|d| d.hint), want_dedup, "dedup {x}");
        }
        for x in [0u64, 7, 23] {
            let p = IdPrefix::of(&cid(x), IdLen::Sixteen);
            let found = ix.get_by_prefix(&p).expect("prefix");
            let want = self.oracle.get(&cid(x)).copied();
            assert_eq!(
                found.iter().find(|c| c.id == cid(x)).map(|c| c.hint),
                want,
                "prefix {x}"
            );
            assert!(found.iter().all(|c| p.matches(&c.id)));
        }
        // Run listing: the manifest's runs are a subset of the files.
        let all: BTreeSet<_> = ix.all_runs().expect("all").iter().map(|r| r.id).collect();
        for r in ix.runs().expect("runs") {
            assert!(all.contains(&r.id));
        }
    }
}

fn cases() -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(5000)
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: cases(),
        max_shrink_iters: 2000,
        failure_persistence: None,
        .. ProptestConfig::default()
    })]

    #[test]
    fn index_matches_the_oracle_through_crashes_and_damage(
        ops in proptest::collection::vec(op(), 1..40),
        max_runs in prop_oneof![Just(2usize), Just(3), Just(8)],
    ) {
        let mut h = Harness::new(max_runs);
        for o in &ops {
            h.apply(o);
            h.check();
        }
    }
}
