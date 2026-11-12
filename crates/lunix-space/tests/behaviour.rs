//! `LunixSpace` beyond the contract suite: the limit, the file system probe,
//! the ladder, timers, read-only and live reconfiguration.

mod common;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lunix_format::journal::RecordKind;
use lunix_format::local::{FreeFloor, OnFull, SpaceSettings};
use lunix_iface::clock::SystemClock;
use lunix_iface::contract::wait_until;
use lunix_iface::journal::record_bytes;
use lunix_iface::space::{
    Freed, Ladder, LadderStep, LadderStepKind, ReserveOutcome, SpaceEvents, SpaceGate, SpaceKind,
    Urgency, WaiterState,
};
use lunix_iface::wake::WakeFd;
use lunix_iface::ErrorKind;
use lunix_space::{FsProbe, LunixSpace, ManualFs, SpaceConfig, StatvfsProbe};

use common::{clock, gate, gate_manual, settings};

const K: SpaceKind = SpaceKind::Segments;

fn reserved(o: ReserveOutcome) {
    assert!(matches!(o, ReserveOutcome::Reserved), "got {o:?}");
}

fn parked(o: ReserveOutcome) -> lunix_iface::space::Waiter {
    match o {
        ReserveOutcome::Parked(w) => w,
        other => panic!("expected Parked, got {other:?}"),
    }
}

#[test]
fn limit_is_min_of_cache_max_and_fs_room_minus_floor_and_reserve() {
    let fs = Arc::new(ManualFs::new(1000, 600));
    let mut s = settings(30, OnFull::Error, None);
    s.free_floor = FreeFloor::Bytes(100);
    let mut cfg = SpaceConfig::new(s.clone());
    cfg.segment_size = 10; // reserve = 2 * 10 + 30 = 50
    cfg.background_timers = false;
    let g = LunixSpace::new(cfg.clone(), fs.clone(), clock()).expect("gate");
    let snap = g.snapshot();
    assert_eq!(snap.limit, 600 - 100 - 50);
    assert_eq!(snap.headroom_total, 50);
    assert_eq!(snap.fs_free, 600);

    s.cache_max = Some(120);
    cfg.settings = s;
    let g = LunixSpace::new(cfg, fs, clock()).expect("gate");
    assert_eq!(g.snapshot().limit, 120, "cache_max caps the limit");
}

#[test]
fn percent_floor_is_of_the_file_system_size() {
    let fs = Arc::new(ManualFs::new(1000, 500));
    let mut s = settings(0, OnFull::Error, None);
    s.free_floor = FreeFloor::Percent("10%".into());
    let mut cfg = SpaceConfig::new(s);
    cfg.segment_size = 0;
    cfg.background_timers = false;
    let g = LunixSpace::new(cfg, fs, clock()).expect("gate");
    assert_eq!(g.snapshot().limit, 400);
}

#[test]
fn room_is_counted_from_the_ledger_level_of_the_reading() {
    // 1000 free, floor 0: the store may grow by 1000 from where it is.
    let (g, fs) = gate(1000, settings(0, OnFull::Error, None), clock());
    reserved(g.reserve(K, 400, None).unwrap());
    // The 400 reached the disk and a stranger took 100 more.
    fs.set_external(100);
    g.refresh();
    assert_eq!(g.snapshot().limit, 400 + 500);
    reserved(g.reserve(K, 500, None).unwrap());
    assert!(matches!(
        g.reserve(K, 1, None).unwrap(),
        ReserveOutcome::NoSpace
    ));
}

#[test]
fn a_reservation_near_the_limit_reads_the_file_system_first() {
    let (g, fs) = gate(1000, settings(0, OnFull::Error, None), clock());
    let before = fs.calls();
    reserved(g.reserve(K, 100, None).unwrap());
    assert_eq!(fs.calls(), before, "far from the limit: no statvfs");
    // Someone fills the disk; the cached reading still says 1000 free.
    fs.set_external(750);
    assert!(matches!(
        g.reserve(K, 860, None).unwrap(),
        ReserveOutcome::NoSpace
    ));
    assert!(fs.calls() > before, "within 5 percent: statvfs on demand");
    assert_eq!(g.snapshot().limit, 250);
    reserved(g.reserve(K, 100, None).unwrap());
    assert_eq!(g.snapshot().by_kind[K.index()], 200);
}

