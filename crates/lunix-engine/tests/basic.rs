//! Named cases of the engine against the testkit fakes: the I/O paths, the
//! commit protocol, recovery, R4 and the D17 rules.

mod common;

use std::time::Duration;

use common::*;
use lunix_engine::{CommitOutcome, OpenError};
use lunix_format::local::RollbackFloor;
use lunix_format::{CommitSeq, TimeMs};
use lunix_iface::block::{BlockEngine, IoOp};
use lunix_iface::clock::Clock;
use lunix_iface::crypto::CommitCodec;
use lunix_iface::space::{SpaceGate, SpaceKind};
use lunix_iface::store::CommitStore;
use lunix_testkit::JournalMode;

const CS: [u32; 2] = [16 << 10, 64 << 10];

fn vol(cs: u32) -> Vol {
    Vol::create(cs, 16, 2, JournalMode::Mixed(7))
}

#[test]
fn writes_read_back_before_and_after_a_commit_and_a_reopen() {
    for cs in CS {
        let mut v = vol(cs);
        let mut r = v.open().map_err(|e| e.0).expect("open");
        let csz = u64::from(cs);
        let a = pattern(1, 3 * 4096);
        assert!(r.write(0, csz + 4096, &a, false).is_ok());
        let (st, got) = r.read(1, csz + 4096, a.len() as u32);
        assert!(st.is_ok());
        assert_eq!(got, a, "overlay read");
        assert!(matches!(
            r.engine.commit_now(false).expect("commit"),
            CommitOutcome::Committed { .. }
        ));
        let (st, got) = r.read(0, csz + 4096, a.len() as u32);
        assert!(st.is_ok());
        assert_eq!(got, a, "map read");
        // Unwritten blocks of the same chunk are zeros (partial chunk RMW).
        let (_, z) = r.read(0, csz, 4096);
        assert_eq!(z, vec![0; 4096]);
        let b = pattern(2, 4096);
        assert!(r.write(1, 0, &b, false).is_ok());
        assert!(r.flush(0).is_ok());
        let p = r.stop();
        v.absorb(p, 1);
        let mut r = v.open().map_err(|e| e.0).expect("reopen");
        assert_eq!(r.read(0, csz + 4096, a.len() as u32).1, a);
        assert_eq!(
            r.read(1, 0, 4096).1,
            b,
            "flushed write replayed from the journal"
        );
        assert!(v.violations().is_empty(), "{:?}", v.violations());
    }
}

#[test]
fn invalid_requests_get_einval() {
    let v = vol(16 << 10);
    let mut r = v.open().map_err(|e| e.0).expect("open");
    let size = r.engine.size_bytes();
    for (off, len) in [
        (1u64, 4096u32),
        (0, 100),
        (size, 4096),
        (size - 4096, 8192),
        (0, 0),
    ] {
        let t = r.submit(0, IoOp::Write, false, off, len, &vec![0; len as usize]);
        assert_eq!(r.wait(&[t])[0].0.errno(), Some(22), "{off} {len}");
        let t = r.submit(0, IoOp::Read, false, off, len, &[]);
        assert_eq!(r.wait(&[t])[0].0.errno(), Some(22));
    }
}

#[test]
fn discard_of_a_partial_chunk_and_write_zeroes_over_holes() {
    for cs in CS {
        let mut v = vol(cs);
        let mut r = v.open().map_err(|e| e.0).expect("open");
        let csz = u64::from(cs);
        let data = pattern(5, cs as usize);
        assert!(r.write(0, 2 * csz, &data, false).is_ok());
        r.engine.commit_now(false).expect("commit");
        // Discard the middle of the chunk.
        assert!(r.hole(1, IoOp::Discard, 2 * csz + 4096, 8192).is_ok());
        let mut want = data.clone();
        want[4096..12288].fill(0);
        assert_eq!(r.read(0, 2 * csz, cs).1, want, "overlay hole");
        r.engine.commit_now(false).expect("commit");
        assert_eq!(r.read(0, 2 * csz, cs).1, want, "partial hole destaged");
        // Write-zeroes over never-written chunks and a whole chunk.
        assert!(r.hole(0, IoOp::WriteZeroes, 5 * csz, 3 * cs).is_ok());
        assert!(r.hole(0, IoOp::WriteZeroes, 2 * csz, cs).is_ok());
        r.engine.commit_now(false).expect("commit");
        assert_eq!(r.read(0, 2 * csz, cs).1, vec![0; cs as usize]);
        assert_eq!(r.read(0, 5 * csz, 3 * cs).1, vec![0; 3 * cs as usize]);
        let p = r.stop();
        v.absorb(p, 3);
        let mut r = v.open().map_err(|e| e.0).expect("reopen");
        assert_eq!(r.read(0, 2 * csz, cs).1, vec![0; cs as usize]);
    }
}

