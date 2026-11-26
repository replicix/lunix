//! The invariant of the base read of a partial overwrite (`read_base`) when
//! the base segment goes away under it: the GC deletes it after a grace, or
//! compaction moved the chunk. Either the cut reads the right base, or it
//! fails and leaves everything as it was. It never merges the new bytes with
//! zeros or with another chunk's bytes, and a stale read is never reported as
//! tampering (`extent_auth_failures` stays 0).
//!
//! `read_base` does not look the chunk up in the map again; it falls back to
//! the chunk index by id prefix. The tests below pin what that gives today.
//! The one that needs the map re-lookup is `#[ignore]`d.

mod common;

use std::sync::Mutex;

use common::*;
use lunix_destage::ErrorClass;
use lunix_format::node::DataEntry;
use lunix_format::{Hint, SegId};
use lunix_iface::map::{entry_hint, BlockMap, ChunkNo};
use lunix_iface::store::SegmentStore;
use lunix_iface::ErrorKind;
use lunix_testkit::{FlatMap, MemIndex};

const CS: u32 = 16 << 10;

fn rig_with(codec: std::sync::Arc<dyn lunix_iface::crypto::ExtentCodec>) -> Rig {
    let mut c = RigConfig::new(CS, 16);
    c.codec = codec;
    Rig::new(c)
}

fn entry_of(r: &Rig, ci: u64) -> DataEntry {
    r.snapshot()[&ci]
}

/// Chunk 0 holds `a` in segment S1. Chunk 4 holds the same bytes stored again
/// in segment S2 (the cut ran against an index that knows nothing of `a`), so
/// the real index knows `a` at both places, as after a compaction copy.
/// Returns (a, S1, S2).
fn base_and_moved_copy(r: &Rig) -> (Vec<u8>, SegId, SegId) {
    let a = random_bytes(31, CS as usize);
    r.write_chunk(0, &a);
    let s1 = r.run_cut(r.last_seq()).segments[0].seg;
    r.write_chunk(4, &a);
    let cut = r.cut(r.last_seq());
    let empty = MemIndex::new(r.store.mem_ids());
    let out = r.destage_with_index(&cut, &empty).expect("destage");
    assert_eq!(out.stats.unique_stored, 1, "stored again, not adopted");
    let s2 = out.segments[0].seg;
    r.finish(&cut, out);
    assert_ne!(s1, s2);
    assert_eq!(entry_of(r, 0).prefix, entry_of(r, 4).prefix);
    (a, s1, s2)
}

fn patch_expected(a: &[u8], patch: &[u8]) -> Vec<u8> {
    let mut want = a.to_vec();
    want[BLOCK..2 * BLOCK].copy_from_slice(patch);
    want
}

/// Reads fail as the GC leaves them once the segment is gone, and run one
/// action (the GC's or the compaction's map update) just before the first
/// read of `seg`, as if it ran concurrently with the cut.
struct RaceStore<'a> {
    inner: lunix_testkit::MemStore,
    map: &'a FlatMap,
    /// The segment to remove and the entry chunk 0 is moved to.
    pending: Mutex<Option<(SegId, DataEntry)>>,
    /// Whether the map update is part of the action (compaction) or not (a
    /// bare delete behind a stale reader).
    move_map: bool,
}

impl SegmentStore for RaceStore<'_> {
    fn open_segment(&self) -> lunix_iface::Result<Box<dyn lunix_iface::store::SegmentWriter>> {
        self.inner.open_segment()
    }

    fn read_extent(&self, hint: &Hint, out: &mut Vec<u8>) -> lunix_iface::Result<()> {
        let act = {
            let mut p = self.pending.lock().expect("pending");
            match *p {
                Some((seg, to)) if seg == hint.seg => p.take().map(|_| (seg, to)),
                _ => None,
            }
        };
        if let Some((seg, to)) = act {
            if self.move_map {
                let mut txn = self.map.begin_update()?;
                txn.set(ChunkNo(0), to)?;
                txn.commit()?;
            }
            self.inner.delete(seg)?;
        }
        self.inner.read_extent(hint, out)
    }

    fn locate(&self, hint: &Hint) -> lunix_iface::Result<lunix_iface::store::ExtentLocation> {
        self.inner.locate(hint)
    }

    fn footer(&self, seg: SegId) -> lunix_iface::Result<lunix_format::segment::FooterBody> {
        self.inner.footer(seg)
    }

    fn list(&self) -> lunix_iface::Result<Vec<lunix_iface::store::SegmentInfo>> {
        self.inner.list()
    }

    fn delete(&self, seg: SegId) -> lunix_iface::Result<u64> {
        self.inner.delete(seg)
    }
}

