//! C7, the revalidating read: a read part whose segment is gone (or whose
//! hint authenticates as another extent) looks the block up again and reads
//! the new entry; only an unchanged entry makes the error real, and only
//! then do `Auth` events count.
//!
//! The parked reads (`ReadFaults::hold`) make the interleavings exact: the
//! helper is stopped at the store while the test overwrites, cuts and
//! collects, and then released.

mod common;

use std::collections::BTreeSet;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::Duration;

use common::*;
use lunix_format::commit::{Commit, MapRoot};
use lunix_format::node::DataEntry;
use lunix_format::{ExtentKind, Hint, IdLen};
use lunix_iface::block::{BlockEngine, IoOp, IoStatus, Tag};
use lunix_iface::crypto::{CommitCodec, ExtentCodec};
use lunix_iface::index::ChunkIndex;
use lunix_iface::map::{entry_hint, BlockMap, ChunkNo};
use lunix_iface::store::{CommitStore, PackRead, SegmentStore};
use lunix_testkit::{FlatMap, JournalMode, PlainCodec};
use proptest::prelude::*;

const CS: u32 = 16 << 10;
const CHUNKS: u64 = 3;
/// `REVALIDATE_MAX` of the reader (crate-private).
const RETRY_BOUND: u32 = 16;
const WAIT: Duration = Duration::from_secs(5);

fn tip_map(r: &Running, codec: PlainCodec, id_len: IdLen) -> FlatMap {
    let tip = CommitStore::tip(&r.store).unwrap().unwrap();
    let bytes = CommitStore::get(&r.store, tip).unwrap();
    let (root, size): (MapRoot, u64) = match codec.open_commit(tip, &bytes).unwrap() {
        Commit::Full(f) => (f.map_root, f.size),
        _ => panic!("not a FULL"),
    };
    let ext: Arc<dyn ExtentCodec> = Arc::new(codec);
    let packs: Arc<dyn PackRead> = Arc::new(r.store.clone());
    FlatMap::open(ext, packs, &root, size / u64::from(CS), id_len).unwrap()
}

/// The committed map entry of chunk `ci`.
fn entry(r: &Running, codec: PlainCodec, id_len: IdLen, ci: u64) -> DataEntry {
    tip_map(r, codec, id_len)
        .get(ChunkNo(ci))
        .unwrap()
        .expect("a stored chunk")
}

fn setup(codec: PlainCodec, id_len: IdLen) -> (Vol, Running) {
    let v = Vol::create_with(CS, 16, 2, JournalMode::Mixed(3), codec, id_len);
    let mut r = v.open().map_err(|e| e.0).expect("open");
    for c in 0..CHUNKS {
        assert!(r
            .write(0, c * u64::from(CS), &pattern(100 + c, CS as usize), false)
            .is_ok());
    }
    r.engine.commit_now(false).expect("commit");
    (v, r)
}

fn auth_failures(r: &Running) -> u64 {
    r.engine.stats().extent_auth_failures
}

fn revalidated(r: &Running) -> u64 {
    r.engine.stats().read_revalidated
}

fn modes() -> Vec<(PlainCodec, IdLen)> {
    let mut v = Vec::new();
    for codec in [PlainCodec::new(), PlainCodec::unkeyed()] {
        for id_len in [IdLen::Sixteen, IdLen::ThirtyTwo, IdLen::Zero] {
            v.push((codec, id_len));
        }
    }
    v
}

/// A parked read: the next read of a hint stops at the store.
struct Hold {
    entered: Receiver<()>,
    release: Sender<()>,
}

/// Parks the next store read of `hint`.
fn arm(v: &Vol, hint: Hint) -> Hold {
    let (entered_tx, entered) = channel();
    let (release, release_rx) = channel();
    *lock(&v.read_faults.hold) = Some(ReadHold {
        hint,
        entered: entered_tx,
        release: release_rx,
    });
    Hold { entered, release }
}

impl Hold {
    fn reached(&self) {
        self.entered
            .recv_timeout(WAIT)
            .expect("the helper reached the store");
    }
}

/// Submits a read of `len` bytes at `off` that parks at `hint`, and waits
/// until its helper is there.
fn park(v: &Vol, r: &mut Running, hint: Hint, off: u64, len: u32) -> (Tag, Hold) {
    let h = arm(v, hint);
    let t = r.submit(0, IoOp::Read, false, off, len, &[]);
    h.reached();
    (t, h)
}

