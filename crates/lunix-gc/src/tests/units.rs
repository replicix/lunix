//! Smaller properties: formats, settings, preconditions, pins, the urgent
//! pass, the dry run, a lean volume and the status snapshot.

use std::time::Duration;

use lunix_format::gcfile::Condemned;
use lunix_format::{CommitSeq, IdLen, ObjId, TimeMs};
use lunix_iface::gc::{Collector, GcState};
use lunix_iface::index::ChunkIndex;
use lunix_iface::map::ChunkNo;
use lunix_iface::store::{GcStateStore, PinKind, PinRoot, SegmentStore};
use lunix_iface::ErrorKind;
use proptest::prelude::*;

use super::sim::{Sim, SimCfg, GRACE};
use crate::condemned::{CondemnedEntry, CondemnedList, ObjectRef};
use crate::evidence::{Action, Evidence};

fn obj() -> impl Strategy<Value = ObjectRef> {
    (1u8..=4, 1u64..(1 << 40)).prop_map(|(k, raw)| match k {
        1 => ObjectRef::Segment(ObjId::from_raw(raw).unwrap()),
        2 => ObjectRef::Pack(ObjId::from_raw(raw).unwrap()),
        3 => ObjectRef::Run(ObjId::from_raw(raw).unwrap()),
        _ => ObjectRef::Commit(CommitSeq(raw)),
    })
}

proptest! {
    #[test]
    fn condemned_list_round_trips(entries in proptest::collection::btree_map(obj(), (any::<u64>(), any::<u64>()), 0..50)) {
        let list = CondemnedList {
            entries: entries
                .into_iter()
                .map(|(o, (t, b))| (o, CondemnedEntry { condemned_at: TimeMs(t), bytes: b }))
                .collect(),
        };
        let file = list.to_file().unwrap();
        let mut bytes = Vec::new();
        file.encode(&mut bytes).unwrap();
        let back = CondemnedList::from_file(&Condemned::decode(&bytes).unwrap()).unwrap();
        prop_assert_eq!(back, list);
    }
}

#[test]
fn a_condemned_body_out_of_order_or_of_another_version_is_lost() {
    let mut list = CondemnedList::default();
    for raw in [3u64, 9] {
        list.entries.insert(
            ObjectRef::Segment(ObjId::from_raw(raw).unwrap()),
            CondemnedEntry {
                condemned_at: TimeMs(1),
                bytes: 2,
            },
        );
    }
    let mut file = list.to_file().unwrap();
    // Swap the two entries: not strictly ascending.
    let (a, b) = file.body[4..].split_at_mut(25);
    a.swap_with_slice(b);
    assert!(CondemnedList::from_file(&file).is_none());
    let mut v2 = list.to_file().unwrap();
    v2.body_ver = 2;
    assert!(CondemnedList::from_file(&v2).is_none());
    let mut trailing = list.to_file().unwrap();
    trailing.body.push(0);
    assert!(CondemnedList::from_file(&trailing).is_none());
    let mut bad_kind = list.to_file().unwrap();
    bad_kind.body[4] = 9;
    assert!(CondemnedList::from_file(&bad_kind).is_none());
}

#[test]
fn evidence_lines_are_one_json_object_each() {
    let mut e = Evidence::default();
    e.add(
        TimeMs(5),
        Action::Condemn,
        ObjectRef::Segment(ObjId::from_raw(0x1f).unwrap()),
        123,
        "unreferenced",
    );
    e.add(
        TimeMs(6),
        Action::Delete,
        ObjectRef::Commit(CommitSeq(2)),
        9,
        "unreferenced after grace and recheck",
    );
    let text = String::from_utf8(e.finish(TimeMs(7))).unwrap();
    assert_eq!(
        text,
        "{\"time\":5,\"action\":\"condemn\",\"kind\":\"segment\",\"id\":\"000000000000001f\",\"bytes\":123,\"reason\":\"unreferenced\"}\n\
         {\"time\":6,\"action\":\"delete\",\"kind\":\"commit\",\"id\":\"0000000000000002\",\"bytes\":9,\"reason\":\"unreferenced after grace and recheck\"}\n"
    );
}

