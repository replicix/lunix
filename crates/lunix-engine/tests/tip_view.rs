//! ADR-33 T5: the tip getters read what the coordinator published and never
//! wait for the coordinator mutex, and what they publish is the committed tip.

mod common;

use std::sync::mpsc;
use std::time::Duration;

use common::*;
use lunix_engine::{Engine, TipView};
use lunix_format::commit::Commit;
use lunix_iface::crypto::CommitCodec;
use lunix_iface::store::CommitStore;
use lunix_testkit::JournalMode;

fn vol() -> Vol {
    Vol::create(16 << 10, 16, 2, JournalMode::Mixed(7))
}

/// The tip as the store holds it: `commit_seq`, `cut_seq`, `created_at`.
fn stored_tip(v: &Vol, r: &Running) -> TipView {
    let seq = CommitStore::tip(&r.store)
        .expect("tip")
        .expect("a committed tip");
    let c = v
        .codec
        .open_commit(seq, &CommitStore::get(&r.store, seq).expect("commit"))
        .expect("open commit");
    let Commit::Full(f) = c else {
        panic!("the tip is a FULL");
    };
    TipView {
        commit_seq: seq,
        cut_seq: f.cut_seq,
        created_at: f.common.created_at,
    }
}

fn assert_published_is_committed(v: &Vol, r: &Running, what: &str) {
    let want = stored_tip(v, r);
    assert_eq!(r.engine.tip_view(), want, "{what}: tip_view");
    assert_eq!(
        r.engine.tip(),
        (want.commit_seq, want.cut_seq),
        "{what}: tip"
    );
    assert_eq!(
        r.engine.tip_created_at(),
        want.created_at,
        "{what}: created_at"
    );
}

/// Reads every status getter on another thread; `None` if they wait.
fn read_getters(engine: &Engine) -> Option<(TipView, u64)> {
    let (tx, rx) = mpsc::channel();
    std::thread::scope(|s| {
        s.spawn(move || {
            let _ = tx.send((engine.tip_view(), engine.window_turn_left()));
        });
        rx.recv_timeout(Duration::from_secs(10)).ok()
    })
}

#[test]
fn the_getters_return_the_new_tip_while_a_commit_holds_the_coordinator() {
    let v = vol();
    let mut r = v.open().map_err(|e| e.0).expect("open");
    assert!(r.write(0, 0, &pattern(1, 4096), false).is_ok());
    r.engine.commit_now(false).expect("commit");
    let old = stored_tip(&v, &r);
    assert!(r.write(0, 4096, &pattern(2, 4096), false).is_ok());
    v.clock.advance(Duration::from_secs(1));

    // Parked before C6: the mutex is held and nothing new is durable, so the
    // view is the old tip.
    let before = v.hooks.park_at("engine.c5.map_sealed");
    let engine = &r.engine;
    let (at_c5, at_c6) = std::thread::scope(|s| {
        let commit = s.spawn(move || engine.commit_now(false));
        before
            .reached
            .recv_timeout(Duration::from_secs(30))
            .expect("parked at C5");
        let at_c5 = read_getters(engine);
        // Park again after C6 (durable, published), still inside the mutex.
        let after = v.hooks.park_at("engine.c6.committed");
        before.release.send(()).expect("release C5");
        after
            .reached
            .recv_timeout(Duration::from_secs(30))
            .expect("parked at C6");
        let at_c6 = read_getters(engine);
        after.release.send(()).expect("release C6");
        commit.join().expect("commit thread").expect("commit");
        (at_c5, at_c6)
    });
    let (view, _) = at_c5.expect("the getters wait for a commit parked at C5");
    assert_eq!(view, old, "nothing is published before C6");
    let (view, owed) = at_c6.expect("the getters wait for a commit parked at C6");
    let new = stored_tip(&v, &r);
    assert!(new.commit_seq > old.commit_seq);
    assert_eq!(view, new, "published once C6 is durable");
    assert_eq!(owed, 0);
    assert_published_is_committed(&v, &r, "after the commit");
}

#[test]
fn a_commit_that_fails_before_c6_leaves_the_published_tip_unchanged() {
    let v = vol();
    let mut r = v.open().map_err(|e| e.0).expect("open");
    assert!(r.write(0, 0, &pattern(1, 4096), false).is_ok());
    r.engine.commit_now(false).expect("commit");
    let old = r.engine.tip_view();
    assert!(r.write(0, 4096, &pattern(2, 4096), false).is_ok());
    v.clock.advance(Duration::from_secs(1));
    v.hooks.arm("engine.c5.map_sealed", 1);
    assert!(r.engine.commit_now(false).is_err());
    assert_eq!(r.engine.tip_view(), old);
    assert_published_is_committed(&v, &r, "failed commit");
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
    // C6 was durable: the view already shows it, before any reopen.
    assert_published_is_committed(&v, &r, "after a crash at C7");
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