fn finish(r: &mut Running, t: Tag, h: Hold) -> (IoStatus, Vec<u8>) {
    h.release.send(()).unwrap();
    r.wait(&[t]).remove(0)
}

fn overwrite(r: &mut Running, ci: u64, seed: u64) -> Vec<u8> {
    let d = pattern(seed, CS as usize);
    assert!(r.write(1, ci * u64::from(CS), &d, false).is_ok());
    r.engine.commit_now(false).expect("commit");
    d
}

/// Parks a read of the first block of chunk 1, overwrites the chunk and
/// commits (`between` then runs with the old and the new hint), and lets the
/// read go. Returns its result and the new chunk. One block: a read of
/// several blocks is several parts, each its own lookup.
fn read_across_overwrite(
    v: &Vol,
    r: &mut Running,
    codec: PlainCodec,
    id_len: IdLen,
    between: impl FnOnce(&Running, Hint, Hint),
) -> (IoStatus, Vec<u8>, Vec<u8>) {
    let old = entry_hint(&entry(r, codec, id_len, 1));
    let (t, h) = park(v, r, old, u64::from(CS), 4096);
    let fresh = overwrite(r, 1, 999);
    let new = entry_hint(&entry(r, codec, id_len, 1));
    assert_ne!(old, new);
    between(r, old, new);
    let (st, data) = finish(r, t, h);
    (st, data, fresh)
}

#[test]
fn a_segment_deleted_between_lookup_and_read_is_read_through_the_new_entry() {
    for (codec, id_len) in modes() {
        let (v, mut r) = setup(codec, id_len);
        let before = auth_failures(&r);
        let (st, data, fresh) = read_across_overwrite(&v, &mut r, codec, id_len, |r, old, _| {
            SegmentStore::delete(&r.store, old.seg).unwrap();
        });
        assert!(st.is_ok(), "{id_len:?}: {st:?}");
        assert_eq!(data, fresh[..4096], "{id_len:?}");
        assert_eq!(auth_failures(&r), before);
        assert_eq!(revalidated(&r), 1, "{id_len:?}");
    }
}

#[test]
fn auth_on_a_stale_hint_is_not_an_integrity_failure() {
    for (codec, id_len) in modes() {
        let (v, mut r) = setup(codec, id_len);
        let before = auth_failures(&r);
        // The old extent authenticates as damaged (it is being collected
        // under the reader); the block already names another extent.
        let (st, data, fresh) = read_across_overwrite(&v, &mut r, codec, id_len, |_, old, _| {
            lock(&v.read_faults.flip).insert(old);
        });
        assert!(st.is_ok(), "{id_len:?}: {st:?}");
        assert_eq!(data, fresh[..4096]);
        assert_eq!(auth_failures(&r), before, "{id_len:?}: stale Auth counted");
    }
}

/// Copies chunk 1's extent into a new segment and points the index at the
/// copy (what compaction does before the map is re-hinted).
fn move_extent(r: &Running, codec: PlainCodec, hint: Hint) {
    let mut bytes = Vec::new();
    SegmentStore::read_extent(&r.store, &hint, &mut bytes).unwrap();
    let id = codec.chunk_id(ExtentKind::Data, &pattern(101, CS as usize));
    let mut w = r.store.open_segment().unwrap();
    let new = w.append_extent(&id, None, &bytes).unwrap();
    w.seal(None).unwrap();
    assert!(r.index.relocate(&id, &hint, &new).unwrap());
}

#[test]
fn auth_on_a_stale_hint_bridged_by_the_index_is_not_counted() {
    for (codec, id_len) in modes() {
        if id_len == IdLen::Zero {
            continue; // no index fallback on a lean volume
        }
        let (v, mut r) = setup(codec, id_len);
        let old = entry_hint(&entry(&r, codec, id_len, 1));
        let before = auth_failures(&r);
        let (t, h) = park(&v, &mut r, old, u64::from(CS), 4096);
        // Compaction moved the extent (a second copy of the content); the
        // reader's hint then authenticates as damaged; the block is
        // overwritten, so the map no longer names the old entry.
        move_extent(&r, codec, old);
        lock(&v.read_faults.flip).insert(old);
        overwrite(&mut r, 1, 999);
        let (st, data) = finish(&mut r, t, h);
        assert!(st.is_ok(), "{id_len:?}: {st:?}");
        // The fallback served the old copy, which the part was entitled to.
        assert_eq!(data, pattern(101, CS as usize)[..4096], "{id_len:?}");
        assert_eq!(auth_failures(&r), before, "{id_len:?}: stale Auth counted");
    }
}

