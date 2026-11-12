//! D17 (dedup confirmation) and R1 (every stored-extent read passes its
//! expectation), over every codec flavour.

mod common;

use common::*;
use lunix_format::node::DataEntry;
use lunix_format::Hint;
use lunix_iface::map::{entry_hint, BlockMap, ChunkNo};
use lunix_iface::ErrorKind;
use lunix_testkit::MemIndex;
use std::sync::Arc;

const CS: u32 = 16 << 10;

fn rig_with(codec: Arc<dyn lunix_iface::crypto::ExtentCodec>) -> Rig {
    let mut c = RigConfig::new(CS, 16);
    c.codec = codec;
    Rig::new(c)
}

fn entry_of(r: &Rig, ci: u64) -> DataEntry {
    r.snapshot()[&ci]
}

#[test]
fn a_hinted_hit_naming_another_valid_extent_is_a_miss() {
    for (name, codec) in codecs(CS) {
        let r = rig_with(codec);
        let b = random_bytes(1, CS as usize);
        r.write_chunk(0, &b);
        r.run_cut(r.last_seq());
        let hint_b = entry_hint(&entry_of(&r, 0));

        // The run says X lives where B does.
        let x = text_bytes(2, CS as usize);
        r.index().insert_hinted(r.id_of(&x), hint_b);
        r.write_chunk(1, &x);
        let out = r.run_cut(r.last_seq());
        assert_eq!(out.stats.dedup_rejected, 1, "{name}");
        assert_eq!(out.stats.dedup_hits, 0);
        assert_eq!(out.stats.unique_stored, 1, "{name}: stored again");
        assert_eq!(r.extent_count(), 2);
        assert_eq!(r.read_chunk(1), x, "{name}: a later read of X is right");
        r.assert_equals(&r.model_at(r.last_seq()));
    }
}

#[test]
fn a_hinted_hit_with_a_damaged_tag_is_a_miss() {
    for (name, codec) in codecs(CS) {
        let r = rig_with(codec);
        let x = text_bytes(3, CS as usize);
        r.write_chunk(0, &x);
        let out = r.run_cut(r.last_seq());
        let seg = out.segments[0].seg;
        let hint = entry_hint(&entry_of(&r, 0));
        r.reopen_index_hinted();

        let t = TamperStore::new(r.store.clone());
        // The last byte of the extent is the last tag byte.
        let extent_len = r.codec.mode().extent_ovh() as usize + hint.clen as usize;
        *t.flip.lock().unwrap() = Some((seg, extent_len - 1));

        r.write_chunk(1, &x);
        let cut = r.cut(r.last_seq());
        let out = r.destage_via(&cut, &t).expect("destage");
        assert_eq!(out.stats.dedup_rejected, 1, "{name}");
        assert_eq!(out.stats.extent_auth_failures, 1);
        assert_eq!(out.stats.unique_stored, 1);
        r.finish(&cut, out);
        r.assert_equals(&r.model_at(r.last_seq()));
        assert_ne!(
            entry_of(&r, 1).seg,
            seg,
            "the new chunk does not point at the damaged copy"
        );
    }
}

#[test]
fn a_correct_hinted_hit_is_adopted_without_a_store() {
    for (name, codec) in codecs(CS) {
        let r = rig_with(codec);
        let x = text_bytes(4, CS as usize);
        r.write_chunk(0, &x);
        r.run_cut(r.last_seq());
        let extents = r.extent_count();
        r.reopen_index_hinted();

        let t = TamperStore::new(r.store.clone());
        r.write_chunk(5, &x);
        let cut = r.cut(r.last_seq());
        let out = r.destage_via(&cut, &t).expect("destage");
        assert_eq!(out.stats.dedup_confirmed, 1, "{name}");
        assert_eq!(out.stats.dedup_hits, 1);
        assert_eq!(out.stats.unique_stored, 0);
        assert!(out.segments.is_empty() && out.open.is_none());
        assert_eq!(t.reads(), 1, "exactly one confirmation read");
        r.finish(&cut, out);
        assert_eq!(r.extent_count(), extents);
        assert_eq!(entry_of(&r, 5), entry_of(&r, 0));
        r.assert_equals(&r.model_at(r.last_seq()));
    }
}

#[test]
fn an_unreadable_or_absurd_hint_is_a_miss_and_absurd_ones_are_not_read() {
    let r = rig_with(codecs(CS).remove(0).1);
    let x = text_bytes(5, CS as usize);
    let seg = {
        r.write_chunk(0, &random_bytes(9, CS as usize));
        r.run_cut(r.last_seq()).segments[0].seg
    };
    let ghost = lunix_format::ObjId::new(0, 9999).unwrap();
    for (hint, reads) in [
        (
            Hint {
                seg: ghost,
                off: 0,
                clen: 100,
            },
            1,
        ),
        (
            Hint {
                seg,
                off: 3,
                clen: 100,
            },
            1,
        ),
        (
            Hint {
                seg,
                off: 0,
                clen: u32::MAX,
            },
            0,
        ),
        (
            Hint {
                seg,
                off: 0,
                clen: 0,
            },
            0,
        ),
        (
            Hint {
                seg,
                off: 0,
                clen: CS + 1,
            },
            0,
        ),
    ] {
        r.index().insert_hinted(r.id_of(&x), hint);
        let t = TamperStore::new(r.store.clone());
        r.write_chunk(1, &x);
        let cut = r.cut(r.last_seq());
        let out = r.destage_via(&cut, &t).expect("destage");
        assert_eq!(out.stats.dedup_rejected, 1, "{hint:?}");
        assert_eq!(t.reads(), reads, "{hint:?}");
        r.finish(&cut, out);
        r.assert_equals(&r.model_at(r.last_seq()));
        // Next round starts from a clean slate for the data.
        r.write_chunk(1, &vec![0u8; CS as usize]);
        r.run_cut(r.last_seq());
    }
}

