//! A guest read of a block whose newest bytes sit in a journal ring file races
//! the cut that covers the record and the reclaim of that file (H1 of the M15
//! zero-blocks audit), through the real stack.
//!
//! The volume holds full chunks in the map, then 4 KiB (and some 8 KiB)
//! writes into those chunks that only the journal has, flushed; the process
//! "crashes" (no final commit) and the volume is attached again, so the
//! records are replayed and nothing is resident in the journal's window: every
//! overlay read is a ring file read. The first such read runs a cut and its
//! reclaim between the journal's lookup and the queued read (the journal's
//! test hook). Every byte the guest reads afterwards must be the model's.
//! Plain, integrity and encrypted volumes.

#![cfg(feature = "testsupport")]

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

use lunix_format::CryptoMode;
use lunix_iface::journal::Journal;
use lunixd::testsupport::{close, payload, Driver, Scratch, TestRng, VolumeBuilder};

const CS: usize = 64 << 10;
const BLK: usize = 4096;
const CHUNKS: usize = 40;
const SIZE: u64 = 64 << 20;

fn run(mode: CryptoMode, tag: &str) {
    let t = Scratch::new(tag);
    let vol = VolumeBuilder::new(t.path())
        .mode(mode)
        .chunk_size(CS as u32)
        .size_bytes(SIZE)
        .queues(2)
        .small_journal(4)
        .manual()
        .create()
        .expect("create");
    let mut model = vec![0u8; CHUNKS * CS];

    // Attach 1: full chunks into the map, then partial overwrites that only
    // the journal holds.
    let stack = vol.open().expect("open");
    let mut d = Driver::new(&stack).expect("driver");
    for c in 0..CHUNKS {
        let data = payload(((c as u64) << 2) | 1, CS);
        assert!(d.write(0, (c * CS) as u64, &data, false).is_ok());
        model[c * CS..(c + 1) * CS].copy_from_slice(&data);
    }
    assert!(d.flush(0).is_ok());
    stack.engine.commit_now(true).expect("base commit");
    let mut rng = TestRng::new(0x006a_7265_6164);
    for i in 0..300u64 {
        let c = rng.below(CHUNKS as u64) as usize;
        // Never block 15: every chunk stays partly overwritten.
        let b = rng.below(14) as usize;
        let n = if i % 7 == 0 { 2 } else { 1 };
        let off = c * CS + b * BLK;
        let data = payload((i << 2) | 2, n * BLK);
        let st = d.write(0, off as u64, &data, false);
        assert!(st.is_ok(), "overlay write {i}: {st:?}");
        model[off..off + n * BLK].copy_from_slice(&data);
    }
    assert!(d.flush(0).is_ok());
    drop(d);
    drop(stack); // a crash: no final commit, the records stay in the ring

    // Attach 2: the records are replayed, nothing is resident.
    let stack = vol.open().expect("reopen");
    let fill = stack.journal.fill();
    assert!(fill.used_bytes > 0, "nothing was replayed: {fill:?}");
    let tip = stack.engine.tip();
    let fired = Arc::new(AtomicBool::new(false));
    let reads = Arc::new(AtomicU32::new(0));
    {
        let engine = Arc::clone(&stack.engine);
        let fired = Arc::clone(&fired);
        let reads = Arc::clone(&reads);
        stack.journal.set_read_gap_hook(Some(Arc::new(move |_loc| {
            reads.fetch_add(1, Ordering::SeqCst);
            if !fired.swap(true, Ordering::SeqCst) {
                let e = Arc::clone(&engine);
                std::thread::spawn(move || e.commit_now(false))
                    .join()
                    .expect("cut thread")
                    .expect("the cut commits");
            }
        })));
    }
    let freed0: u64 = stack.journal.freed_files_by_lane().iter().sum();
    let mut d = Driver::new(&stack).expect("driver");
    for blk in 0..CHUNKS * CS / BLK {
        let off = blk * BLK;
        let got = d.read(blk % 2, off as u64, BLK as u32).expect("read");
        assert_eq!(got, model[off..off + BLK], "block {blk} ({mode:?})");
    }
    assert!(fired.load(Ordering::SeqCst), "the race was never staged");
    assert_ne!(stack.engine.tip(), tip, "the cut committed");
    let freed1: u64 = stack.journal.freed_files_by_lane().iter().sum();
    assert!(freed1 > freed0, "the cut reclaimed ring files");
    stack.journal.set_read_gap_hook(None);
    // And once more in full, with the journal released.
    let all = d.read_all(model.len() as u64).expect("read_all");
    assert!(all == model, "second full read differs ({mode:?})");
    drop(d);
    close(stack).expect("close");
}

#[test]
fn plain() {
    run(CryptoMode::Plain, "jread-plain");
}

#[test]
fn integrity() {
    run(CryptoMode::Integrity, "jread-integrity");
}

#[test]
fn encrypted() {
    run(CryptoMode::Encrypted, "jread-enc");
}