#[test]
fn auth_bridged_by_the_index_on_the_current_entry_is_counted() {
    for (codec, id_len) in modes() {
        if id_len == IdLen::Zero {
            continue;
        }
        let (v, mut r) = setup(codec, id_len);
        let old = entry_hint(&entry(&r, codec, id_len, 1));
        move_extent(&r, codec, old);
        lock(&v.read_faults.flip).insert(old);
        let before = auth_failures(&r);
        // The map still names the damaged extent: as before C7, an event.
        let (st, data) = r.read(0, u64::from(CS), 4096);
        assert!(st.is_ok(), "{id_len:?}: {st:?}");
        assert_eq!(data, pattern(101, CS as usize)[..4096]);
        assert_eq!(auth_failures(&r), before + 1, "{id_len:?}");
    }
}

#[test]
fn a_real_tamper_is_still_reported_and_counted() {
    for (codec, id_len) in modes() {
        let (v, mut r) = setup(codec, id_len);
        let a = entry_hint(&entry(&r, codec, id_len, 2));
        lock(&v.read_faults.flip).insert(a);
        let before = auth_failures(&r);
        let (st, data) = r.read(0, 2 * u64::from(CS), 4096);
        assert_eq!(st.errno(), Some(5), "{id_len:?}: EIO, never data");
        assert!(data.is_empty());
        assert_eq!(auth_failures(&r), before + 1, "{id_len:?}");
        assert_eq!(revalidated(&r), 0);
    }
}

#[test]
fn a_real_tamper_found_by_a_read_that_had_a_chance_to_revalidate_is_counted() {
    for (codec, id_len) in modes() {
        let (v, mut r) = setup(codec, id_len);
        let a = entry_hint(&entry(&r, codec, id_len, 2));
        let before = auth_failures(&r);
        let (t, h) = park(&v, &mut r, a, 2 * u64::from(CS), 4096);
        // Damaged under the parked reader; the entry does not change.
        lock(&v.read_faults.flip).insert(a);
        let (st, data) = finish(&mut r, t, h);
        assert_eq!(st.errno(), Some(5), "{id_len:?}");
        assert!(data.is_empty());
        assert_eq!(auth_failures(&r), before + 1, "{id_len:?}");
        assert_eq!(revalidated(&r), 0);
    }
}

#[test]
fn a_tampered_replacement_is_counted_once_the_entry_stops_changing() {
    for (codec, id_len) in modes() {
        let (v, mut r) = setup(codec, id_len);
        let before = auth_failures(&r);
        let (st, data, _) = read_across_overwrite(&v, &mut r, codec, id_len, |_, old, new| {
            let mut f = lock(&v.read_faults.flip);
            f.insert(old);
            f.insert(new);
        });
        assert_eq!(st.errno(), Some(5), "{id_len:?}");
        assert!(data.is_empty());
        // The stale attempt's event is void; the replacement's is real.
        assert_eq!(auth_failures(&r), before + 1, "{id_len:?}");
    }
}

#[test]
fn a_part_whose_entry_keeps_changing_gives_up_after_the_bound() {
    let (codec, id_len) = (PlainCodec::new(), IdLen::Sixteen);
    let (v, mut r) = setup(codec, id_len);
    let mut cur = entry_hint(&entry(&r, codec, id_len, 1));
    let (t, mut held) = park(&v, &mut r, cur, u64::from(CS), 4096);
    // Every attempt reads a copy that is overwritten, and damaged, before
    // the helper gets to it: RETRY_BOUND retries, then the bound.
    for i in 0..=RETRY_BOUND {
        overwrite(&mut r, 1, 3000 + u64::from(i));
        let next = entry_hint(&entry(&r, codec, id_len, 1));
        assert_ne!(cur, next);
        lock(&v.read_faults.flip).insert(cur);
        let next_hold = (i < RETRY_BOUND).then(|| arm(&v, next));
        held.release.send(()).unwrap();
        if let Some(n) = next_hold {
            n.reached();
            held = n;
        }
        cur = next;
    }
    let (st, data) = r.wait(&[t]).remove(0);
    assert_eq!(st.errno(), Some(5));
    assert!(data.is_empty());
    let s = r.engine.stats();
    assert_eq!(s.read_revalidated, u64::from(RETRY_BOUND));
    assert_eq!(s.read_revalidate_exhausted, 1);
    assert_eq!(s.extent_auth_failures, 0, "every failed entry was stale");
}