#[test]
fn fua_and_flush_survive_a_crash_that_drops_everything_unflushed() {
    for cs in CS {
        let mut v = vol(cs);
        let mut r = v.open().map_err(|e| e.0).expect("open");
        let a = pattern(9, 8192);
        assert!(r.write(0, 0, &a, true).is_ok(), "fua");
        let b = pattern(10, 4096);
        assert!(r.write(1, 65536, &b, false).is_ok());
        assert!(r.flush(1).is_ok());
        let p = r.stop();
        v.store = p.store.crash(1);
        v.index = p.index.image();
        v.journal = p
            .journal
            .crash_with(1, lunix_testkit::CrashModel::lose_all());
        let mut r = v.open().map_err(|e| e.0).expect("reopen");
        assert_eq!(r.read(0, 0, 8192).1, a);
        assert_eq!(r.read(0, 65536, 4096).1, b);
    }
}

#[test]
fn idle_volume_creates_no_commit_in_five_intervals() {
    let v = vol(16 << 10);
    let mut r = v.open().map_err(|e| e.0).expect("open");
    let t = r.engine.tip();
    for _ in 0..50 {
        v.clock.advance(Duration::from_secs(3));
        assert_eq!(r.engine.tick().expect("tick"), CommitOutcome::Idle);
    }
    assert_eq!(r.engine.tip(), t);
    // One write: the interval trigger fires once.
    assert!(r.write(0, 0, &pattern(1, 4096), false).is_ok());
    assert_eq!(r.engine.tick().expect("tick"), CommitOutcome::Idle);
    v.clock.advance(Duration::from_secs(31));
    assert!(matches!(
        r.engine.tick().expect("tick"),
        CommitOutcome::Committed { .. }
    ));
    v.clock.advance(Duration::from_secs(31));
    assert_eq!(r.engine.tick().expect("tick"), CommitOutcome::Idle);
}

#[test]
fn created_at_is_clamped_after_a_clock_step_back() {
    let v = vol(16 << 10);
    let mut r = v.open().map_err(|e| e.0).expect("open");
    assert!(r.write(0, 0, &pattern(1, 4096), false).is_ok());
    r.engine.commit_now(false).expect("commit");
    let (tip, _) = r.engine.tip();
    let parent = v
        .codec
        .open_commit(tip, &CommitStore::get(&r.store, tip).unwrap())
        .unwrap();
    let parent_at = parent.common().created_at;
    v.clock.set_wall(TimeMs(parent_at.0 - 3_600_000));
    assert!(r.write(0, 4096, &pattern(2, 4096), false).is_ok());
    r.engine.commit_now(false).expect("commit");
    let (tip2, _) = r.engine.tip();
    let child = v
        .codec
        .open_commit(tip2, &CommitStore::get(&r.store, tip2).unwrap())
        .unwrap();
    assert_eq!(child.common().created_at, parent_at);
    assert!(v.clock.now() < parent_at);
}