#[test]
fn space_freed_outside_the_gate_grants_a_waiter_on_refresh() {
    let (g, fs) = gate_manual(100, 100, settings(0, OnFull::Wait, None), clock());
    reserved(g.reserve(K, 100, None).unwrap());
    fs.set_free(0);
    g.refresh();
    let w = parked(g.reserve(K, 50, None).unwrap());
    assert_eq!(w.state(), WaiterState::Pending);
    fs.set_free(60);
    g.refresh();
    assert_eq!(w.state(), WaiterState::Granted);
    assert_eq!(g.snapshot().used(), 150);
}

#[test]
fn the_reserve_is_for_commits_and_never_for_writes() {
    let (g, _fs) = gate(100, settings(40, OnFull::Error, None), clock());
    reserved(g.reserve(SpaceKind::JournalReserved, 60, None).unwrap());
    assert!(matches!(
        g.reserve(SpaceKind::JournalReserved, 1, None).unwrap(),
        ReserveOutcome::NoSpace
    ));
    // A commit bound, a map pack and the checkpoint may use the region.
    g.reserve_headroom(SpaceKind::Headroom, 25).unwrap();
    g.reserve_headroom(SpaceKind::Commits, 15).unwrap();
    // The whole store (limit 60 + region 40) is spent.
    let err = g.reserve_headroom(SpaceKind::Map, 1).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::NoSpace);
    // The pseudo-kind cannot outgrow the region even if the store has room.
    g.release(SpaceKind::JournalReserved, 60);
    let err = g.reserve_headroom(SpaceKind::Headroom, 16).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::NoSpace);
    g.reserve_headroom(SpaceKind::Headroom, 15).unwrap();
    assert_eq!(g.snapshot().by_kind[SpaceKind::Headroom.index()], 40);
    assert_eq!(g.snapshot().used(), 15);
}

#[test]
fn segment_reserve_is_twice_the_segment_size_on_top_of_the_headroom() {
    let mut cfg = common::config(settings(100, OnFull::Error, None));
    cfg.segment_size = 50;
    let (g, _fs) = common::gate_cfg(1000, 0, cfg, clock());
    assert_eq!(g.snapshot().limit, 800);
    reserved(g.reserve(K, 800, None).unwrap());
    assert!(matches!(
        g.reserve(K, 1, None).unwrap(),
        ReserveOutcome::NoSpace
    ));
    g.reserve_headroom(SpaceKind::Commits, 200).unwrap();
}

#[test]
fn error_mode_changes_no_state() {
    let (g, _fs) = gate(100, settings(0, OnFull::Error, None), clock());
    reserved(g.reserve(K, 90, None).unwrap());
    let before = g.snapshot();
    assert!(matches!(
        g.reserve(SpaceKind::Map, 11, None).unwrap(),
        ReserveOutcome::NoSpace
    ));
    let after = g.snapshot();
    assert_eq!(before.by_kind, after.by_kind);
    assert_eq!(before.waiters, 0);
    assert_eq!(after.waiters, 0);
}

#[test]
fn wait_mode_parks_and_proceeds_after_a_release() {
    let (g, _fs) = gate(100, settings(0, OnFull::Wait, None), clock());
    reserved(g.reserve(K, 100, None).unwrap());
    let w = parked(g.reserve(K, 30, None).unwrap());
    assert_eq!(g.snapshot().used(), 100, "a parked request is not charged");
    g.release(K, 30);
    assert_eq!(w.state(), WaiterState::Granted);
    assert_eq!(g.snapshot().used(), 100, "charged at grant");
}

#[test]
fn headroom_kind_cannot_be_reserved_as_a_write() {
    let (g, _fs) = gate(100, settings(0, OnFull::Error, None), clock());
    let e = g.reserve(SpaceKind::Headroom, 1, None).unwrap_err();
    assert_eq!(e.kind(), ErrorKind::Invalid);
    reserved(g.reserve(K, 0, None).unwrap());
}

#[test]
fn journal_is_charged_and_released_in_encoded_record_length() {
    let (g, _fs) = gate(1 << 30, settings(0, OnFull::Error, None), clock());
    let w = record_bytes(RecordKind::Write, 3);
    let h = record_bytes(RecordKind::Hole, 0);
    assert_eq!((w, h), (64 + 3 * 4096, 64));
    reserved(g.reserve(SpaceKind::JournalReserved, w, None).unwrap());
    reserved(g.reserve(SpaceKind::JournalReserved, h, None).unwrap());
    assert_eq!(g.snapshot().by_kind[0], w + h);
    g.release(SpaceKind::JournalReserved, w + h);
    assert_eq!(g.snapshot().used(), 0);
}

