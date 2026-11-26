//! Deterministic behaviour tests: the on-disk run against the reference codec,
//! the D17 trust rules, the persist rule, recovery from damage and compaction.

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use lunix_format::index_run::{IndexEntry, IndexRun};
use lunix_format::{ChunkId, Hint, IdLen, IdPrefix};
use lunix_iface::index::{ChunkIndex, Confirm};
use lunix_iface::ErrorKind;
use lunix_index::LsmIndex;

use common::{cid, hint, open, open_with, seg, Footers, Ids};

fn blake(b: &[u8]) -> [u8; 32] {
    *blake3::hash(b).as_bytes()
}

fn run_bytes(root: &Path, id: lunix_format::RunId) -> Vec<u8> {
    fs::read(root.join("index").join(id.to_string())).expect("read run")
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).expect("mkdir");
    for e in fs::read_dir(from).expect("readdir") {
        let e = e.expect("entry");
        let dest = to.join(e.file_name());
        if e.file_type().expect("type").is_dir() {
            copy_dir(&e.path(), &dest);
        } else {
            fs::copy(e.path(), dest).expect("copy");
        }
    }
}

fn all_entries(ix: &LsmIndex) -> BTreeMap<ChunkId, (Hint, bool)> {
    let mut m = BTreeMap::new();
    ix.iter_entries(&mut |e| {
        m.insert(e.id, (e.hint, e.condemned));
    })
    .expect("iter");
    m
}

#[test]
fn runs_are_byte_identical_to_the_reference_codec() {
    let dir = tempfile::tempdir().expect("tmp");
    let ids = Ids::new();
    let ix = open(dir.path(), &ids);
    let mut expect = BTreeMap::new();
    for n in 0..400u64 {
        let h = hint(1 + n % 3, (n * 100) as u32);
        ix.insert(cid(n), h).expect("insert");
        expect.insert(cid(n), h);
    }
    let made = ix.persist(seg(3)).expect("persist");
    assert_eq!(made.len(), 1);
    let bytes = run_bytes(dir.path(), made[0].id);
    assert_eq!(made[0].size_bytes, bytes.len() as u64);
    let decoded = IndexRun::decode(&bytes).expect("reference decode");
    assert_eq!(decoded.run_id, made[0].id);
    assert_eq!(decoded.entries.len(), 400);
    assert_eq!(decoded.covered, vec![seg(1), seg(2), seg(3)]);
    for e in &decoded.entries {
        assert_eq!(hint_of(e), expect[&e.chunk_id]);
    }
    assert_eq!(
        decoded.run_hash,
        blake(IndexRun::hash_input(&bytes).expect("input"))
    );
    // The reference encoder reproduces the file exactly.
    let mut again = Vec::new();
    decoded.encode(&mut again).expect("encode");
    assert_eq!(again, bytes);
}

fn hint_of(e: &IndexEntry) -> Hint {
    Hint {
        seg: e.seg,
        off: e.off,
        clen: e.clen,
    }
}

#[test]
fn a_run_entry_forged_to_another_extent_is_hinted_never_verified() {
    let dir = tempfile::tempdir().expect("tmp");
    let ids = Ids::new();
    let ix = open(dir.path(), &ids);
    ix.insert(cid(1), hint(1, 0)).expect("insert");
    let made = ix.persist(seg(1)).expect("persist");
    let id = made[0].id;
    drop(ix);
    // An attacker with write access to index/ replaces the run with a valid
    // checksummed one of the same size that points X at some other extent.
    let forged = IndexRun::seal(
        id,
        vec![IndexEntry {
            chunk_id: cid(1),
            seg: seg(1),
            off: 4096,
            clen: 7,
        }],
        vec![seg(1)],
        blake,
    )
    .expect("seal");
    let mut bytes = Vec::new();
    forged.encode(&mut bytes).expect("encode");
    fs::write(dir.path().join("index").join(id.to_string()), bytes).expect("write");
    let ix = open(dir.path(), &ids);
    let hit = ix.get_for_dedup(&cid(1)).expect("dedup").expect("hit");
    assert_eq!(hit.hint.off, 4096);
    assert_eq!(hit.confirm, Confirm::Hinted);
}