#[test]
fn rollback_floor_r4() {
    let mut v = vol(16 << 10);
    let mut r = v.open().map_err(|e| e.0).expect("open");
    assert!(v.hooks.crashed().is_none());
    for i in 0..3u64 {
        assert!(r.write(0, i * 4096, &pattern(i, 4096), false).is_ok());
        r.engine.commit_now(false).expect("commit");
    }
    let (tip, _) = r.engine.tip();
    let f = (*lock(&v.floor.floor)).expect("floor");
    assert_eq!(f.max_commit_seq, tip, "C7 raised the floor to the tip");
    let hash1 = v
        .codec
        .object_hash(&CommitStore::get(&r.store, CommitSeq(1)).unwrap());
    let p = r.stop();
    v.absorb(p, 1);
    // Tip below the floor: Rollback.
    *lock(&v.floor.floor) = Some(RollbackFloor {
        max_commit_seq: CommitSeq(tip.0 + 1),
        tip_hash: [0; 32],
    });
    assert!(matches!(
        v.open().map(|_| ()).map_err(|e| e.0),
        Err(OpenError::Rollback { .. })
    ));
    // Same height, another hash: Fork.
    *lock(&v.floor.floor) = Some(RollbackFloor {
        max_commit_seq: tip,
        tip_hash: [9; 32],
    });
    assert!(matches!(
        v.open().map(|_| ()).map_err(|e| e.0),
        Err(OpenError::Fork { .. })
    ));
    // Below the tip, another commit at its height on the path (ADR-31,
    // 2026-11-13): Fork, also when the floor is below the tip's chain_floor.
    *lock(&v.floor.floor) = Some(RollbackFloor {
        max_commit_seq: CommitSeq(1),
        tip_hash: [1; 32],
    });
    match v.open().map(|_| ()).map_err(|e| e.0) {
        Err(OpenError::Fork { tip: at }) => assert_eq!(at, CommitSeq(1)),
        other => panic!("expected Fork at the floor's height, got {other:?}"),
    }
    // Below the tip, the recorded commit on the path: opens and raises it.
    *lock(&v.floor.floor) = Some(RollbackFloor {
        max_commit_seq: CommitSeq(1),
        tip_hash: hash1,
    });
    let r = v.open().map_err(|e| e.0).expect("open above the floor");
    assert!(!r.engine.recovery_report().first_attach);
    assert_eq!((*lock(&v.floor.floor)).expect("floor").max_commit_seq, tip);
    drop(r.stop());
    // No floor: first attach, sets it.
    *lock(&v.floor.floor) = None;
    let r = v.open().map_err(|e| e.0).expect("first attach");
    assert!(r.engine.recovery_report().first_attach);
    assert_eq!((*lock(&v.floor.floor)).expect("floor").max_commit_seq, tip);
    assert!(v.violations().is_empty(), "{:?}", v.violations());
}

#[test]
fn a_crash_between_c6_and_the_floor_reopens_with_the_lower_floor() {
    let mut v = vol(16 << 10);
    let mut r = v.open().map_err(|e| e.0).expect("open");
    assert!(r.write(0, 0, &pattern(1, 4096), false).is_ok());
    r.engine.commit_now(false).expect("commit");
    let before = (*lock(&v.floor.floor)).expect("floor");
    assert!(r.write(0, 0, &pattern(2, 4096), false).is_ok());
    v.hooks.arm("engine.c7.checkpoint", 1);
    assert!(r.engine.commit_now(false).is_err());
    assert_eq!(
        (*lock(&v.floor.floor)).expect("floor"),
        before,
        "floor not raised yet"
    );
    let p = r.stop();
    v.absorb(p, 2);
    let mut r = v.open().map_err(|e| e.0).expect("reopen");
    assert_eq!(r.read(0, 0, 4096).1, pattern(2, 4096));
    assert!(
        (*lock(&v.floor.floor)).expect("floor").max_commit_seq > before.max_commit_seq,
        "the open raised it to the durable tip"
    );
}

#[test]
fn a_tip_that_does_not_authenticate_is_a_typed_error_without_fallback() {
    let mut v = vol(16 << 10);
    let mut r = v.open().map_err(|e| e.0).expect("open");
    assert!(r.write(0, 0, &pattern(1, 4096), false).is_ok());
    r.engine.commit_now(false).expect("commit");
    let (tip, _) = r.engine.tip();
    let mut bytes = CommitStore::get(&r.store, tip).unwrap();
    let n = bytes.len();
    bytes[n - 1] ^= 1;
    r.store.corrupt_commit(tip, &bytes);
    let p = r.stop();
    v.absorb(p, 1);
    match v.open() {
        Err((OpenError::TipUnauthenticated { tip: t, .. }, _)) => assert_eq!(t, tip),
        Err((e, _)) => panic!("unexpected {e}"),
        Ok(_) => panic!("opened a tampered tip"),
    }
}