/// The map still names S1, S1 is deleted, and the index knows the moved copy:
/// the stale hint falls back to the index and the right base is patched.
#[test]
fn base_segment_deleted_after_a_move_the_index_finds_the_copy() {
    for (name, codec) in codecs(CS) {
        let r = rig_with(codec);
        let (a, s1, _s2) = base_and_moved_copy(&r);
        r.store.delete(s1).expect("gc delete");
        let patch = random_bytes(32, BLOCK);
        r.write(BLOCK, &patch);
        let out = r.run_cut(r.last_seq());
        assert_eq!(out.stats.stale_hint_fallbacks, 1, "{name}");
        assert_eq!(
            out.stats.extent_auth_failures, 0,
            "{name}: stale, not tamper"
        );
        assert_eq!(r.read_chunk(0), patch_expected(&a, &patch), "{name}");
        assert_eq!(r.read_chunk(4), a, "{name}: the other chunk is untouched");
    }
}

/// The map still names S1, S1 is deleted and nothing else holds the chunk:
/// the cut fails, applies nothing, patches nothing and reports no tampering.
/// The class is `Permanent` (the engine never retries it), which is safe only
/// because nothing deletes a segment the map names while a cut runs.
#[test]
fn base_segment_deleted_with_no_copy_fails_the_cut_safely() {
    for (name, codec) in codecs(CS) {
        let r = rig_with(codec);
        let a = random_bytes(33, CS as usize);
        r.write_chunk(0, &a);
        let s1 = r.run_cut(r.last_seq()).segments[0].seg;
        r.store.delete(s1).expect("gc delete");
        let before = r.snapshot();
        r.write(BLOCK, &random_bytes(34, BLOCK));
        let cut = r.cut(r.last_seq());
        let err = r.destage(&cut).expect_err("no base to read");
        assert_eq!(err.kind(), ErrorKind::NotFound, "{name}");
        assert_eq!(err.class, ErrorClass::Permanent, "{name}");
        assert_eq!(err.chunk, Some(0));
        assert_eq!(err.partial.stats.extent_auth_failures, 0, "{name}");
        assert!(
            err.partial.segments.is_empty(),
            "{name}: nothing was stored"
        );
        assert_eq!(r.snapshot(), before, "{name}: the map is untouched");
    }
}

/// The only other copy is damaged: the cut fails and says so, and never
/// patches the damaged bytes.
#[test]
fn a_damaged_moved_copy_is_never_patched_and_is_counted() {
    for (name, codec) in codecs(CS) {
        let r = rig_with(codec);
        let (_a, s1, s2) = base_and_moved_copy(&r);
        r.store.delete(s1).expect("gc delete");
        let hint2 = entry_hint(&entry_of(&r, 4));
        let t = TamperStore::new(r.store.clone());
        *t.flip.lock().unwrap() = Some((
            s2,
            r.codec.mode().extent_ovh() as usize + hint2.clen as usize - 1,
        ));
        let before = r.snapshot();
        r.write(BLOCK, &random_bytes(35, BLOCK));
        let cut = r.cut(r.last_seq());
        let err = r.destage_via(&cut, &t);
        if name.contains("plain") {
            // No tag to check on a plain volume: the flip is not detectable.
            return;
        }
        let err = err.expect_err("damaged copy");
        assert!(
            matches!(err.kind(), ErrorKind::NotFound | ErrorKind::Auth),
            "{name}: {:?}",
            err.kind()
        );
        assert_eq!(err.partial.stats.extent_auth_failures, 1, "{name}");
        assert!(err.partial.segments.is_empty(), "{name}");
        assert_eq!(r.snapshot(), before, "{name}");
    }
}