#[test]
fn only_sealed_named_segments_come_back_after_a_crash() {
    let dir = tempfile::tempdir().expect("tmp");
    let ids = Ids::new();
    let footers = Footers::default();
    footers.add(seg(1), &[(cid(1), hint(1, 0))]);
    // Segment 2 exists on disk but no commit names it (an orphan).
    footers.add(seg(2), &[(cid(2), hint(2, 0))]);
    let ix = open(dir.path(), &ids);
    ix.insert(cid(1), hint(1, 0)).expect("insert");
    ix.insert(cid(2), hint(2, 0)).expect("insert");
    ix.persist(seg(1)).expect("persist");
    drop(ix);
    let ix = open(dir.path(), &ids);
    assert_eq!(ix.get_for_dedup(&cid(2)).expect("dedup"), None);
    let r = ix.catch_up(&[seg(1)], &footers).expect("catch_up");
    assert_eq!(r.inserted, 0, "segment 1 is covered by the run");
    assert_eq!(ix.get_for_dedup(&cid(2)).expect("dedup"), None);
    assert!(ix.get_for_dedup(&cid(1)).expect("dedup").is_some());
}

#[test]
fn condemned_flag_survives_reopen_and_blocks_dedup_only() {
    let dir = tempfile::tempdir().expect("tmp");
    let ids = Ids::new();
    let ix = open(dir.path(), &ids);
    ix.insert(cid(1), hint(1, 0)).expect("insert");
    ix.insert(cid(2), hint(2, 0)).expect("insert");
    ix.persist(seg(2)).expect("persist");
    ix.mark_condemned(seg(1)).expect("condemn");
    assert_eq!(ix.get_for_dedup(&cid(1)).expect("dedup"), None);
    drop(ix);
    let ix = open(dir.path(), &ids);
    assert_eq!(ix.get_for_dedup(&cid(1)).expect("dedup"), None);
    assert!(ix.get_for_dedup(&cid(2)).expect("dedup").is_some());
    let got = ix.get(&cid(1)).expect("get").expect("entry");
    assert!(got.condemned);
    let p = IdPrefix::of(&cid(1), IdLen::Sixteen);
    assert!(ix
        .get_by_prefix(&p)
        .expect("prefix")
        .iter()
        .any(|c| c.id == cid(1) && c.condemned));
    assert_eq!(ix.condemned().expect("list"), vec![seg(1)]);
}

#[test]
fn relocated_entry_is_in_no_run_until_its_segment_is_named() {
    let dir = tempfile::tempdir().expect("tmp");
    let ids = Ids::new();
    let ix = open(dir.path(), &ids);
    // 1 is relocated before it was ever persisted; 2 after.
    ix.insert(cid(1), hint(1, 0)).expect("insert");
    ix.insert(cid(2), hint(1, 600)).expect("insert");
    assert!(ix
        .relocate(&cid(1), &hint(1, 0), &hint(9, 0))
        .expect("relocate"));
    ix.persist(seg(1)).expect("persist");
    assert!(ix
        .relocate(&cid(2), &hint(1, 600), &hint(9, 64))
        .expect("relocate"));
    ix.persist(seg(5)).expect("persist");
    let segs_in_runs = |ix: &LsmIndex| -> Vec<(ChunkId, Hint)> {
        let mut v = Vec::new();
        for r in ix.all_runs().expect("all_runs") {
            let run = IndexRun::decode(&run_bytes(dir.path(), r.id)).expect("decode");
            v.extend(run.entries.iter().map(|e| (e.chunk_id, hint_of(e))));
        }
        v
    };
    let in_runs = segs_in_runs(&ix);
    assert!(
        in_runs.iter().all(|(_, h)| h.seg < seg(9)),
        "nothing names segment 9 yet"
    );
    assert!(in_runs.contains(&(cid(1), hint(1, 0))), "old location kept");
    assert_eq!(ix.get(&cid(1)).expect("get").expect("e").hint, hint(9, 0));
    drop(ix);
    let ix = open(dir.path(), &ids);
    assert_eq!(ix.get(&cid(1)).expect("get").expect("e").hint, hint(1, 0));
    assert_eq!(ix.get(&cid(2)).expect("get").expect("e").hint, hint(1, 600));
    // Naming segment 9 makes the new locations durable.
    assert!(ix
        .relocate(&cid(1), &hint(1, 0), &hint(9, 0))
        .expect("relocate"));
    ix.persist(seg(9)).expect("persist");
    drop(ix);
    let ix = open(dir.path(), &ids);
    assert_eq!(ix.get(&cid(1)).expect("get").expect("e").hint, hint(9, 0));
}

