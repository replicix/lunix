//! ADR-33 T5: `status` and `gc --status` answer while a commit holds the
//! engine's coordinator mutex. In-process: a test hook holds the mutex, as a
//! long commit does, and the calls must still return.

#![cfg(feature = "testsupport")]

mod common;

use std::sync::{mpsc, Arc};
use std::time::Duration;

use lunix_iface::ctl::{
    Attach, Compress, NameParam, Request, Response, VolumeCreate, VolumeMode, VolumeState,
};
use lunixd::audit::Peer;
use lunixd::config::{DaemonConfig, DeviceKind};
use lunixd::daemon::Daemon;

fn call(d: &Daemon, r: Request) -> Response {
    d.handle(Peer::default(), r).0.expect("call")
}

#[test]
fn status_and_gc_status_return_while_the_coordinator_mutex_is_held() {
    let Some(t) = common::scratch("tipstat") else {
        return;
    };
    let cfg = DaemonConfig {
        state_dir: t.path().join("state"),
        socket: t.path().join("ctl.sock"),
        dev_dir: t.path().join("dev"),
        device: DeviceKind::None,
        harden: false,
        ..DaemonConfig::default()
    };
    let d = Arc::new(Daemon::new(cfg).expect("daemon"));
    let vols = t.path().join("vols");
    let n = "v";
    let r = call(
        &d,
        Request::VolumeCreate(VolumeCreate {
            name: n.into(),
            dir: vols.to_string_lossy().into_owned(),
            size_bytes: 64 << 20,
            chunk_size: 64 << 10,
            mode: VolumeMode::Integrity,
            compress: Compress::Auto,
            id_len: 16,
            map_fanout: None,
            key_file: None,
            passphrase: None,
            store_max_percent: None,
        }),
    );
    assert!(matches!(r, Response::VolumeCreated(_)), "{r:?}");
    let r = call(
        &d,
        Request::Attach(Attach {
            name: n.into(),
            queues: None,
            depth: None,
            cpus: None,
            max_io_buf_bytes: None,
            credential: None,
            on_journal_damage: None,
            allow_unverifiable_fork: false,
        }),
    );
    assert!(matches!(r, Response::Attached(_)), "{r:?}");

    let engine = d.engine_of(n).expect("attached");
    engine.commit_now(true).expect("commit");
    let want = engine.tip_view();

    let hold = engine.hold_coordinator();
    let (tx, rx) = mpsc::channel();
    let d2 = Arc::clone(&d);
    let h = std::thread::spawn(move || {
        let p = || NameParam { name: n.into() };
        let st = d2.handle(Peer::default(), Request::VolumeStatus(p())).0;
        let gs = d2.handle(Peer::default(), Request::GcStatus(p())).0;
        let _ = tx.send((st, gs));
    });
    let got = rx.recv_timeout(Duration::from_secs(10));
    // Release before joining, so a failure reports instead of hanging.
    drop(hold);
    h.join().expect("status thread");
    let (st, gs) = got.expect("status waited for the coordinator mutex");
    match st.expect("status") {
        Response::VolumeStatus(s) => {
            assert_eq!(s.state, VolumeState::Attached);
            assert_eq!(s.last_commit_seq, want.commit_seq.0);
            assert_eq!(s.last_commit_ms, Some(want.created_at.0));
            // The write path's counters come from atomics and the journal's
            // window, not from the coordinator (ADR-33 T4).
            let cuts = engine.cut_stats();
            assert_eq!(s.write_path.ring_waiters, 0);
            assert_eq!(s.write_path.last_cut_bytes, cuts.last_cut_bytes);
            assert_eq!(s.write_path.last_cut_ms, cuts.last_cut_ms);
            assert_eq!(s.write_path.window_resident_bytes, 0);
            // The jstall figures: the same atomics, no coordinator.
            assert_eq!(s.write_path.ring_parked_writes, 0);
            assert_eq!(s.write_path.ring_wait_max_60s_ms, cuts.ring_wait_max_60s_ms);
            assert_eq!(s.write_path.ring_pace_rate, 0);
            let ws = engine.write_stats();
            assert_eq!(s.write_path.freed_files_by_lane, ws.freed_files_by_lane);
            assert!(!s.write_path.freed_files_by_lane.is_empty());
            assert_eq!(
                s.write_path.write_latency_log2_us.len(),
                lunix_engine::LATENCY_BUCKETS
            );
        }
        r => panic!("{r:?}"),
    }
    assert!(matches!(gs.expect("gc status"), Response::GcStatus(_)));
}