/// Compaction moves the chunk and the GC deletes S1 while the cut is between
/// reading the map entry and reading the extent; the index already knows the
/// copy: the right base is patched through the index.
#[test]
fn a_move_racing_the_base_read_is_found_through_the_index() {
    for (name, codec) in codecs(CS) {
        let r = rig_with(codec);
        let (a, s1, _s2) = base_and_moved_copy(&r);
        let to = entry_of(&r, 4);
        let race = RaceStore {
            inner: r.store.clone(),
            map: &r.map,
            pending: Mutex::new(Some((s1, to))),
            move_map: true,
        };
        let patch = random_bytes(36, BLOCK);
        r.write(BLOCK, &patch);
        let cut = r.cut(r.last_seq());
        let out = r.destage_via(&cut, &race).expect("destage");
        assert_eq!(out.stats.extent_auth_failures, 0, "{name}");
        r.finish(&cut, out);
        assert_eq!(r.read_chunk(0), patch_expected(&a, &patch), "{name}");
    }
}

/// The same race while the index does not yet know the copy (a compaction
/// inserts into the index after its commit). The map already names the copy,
/// so a re-lookup through the map would succeed. Today `read_base` fails the
/// cut with `NotFound`/`Permanent` instead: safe (nothing is applied, no
/// tamper report, no wrong data) but it is not a revalidation. Finding for
/// the short-grace design; the daemon avoids it only by never running a GC
/// step during a cut.
#[test]
#[ignore = "read_base does not re-resolve through the map: fails by design today (short-grace finding)"]
fn a_move_racing_the_base_read_is_found_through_the_map_when_the_index_lags() {
    for (name, codec) in codecs(CS) {
        let r = rig_with(codec);
        let (a, s1, _s2) = base_and_moved_copy(&r);
        let to = entry_of(&r, 4);
        let race = RaceStore {
            inner: r.store.clone(),
            map: &r.map,
            pending: Mutex::new(Some((s1, to))),
            move_map: true,
        };
        // The index forgot the moved copy: only the old place is known.
        let lag = MemIndex::new(r.store.mem_ids());
        let patch = random_bytes(37, BLOCK);
        r.write(BLOCK, &patch);
        let cut = r.cut(r.last_seq());
        let out = r
            .destage_with_index_via(&cut, &lag, &race)
            .unwrap_or_else(|e| panic!("{name}: {:?} {:?}", e.kind(), e.class));
        r.finish(&cut, out);
        assert_eq!(r.read_chunk(0), patch_expected(&a, &patch), "{name}");
    }
}

/// The same race, asserted as it behaves today: a safe failure.
#[test]
fn a_move_racing_the_base_read_with_a_lagging_index_fails_safely_today() {
    for (name, codec) in codecs(CS) {
        let r = rig_with(codec);
        let (_a, s1, _s2) = base_and_moved_copy(&r);
        let to = entry_of(&r, 4);
        let race = RaceStore {
            inner: r.store.clone(),
            map: &r.map,
            pending: Mutex::new(Some((s1, to))),
            move_map: true,
        };
        let lag = MemIndex::new(r.store.mem_ids());
        r.write(BLOCK, &random_bytes(38, BLOCK));
        let cut = r.cut(r.last_seq());
        let err = r
            .destage_with_index_via(&cut, &lag, &race)
            .expect_err("no revalidation through the map");
        assert_eq!(err.kind(), ErrorKind::NotFound, "{name}");
        assert_eq!(err.partial.stats.extent_auth_failures, 0, "{name}");
        assert!(err.partial.segments.is_empty(), "{name}");
        // The chunk's current map entry is the moved copy, untouched by the cut.
        assert_eq!(entry_of(&r, 0), entry_of(&r, 4), "{name}");
    }
}
