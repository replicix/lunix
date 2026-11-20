//! Flush grouping, journal-full parking, space admission, the read path
//! under a slow store, and the queue-thread rule.

mod common;

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use common::*;
use lunix_iface::block::{BlockEngine, IoOp};
use lunix_iface::space::SpaceGate;
use lunix_testkit::{JournalMode, MemJournalConfig, MemSpaceConfig};

const CS: u32 = 16 << 10;

#[test]
fn a_thousand_flushes_form_few_groups_and_all_complete() {
    let v = Vol::create(CS, 16, 2, JournalMode::Mixed(11));
    let mut r = v.open().map_err(|e| e.0).expect("open");
    for i in 0..8u64 {
        assert!(r
            .write(i as usize, i * 4096, &pattern(i, 4096), false)
            .is_ok());
    }
    let mut tags = Vec::new();
    for i in 0..1000usize {
        // Writes interleaved with the flushes: a flush must cover the
        // writes acknowledged before it.
        if i % 100 == 0 {
            assert!(r.write(i, 65536, &pattern(i as u64, 4096), false).is_ok());
        }
        tags.push(r.submit(i, IoOp::Flush, false, 0, 0, &[]));
    }
    for (st, _) in r.wait(&tags) {
        assert!(st.is_ok());
    }
    let s = r.engine.stats();
    assert_eq!(s.flush_group_waiters, 1000);
    assert!(s.flush_groups >= 1 && s.flush_group_max >= 1);
    assert_eq!(
        v.checks.flush_submitted.load(Ordering::SeqCst),
        s.flush_groups
    );
    assert!(
        v.checks.flush_outstanding_max.load(Ordering::SeqCst) <= 1,
        "at most one group in flight"
    );
    assert_eq!(
        r.journal.unflushed_records(),
        0,
        "every acknowledged write is durable"
    );
}

#[test]
fn a_full_ring_parks_writes_without_consuming_numbers_and_a_cut_resumes_them() {
    let mut v = Vol::create(CS, 64, 2, JournalMode::Mixed(5));
    v.set_journal(MemJournalConfig {
        lanes: 2,
        file_bytes: lunix_testkit::journal::MIN_FILE_BYTES,
        max_files: 4,
        pool_depth: 64,
        mode: JournalMode::Mixed(5),
    });
    let mut r = v.open().map_err(|e| e.0).expect("open");
    let size = r.engine.size_bytes();
    let mut written = Vec::new();
    let mut tags = Vec::new();
    for i in 0..40u64 {
        let off = (i * 65536) % size;
        let d = pattern(i, 65536);
        tags.push(r.submit(i as usize, IoOp::Write, false, off, 65536, &d));
        written.push((off, d));
    }
    // The ring holds about 4 MiB: some writes park until a cut reclaims.
    let parked = tags.iter().filter(|t| !r.is_done(**t)).count();
    assert!(parked > 0, "nothing parked");
    for (st, _) in r.wait(&tags) {
        assert!(st.is_ok(), "{st:?}");
    }
    // Numbers stay contiguous: every sequenced record is accounted for.
    let seqs = v.hooks.take_seqs();
    for w in seqs.windows(2) {
        assert_eq!(
            w[1].2 .0,
            w[0].2 .0 + 1,
            "a refused reservation consumed a number"
        );
    }
    let mut want = vec![0u8; size as usize];
    for (off, d) in &written {
        want[*off as usize..*off as usize + d.len()].copy_from_slice(d);
    }
    assert_eq!(r.read_all(), want);
}

#[test]
fn on_full_error_fails_a_write_with_enospc() {
    let mut v = Vol::create(CS, 16, 1, JournalMode::Immediate);
    v.space_cfg = Some(MemSpaceConfig::new(64 << 10));
    let mut r = v.open().map_err(|e| e.0).expect("open");
    assert!(r.write(0, 0, &pattern(1, 4096), false).is_ok());
    let st = r.write(0, 0, &pattern(2, 65536), false);
    assert_eq!(st.errno(), Some(28));
    // Reads still work and the volume is not read-only.
    assert_eq!(r.read(0, 0, 4096).1, pattern(1, 4096));
}