fn sim() -> Sim {
    Sim::new(SimCfg::new(16 << 10, IdLen::Sixteen))
}

fn churn(s: &mut Sim, rounds: u64) {
    for r in 0..rounds {
        for i in 0..12 {
            s.write(i, r * 100 + i);
        }
        s.commit().unwrap();
    }
}

#[test]
fn a_grace_below_its_floor_is_refused() {
    let mut s = sim();
    churn(&mut s, 2);
    let mut gc = std::mem::replace(
        &mut s.gc,
        crate::Gc::new(std::sync::Arc::new(s.store.clone()), s.cfg.gc),
    );
    let mut params = s.params();
    params.grace = Duration::from_secs(29);
    let e = s
        .with_view(|v| {
            let view = lunix_iface::gc::GcView { params, ..*v };
            gc.step(&view, Sim::budget())
        })
        .unwrap_err();
    assert_eq!(e.kind(), ErrorKind::Invalid);
    // 10 s + 2 x read_timeout: a longer read timeout raises the floor.
    params.grace = Duration::from_secs(30);
    params.read_timeout = Duration::from_secs(11);
    let e = s
        .with_view(|v| gc.step(&lunix_iface::gc::GcView { params, ..*v }, Sim::budget()))
        .unwrap_err();
    assert_eq!(e.kind(), ErrorKind::Invalid);
    params.read_timeout = Duration::from_secs(10);
    s.with_view(|v| gc.step(&lunix_iface::gc::GcView { params, ..*v }, Sim::budget()))
        .unwrap();
}

#[test]
fn a_step_with_a_dirty_map_is_refused() {
    let mut s = sim();
    churn(&mut s, 2);
    let e = s.map.get(ChunkNo(0)).unwrap().unwrap();
    {
        let mut txn = s.map.begin_update().unwrap();
        txn.set(ChunkNo(1), e).unwrap();
        txn.commit().unwrap();
    }
    assert_eq!(s.step().unwrap_err().kind(), ErrorKind::Invalid);
    assert_eq!(s.urgent(1).unwrap_err().kind(), ErrorKind::Invalid);
}

#[test]
fn an_unreadable_pin_stops_the_pass_and_a_pin_keeps_its_commit_and_data() {
    let mut s = sim();
    churn(&mut s, 2);
    s.pin(0);
    let pinned = s.pins[0].commit_seq;
    s.store.fail_pins(true);
    let before: Vec<_> = SegmentStore::list(&s.store).unwrap();
    churn(&mut s, 4);
    s.clock.advance(GRACE * 2);
    assert!(s.step().is_err());
    s.clock.advance(GRACE * 2);
    assert!(s.step().is_err());
    for seg in &before {
        assert!(SegmentStore::list(&s.store).unwrap().contains(seg));
    }
    s.store.fail_pins(false);
    s.quiesce().unwrap();
    assert!(s.commit_exists(pinned.0));
    s.check_roots().unwrap();
    // Unpinned, its commit and data go.
    s.unpin(0);
    s.quiesce().unwrap();
    assert!(!s.commit_exists(pinned.0));
    s.check_roots().unwrap();
}

#[test]
fn a_pin_on_a_reader_or_hold_is_a_root_too() {
    for kind in [PinKind::Reader, PinKind::Hold] {
        let mut s = sim();
        churn(&mut s, 2);
        let seq = s.tip.common.commit_seq;
        s.pins.push(PinRoot {
            kind,
            name: "p".into(),
            commit_seq: seq,
            deadline: Some(TimeMs(1)),
        });
        s.store.set_pins(s.pins.clone());
        churn(&mut s, 5);
        s.quiesce().unwrap();
        assert!(s.commit_exists(seq.0));
        s.check_roots().unwrap();
    }
}