#[test]
fn a_checkpoint_from_beyond_the_tip_is_ignored_and_the_records_replay() {
    let mut v = vol(16 << 10);
    let mut r = v.open().map_err(|e| e.0).expect("open");
    assert!(r.write(0, 0, &pattern(1, 4096), false).is_ok());
    r.engine.commit_now(false).expect("commit");
    assert!(r.write(1, 4096, &pattern(2, 4096), false).is_ok());
    assert!(r.flush(1).is_ok());
    r.engine.commit_now(false).expect("commit");
    let (tip, _) = r.engine.tip();
    // fsck --truncate-tip, without zeroing the checkpoint; it lowers the
    // rollback floor to the new tip (D19).
    r.store.set_aside(tip).expect("set aside");
    let prev = CommitSeq(tip.0 - 1);
    let prev_bytes = CommitStore::get(&r.store, prev).unwrap();
    *lock(&v.floor.floor) = Some(RollbackFloor {
        max_commit_seq: prev,
        tip_hash: v.codec.object_hash(&prev_bytes),
    });
    let p = r.stop();
    v.absorb(p, 1);
    let mut r = v.open().map_err(|e| e.0).expect("reopen");
    assert!(!r.engine.recovery_report().checkpoint_used);
    assert_eq!(r.engine.tip().0, CommitSeq(tip.0 - 1));
    assert_eq!(
        (*lock(&v.floor.floor)).expect("floor").max_commit_seq,
        prev,
        "the stale checkpoint does not move the floor"
    );
    // The second write is above the new floor and still in the journal.
    assert_eq!(r.read(0, 4096, 4096).1, pattern(2, 4096));
}

#[test]
fn orphans_above_the_tip_are_swept_and_their_ids_never_reissued() {
    let mut v = vol(16 << 10);
    let mut r = v.open().map_err(|e| e.0).expect("open");
    for i in 0..4u64 {
        assert!(r.write(0, i * 16384, &pattern(i, 16384), false).is_ok());
    }
    assert!(r.flush(0).is_ok());
    // Crash between C4 and C6: the segment and pack are orphans.
    v.hooks.arm("engine.c5.map_sealed", 1);
    assert!(r.engine.commit_now(false).is_err());
    let seg_ids: Vec<u64> = lunix_iface::store::SegmentStore::list(&r.store)
        .unwrap()
        .iter()
        .map(|s| s.seg.raw())
        .collect();
    assert!(!seg_ids.is_empty());
    let p = r.stop();
    v.absorb(p, 4);
    let mut r = v.open().map_err(|e| e.0).expect("reopen");
    let rep = r.engine.recovery_report().clone();
    assert!(!rep.sweep.deleted_segments.is_empty(), "{rep:?}");
    let highest = rep.sweep.highest_id_before;
    // Re-destage the same data: dedup never adopts the swept orphans, and
    // new ids are above everything that existed.
    r.engine.commit_now(false).expect("commit");
    for s in lunix_iface::store::SegmentStore::list(&r.store).unwrap() {
        assert!(s.seg.raw() > highest, "id {} reissued", s.seg);
    }
    for i in 0..4u64 {
        assert_eq!(r.read(0, i * 16384, 16384).1, pattern(i, 16384));
    }
    let p = r.stop();
    v.absorb(p, 5);
    let mut r = v.open().map_err(|e| e.0).expect("reopen");
    for i in 0..4u64 {
        assert_eq!(r.read(0, i * 16384, 16384).1, pattern(i, 16384));
    }
    assert!(v.violations().is_empty(), "{:?}", v.violations());
}

#[test]
fn the_ledger_equals_the_files_after_every_commit() {
    let v = vol(16 << 10);
    let mut r = v.open().map_err(|e| e.0).expect("open");
    for round in 0..6u64 {
        for i in 0..5u64 {
            assert!(r
                .write(
                    i as usize,
                    ((round * 5 + i) % 16) * 16384,
                    &pattern(round * 10 + i, 16384),
                    false
                )
                .is_ok());
        }
        r.engine.commit_now(false).expect("commit");
        let snap = r.space.snapshot();
        let segs: u64 = lunix_iface::store::SegmentStore::list(&r.store)
            .unwrap()
            .iter()
            .map(|s| s.size_bytes)
            .sum();
        let packs: u64 = lunix_iface::store::PackRead::list_packs(&r.store)
            .unwrap()
            .iter()
            .map(|s| s.size_bytes)
            .sum();
        let commits: u64 = CommitStore::list(&r.store)
            .unwrap()
            .iter()
            .map(|c| c.size_bytes)
            .sum();
        let runs: u64 = lunix_iface::index::ChunkIndex::all_runs(&*r.index)
            .unwrap()
            .iter()
            .map(|c| c.size_bytes)
            .sum();
        assert_eq!(snap.by_kind[SpaceKind::Segments.index()], segs);
        assert_eq!(snap.by_kind[SpaceKind::Map.index()], packs);
        assert_eq!(snap.by_kind[SpaceKind::Commits.index()], commits);
        assert_eq!(snap.by_kind[SpaceKind::Index.index()], runs);
        assert_eq!(snap.by_kind[SpaceKind::Headroom.index()], 0);
        assert_eq!(
            snap.by_kind[SpaceKind::JournalReserved.index()],
            r.engine.journal_charged()
        );
    }
    let _ = r.space.snapshot().used();
}

