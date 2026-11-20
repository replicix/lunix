//! A write must not stall for good after a kill -9 during ingest and the
//! recovery (milestone jcut, a product bug M15 found).
//!
//! A process killed between C6 (the FULL is durable) and C9 (the journal
//! reclaim) leaves ring files that hold only records at or below the tip's
//! cut. Recovery replays nothing of them, nothing is dirty, so no commit
//! trigger fired and no commit reclaimed them: a lane full of them parked its
//! writes until they failed (`journal ring full`, EIO after 60 s). The
//! coordinator's tick now reclaims the covered files while a write is parked
//! (`Inner::reclaim_covered`, with the engine test
//! `a_lane_left_full_of_covered_records_by_a_crash_is_freed_for_its_parked_write`).
//!
//! This is the real-stack equivalent of M15's `tamper.sh` without root: a
//! child process ingests 256 KiB writes over four queues on a small ring (two
//! files per lane, the background coordinator committing all the time) and
//! kills itself at a crash point between C6 and C9 (`LUNIX_CRASH_AT`); the parent reopens the volume (the recovery)
//! and writes more; every write must complete within seconds. The single
//! writer blocks on a full lane, so the cut that follows covers everything and
//! the kill leaves no record above the tip. Needs the `testsupport` and
//! `crashpoints` features.

#![cfg(all(feature = "testsupport", feature = "crashpoints"))]

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use lunix_format::CryptoMode;
use lunixd::testsupport::{close, payload, Driver, Scratch, TestVolume, VolumeBuilder};

const CHILD_ENV: &str = "LUNIX_RING_CHILD";
const QUEUES: u16 = 4;
const SIZE: u64 = 256 << 20;
const IO: usize = 256 << 10;

fn builder(parent: &Path) -> VolumeBuilder {
    VolumeBuilder::new(parent)
        .mode(CryptoMode::Plain)
        .size_bytes(SIZE)
        .queues(QUEUES)
        .small_journal(2)
}

/// The ingesting child: fills every lane's ring exactly (4 records of 256 KiB
/// per file, two files per lane), makes them durable, and cuts them all; the
/// kill comes from `LUNIX_CRASH_AT` inside that commit, after its FULL.
#[test]
fn ring_child() {
    let Some(dir) = std::env::var_os(CHILD_ENV) else {
        return;
    };
    let dir = std::path::PathBuf::from(dir);
    let parent = dir.parent().expect("parent").to_path_buf();
    let vol = TestVolume::at(&dir, builder(&parent), None).expect("volume");
    let stack = vol
        .open_with(|p| {
            p.tweak = Some(std::sync::Arc::new(|c: &mut lunix_engine::EngineConfig| {
                c.background = false;
                c.read_helpers = 2;
            }));
        })
        .expect("child open");
    let mut d = Driver::new(&stack).expect("driver");
    for i in 0..u64::from(QUEUES) * 8 {
        let off = (i * IO as u64) % SIZE;
        let st = d.write(
            (i % u64::from(QUEUES)) as usize,
            off,
            &payload(i, IO),
            false,
        );
        assert!(st.is_ok(), "child write {i}: {st:?}");
    }
    assert!(d.flush(0).is_ok());
    let _ = stack.engine.commit_now(true);
    eprintln!("the child was not killed");
    std::process::exit(3);
}

#[test]
fn a_write_after_a_kill_during_ingest_and_the_recovery_does_not_stall() {
    // After C6, before the reclaim of C9.
    const POINTS: [&str; 4] = [
        "engine.c6.committed",
        "engine.c7.checkpoint",
        "engine.c7.floor",
        "engine.c8.index",
    ];
    let iters: u64 = POINTS.len() as u64;
    let t = Scratch::new("ring-recover");
    let mut worst = Duration::ZERO;
    for it in 0..iters {
        // A fresh volume: the child fills a ring that is empty.
        let vol = builder(&t.path().join(format!("v{it}")))
            .create()
            .expect("create");
        let at = format!("{}:1", POINTS[it as usize]);
        let log = t.path().join(format!("child-{it}.log"));
        let out = std::fs::File::create(&log).expect("child log");
        let err = out.try_clone().expect("child log");
        let mut child = Command::new(std::env::current_exe().expect("exe"))
            .args(["--exact", "ring_child", "--nocapture", "--test-threads=1"])
            .env(CHILD_ENV, &vol.voldir)
            .env("LUNIX_CRASH_AT", &at)
            .stdout(out)
            .stderr(err)
            .spawn()
            .expect("spawn the child");
        let start = Instant::now();
        let status = loop {
            if let Some(st) = child.try_wait().expect("wait") {
                break st;
            }
            if start.elapsed() > Duration::from_secs(120) {
                let _ = child.kill();
                panic!("iteration {it}: the child never reached {at}");
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        assert_eq!(
            std::os::unix::process::ExitStatusExt::signal(&status),
            Some(9),
            "iteration {it}: the child ends at {at}: {status}: {}",
            std::fs::read_to_string(&log).unwrap_or_default()
        );

        let stack = vol.open().expect("recovery");
        // The precondition of the stall: every lane is full of records the
        // tip covers (two files per lane, all holding records), so the first
        // write has no room. Without it the test would pass without the fix.
        let fill = lunix_iface::journal::Journal::fill(&*stack.journal);
        assert_eq!(
            fill.used_bytes, fill.capacity_bytes,
            "iteration {it}: the ring is not full after the recovery: {fill:?}"
        );
        let mut d = Driver::new(&stack).expect("driver");
        let t0 = Instant::now();
        for i in 0..96u64 {
            let off = (i * IO as u64) % SIZE;
            let st = d.write(
                (i % u64::from(QUEUES)) as usize,
                off,
                &payload(i + 7, IO),
                false,
            );
            assert!(
                st.is_ok(),
                "iteration {it}: write {i} failed after {:?}: {st:?}",
                t0.elapsed()
            );
        }
        let took = t0.elapsed();
        worst = worst.max(took);
        assert!(
            took < Duration::from_secs(30),
            "iteration {it}: 96 writes took {took:?} after the recovery"
        );
        drop(d);
        close(stack).expect("close");
    }
    eprintln!("ring recover: {iters} kills, the slowest 96 writes took {worst:?}");
}