#[test]
fn the_urgent_pass_deletes_only_after_the_floor_grace_and_reports_bytes() {
    let mut s = sim();
    churn(&mut s, 6);
    s.clock.advance(Duration::from_secs(5));
    // First urgent pass: condemns, deletes nothing (floor grace not over).
    let p = s.urgent(u64::MAX).unwrap();
    assert!(p.done);
    assert_eq!(p.freed, 0);
    assert!(!s.gc.condemned().is_empty());
    // 29 s later: still inside the 30 s floor grace.
    s.clock.advance(Duration::from_secs(29));
    assert_eq!(s.urgent(u64::MAX).unwrap().freed, 0);
    s.clock.advance(Duration::from_secs(2));
    let p = s.urgent(u64::MAX).unwrap();
    assert!(p.freed > 0);
    s.check_roots().unwrap();
    s.check_ledger().unwrap();
    // A small want stops deleting once it is met.
    churn(&mut s, 6);
    s.urgent(u64::MAX).unwrap();
    s.clock.advance(Duration::from_secs(31));
    let n = s.gc.condemned().len();
    let p = s.urgent(1).unwrap();
    assert!(p.freed > 0);
    assert!(!s.gc.condemned().is_empty() || n <= 1);
    s.check_roots().unwrap();
}

#[test]
fn a_dry_run_changes_nothing_and_predicts_the_pass() {
    let mut s = sim();
    churn(&mut s, 6);
    for i in 0..4 {
        s.write(20 + i, 900 + i);
    }
    s.commit().unwrap();
    s.clock.advance(Duration::from_secs(5));
    let segs_before = SegmentStore::list(&s.store).unwrap();
    let list_before = s.store.load_condemned().unwrap();
    let flags_before = s.index.condemned().unwrap();
    let passes_before = s.store.evidence_passes().unwrap();
    let mut gc = std::mem::replace(
        &mut s.gc,
        crate::Gc::new(std::sync::Arc::new(s.store.clone()), s.cfg.gc),
    );
    let dry = s.with_view(|v| gc.dry_run(v)).unwrap();
    s.gc = gc;
    assert!(dry.dry_run);
    assert_eq!(SegmentStore::list(&s.store).unwrap(), segs_before);
    assert_eq!(s.store.load_condemned().unwrap(), list_before);
    assert_eq!(s.index.condemned().unwrap(), flags_before);
    assert_eq!(s.store.evidence_passes().unwrap(), passes_before);
    assert!(dry.segments_condemned > 0);
    s.pass().unwrap();
    let real = s.gc.status().last_pass.unwrap();
    assert_eq!(real.segments_condemned, dry.segments_condemned);
    assert_eq!(real.segments_deleted, dry.segments_deleted);
    assert_eq!(real.packs_deleted, dry.packs_deleted);
    assert_eq!(real.commits_deleted, dry.commits_deleted);
    assert_eq!(real.bytes_freed, dry.bytes_freed);
    assert_eq!(real.segments_compacted, dry.segments_compacted);
    assert_eq!(real.bytes_moved, dry.bytes_moved);
}

#[test]
fn a_lean_volume_never_compacts_but_deletes_dead_segments() {
    let mut s = Sim::new(SimCfg::new(16 << 10, IdLen::Zero));
    for i in 0..4 {
        s.write(i, 200 + i);
    }
    s.commit().unwrap();
    for i in 1..4 {
        s.write(i, 300 + i);
    }
    churn(&mut s, 4);
    s.quiesce().unwrap();
    assert_eq!(s.gc.totals().segments_compacted, 0);
    assert!(s.gc.totals().segments_deleted > 0);
    let mut gc = std::mem::replace(
        &mut s.gc,
        crate::Gc::new(std::sync::Arc::new(s.store.clone()), s.cfg.gc),
    );
    assert_eq!(
        s.with_view(|v| gc.dry_run(v)).unwrap().segments_compacted,
        0
    );
    s.gc = gc;
    s.check_roots().unwrap();
}

#[test]
fn status_and_stats_follow_the_passes() {
    let mut s = sim();
    churn(&mut s, 6);
    assert_eq!(s.gc.status().state, None);
    s.clock.advance(Duration::from_secs(5));
    let p = s.step().unwrap();
    assert_eq!(p.state, GcState::Grace);
    assert_eq!(s.gc.status().state, Some(GcState::Grace));
    assert!(s.gc.status().condemned_pending > 0);
    s.pass().unwrap();
    let st = s.gc.status();
    assert!(st.last_pass.as_ref().unwrap().pass >= 1);
    assert_eq!(st.stats.passes, 1);
    assert!(st.stats.bytes_freed > 0);
    assert!(st.stats.live_bytes > 0);
    assert_eq!(s.gc.stats(), st.stats);
    assert!(st.last_finished_ms.is_some());
    assert_eq!(
        s.store.evidence_passes().unwrap(),
        vec![st.last_pass.unwrap().pass]
    );
}