#[test]
fn grow_across_a_leaf_boundary_keeps_old_data_and_serves_new_space() {
    let cs = 16u64 << 10;
    let mut v = Vol::create(cs as u32, 500, 1, JournalMode::Mixed(2));
    let mut r = v.open().map_err(|e| e.0).expect("open");
    assert!(r
        .write(0, 499 * cs, &pattern(1, cs as usize), false)
        .is_ok());
    // Past the end before the grow: EINVAL.
    let t = r.submit(0, IoOp::Write, false, 600 * cs, 4096, &pattern(2, 4096));
    assert_eq!(r.wait(&[t])[0].0.errno(), Some(22));
    r.engine.grow(1100 * cs).expect("grow");
    assert_eq!(r.engine.size_bytes(), 1100 * cs);
    assert!(matches!(
        r.engine.grow(10 * cs).map_err(|e| e.kind()),
        Err(lunix_iface::ErrorKind::Unsupported)
    ));
    assert!(r
        .write(0, 1099 * cs, &pattern(3, cs as usize), false)
        .is_ok());
    assert_eq!(r.read(0, 1000 * cs, 4096).1, vec![0; 4096]);
    r.engine.commit_now(false).expect("commit");
    let p = r.stop();
    v.absorb(p, 1);
    let mut r = v.open().map_err(|e| e.0).expect("reopen");
    assert_eq!(r.engine.size_bytes(), 1100 * cs);
    assert_eq!(r.read(0, 499 * cs, cs as u32).1, pattern(1, cs as usize));
    assert_eq!(r.read(0, 1099 * cs, cs as u32).1, pattern(3, cs as usize));
}

#[test]
fn a_chain_whose_created_at_decreases_mounts() {
    use lunix_format::commit::Commit;
    let mut v = vol(16 << 10);
    let mut r = v.open().map_err(|e| e.0).expect("open");
    assert!(r.write(0, 0, &pattern(1, 4096), false).is_ok());
    r.engine.commit_now(false).expect("commit");
    let (tip, _) = r.engine.tip();
    let bytes = CommitStore::get(&r.store, tip).unwrap();
    let Commit::Full(mut f) = v.codec.open_commit(tip, &bytes).unwrap() else {
        panic!("full");
    };
    // Another writer with a slow clock appended a commit.
    f.common.commit_seq = CommitSeq(tip.0 + 1);
    f.common.parent_hash = v.codec.object_hash(&bytes);
    f.common.created_at = TimeMs(f.common.created_at.0 - 60_000);
    f.chain_floor = tip;
    let sealed = v.codec.seal_commit(&Commit::Full(f)).unwrap();
    CommitStore::put(&r.store, CommitSeq(tip.0 + 1), &sealed.bytes).unwrap();
    let p = r.stop();
    v.absorb(p, 1);
    let mut r = v
        .open()
        .map_err(|e| e.0)
        .expect("a decreasing created_at is not a mount error");
    assert_eq!(r.engine.tip().0, CommitSeq(tip.0 + 1));
    assert_eq!(r.read(0, 0, 4096).1, pattern(1, 4096));
}