#[test]
fn on_full_wait_times_out_with_enospc() {
    let mut v = Vol::create(CS, 16, 1, JournalMode::Immediate);
    v.space_cfg = Some(MemSpaceConfig::new(64 << 10).wait(Some(Duration::from_secs(5))));
    let mut r = v.open().map_err(|e| e.0).expect("open");
    let t = r.submit(0, IoOp::Write, false, 0, 65536, &pattern(2, 65536));
    for _ in 0..50 {
        r.pump();
    }
    assert!(!r.is_done(t), "the write waits for space");
    v.clock.advance(Duration::from_secs(6));
    assert_eq!(r.space.tick(), 1);
    assert_eq!(r.wait(&[t])[0].0.errno(), Some(28));
}

#[test]
fn on_full_wait_is_granted_after_a_cut_reclaims_the_journal() {
    let mut v = Vol::create(64 << 10, 128, 1, JournalMode::Immediate);
    v.space_cfg = Some(MemSpaceConfig::new(5 << 20).wait(None));
    let mut r = v.open().map_err(|e| e.0).expect("open");
    let d = pattern(77, 1 << 20);
    for i in 0..4u64 {
        assert!(r.write(0, i << 20, &d, false).is_ok(), "write {i}");
    }
    let t = r.submit(0, IoOp::Write, false, 4 << 20, 1 << 20, &d);
    for _ in 0..50 {
        r.pump();
    }
    if r.is_done(t) {
        let st = r.wait(&[t]);
        panic!(
            "the fifth MiB did not wait: {:?} {:?}",
            st[0].0,
            r.space.snapshot()
        );
    }
    // The cut dedups the four identical MiB into one and reclaims the ring.
    r.engine.commit_now(false).expect("commit");
    assert!(r.wait(&[t])[0].0.is_ok());
    assert_eq!(r.read(0, 4 << 20, 4096).1, d[..4096].to_vec());
}

#[test]
fn cache_hits_are_unaffected_by_a_slow_store_and_no_tag_is_lost() {
    let v = Vol::create(CS, 16, 1, JournalMode::Mixed(9));
    let mut r = v.open().map_err(|e| e.0).expect("open");
    for c in 0..4u64 {
        assert!(r
            .write(0, c * u64::from(CS), &pattern(c, CS as usize), false)
            .is_ok());
    }
    r.engine.commit_now(false).expect("commit");
    // Warm chunk 0.
    assert_eq!(r.read(0, 0, CS).1, pattern(0, CS as usize));
    v.read_faults.delay_us.store(10_000, Ordering::SeqCst);
    let misses: Vec<_> = (1..4u64)
        .map(|c| r.submit(0, IoOp::Read, false, c * u64::from(CS), CS, &[]))
        .collect();
    let start = Instant::now();
    for _ in 0..200 {
        let t = r.submit(0, IoOp::Read, false, 4096, 8192, &[]);
        assert!(r.is_done(t), "a cache hit completes inline");
        let got = r.wait(&[t]).remove(0);
        assert_eq!(got.1, pattern(0, CS as usize)[4096..12288].to_vec());
    }
    assert!(
        start.elapsed() < Duration::from_millis(500),
        "hits took {:?}",
        start.elapsed()
    );
    for (i, (st, data)) in r.wait(&misses).into_iter().enumerate() {
        assert!(st.is_ok());
        assert_eq!(data, pattern(i as u64 + 1, CS as usize));
    }
}