#[test]
fn a_damaged_manifest_at_any_byte_falls_back_and_catch_up_restores_everything() {
    let base = tempfile::tempdir().expect("tmp");
    let ids = Ids::new();
    let footers = Footers::default();
    let ix = open(base.path(), &ids);
    let mut expect = BTreeMap::new();
    for s in 1..=3u64 {
        let entries: Vec<(ChunkId, Hint)> = (0..50)
            .map(|n| (cid(s * 1000 + n), hint(s, (n * 10) as u32)))
            .collect();
        footers.add(seg(s), &entries);
        for (id, h) in &entries {
            ix.insert(*id, *h).expect("insert");
            expect.insert(*id, *h);
        }
        ix.persist(seg(s)).expect("persist");
    }
    ix.mark_condemned(seg(2)).expect("condemn");
    drop(ix);
    let len = fs::metadata(base.path().join("index/manifest"))
        .expect("meta")
        .len();
    for cut in 0..len {
        let work = tempfile::tempdir().expect("tmp");
        copy_dir(base.path(), work.path());
        let f = fs::OpenOptions::new()
            .write(true)
            .open(work.path().join("index/manifest"))
            .expect("open");
        f.set_len(cut).expect("truncate");
        drop(f);
        let ix = open(work.path(), &ids);
        assert!(ix.open_report().fell_back, "cut {cut}");
        ix.catch_up(&[seg(1), seg(2), seg(3)], &footers)
            .expect("catch_up");
        for (id, h) in &expect {
            assert_eq!(
                ix.get(id).expect("get").expect("entry").hint,
                *h,
                "cut {cut}"
            );
        }
        // A second open finds a clean manifest.
        drop(ix);
        let ix = open(work.path(), &ids);
        assert!(!ix.open_report().fell_back, "cut {cut}");
    }
}

#[test]
fn a_truncated_newest_run_falls_back_to_the_previous_manifest() {
    let dir = tempfile::tempdir().expect("tmp");
    let ids = Ids::new();
    let footers = Footers::default();
    let ix = open(dir.path(), &ids);
    let mut expect = BTreeMap::new();
    let mut newest = None;
    for s in 1..=2u64 {
        let entries: Vec<(ChunkId, Hint)> = (0..300)
            .map(|n| (cid(s * 1000 + n), hint(s, n as u32)))
            .collect();
        footers.add(seg(s), &entries);
        for (id, h) in &entries {
            ix.insert(*id, *h).expect("insert");
            expect.insert(*id, *h);
        }
        newest = Some(ix.persist(seg(s)).expect("persist")[0]);
    }
    drop(ix);
    let newest = newest.expect("run");
    let path = dir.path().join("index").join(newest.id.to_string());
    let f = fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .expect("open");
    f.set_len(newest.size_bytes / 2 + 1).expect("truncate");
    drop(f);
    let ix = open(dir.path(), &ids);
    let rep = ix.open_report();
    assert!(rep.fell_back);
    assert_eq!(rep.discarded_runs, vec![newest.id]);
    ix.catch_up(&[seg(1), seg(2)], &footers).expect("catch_up");
    for (id, h) in &expect {
        assert_eq!(ix.get(id).expect("get").expect("entry").hint, *h);
    }
    // The damaged file is still listed so GC can delete it.
    assert!(ix
        .all_runs()
        .expect("all_runs")
        .iter()
        .any(|r| r.id == newest.id));
}

#[test]
fn a_bit_flip_inside_a_run_is_caught_at_open_or_at_the_block() {
    let dir = tempfile::tempdir().expect("tmp");
    let ids = Ids::new();
    let ix = open(dir.path(), &ids);
    for n in 0..200 {
        ix.insert(cid(n), hint(1, n as u32)).expect("insert");
    }
    let made = ix.persist(seg(1)).expect("persist");
    // Flip a byte after open: the block checksum catches it on lookup.
    let path = dir.path().join("index").join(made[0].id.to_string());
    let mut bytes = fs::read(&path).expect("read");
    bytes[100] ^= 0x40;
    fs::write(&path, &bytes).expect("write");
    let mut corrupt = 0;
    for n in 0..200 {
        if let Err(e) = ix.get(&cid(n)) {
            assert_eq!(e.kind(), ErrorKind::Corrupt);
            corrupt += 1;
        }
    }
    assert!(corrupt > 0, "lookups into the damaged block fail");
    drop(ix);
    // A fresh open validates every block and the run hash and discards it.
    let ix = open(dir.path(), &ids);
    assert!(ix.open_report().fell_back);
    assert!(ix.runs().expect("runs").is_empty());
}