#[test]
fn after_truncate_tip_and_the_sweep_no_old_id_is_reissued() {
    let mut v = vol(16 << 10);
    let mut r = v.open().map_err(|e| e.0).expect("open");
    assert!(r.write(0, 0, &pattern(1, 16384), false).is_ok());
    r.engine.commit_now(false).expect("commit");
    assert!(r.write(0, 16384, &pattern(2, 16384), false).is_ok());
    assert!(r.flush(0).is_ok());
    r.engine.commit_now(false).expect("commit");
    let (tip, _) = r.engine.tip();
    let highest = lunix_iface::store::SegmentStore::list(&r.store)
        .unwrap()
        .iter()
        .map(|s| s.seg.raw())
        .chain(
            lunix_iface::store::PackRead::list_packs(&r.store)
                .unwrap()
                .iter()
                .map(|p| p.pack.raw()),
        )
        .max()
        .unwrap();
    // fsck --repair --truncate-tip: tip aside, checkpoint zeroed, floor lowered.
    r.store.set_aside(tip).unwrap();
    lunix_iface::store::CheckpointStore::zero(&r.store).unwrap();
    let prev = CommitSeq(tip.0 - 1);
    let prev_bytes = CommitStore::get(&r.store, prev).unwrap();
    *lock(&v.floor.floor) = Some(RollbackFloor {
        max_commit_seq: prev,
        tip_hash: v.codec.object_hash(&prev_bytes),
    });
    let p = r.stop();
    v.absorb(p, 1);
    let mut r = v.open().map_err(|e| e.0).expect("reopen");
    let rep = r.engine.recovery_report().clone();
    assert!(!rep.sweep.deleted_segments.is_empty(), "{rep:?}");
    assert!(rep.sweep.highest_id_before >= highest);
    assert!(r.write(0, 32768, &pattern(3, 16384), false).is_ok());
    r.engine.commit_now(false).expect("commit");
    let new_ids: Vec<u64> = lunix_iface::store::SegmentStore::list(&r.store)
        .unwrap()
        .iter()
        .map(|s| s.seg.raw())
        .filter(|id| !rep.sweep.deleted_segments.iter().any(|d| d.raw() == *id))
        .collect();
    assert!(
        new_ids.iter().any(|id| *id > highest),
        "a new segment was written"
    );
    for d in &rep.sweep.deleted_segments {
        assert!(!new_ids.contains(&d.raw()), "swept id {d} reissued");
    }
    // The flushed write above the old floor replays; the new one is there.
    assert_eq!(r.read(0, 16384, 16384).1, pattern(2, 16384));
    assert_eq!(r.read(0, 32768, 16384).1, pattern(3, 16384));
}

#[test]
fn a_wrapped_ring_with_a_lost_checkpoint_recovers() {
    let mut v = Vol::create(16 << 10, 64, 2, JournalMode::Mixed(4));
    v.set_journal(lunix_testkit::MemJournalConfig {
        lanes: 2,
        file_bytes: lunix_testkit::journal::MIN_FILE_BYTES,
        max_files: 6,
        pool_depth: 64,
        mode: JournalMode::Mixed(4),
    });
    let mut r = v.open().map_err(|e| e.0).expect("open");
    let size = r.engine.size_bytes();
    let mut want = vec![0u8; size as usize];
    for i in 0..300u64 {
        let off = (i * 3 * 16384) % size;
        let d = pattern(i, 16384);
        assert!(r.write(i as usize, off, &d, false).is_ok());
        want[off as usize..off as usize + 16384].copy_from_slice(&d);
        if i % 10 == 9 {
            let _ = r.engine.tick();
        }
    }
    assert!(r.journal.reclaim_calls() > 0, "the ring wrapped");
    assert!(r.flush(0).is_ok());
    lunix_iface::store::CheckpointStore::zero(&r.store).unwrap();
    let p = r.stop();
    v.absorb(p, 9);
    let mut r = v.open().map_err(|e| e.0).expect("reopen");
    assert!(!r.engine.recovery_report().checkpoint_used);
    assert_eq!(r.read_all(), want);
}

fn small_cuts(cfg: &mut lunix_engine::EngineConfig) {
    cfg.map_fanout = 4;
    cfg.max_leaves_per_cut = 2;
}

#[test]
fn a_cut_is_bounded_by_max_leaves_per_cut_and_big_discards_are_split() {
    let cs = 16u64 << 10;
    let mut v = Vol::create(cs as u32, 64, 2, JournalMode::Mixed(6));
    v.tweak = small_cuts;
    let mut r = v.open().map_err(|e| e.0).expect("open");
    for c in 0..20u64 {
        assert!(r
            .write(c as usize, c * cs, &pattern(c, cs as usize), false)
            .is_ok());
    }
    let mut commits = 0;
    while let CommitOutcome::Committed { cut_seq, .. } = r.engine.commit_now(false).expect("commit")
    {
        commits += 1;
        let tip = r.engine.tip().0;
        let bytes = CommitStore::get(&r.store, tip).unwrap();
        let lunix_format::commit::Commit::Full(f) = v.codec.open_commit(tip, &bytes).unwrap()
        else {
            panic!("full");
        };
        assert_eq!(f.cut_seq, cut_seq);
        assert!(commits < 50);
    }
    assert!(
        commits >= 3,
        "20 chunks, 4 per leaf, at most 2 leaves per cut: {commits} commits"
    );
    // A discard of the whole device is several records.
    v.hooks.take_seqs();
    assert!(r.hole(0, IoOp::Discard, 0, (64 * cs) as u32).is_ok());
    assert!(
        v.hooks.take_seqs().len() >= 64 / 4,
        "split into records of one leaf (4 chunks)"
    );
    while r.engine.commit_now(false).expect("commit") != CommitOutcome::Idle {}
    let p = r.stop();
    v.absorb(p, 1);
    let mut r = v.open().map_err(|e| e.0).expect("reopen");
    assert_eq!(r.read_all(), vec![0; (64 * cs) as usize]);
}