#[test]
fn charge_is_a_limitless_startup_accounting() {
    let (g, _fs) = gate(100, settings(0, OnFull::Error, None), clock());
    g.charge(SpaceKind::Segments, 500).unwrap();
    assert_eq!(g.snapshot().by_kind[K.index()], 500);
    assert!(matches!(
        g.reserve(K, 1, None).unwrap(),
        ReserveOutcome::NoSpace
    ));
}

#[test]
fn wake_fd_is_signalled_on_grant_and_on_timeout() {
    let ck = clock();
    let s = settings(0, OnFull::Wait, Some(Duration::from_secs(1)));
    let (g, _fs) = gate(100, s, ck.clone());
    let wake = WakeFd::new().unwrap();
    reserved(g.reserve(K, 100, None).unwrap());
    let a = parked(g.reserve(K, 10, Some(&wake)).unwrap());
    let b = parked(g.reserve(K, 10, Some(&wake)).unwrap());
    g.release(K, 10);
    assert_eq!(a.state(), WaiterState::Granted);
    assert_eq!(wake.drain(), 1);
    ck.advance(Duration::from_millis(1000));
    assert_eq!(g.tick(), 1);
    assert_eq!(b.state(), WaiterState::TimedOut);
    assert_eq!(wake.drain(), 1);
}

#[test]
fn an_expired_head_unblocks_the_smaller_request_behind_it() {
    let ck = clock();
    let (g, _fs) = gate(
        100,
        settings(0, OnFull::Wait, Some(Duration::from_secs(1))),
        ck.clone(),
    );
    reserved(g.reserve(K, 80, None).unwrap());
    let big = parked(g.reserve(K, 50, None).unwrap());
    ck.advance(Duration::from_millis(500));
    let small = parked(g.reserve(K, 10, None).unwrap());
    // The small one fits, but may not overtake.
    assert_eq!(small.state(), WaiterState::Pending);
    ck.advance(Duration::from_millis(500));
    assert_eq!(g.tick(), 1);
    assert_eq!(big.state(), WaiterState::TimedOut);
    assert_eq!(small.state(), WaiterState::Granted);
    assert_eq!(g.snapshot().used(), 90);
}

#[test]
fn a_cancelled_waiter_is_skipped_and_charged_nothing() {
    let (g, _fs) = gate(100, settings(0, OnFull::Wait, None), clock());
    reserved(g.reserve(K, 100, None).unwrap());
    let a = parked(g.reserve(K, 50, None).unwrap());
    let b = parked(g.reserve(K, 10, None).unwrap());
    assert!(a.cancel());
    assert_eq!(g.snapshot().waiters, 1);
    g.release(K, 10);
    assert_eq!(b.state(), WaiterState::Granted);
    assert_eq!(g.snapshot().used(), 100);
}

#[test]
fn a_granted_waiter_that_loses_the_cancel_race_must_release() {
    let (g, _fs) = gate(100, settings(0, OnFull::Wait, None), clock());
    reserved(g.reserve(K, 100, None).unwrap());
    let a = parked(g.reserve(K, 10, None).unwrap());
    g.release(K, 10);
    assert!(!a.cancel(), "already granted");
    g.release(K, 10);
    assert_eq!(g.snapshot().used(), 90);
}

#[test]
fn the_real_clock_expires_a_waiter_by_itself() {
    let s = settings(0, OnFull::Wait, Some(Duration::from_millis(80)));
    let mut cfg = common::config(s);
    cfg.background_timers = true;
    let (g, _fs) = common::gate_cfg(100, 0, cfg, Arc::new(SystemClock));
    reserved(g.reserve(K, 100, None).unwrap());
    let t0 = Instant::now();
    let w = parked(g.reserve(K, 1, None).unwrap());
    wait_until(
        "the background thread to expire it",
        Duration::from_secs(5),
        || w.state() == WaiterState::TimedOut,
    );
    let waited = t0.elapsed();
    assert!(waited >= Duration::from_millis(80), "not early: {waited:?}");
    assert!(waited < Duration::from_millis(1500), "prompt: {waited:?}");
    assert_eq!(g.snapshot().used(), 100);
    g.shutdown();
}

#[test]
fn the_background_thread_rereads_the_file_system() {
    let fs = Arc::new(ManualFs::new(100, 0));
    let s = settings(0, OnFull::Wait, None);
    let mut cfg = SpaceConfig::new(s);
    cfg.segment_size = 0;
    let g = LunixSpace::new(cfg, fs.clone(), Arc::new(SystemClock)).unwrap();
    let w = parked(g.reserve(K, 10, None).unwrap());
    fs.set_free(50);
    wait_until("the 1 s refresh", Duration::from_secs(10), || {
        w.state() == WaiterState::Granted
    });
    g.shutdown();
}

