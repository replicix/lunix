//! C7, the revalidating read: a read part whose segment is gone (or whose
//! hint authenticates as another extent) looks the block up again and reads
//! the new entry; only an unchanged entry makes the error real, and only
//! then do `Auth` events count.

mod common;

use std::sync::mpsc::channel;
use std::sync::Arc;
use std::time::Duration;

use common::*;
use lunix_format::node::DataEntry;
use lunix_format::{Hint, IdLen};
use lunix_iface::block::{BlockEngine, IoOp};
use lunix_iface::crypto::ExtentCodec;
use lunix_iface::map::{entry_hint, BlockMap, ChunkNo};
use lunix_iface::store::{CommitStore, PackRead, SegmentStore};
use lunix_testkit::{FlatMap, JournalMode, PlainCodec};
use proptest::prelude::*;

const CS: u32 = 16 << 10;
const CHUNKS: u64 = 3;

fn entry(r: &Running, codec: PlainCodec, id_len: IdLen, ci: u64) -> DataEntry {
    use lunix_format::commit::{Commit, MapRoot};
    use lunix_iface::crypto::CommitCodec;
    let tip = CommitStore::tip(&r.store).unwrap().unwrap();
    let bytes = CommitStore::get(&r.store, tip).unwrap();
    let (root, size): (MapRoot, u64) = match codec.open_commit(tip, &bytes).unwrap() {
        Commit::Full(f) => (f.map_root, f.size),
        _ => panic!("not a FULL"),
    };
    let ext: Arc<dyn ExtentCodec> = Arc::new(codec);
    let packs: Arc<dyn PackRead> = Arc::new(r.store.clone());
    let m = FlatMap::open(ext, packs, &root, size / u64::from(CS), id_len).unwrap();
    m.get(ChunkNo(ci)).unwrap().expect("a stored chunk")
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

fn modes() -> Vec<(PlainCodec, IdLen)> {
    let mut v = Vec::new();
    for codec in [PlainCodec::new(), PlainCodec::unkeyed()] {
        for id_len in [IdLen::Sixteen, IdLen::ThirtyTwo, IdLen::Zero] {
            v.push((codec, id_len));
        }
    }
    v
}

/// Reads the first block of chunk 1 and parks the helper at the store, then overwrites the
/// chunk and commits (`between` runs after the commit, with the old and the
/// new hint), then lets the read go. Returns the read's result.
fn read_across_overwrite(
    v: &Vol,
    r: &mut Running,
    codec: PlainCodec,
    id_len: IdLen,
    between: impl FnOnce(&Running, Hint, Hint),
) -> (lunix_iface::block::IoStatus, Vec<u8>, Vec<u8>) {
    let old = entry_hint(&entry(r, codec, id_len, 1));
    let (entered_tx, entered_rx) = channel();
    let (release_tx, release_rx) = channel();
    *lock(&v.read_faults.hold) = Some(ReadHold {
        hint: old,
        entered: entered_tx,
        release: release_rx,
    });
    // One block: a read of several blocks is several parts, each its own
    // lookup, and only one of them would be parked.
    let t = r.submit(0, IoOp::Read, false, u64::from(CS), 4096, &[]);
    entered_rx
        .recv_timeout(Duration::from_secs(20))
        .expect("the helper reached the store");
    let fresh = pattern(999, CS as usize);
    assert!(r.write(1, u64::from(CS), &fresh, false).is_ok());
    r.engine.commit_now(false).expect("commit");
    let new = entry_hint(&entry(r, codec, id_len, 1));
    assert_ne!(old, new);
    between(r, old, new);
    release_tx.send(()).unwrap();
    let (st, data) = r.wait(&[t]).remove(0);
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

#[derive(Clone, Debug)]
enum Op {
    Read(u8, u8),
    Write(u8, u16),
    Gc,
    Drain,
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        4 => (0..CHUNKS as u8, 0..4u8).prop_map(|(c, b)| Op::Read(c, b)),
        3 => (0..CHUNKS as u8, any::<u16>()).prop_map(|(c, s)| Op::Write(c, s)),
        3 => Just(Op::Gc),
        1 => Just(Op::Drain),
    ]
}

struct Pending {
    tag: lunix_iface::block::Tag,
    chunk: u8,
    block: usize,
    allowed: Vec<u64>,
    writes_seen: u32,
}

fn drain(r: &mut Running, pending: &mut Vec<Pending>) -> Result<(), TestCaseError> {
    for p in std::mem::take(pending) {
        let (st, data) = r.wait(&[p.tag]).remove(0);
        prop_assert!(st.is_ok(), "chunk {}: {st:?}", p.chunk);
        prop_assert!(
            p.allowed.iter().any(|s| data == pattern(*s, CS as usize)[p.block * 4096..][..4096]),
            "chunk {} returned bytes of no version it could see ({} bytes, allowed {:?}, first {:?})",
            p.chunk, data.len(), p.allowed, &data[..data.len().min(8)]
        );
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 24, ..ProptestConfig::default() })]

    /// Reads race overwrites and a collector that deletes every segment no
    /// map entry names (grace 0): no read fails or returns bytes of a
    /// version it could not have seen.
    #[test]
    fn reads_racing_overwrite_and_gc_never_fail_or_mis_read(
        ops in proptest::collection::vec(op(), 1..24),
        mode in 0usize..6,
    ) {
        let (codec, id_len) = modes()[mode];
        let (v, mut r) = setup(codec, id_len);
        v.read_faults.delay_us.store(200, std::sync::atomic::Ordering::SeqCst);
        let mut cur: Vec<u64> = (0..CHUNKS).map(|c| 100 + c).collect();
        let mut pending: Vec<Pending> = Vec::new();
        for o in ops {
            match o {
                Op::Read(c, b) => {
                    // One block, one part: a request of several parts is
                    // torn across a concurrent write by design.
                    let off = u64::from(c) * u64::from(CS) + u64::from(b) * 4096;
                    let tag = r.submit(0, IoOp::Read, false, off, 4096, &[]);
                    pending.push(Pending {
                        tag, chunk: c, block: usize::from(b), allowed: vec![cur[usize::from(c)]], writes_seen: 0,
                    });
                }
                Op::Write(c, s) => {
                    // The retry bound is 3: keep a read from straddling more
                    // writes than a real one could outlast.
                    if pending.iter().any(|p| p.writes_seen >= 2) {
                        drain(&mut r, &mut pending)?;
                    }
                    let seed = 1000 + u64::from(s);
                    prop_assert!(r
                        .write(1, u64::from(c) * u64::from(CS), &pattern(seed, CS as usize), false)
                        .is_ok());
                    r.engine.commit_now(false).expect("commit");
                    cur[usize::from(c)] = seed;
                    for p in &mut pending {
                        p.writes_seen += 1;
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
                        }
                    }
                }
                Op::Drain => drain(&mut r, &mut pending)?,
            }
        }
        drain(&mut r, &mut pending)?;
        prop_assert_eq!(auth_failures(&r), 0);
    }
}
