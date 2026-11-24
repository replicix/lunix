//! The pin rule of condemn-on-cut on the real stack (milestone condemncut):
//! while a snapshot pin exists nothing is offered or condemned by a proposal
//! (the table knows the tip only; the pin is a root it cannot see), the
//! pinned commit stays readable, and once the pin is gone the segments the
//! overwrites killed are proposed, condemned and deleted.
//!
//! A snapshot pin is a sealed object `snapshots/<name>` of the volume
//! directory; the test seals one with the stack's own `MetaCodec` (plain mode)
//! over the tip commit.

#![cfg(feature = "testsupport")]

use std::sync::Arc;
use std::time::{Duration, Instant};

use lunix_engine::{CondemnOnCut, EngineConfig};
use lunix_format::pins::{Consistency, SnapshotPin};
use lunix_format::{CryptoMode, ObjectKind, TimeMs};
use lunix_gc::GcConfig;
use lunix_iface::crypto::MetaCodec;
use lunix_iface::gc::RefsState;
use lunixd::stack::VolumeStack;
use lunixd::testsupport::{close, payload, Driver, Scratch, VolumeBuilder};

const CS: u32 = 16 << 10;
const CHUNKS: u64 = 256;
const NAME: &str = "keep";

fn status(stack: &VolumeStack) -> lunix_iface::gc::GcStatusSnapshot {
    stack.engine.gc_status_handle().get()
}

/// Overwrites the first `CHUNKS / 2` chunks once and cuts.
fn round(stack: &VolumeStack, d: &mut Driver, seed: u64) {
    for c in 0..CHUNKS / 2 {
        let data = payload(seed * 1_000 + c, CS as usize);
        assert!(d.write(0, c * u64::from(CS), &data, false).is_ok());
    }
    assert!(d.flush(0).is_ok());
    stack.engine.commit_now(true).expect("cut");
}

/// Ticks until `done` or the deadline.
fn tick_until(stack: &VolumeStack, what: &str, secs: u64, done: impl Fn(&VolumeStack) -> bool) {
    let start = Instant::now();
    while !done(stack) {
        stack.engine.tick().expect("tick");
        assert!(
            start.elapsed() < Duration::from_secs(secs),
            "{what}: {:?}",
            status(stack)
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn nothing_is_proposed_while_a_snapshot_pin_exists_and_proposals_resume_without_it() {
    let t = Scratch::new("condemncut-pin");
    let mut b = VolumeBuilder::new(t.path())
        .manual()
        .mode(CryptoMode::Plain)
        .chunk_size(CS)
        .size_bytes(CHUNKS * u64::from(CS));
    b.engine = Some(Arc::new(|c: &mut EngineConfig| {
        if let Some(p) = &mut c.gc_params {
            p.read_timeout = Duration::ZERO;
            p.grace = Duration::from_secs(10);
        }
    }));
    let vol = b.create().expect("create");
    let stack = vol
        .open_with(|o| {
            o.gc = Some(GcConfig {
                pass_interval: Duration::from_secs(20),
                busy_interval: Duration::from_secs(20),
                fast_recheck: Some(Duration::from_secs(1)),
                fast_recheck_always: true,
                ..GcConfig::daemon()
            });
            o.gc_condemn_on_cut = CondemnOnCut::On;
        })
        .expect("open");
    let mut d = Driver::new(&stack).expect("driver");
    let size = CHUNKS * u64::from(CS);
    // The table's base: the first mark after the attach.
    round(&stack, &mut d, 1);
    tick_until(&stack, "the table is not ready", 120, |s| {
        status(s).refs.is_some_and(|t| t.state == RefsState::Ready)
    });
    // The pin: the tip commit, sealed as the store would read it.
    let (tip, _) = stack.engine.tip();
    let mut body = Vec::new();
    SnapshotPin {
        consistency: Consistency::Crash,
        commit_seq: tip,
        created: TimeMs(1),
        label: "the pin of the test".into(),
    }
    .encode(&mut body)
    .expect("encode");
    let name = lunix_format::paths::snapshot(NAME).expect("name");
    let sealed = stack
        .crypto
        .meta()
        .seal(ObjectKind::Snapshots, &name, &body)
        .expect("seal");
    let pin_file = stack.voldir.join("snapshots").join(NAME);
    std::fs::create_dir_all(pin_file.parent().expect("dir")).expect("dir");
    std::fs::write(&pin_file, sealed).expect("pin");
    let pinned_data = d.read_all(size).expect("read");
    // The overwrites kill every segment the pinned commit names, one cut
    // after another (more than the lag of two cuts).
    for r in 2..8 {
        round(&stack, &mut d, r);
        stack.engine.tick().expect("tick");
    }
    let before = status(&stack);
    // Ticks for longer than a grace: a table without the pin rule would have
    // proposed everything by now.
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(14) {
        stack.engine.tick().expect("tick");
        std::thread::sleep(Duration::from_millis(100));
    }
    let st = status(&stack);
    let r = st.reclaim;
    let t = st.refs.expect("table");
    assert_eq!(
        r.proposal_condemned, 0,
        "a proposal condemned a segment while a pin existed: {r:?}"
    );
    assert_eq!(r.proposals_offered, 0, "{r:?}");
    assert!(
        t.pin_blocked > 0,
        "the pin withheld no offer: {t:?} (before: {:?})",
        before.refs
    );
    assert_eq!(
        (t.drift_segments, t.shadow_unconfirmed_named),
        (0, 0),
        "{t:?}"
    );
    assert_eq!(r.proposal_rejected_early, 0, "{r:?}");
    // The data is what the last round wrote, and the pinned commit's
    // segments are all still there (the marks keep them too).
    let now = d.read_all(size).expect("read");
    assert_ne!(now, pinned_data);
    let pinned_segments = lunix_iface::store::SegmentStore::list(&stack.store)
        .expect("list")
        .len();

    // The pin goes. The proposals resume, condemn and delete.
    std::fs::remove_file(&pin_file).expect("unpin");
    for r in 8..12 {
        round(&stack, &mut d, r);
    }
    tick_until(
        &stack,
        "no proposal condemned and deleted a segment",
        120,
        |s| {
            let r = status(s).reclaim;
            r.proposal_condemned > 0 && r.proposal_deleted > 0
        },
    );
    let st = status(&stack);
    let r = st.reclaim;
    assert_eq!(r.proposal_rejected_early, 0, "{r:?}");
    assert_eq!(r.propose_errors, 0, "{r:?}");
    let t = st.refs.expect("table");
    assert_eq!(
        (t.drift_segments, t.shadow_unconfirmed_named),
        (0, 0),
        "{t:?}"
    );
    assert!(
        lunix_iface::store::SegmentStore::list(&stack.store)
            .expect("list")
            .len()
            < pinned_segments + 64,
        "the store grew without bound"
    );
    let end = d.read_all(size).expect("read");
    for c in 0..CHUNKS / 2 {
        let want = payload(11 * 1_000 + c, CS as usize);
        let at = (c * u64::from(CS)) as usize;
        assert!(
            end[at..at + CS as usize] == want[..],
            "chunk {c} after the pin"
        );
    }
    drop(d);
    close(stack).expect("close");
}