struct Recorder {
    name: &'static str,
    log: Arc<Mutex<Vec<(&'static str, Urgency)>>>,
    gate: Option<Arc<LunixSpace>>,
    frees: u64,
}

impl LadderStep for Recorder {
    fn run(&mut self, want: u64, urgency: Urgency) -> lunix_iface::Result<Freed> {
        self.log.lock().unwrap().push((self.name, urgency));
        if self.frees > 0 {
            if let Some(g) = &self.gate {
                g.release(K, self.frees.min(want.max(self.frees)));
            }
        }
        Ok(Freed(self.frees))
    }
}

#[test]
fn the_ladder_runs_in_order_until_enough_is_freed() {
    let (g, _fs) = gate(100, settings(0, OnFull::Wait, None), clock());
    let log = Arc::new(Mutex::new(Vec::new()));
    let mk = |name, frees, gate: Option<Arc<LunixSpace>>| {
        Box::new(Recorder {
            name,
            log: log.clone(),
            gate,
            frees,
        })
    };
    // Registered out of order on purpose.
    g.register(LadderStepKind::ThrottleDestage, mk("throttle", 0, None));
    g.register(LadderStepKind::Compact, mk("compact", 40, Some(g.clone())));
    g.register(LadderStepKind::EvictSynced, mk("synced", 0, None));
    g.register(LadderStepKind::EvictCache, mk("cache", 0, None));
    reserved(g.reserve(K, 100, None).unwrap());
    let w = parked(g.reserve(K, 30, None).unwrap());
    wait_until("the waiter", Duration::from_secs(5), || {
        w.state() == WaiterState::Granted
    });
    let seen: Vec<_> = log.lock().unwrap().iter().map(|e| e.0).collect();
    assert_eq!(seen, ["cache", "synced", "compact"], "stops once it fits");
    assert!(log.lock().unwrap().iter().all(|e| e.1 == Urgency::Urgent));
    g.shutdown();
}

#[test]
fn a_failing_step_is_skipped_and_counted() {
    struct Bad;
    impl LadderStep for Bad {
        fn run(&mut self, _w: u64, _u: Urgency) -> lunix_iface::Result<Freed> {
            Err(lunix_iface::Error::invalid("boom"))
        }
    }
    struct Good(Arc<LunixSpace>);
    impl LadderStep for Good {
        fn run(&mut self, w: u64, _u: Urgency) -> lunix_iface::Result<Freed> {
            self.0.release(K, w);
            Ok(Freed(w))
        }
    }
    let (g, _fs) = gate(100, settings(0, OnFull::Wait, None), clock());
    g.register(LadderStepKind::EvictCache, Box::new(Bad));
    g.register(LadderStepKind::Compact, Box::new(Good(g.clone())));
    reserved(g.reserve(K, 100, None).unwrap());
    let w = parked(g.reserve(K, 5, None).unwrap());
    wait_until("the waiter", Duration::from_secs(5), || {
        w.state() == WaiterState::Granted
    });
    assert_eq!(g.step_errors(), 1);
    g.shutdown();
}

#[test]
fn error_mode_still_starts_the_ladder_for_the_next_write() {
    let (g, _fs) = gate(100, settings(0, OnFull::Error, None), clock());
    let ran = Arc::new(AtomicU64::new(0));
    struct Count(Arc<AtomicU64>, Arc<LunixSpace>);
    impl LadderStep for Count {
        fn run(&mut self, w: u64, _u: Urgency) -> lunix_iface::Result<Freed> {
            self.0.fetch_add(1, Ordering::SeqCst);
            self.1.release(K, w);
            Ok(Freed(w))
        }
    }
    g.register(
        LadderStepKind::Compact,
        Box::new(Count(ran.clone(), g.clone())),
    );
    reserved(g.reserve(K, 100, None).unwrap());
    assert!(matches!(
        g.reserve(K, 10, None).unwrap(),
        ReserveOutcome::NoSpace
    ));
    wait_until("the ladder", Duration::from_secs(5), || {
        ran.load(Ordering::SeqCst) == 1
    });
    wait_until("space to come back", Duration::from_secs(5), || {
        g.snapshot().used() == 90
    });
    reserved(g.reserve(K, 10, None).unwrap());
    g.shutdown();
}

#[test]
fn fatal_io_goes_read_only_and_wakes_the_parked() {
    let (g, _fs) = gate(100, settings(0, OnFull::Wait, None), clock());
    reserved(g.reserve(K, 100, None).unwrap());
    let w = parked(g.reserve(K, 1, None).unwrap());
    g.fatal_io("write");
    assert_eq!(w.state(), WaiterState::TimedOut);
    assert!(g.is_read_only());
    assert_eq!(
        g.reserve(K, 1, None).unwrap_err().kind(),
        ErrorKind::ReadOnly
    );
    // Releases and transfers still work: GC and the engine wind down.
    g.release(K, 100);
    assert_eq!(g.snapshot().used(), 0);
}

#[test]
fn an_eio_from_statvfs_goes_read_only() {
    let (g, fs) = gate_manual(100, 100, settings(0, OnFull::Error, None), clock());
    fs.set_failing(true);
    g.refresh();
    assert!(g.is_read_only());
}

#[test]
fn reconfigure_applies_live_and_rejects_invalid_settings() {
    let ck = clock();
    let (g, _fs) = gate(1000, settings(0, OnFull::Error, None), ck.clone());
    reserved(g.reserve(K, 900, None).unwrap());
    assert!(matches!(
        g.reserve(K, 200, None).unwrap(),
        ReserveOutcome::NoSpace
    ));
    // Raising the headroom shrinks the limit; a bigger floor too.
    let mut s = settings(0, OnFull::Wait, Some(Duration::from_millis(500)));
    s.headroom = 50;
    g.reconfigure(&s).unwrap();
    assert_eq!(g.snapshot().limit, 950);
    s.free_floor = FreeFloor::Percent("10%".into());
    g.reconfigure(&s).unwrap();
    assert_eq!(g.snapshot().limit, 850);
    let w = parked(g.reserve(K, 1, None).unwrap());
    // Relaxing the floor grants the parked request at once.
    s.free_floor = FreeFloor::Bytes(0);
    g.reconfigure(&s).unwrap();
    assert_eq!(w.state(), WaiterState::Granted);
    // Timeout applies to new waiters.
    let w2 = parked(g.reserve(K, 100, None).unwrap());
    ck.advance(Duration::from_millis(500));
    assert_eq!(g.tick(), 1);
    assert_eq!(w2.state(), WaiterState::TimedOut);

    let bad = SpaceSettings {
        full_timeout_ms: Some(0),
        ..settings(0, OnFull::Wait, None)
    };
    assert_eq!(g.reconfigure(&bad).unwrap_err().kind(), ErrorKind::Invalid);
    for p in ["101%", "x%", "%", "10", "-1%", "1000%"] {
        let bad = SpaceSettings {
            free_floor: FreeFloor::Percent(p.into()),
            ..settings(0, OnFull::Wait, None)
        };
        assert_eq!(
            g.reconfigure(&bad).unwrap_err().kind(),
            ErrorKind::Invalid,
            "{p}"
        );
    }
}

#[test]
fn a_transfer_larger_than_the_holding_is_refused_and_changes_nothing() {
    let (g, _fs) = gate(100, settings(0, OnFull::Error, None), clock());
    reserved(g.reserve(K, 10, None).unwrap());
    let before = g.snapshot().by_kind;
    let e = g.transfer(K, SpaceKind::Map, 11, 5).unwrap_err();
    assert_eq!(e.kind(), ErrorKind::Invalid);
    assert_eq!(g.snapshot().by_kind, before);
}

#[test]
fn statvfs_probe_reads_the_host_file_system() {
    let p = StatvfsProbe::new(std::env::temp_dir());
    let s = p.stat().expect("statvfs");
    assert!(s.total > 0);
    assert!(s.free <= s.total);
    assert!(StatvfsProbe::new("/definitely/not/here").stat().is_err());
    // A gate on the real file system starts with a sane limit.
    let cfg = SpaceConfig::new(SpaceSettings::default());
    let g = LunixSpace::new(cfg, Arc::new(p), clock()).unwrap();
    let snap = g.snapshot();
    assert!(snap.limit <= snap.fs_free);
    g.shutdown();
}

#[test]
fn shutdown_stops_the_thread_and_is_idempotent() {
    let (g, _fs) = gate(100, settings(0, OnFull::Error, None), clock());
    g.shutdown();
    g.shutdown();
    reserved(g.reserve(K, 10, None).unwrap());
}
