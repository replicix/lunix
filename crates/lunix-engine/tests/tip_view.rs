//! ADR-33 T5: the tip getters read what the coordinator published and never
//! wait for the coordinator mutex, and what they publish is the committed tip.

mod common;

use std::sync::mpsc;
use std::time::Duration;

use common::*;
use lunix_format::CommitSeq;
use lunix_iface::crypto::CommitCodec;
use lunix_iface::store::CommitStore;
use lunix_testkit::JournalMode;

fn vol() -> Vol {
    Vol::create(16 << 10, 16, 2, JournalMode::Mixed(7))
}

/// The tip and its `created_at` as the store holds them.
fn stored_tip(v: &Vol, r: &Running) -> (CommitSeq, u64) {
    let seq = CommitStore::tip(&r.store)
        .expect("tip")
        .expect("a committed tip");
    let c = v
        .codec
        .open_commit(seq, &CommitStore::get(&r.store, seq).expect("commit"))
        .expect("open commit");
    (seq, c.common().created_at.0)
}

fn assert_published_is_committed(v: &Vol, r: &Running, what: &str) {
    let (seq, at) = stored_tip(v, r);
    assert_eq!(r.engine.tip().0, seq, "{what}: commit_seq");
    assert_eq!(r.engine.tip_created_at().0, at, "{what}: created_at");
}

#[test]
fn the_status_getters_return_while_the_coordinator_mutex_is_held() {
    let v = vol();
    let mut r = v.open().map_err(|e| e.0).expect("open");
    assert!(r.write(0, 0, &pattern(1, 4096), false).is_ok());
    r.engine.commit_now(false).expect("commit");
    let want = (r.engine.tip(), r.engine.tip_created_at());
    let hold = r.engine.hold_coordinator();
    let (tx, rx) = mpsc::channel();
    let engine = &r.engine;
    let got = std::thread::scope(|s| {
        s.spawn(move || {
            let got = (
                engine.tip(),
                engine.tip_created_at(),
                engine.window_turn_left(),
            );
            let _ = tx.send(got);
        });
        let got = rx.recv_timeout(Duration::from_secs(10));
        // Release the mutex before the scope joins, so a failure reports
        // instead of hanging.
        drop(hold);
        got
    });
    let (tip, at, owed) = got.expect("the getters wait for the coordinator mutex");
    assert_eq!((tip, at), want);
    assert_eq!(owed, 0);
}

#[test]
fn the_published_tip_equals_the_committed_tip_after_every_commit() {
    let v = vol();
    let mut r = v.open().map_err(|e| e.0).expect("open");
    assert_published_is_committed(&v, &r, "open");
    let mut last = r.engine.tip().0;
    for i in 0..6u64 {
        assert!(r
            .write(0, (i % 4) * 4096, &pattern(i + 1, 4096), false)
            .is_ok());
        v.clock.advance(Duration::from_secs(1));
        r.engine.commit_now(false).expect("commit");
        assert_published_is_committed(&v, &r, "commit_now");
        assert!(r.engine.tip().0 > last, "the tip advanced");
        last = r.engine.tip().0;
    }
    // A forced FULL with nothing new is a commit too.
    v.clock.advance(Duration::from_secs(1));
    r.engine.commit_now(true).expect("forced commit");
    assert_published_is_committed(&v, &r, "forced");
    // The interval trigger.
    assert!(r.write(0, 0, &pattern(99, 4096), false).is_ok());
    v.clock.advance(Duration::from_secs(31));
    r.engine.tick().expect("tick");
    assert_published_is_committed(&v, &r, "tick");
}

#[test]
fn the_published_tip_equals_the_committed_tip_after_a_crash_and_reopen() {
    let mut v = vol();
    let mut r = v.open().map_err(|e| e.0).expect("open");
    assert!(r.write(0, 0, &pattern(1, 4096), false).is_ok());
    r.engine.commit_now(false).expect("commit");
    // A commit that dies after C6: the durable tip is whatever the store
    // holds, and a reopen must publish exactly that.
    assert!(r.write(0, 0, &pattern(2, 4096), false).is_ok());
    v.hooks.arm("engine.c7.checkpoint", 1);
    assert!(r.engine.commit_now(false).is_err());
    let p = r.stop();
    v.store = p.store.crash(1);
    v.index = p.index.image();
    v.journal = p
        .journal
        .crash_with(1, lunix_testkit::CrashModel::lose_all());
    v.hooks.disarm();
    let mut r = v.open().map_err(|e| e.0).expect("reopen");
    assert_published_is_committed(&v, &r, "reopen");
    assert!(r.write(0, 4096, &pattern(3, 4096), false).is_ok());
    r.engine.commit_now(false).expect("commit after reopen");
    assert_published_is_committed(&v, &r, "commit after reopen");
}