#[test]
fn a_map_full_cut_is_retried_smaller_and_leaves_no_segment_behind() {
    let cs = 16u64 << 10;
    let mut v = Vol::create(cs as u32, 1100, 1, JournalMode::Mixed(8));
    v.dirty_cap = Some(1);
    let mut r = v.open().map_err(|e| e.0).expect("open");
    assert!(r.write(0, 0, &pattern(1, cs as usize), false).is_ok());
    assert!(r
        .write(0, 600 * cs, &pattern(2, cs as usize), false)
        .is_ok());
    // Two leaves dirty in one cut exceed the cap of 1: the cut is retried at
    // the first write only, then the second commit takes the rest.
    let first = r.engine.commit_now(false).expect("commit");
    assert!(
        matches!(first, CommitOutcome::Committed { cut_seq, .. } if cut_seq.0 == 1),
        "{first:?}"
    );
    assert!(matches!(
        r.engine.commit_now(false).expect("commit"),
        CommitOutcome::Committed { .. }
    ));
    // Every segment on disk is named by a commit: the failed attempt's
    // segment was deleted by the destager.
    let mut named = std::collections::HashSet::new();
    for c in CommitStore::list(&r.store).unwrap() {
        let bytes = CommitStore::get(&r.store, c.seq).unwrap();
        if let lunix_format::commit::Commit::Full(f) = v.codec.open_commit(c.seq, &bytes).unwrap() {
            named.extend(f.new_segments);
        }
    }
    for s in lunix_iface::store::SegmentStore::list(&r.store).unwrap() {
        assert!(named.contains(&s.seg), "orphan segment {}", s.seg);
    }
    assert_eq!(r.read(0, 0, cs as u32).1, pattern(1, cs as usize));
    assert_eq!(r.read(0, 600 * cs, cs as u32).1, pattern(2, cs as usize));
}

#[test]
fn an_unwalkable_chain_back_to_the_floor_is_refused_unless_overridden() {
    let mut v = vol(16 << 10);
    let mut r = v.open().map_err(|e| e.0).expect("open");
    for i in 0..4u64 {
        assert!(r.write(0, i * 4096, &pattern(i, 4096), false).is_ok());
        r.engine.commit_now(false).expect("commit");
    }
    let hash1 = v
        .codec
        .object_hash(&CommitStore::get(&r.store, CommitSeq(1)).unwrap());
    let (tip, _) = r.engine.tip();
    // Commit 1 is below the tip's chain_floor: a GC may have deleted it.
    CommitStore::delete(&r.store, CommitSeq(1), CommitSeq(tip.0 - 1)).expect("delete");
    let p = r.stop();
    v.absorb(p, 1);
    *lock(&v.floor.floor) = Some(RollbackFloor {
        max_commit_seq: CommitSeq(1),
        tip_hash: hash1,
    });
    match v.open().map(|_| ()).map_err(|e| e.0) {
        Err(OpenError::ForkUnverifiable { floor, tip: t, .. }) => {
            assert_eq!((floor, t), (CommitSeq(1), tip));
        }
        other => panic!("expected ForkUnverifiable, got {other:?}"),
    }
    assert_eq!(
        (*lock(&v.floor.floor)).expect("floor").max_commit_seq,
        CommitSeq(1),
        "a refusal raises nothing"
    );
    fn allow(c: &mut lunix_engine::EngineConfig) {
        c.allow_unverifiable_fork = true;
    }
    v.tweak = allow;
    let r = v.open().map_err(|e| e.0).expect("the operator override opens");
    assert_eq!((*lock(&v.floor.floor)).expect("floor").max_commit_seq, tip);
    drop(r.stop());
}
