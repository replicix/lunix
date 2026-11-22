//! What the collector's condemn and delete phases cost per object on the
//! stack (milestone gcbatch), on the file system under `TMPDIR`/the scratch
//! directory: a backlog of dead segments is built with the collector off,
//! then the volume is opened with the daemon's collector and the `gc pass
//! done` lines (per-phase time and units, counts) are read from stderr.
//!
//! By hand (a measurement, not a gate):
//!
//! ```text
//! LUNIX_GC_COST_SEGMENTS=3000 cargo test --release -p lunixd --features testsupport \
//!     --test stack_gc_cost -- --ignored --nocapture 2> gc-cost.log
//! grep 'gc pass done' gc-cost.log
//! ```
//!
//! Every cut of one 64 KiB chunk seals one segment; the chunk is overwritten
//! eight cuts later, so all but the last eight segments are dead once the
//! retained window has passed. `LUNIX_GC_COST_SEGMENTS` is the number of cuts
//! (default 3000), `LUNIX_GC_COST_SECS` the longest wait for the backlog to
//! drain (default 1800).

#![cfg(feature = "testsupport")]

use std::sync::Arc;
use std::time::{Duration, Instant};

use lunix_engine::EngineConfig;
use lunix_format::CryptoMode;
use lunixd::testsupport::{close, fsck, payload, Driver, Scratch, VolumeBuilder};

fn env_or(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn count(dir: &std::path::Path) -> usize {
    std::fs::read_dir(dir).map_or(0, |d| d.flatten().count())
}

#[test]
#[ignore = "a measurement; minutes (see the module doc)"]
fn a_backlog_of_dead_segments_is_condemned_and_deleted() {
    lunixd::log::StderrLog::install(tracing::Level::INFO);
    let n = env_or("LUNIX_GC_COST_SEGMENTS", 3000);
    let wait = Duration::from_secs(env_or("LUNIX_GC_COST_SECS", 1800));
    let cs: u32 = 64 << 10;
    let cs64 = u64::from(cs);
    let t = Scratch::new("gccost");
    let grace = Arc::new(|c: &mut EngineConfig| {
        if let Some(p) = &mut c.gc_params {
            p.read_timeout = Duration::ZERO;
            p.grace = Duration::from_secs(10);
        }
    });
    let mut b = VolumeBuilder::new(t.path())
        .mode(CryptoMode::Integrity)
        .chunk_size(cs)
        .size_bytes(1 << 30);
    b.engine = Some(grace.clone());
    let vol = b.create().expect("create");

    // The backlog, collector off.
    let started = Instant::now();
    {
        let off = grace.clone();
        let stack = vol
            .open_with(|o| {
                o.tweak = Some(Arc::new(move |c: &mut EngineConfig| {
                    off(c);
                    c.gc_budget = None;
                }));
            })
            .expect("open without the collector");
        let mut d = Driver::new(&stack).expect("driver");
        for i in 0..n {
            let seed = i | 3;
            assert!(d
                .write(0, (i % 8) * cs64, &payload(seed, cs as usize), false)
                .is_ok());
            stack.engine.commit_now(true).expect("cut");
        }
        for _ in 0..4 {
            stack.engine.commit_now(true).expect("cut");
        }
        assert!(d.flush(0).is_ok());
        drop(d);
        close(stack).expect("close");
    }
    let segments = count(&vol.voldir.join("segments"));
    eprintln!(
        "gccost: backlog of {segments} segments built in {:.1?}",
        started.elapsed()
    );

    // The collector on: the daemon's settings (pipelined, 256 MiB per pass).
    let stack = vol
        .open_with(|o| o.gc = Some(lunix_gc::GcConfig::daemon()))
        .expect("open with the collector");
    let status = stack.engine.gc_status_handle();
    let t0 = Instant::now();
    let mut drained = None;
    while t0.elapsed() < wait {
        std::thread::sleep(Duration::from_millis(200));
        let s = status.get();
        if drained.is_none() && s.stats.passes >= 2 && s.condemned_pending == 0 {
            drained = Some(t0.elapsed());
            break;
        }
    }
    let left = count(&vol.voldir.join("segments"));
    eprintln!(
        "gccost: drained {:?} after {:.1?}: {segments} segments before, {left} after, {} passes",
        drained.is_some(),
        t0.elapsed(),
        status.get().stats.passes
    );
    close(stack).expect("close");
    if let Some(f) = fsck(&vol) {
        assert!(f.no_errors(), "lunix-fsck:\n{}", f.text);
    }
    assert!(drained.is_some(), "the backlog did not drain in time");
}