#[test]
fn drop_segment_persists_and_compaction_purges_it() {
    let dir = tempfile::tempdir().expect("tmp");
    let ids = Ids::new();
    let ix = open_with(dir.path(), &ids, 2);
    for n in 0..20 {
        ix.insert(cid(n), hint(1 + n % 2, n as u32))
            .expect("insert");
    }
    ix.persist(seg(2)).expect("persist");
    ix.drop_segment(seg(1)).expect("drop");
    assert_eq!(all_entries(&ix).len(), 10);
    drop(ix);
    let ix = open_with(dir.path(), &ids, 2);
    assert_eq!(all_entries(&ix).len(), 10, "dropped entries stay hidden");
    assert_eq!(ix.get(&cid(0)).expect("get"), None);
    // More runs force a merge of the oldest window, which purges them.
    for s in 3..8u64 {
        ix.insert(cid(1000 + s), hint(s, 0)).expect("insert");
        ix.persist(seg(s)).expect("persist");
    }
    let runs = ix.runs().expect("runs");
    assert!(runs.len() <= 3, "{} runs", runs.len());
    assert_eq!(ix.get(&cid(0)).expect("get"), None);
    assert!(ix.get(&cid(1)).expect("get").is_some());
    let in_runs: u64 = runs.iter().map(|r| r.entries).sum();
    assert!(in_runs <= 10 + 5, "purged: {in_runs}");
}

#[test]
fn size_tiered_compaction_bounds_the_runs_per_level_and_keeps_newest() {
    let dir = tempfile::tempdir().expect("tmp");
    let ids = Ids::new();
    let ix = open_with(dir.path(), &ids, 4);
    let mut expect = BTreeMap::new();
    for round in 0..40u64 {
        for n in 0..30u64 {
            // Ids repeat across rounds: the newest location must win.
            let id = cid(n * 7 + round % 5);
            let h = hint(round + 1, n as u32);
            ix.insert(id, h).expect("insert");
            expect.insert(id, h);
        }
        ix.persist(seg(round + 1)).expect("persist");
        assert!(ix.runs().expect("runs").len() <= 5, "round {round}");
    }
    for (id, h) in &expect {
        assert_eq!(ix.get(id).expect("get").expect("entry").hint, *h);
    }
    assert!(ix.all_runs().expect("all").len() > ix.runs().expect("runs").len());
    drop(ix);
    let ix = open_with(dir.path(), &ids, 4);
    assert_eq!(all_entries(&ix).len(), expect.len());
}

#[test]
fn prefix_lookup_spans_memtable_and_runs_and_returns_the_newest_copy() {
    let dir = tempfile::tempdir().expect("tmp");
    let ids = Ids::new();
    let ix = open(dir.path(), &ids);
    for n in 0..300 {
        ix.insert(cid(n), hint(1, n as u32)).expect("insert");
    }
    ix.persist(seg(1)).expect("persist");
    ix.insert(cid(7), hint(2, 0)).expect("insert");
    for probe in [3u64, 7, 250] {
        let p = IdPrefix::of(&cid(probe), IdLen::Sixteen);
        let found = ix.get_by_prefix(&p).expect("prefix");
        let want = if probe == 7 {
            hint(2, 0)
        } else {
            hint(1, probe as u32)
        };
        assert!(found.iter().any(|c| c.id == cid(probe) && c.hint == want));
        assert!(found.iter().all(|c| p.matches(&c.id)));
        assert_eq!(
            found.iter().filter(|c| c.id == cid(probe)).count(),
            1,
            "one candidate per id"
        );
    }
}

#[test]
fn prefix_lookup_is_unsupported_without_a_prefix() {
    let dir = tempfile::tempdir().expect("tmp");
    let ids = Ids::new();
    let ix = LsmIndex::open(dir.path(), ids, IdLen::Zero).expect("open");
    let p = IdPrefix::of(&cid(1), IdLen::Sixteen);
    let e = ix.get_by_prefix(&p).expect_err("unsupported");
    assert_eq!(e.kind(), ErrorKind::Unsupported);
}