#[test]
#[should_panic(expected = "blocking call")]
fn the_queue_thread_check_fires_on_a_blocking_call() {
    use lunix_engine::EngineHooks;
    let v = Vol::create(CS, 16, 1, JournalMode::Immediate);
    let r = v.open().map_err(|e| e.0).expect("open");
    let g = GStore {
        inner: r.store.clone(),
        checks: std::sync::Arc::clone(&v.checks),
        faults: std::sync::Arc::clone(&v.read_faults),
    };
    v.hooks.queue_thread(true);
    let _ = lunix_iface::store::SegmentStore::list(&g);
}

fn background(cfg: &mut lunix_engine::EngineConfig) {
    cfg.background = true;
    cfg.background_period = Duration::from_millis(1);
    cfg.dirty_bytes = 64 << 10;
}

#[test]
fn the_background_coordinator_commits_under_load_without_losing_a_write() {
    let mut v = Vol::create(CS, 64, 2, JournalMode::Mixed(21));
    v.tweak = background;
    let mut r = v.open().map_err(|e| e.0).expect("open");
    let size = r.engine.size_bytes();
    let mut want = vec![0u8; size as usize];
    let start = Instant::now();
    let mut i = 0u64;
    while start.elapsed() < Duration::from_secs(3) || r.engine.tip().0 .0 < 3 {
        let off = (i * 12288) % (size - 16384);
        let d = pattern(i, 12288);
        // The writes that were acknowledged before a later one of the
        // iteration was submitted: their numbers must be below its number.
        let mut tags: Vec<(u64, lunix_iface::block::Tag)> = Vec::new();
        let mut acked_before: Vec<(usize, usize)> = Vec::new();
        for k in 0..4u64 {
            let o = (off + k * 4096) % (size - 12288);
            for (e, (_, et)) in tags.iter().enumerate() {
                if r.is_done(*et) {
                    acked_before.push((e, k as usize));
                }
            }
            tags.push((o, r.submit(k as usize, IoOp::Write, k == 3, o, 12288, &d)));
        }
        for (_, t) in &tags {
            assert!(r.wait(&[*t])[0].0.is_ok());
        }
        // Four overlapping writes in flight together have no order but their
        // `write_seq`s: a write that parked for the ring (on its own lane)
        // is journaled after one that did not, whatever order they arrived
        // in. The model applies them in the order the engine numbered them,
        // and a write acknowledged before another was submitted must have the
        // lower number. The log of numbers is emptied every iteration.
        let seq_of: Vec<u64> = {
            let mut seqs = lock(&v.hooks.seqs);
            let out = tags
                .iter()
                .map(|(_, t)| {
                    seqs.iter()
                        .rev()
                        .find(|(_, tag, _)| tag == t)
                        .map(|(_, _, s)| s.0)
                        .expect("every write was numbered")
                })
                .collect();
            seqs.clear();
            out
        };
        for (e, k) in acked_before {
            assert!(
                seq_of[e] < seq_of[k],
                "write {e} was acknowledged before write {k} was submitted: {seq_of:?}"
            );
        }
        let mut order: Vec<(u64, u64)> = tags
            .iter()
            .zip(&seq_of)
            .map(|((o, _), s)| (*s, *o))
            .collect();
        order.sort_unstable();
        for (_, o) in order {
            want[o as usize..o as usize + d.len()].copy_from_slice(&d);
        }
        let ro = (i * 7 * 4096) % (size - 8192);
        let (st, got) = r.read(1, ro, 8192);
        assert!(st.is_ok());
        assert_eq!(
            got,
            want[ro as usize..ro as usize + 8192].to_vec(),
            "read at {ro}"
        );
        i += 1;
        assert!(
            start.elapsed() < Duration::from_secs(60),
            "no background commit"
        );
    }
    assert!(r.engine.tip().0 .0 >= 3, "background commits happened");
    assert_eq!(r.read_all(), want);
    let p = r.stop();
    v.absorb(p, 5);
    v.tweak = default_tweak;
    let mut r = v.open().map_err(|e| e.0).expect("reopen");
    assert_eq!(r.read_all().len() as u64, size);
    assert!(v.violations().is_empty(), "{:?}", v.violations());
}