/// Stores A at chunk 0 and B at chunk 1, then makes chunk 2's leaf name A
/// while its hint points at B's extent.
fn forged_leaf(r: &Rig) -> (Vec<u8>, Vec<u8>) {
    let a = random_bytes(11, CS as usize);
    let b = text_bytes(12, CS as usize);
    r.write_chunk(0, &a);
    r.write_chunk(1, &b);
    r.run_cut(r.last_seq());
    let (ea, eb) = (entry_of(r, 0), entry_of(r, 1));
    let forged = DataEntry {
        prefix: ea.prefix,
        ..eb
    };
    let mut txn = r.map.begin_update().unwrap();
    txn.set(ChunkNo(2), forged).unwrap();
    txn.commit().unwrap();
    (a, b)
}

#[test]
fn r1_a_leaf_naming_a_with_a_hint_at_b_fails_the_rmw() {
    for (name, codec) in codecs(CS) {
        let r = rig_with(codec);
        forged_leaf(&r);
        let before = r.snapshot();
        // A partial write into chunk 2 needs its base.
        r.write(2 * CS as usize + BLOCK, &random_bytes(13, BLOCK));
        let cut = r.cut(r.last_seq());
        // An index that knows nothing of A cannot recover the stale hint.
        let empty = MemIndex::new(r.store.mem_ids());
        let err = r.destage_with_index(&cut, &empty).expect_err("must fail");
        assert_eq!(err.kind(), ErrorKind::Auth, "{name}");
        assert!(err.partial.stats.extent_auth_failures >= 1);
        assert_eq!(r.snapshot(), before, "{name}: the map is untouched");
        assert_eq!(
            err.partial.segments.len(),
            0,
            "nothing of B was patched into a new chunk"
        );
    }
}

#[test]
fn r1_a_stale_hint_falls_back_to_the_index_and_still_confirms() {
    for (name, codec) in codecs(CS) {
        let r = rig_with(codec);
        let (a, _) = forged_leaf(&r);
        // The real index knows A at its true place: the forged hint is just stale.
        let patch = random_bytes(14, BLOCK);
        r.write(2 * CS as usize + BLOCK, &patch);
        let out = r.run_cut(r.last_seq());
        assert_eq!(out.stats.stale_hint_fallbacks, 1, "{name}");
        assert_eq!(out.stats.cold_rmw, 1);
        let mut want = a.clone();
        want[BLOCK..2 * BLOCK].copy_from_slice(&patch);
        assert_eq!(r.read_chunk(2), want, "{name}: A patched, never B");
    }
}

#[test]
fn r1_a_damaged_base_extent_fails_the_rmw() {
    for (name, codec) in codecs(CS) {
        let r = rig_with(codec);
        let a = random_bytes(21, CS as usize);
        r.write_chunk(3, &a);
        let out = r.run_cut(r.last_seq());
        let seg = out.segments[0].seg;
        let hint = entry_hint(&entry_of(&r, 3));
        let t = TamperStore::new(r.store.clone());
        *t.flip.lock().unwrap() = Some((
            seg,
            r.codec.mode().extent_ovh() as usize + hint.clen as usize - 1,
        ));
        r.write(3 * CS as usize, &random_bytes(22, BLOCK));
        let cut = r.cut(r.last_seq());
        let err = r.destage_via(&cut, &t).expect_err("damaged base");
        assert_eq!(err.kind(), ErrorKind::Auth, "{name}");
        assert_eq!(r.read_chunk(3), a, "{name}: the old chunk is intact");
    }
}

#[test]
fn a_base_read_with_a_zero_length_id_prefix_has_no_fallback() {
    // A lean map names no prefix: the original error stands.
    let mut c = RigConfig::new(CS, 8);
    c.id_len = lunix_format::IdLen::Zero;
    let r = Rig::new(c);
    r.write_chunk(0, &random_bytes(1, CS as usize));
    let out = r.run_cut(r.last_seq());
    let seg = out.segments[0].seg;
    let hint = entry_hint(&entry_of(&r, 0));
    let t = TamperStore::new(r.store.clone());
    *t.flip.lock().unwrap() = Some((
        seg,
        r.codec.mode().extent_ovh() as usize + hint.clen as usize - 1,
    ));
    r.write(0, &random_bytes(2, BLOCK));
    let cut = r.cut(r.last_seq());
    let err = r.destage_via(&cut, &t).expect_err("damaged");
    assert_eq!(err.kind(), ErrorKind::Auth);
}