#[test]
fn a_block_discarded_under_the_read_reads_as_zeros() {
    for (codec, id_len) in modes() {
        let (v, mut r) = setup(codec, id_len);
        let old = entry_hint(&entry(&r, codec, id_len, 1));
        let (t, h) = park(&v, &mut r, old, u64::from(CS), 4096);
        assert!(r.hole(1, IoOp::Discard, u64::from(CS), CS).is_ok());
        r.engine.commit_now(false).expect("commit");
        assert!(tip_map(&r, codec, id_len)
            .get(ChunkNo(1))
            .unwrap()
            .is_none());
        SegmentStore::delete(&r.store, old.seg).unwrap();
        let (st, data) = finish(&mut r, t, h);
        assert!(st.is_ok(), "{id_len:?}: {st:?}");
        assert_eq!(data, vec![0u8; 4096], "{id_len:?}");
        assert_eq!(revalidated(&r), 1);
    }
}

#[test]
fn a_multi_part_read_retries_each_part_on_its_own() {
    for (codec, id_len) in modes() {
        let (v, mut r) = setup(codec, id_len);
        let old = entry_hint(&entry(&r, codec, id_len, 1));
        let (t, h) = park(&v, &mut r, old, u64::from(CS), CS);
        let fresh = overwrite(&mut r, 1, 999);
        SegmentStore::delete(&r.store, old.seg).unwrap();
        let (st, data) = finish(&mut r, t, h);
        assert!(st.is_ok(), "{id_len:?}: {st:?}");
        // Four parts: each is the old or the new block (they need not be one
        // snapshot), and at least the parked one went through the new entry.
        let was = pattern(101, CS as usize);
        for b in 0..4 {
            let blk = &data[b * 4096..][..4096];
            assert!(
                blk == &was[b * 4096..][..4096] || blk == &fresh[b * 4096..][..4096],
                "{id_len:?}: block {b} is neither version"
            );
        }
        assert!(revalidated(&r) >= 1, "{id_len:?}");
    }
}

#[test]
fn a_partial_chunk_overwrite_under_the_read_leaves_the_unwritten_blocks() {
    for (codec, id_len) in modes() {
        let (v, mut r) = setup(codec, id_len);
        let old = entry_hint(&entry(&r, codec, id_len, 1));
        let (t, h) = park(&v, &mut r, old, u64::from(CS), 4096);
        // One block of chunk 1: the cut reads the old extent as the base
        // (a cold read-modify-write) and builds a new one.
        let patch = pattern(777, 4096);
        assert!(r.write(1, u64::from(CS) + 8192, &patch, false).is_ok());
        r.engine.commit_now(false).expect("commit");
        assert!(r.engine.stats().cold_rmw >= 1, "{id_len:?}");
        assert_ne!(old, entry_hint(&entry(&r, codec, id_len, 1)));
        SegmentStore::delete(&r.store, old.seg).unwrap();
        let (st, data) = finish(&mut r, t, h);
        assert!(st.is_ok(), "{id_len:?}: {st:?}");
        assert_eq!(data, pattern(101, CS as usize)[..4096], "{id_len:?}");
        assert_eq!(revalidated(&r), 1);
        // And the patched block is in the new extent.
        let (st, got) = r.read(0, u64::from(CS) + 8192, 4096);
        assert!(st.is_ok());
        assert_eq!(got, patch);
    }
}

#[derive(Clone, Debug)]
enum Op {
    Read(u8, u8),
    /// A read parked at the store until the next `Drain` (when the chunk's
    /// current extent has not been read yet).
    Held(u8, u8),
    Write(u8),
    Gc,
    Drain,
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        3 => (0..CHUNKS as u8, 0..4u8).prop_map(|(c, b)| Op::Read(c, b)),
        3 => (0..CHUNKS as u8, 0..4u8).prop_map(|(c, b)| Op::Held(c, b)),
        3 => (0..CHUNKS as u8).prop_map(Op::Write),
        3 => Just(Op::Gc),
        1 => Just(Op::Drain),
    ]
}