/// A lookup that is running while the collector deletes the runs no manifest
/// names (the dead inputs of a compaction) is never failed by the unlink: it
/// holds the run list under the read lock and the runs by `Arc`, and a scan
/// keeps the file it opened. The answers are the same before, during and
/// after.
#[test]
fn delete_run_does_not_fail_or_change_a_lookup_in_flight() {
    use std::sync::atomic::{AtomicBool, Ordering};

    let dir = tempfile::tempdir().expect("tmp");
    let ids = Ids::new();
    let ix = open_with(dir.path(), &ids, 2);
    let mut expect = BTreeMap::new();
    for round in 0..30u64 {
        for n in 0..40u64 {
            let id = cid(n * 11 + round % 3);
            let h = hint(round + 1, n as u32);
            ix.insert(id, h).expect("insert");
            expect.insert(id, h);
        }
        ix.persist(seg(round + 1)).expect("persist");
    }
    let named: Vec<_> = ix.runs().expect("runs").iter().map(|r| r.id).collect();
    let dead: Vec<_> = ix
        .all_runs()
        .expect("all")
        .into_iter()
        .filter(|r| !named.contains(&r.id))
        .collect();
    assert!(dead.len() >= 4, "compaction left dead runs: {}", dead.len());

    let stop = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let readers: Vec<_> = (0..4)
            .map(|t| {
                let (ix, expect, stop) = (&ix, &expect, &stop);
                scope.spawn(move || {
                    let mut passes = 0u32;
                    while !stop.load(Ordering::Acquire) {
                        for (id, h) in expect {
                            let got = ix.get(id).expect("a lookup never fails");
                            assert_eq!(got.expect("the entry stays").hint, *h);
                        }
                        match t {
                            0 => assert_eq!(all_entries(ix), {
                                let mut m = BTreeMap::new();
                                for (id, h) in expect {
                                    m.insert(*id, (*h, false));
                                }
                                m
                            }),
                            1 => {
                                for id in expect.keys().take(30) {
                                    let p = IdPrefix::of(id, IdLen::Sixteen);
                                    let found = ix.get_by_prefix(&p).expect("prefix");
                                    assert!(found.iter().any(|c| c.id == *id));
                                }
                            }
                            _ => {}
                        }
                        passes += 1;
                    }
                    passes
                })
            })
            .collect();
        for r in &dead {
            std::thread::yield_now();
            assert_eq!(ix.delete_run(*r).expect("delete"), r.size_bytes);
        }
        stop.store(true, Ordering::Release);
        for r in readers {
            assert!(r.join().expect("a reader failed") > 0);
        }
    });
    assert_eq!(ix.all_runs().expect("all").len(), named.len());
}

#[test]
fn delete_run_removes_file_and_manifest_entry() {
    let dir = tempfile::tempdir().expect("tmp");
    let ids = Ids::new();
    let ix = open(dir.path(), &ids);
    ix.insert(cid(1), hint(1, 0)).expect("insert");
    let made = ix.persist(seg(1)).expect("persist");
    let freed = ix.delete_run(made[0]).expect("delete");
    assert_eq!(freed, made[0].size_bytes);
    assert_eq!(ix.delete_run(made[0]).expect("again"), 0);
    drop(ix);
    let ix = open(dir.path(), &ids);
    assert!(ix.runs().expect("runs").is_empty());
    assert!(!ix.open_report().fell_back);
}

#[test]
fn rebuild_replaces_runs_and_the_old_ones_stay_for_gc() {
    let dir = tempfile::tempdir().expect("tmp");
    let ids = Ids::new();
    let footers = Footers::default();
    let ix = open(dir.path(), &ids);
    let e1 = [(cid(1), hint(1, 0)), (cid(2), hint(1, 10))];
    let e2 = [(cid(2), hint(2, 0)), (cid(3), hint(2, 10))];
    footers.add(seg(1), &e1);
    footers.add(seg(2), &e2);
    for (i, h) in e1.iter().chain(&e2) {
        ix.insert(*i, *h).expect("insert");
    }
    ix.persist(seg(2)).expect("persist");
    let old = ix.runs().expect("runs");
    ix.rebuild(&[seg(2), seg(1)], &footers).expect("rebuild");
    let new = ix.runs().expect("runs");
    assert_eq!(new.len(), 1, "one new run, the old one replaced");
    assert!(old.iter().all(|o| new.iter().all(|n| n.id != o.id)));
    assert_eq!(ix.all_runs().expect("all").len(), 2);
    let m = all_entries(&ix);
    assert_eq!(m[&cid(2)].0, hint(2, 0), "highest segment wins");
    assert_eq!(m.len(), 3);
    // Durable without a persist: a reopen sees every entry (R-1).
    drop(ix);
    let ix = open(dir.path(), &ids);
    let m = all_entries(&ix);
    assert_eq!(m.len(), 3);
    assert_eq!(m[&cid(2)].0, hint(2, 0));
}