#[test]
fn a_pass_that_finds_nothing_writes_no_evidence() {
    let mut s = sim();
    s.write(0, 1);
    s.commit().unwrap();
    s.pass().unwrap();
    assert!(s.store.evidence_passes().unwrap().is_empty());
    assert_eq!(s.gc.status().last_pass.unwrap().pass, 0);
}

#[test]
fn a_lost_condemned_list_is_rebuilt_from_the_index_flags() {
    let mut s = sim();
    churn(&mut s, 6);
    s.clock.advance(Duration::from_secs(5));
    s.step().unwrap();
    let condemned = s.gc.condemned();
    assert!(!condemned.is_empty());
    // A crash with the list lost: the flags remain; the reopened collector
    // adopts the flagged segments and restarts their grace.
    s.store
        .store_condemned(&Condemned {
            body_ver: 7,
            body: vec![1, 2, 3],
        })
        .unwrap();
    s.crash_reopen(3).unwrap();
    let flagged = s.index.condemned().unwrap();
    assert!(!flagged.is_empty());
    s.check_roots().unwrap();
    s.clock.advance(Duration::from_secs(5));
    s.step().unwrap();
    for seg in &flagged {
        assert!(s.gc.condemned().contains(&ObjectRef::Segment(*seg)));
    }
    assert!(s
        .index
        .get_for_dedup(&lunix_format::ChunkId([0; 32]))
        .unwrap()
        .is_none());
    s.quiesce().unwrap();
    s.check_roots().unwrap();
    assert!(s.index.condemned().unwrap().is_empty());
}

#[test]
fn an_index_run_the_manifest_does_not_name_is_collected() {
    use lunix_iface::space::{SpaceGate, SpaceKind};
    let mut s = sim();
    churn(&mut s, 2);
    // A persist that wrote its run file but failed before the manifest named
    // it: the run is in `all_runs` only.
    for i in 0..5 {
        s.write(30 + i, 5000 + i);
    }
    s.commit().unwrap();
    s.write(40, 6000);
    s.commit().unwrap();
    s.index
        .faults()
        .arm("index.persist.before_manifest", 1, lunix_testkit::Fault::Io);
    let e = s.index.get(&lunix_format::ChunkId([0; 32])).unwrap();
    assert!(e.is_none());
    let upto = s.upto_seg.unwrap();
    // A new entry so that persist has something to write.
    let (id, hint) = {
        let mut found = None;
        s.index
            .iter_entries(&mut |c| {
                if found.is_none() {
                    found = Some((c.id, c.hint));
                }
            })
            .unwrap();
        found.unwrap()
    };
    s.index.insert(id, hint).unwrap();
    assert!(s.index.persist(upto).is_err());
    let named: Vec<_> = s.index.runs().unwrap().iter().map(|r| r.id).collect();
    let orphan = *s
        .index
        .all_runs()
        .unwrap()
        .iter()
        .find(|r| !named.contains(&r.id))
        .expect("an unnamed run file");
    s.space.charge(SpaceKind::Index, orphan.size_bytes).unwrap();
    s.check_ledger().unwrap();
    // Above the high-water mark it is never condemned...
    s.clock.advance(GRACE * 2);
    s.urgent(u64::MAX).unwrap();
    s.clock.advance(GRACE * 2);
    s.urgent(u64::MAX).unwrap();
    assert!(s
        .index
        .all_runs()
        .unwrap()
        .iter()
        .any(|r| r.id == orphan.id));
    // ...until later cuts name higher ids.
    for i in 0..3 {
        s.write(41 + i, 7000 + i);
        s.commit().unwrap();
    }
    s.quiesce().unwrap();
    assert!(!s
        .index
        .all_runs()
        .unwrap()
        .iter()
        .any(|r| r.id == orphan.id));
    assert!(s.gc.totals().runs_deleted >= 1);
    s.check_ledger().unwrap();
    s.check_roots().unwrap();
}