struct Pending {
    tag: Tag,
    chunk: u8,
    block: usize,
    allowed: Vec<u64>,
}

fn drain(
    r: &mut Running,
    pending: &mut Vec<Pending>,
    hold: &mut Option<Hold>,
) -> Result<(), TestCaseError> {
    if let Some(h) = hold.take() {
        h.release.send(()).unwrap();
    }
    for p in std::mem::take(pending) {
        let (st, data) = r.wait(&[p.tag]).remove(0);
        prop_assert!(st.is_ok(), "chunk {}: {st:?}", p.chunk);
        prop_assert!(
            p.allowed
                .iter()
                .any(|s| data == pattern(*s, CS as usize)[p.block * 4096..][..4096]),
            "chunk {} returned bytes of no version it could see",
            p.chunk
        );
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 32, ..ProptestConfig::default() })]

    /// Reads race overwrites and a collector that deletes every segment no
    /// map entry names (grace 0): no read fails or returns bytes of a
    /// version it could not have seen, and every parked read whose segment
    /// was deleted under it is counted as revalidated.
    #[test]
    fn reads_racing_overwrite_and_gc_never_fail_or_mis_read(
        ops in proptest::collection::vec(op(), 1..28),
        mode in 0usize..6,
    ) {
        let (codec, id_len) = modes()[mode];
        let (v, mut r) = setup(codec, id_len);
        // Seeds are never reused, so no write dedups onto an extent the
        // collector below deleted behind the index's back.
        let mut next_seed = 1000u64;
        let mut cur: Vec<u64> = (0..CHUNKS).map(|c| 100 + c).collect();
        // Chunks whose current extent no read has decoded yet (a decoded one
        // is served from the cache without the store).
        let mut cold = [true; CHUNKS as usize];
        let mut pending: Vec<Pending> = Vec::new();
        let mut hold: Option<Hold> = None;
        // The segment of the parked read, and how many parked reads lost it.
        let mut parked_seg = None;
        let mut must_revalidate = 0u64;
        for o in ops {
            match o {
                Op::Read(c, b) | Op::Held(c, b) => {
                    let held = matches!(o, Op::Held(..));
                    let ci = usize::from(c);
                    let off = u64::from(c) * u64::from(CS) + u64::from(b) * 4096;
                    let tag = if held && hold.is_none() && cold[ci] {
                        let hint = entry_hint(&entry(&r, codec, id_len, u64::from(c)));
                        let h = arm(&v, hint);
                        let tag = r.submit(0, IoOp::Read, false, off, 4096, &[]);
                        h.reached();
                        hold = Some(h);
                        parked_seg = Some(hint.seg);
                        tag
                    } else {
                        r.submit(0, IoOp::Read, false, off, 4096, &[])
                    };
                    cold[ci] = false;
                    pending.push(Pending {
                        tag, chunk: c, block: usize::from(b), allowed: vec![cur[ci]],
                    });
                }
                Op::Write(c) => {
                    let seed = next_seed;
                    next_seed += 1;
                    prop_assert!(r
                        .write(1, u64::from(c) * u64::from(CS), &pattern(seed, CS as usize), false)
                        .is_ok());
                    r.engine.commit_now(false).expect("commit");
                    cur[usize::from(c)] = seed;
                    // A read still pending for the chunk may retry onto the new
                    // extent and leave it in the decoded cache.
                    cold[usize::from(c)] = !pending.iter().any(|p| p.chunk == c);
                    for p in &mut pending {
                        if p.chunk == c {
                            p.allowed.push(seed);
                        }
                    }
                }
                Op::Gc => {
                    let live: Vec<_> = (0..CHUNKS)
                        .map(|c| entry(&r, codec, id_len, c).seg)
                        .collect();
                    for info in SegmentStore::list(&r.store).unwrap() {
                        if !live.contains(&info.seg) {
                            SegmentStore::delete(&r.store, info.seg).unwrap();
                            if hold.is_some() && parked_seg == Some(info.seg) {
                                must_revalidate += 1;
                                parked_seg = None;
                            }
                        }
                    }
                }
                Op::Drain => {
                    drain(&mut r, &mut pending, &mut hold)?;
                    parked_seg = None;
                }
            }
        }
        drain(&mut r, &mut pending, &mut hold)?;
        prop_assert_eq!(auth_failures(&r), 0);
        prop_assert!(
            revalidated(&r) >= must_revalidate,
            "{} parked reads lost their segment, {} retries",
            must_revalidate,
            revalidated(&r)
        );
        prop_assert_eq!(r.engine.stats().read_revalidate_exhausted, 0);
    }
}

/// The invariant behind the destage base read not revalidating: a GC step
/// runs only between cuts with the map at the tip. With the real collector
/// at its smallest grace and compaction eager, partial-chunk overwrites (each
/// a base read inside a cut) over many collected generations never see a
/// missing extent, and the device stays what the model says.
#[test]
fn the_real_collector_at_grace_zero_never_breaks_a_partial_overwrite() {
    use lunix_gc::{FloorSource, Gc, GcConfig};
    use lunix_iface::gc::GcParams;

    struct NoFloor;
    impl FloorSource for NoFloor {
        fn current_floor(&self) -> lunix_iface::Result<Option<lunix_format::local::RollbackFloor>> {
            Ok(None)
        }
    }

    const N: u64 = 6;
    let mut v = Vol::create(CS, 16, 2, JournalMode::Mixed(3));
    v.tweak = |c| {
        c.gc_budget = Some(lunix_iface::gc::GcBudget {
            max_duration: Duration::from_secs(1),
            max_bytes: 1 << 30,
        });
        c.gc_params = Some(GcParams {
            retain_last: 1,
            // The floor with `read_timeout` 0: the smallest grace there is.
            grace: Duration::from_secs(10),
            read_timeout: Duration::ZERO,
            rate_bytes_per_s: 1 << 30,
            live_threshold: 0.9,
            chunk_size: CS,
            id_len: IdLen::Sixteen,
        });
    };
    *lock(&v.collector) = Some(Box::new(Gc::new(
        Arc::new(NoFloor),
        GcConfig {
            pass_interval: Duration::ZERO,
            ..GcConfig::default()
        },
    )));
    let mut r = v.open().map_err(|e| e.0).expect("open");
    let mut model = vec![0u8; (N * u64::from(CS)) as usize];
    let mut seen: BTreeSet<_> = BTreeSet::new();
    let mut seed = 10_000u64;
    for round in 0..40u64 {
        // A partial overwrite of every chunk (a base read inside the cut),
        // and a whole one of one chunk in turn (its old extent dies).
        for c in 0..N {
            let blk = (round + c) % 4;
            let off = c * u64::from(CS) + blk * 4096;
            let d = pattern(seed, 4096);
            seed += 1;
            assert!(r.write(1, off, &d, false).is_ok());
            model[off as usize..][..4096].copy_from_slice(&d);
        }
        let c = round % N;
        let d = pattern(seed, CS as usize);
        seed += 1;
        assert!(r.write(0, c * u64::from(CS), &d, false).is_ok());
        model[(c * u64::from(CS)) as usize..][..CS as usize].copy_from_slice(&d);
        r.engine
            .commit_now(false)
            .unwrap_or_else(|e| panic!("round {round}: a cut failed: {e}"));
        for _ in 0..6 {
            v.clock.advance(Duration::from_secs(11));
            r.engine
                .tick()
                .unwrap_or_else(|e| panic!("round {round}: a tick failed: {e}"));
        }
        for i in SegmentStore::list(&r.store).unwrap() {
            seen.insert(i.seg);
        }
        let mut got = Vec::new();
        for c in 0..N {
            let (st, d) = r.read(0, c * u64::from(CS), CS);
            assert!(st.is_ok(), "round {round}: read of chunk {c}: {st:?}");
            got.extend_from_slice(&d);
        }
        assert!(
            got == model,
            "round {round}: the device differs from the model"
        );
    }
    let alive: BTreeSet<_> = SegmentStore::list(&r.store)
        .unwrap()
        .iter()
        .map(|i| i.seg)
        .collect();
    assert!(
        seen.iter().any(|s| !alive.contains(s)),
        "the collector deleted nothing: the test did not exercise it"
    );
    let s = r.engine.stats();
    assert!(s.cold_rmw >= 40, "partial overwrites read their base");
    assert_eq!(s.extent_auth_failures, 0);
    assert_eq!(s.read_revalidate_exhausted, 0);
}
