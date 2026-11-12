//! Round trips of segments and packs at the sizes the gate names.

use lunix_format::{ChunkId, Codec, CryptoMode, Hint};
use lunix_iface::crypto::{ExtentCodec, OpenExpect, OpenOutput};
use lunix_iface::store::{PackRead, PackStore, SegmentStore};

use super::codec::{build_extent, open_extent, TestCodec};
use super::{cid, create, node_extent, open, payload, tempdir, test_opts, UUID};
use crate::LocalVolume;

/// What one appended extent should read back as.
struct Want {
    id: ChunkId,
    bytes: Vec<u8>,
    payload: Vec<u8>,
    hint: Hint,
}

/// Builds the extents of a segment with a mix of all codecs: codec 0 (stored
/// as is, `clen == ulen`), 1 (zstd), 2 (lz4) and 3 (zstd with a dictionary).
/// The store treats the payload as opaque, so the bytes need not decompress.
fn fill_segment(
    vol: &LocalVolume,
    mode: CryptoMode,
    chunk: u32,
    n: u32,
) -> (lunix_format::SegId, Vec<Want>, u64) {
    let mut w = vol.open_segment().expect("open segment");
    let mut wants = Vec::new();
    for i in 0..n {
        let id = cid(i.wrapping_mul(2_654_435_761)); // distinct, not in order
        let (codec, level, clen, dict) = match i % 64 {
            0 => (Codec::Raw, 0, chunk as usize, None),
            1..=40 => (Codec::Zstd, 3, 40 + (i as usize * 13) % 300, None),
            41..=60 => (Codec::Lz4, 0, 60 + (i as usize * 7) % 200, None),
            _ => (
                Codec::ZstdDict,
                3,
                50 + (i as usize % 90),
                Some(100 + i % 3),
            ),
        };
        let pl = payload(&id, clen);
        let bytes = build_extent(mode, codec, level, chunk, &id, &pl);
        let hint = w.append_extent(&id, dict, &bytes).expect("append");
        wants.push(Want {
            id,
            bytes,
            payload: pl,
            hint,
        });
    }
    let seg = w.id();
    let sealed = w.seal(None).expect("seal");
    assert_eq!((sealed.seg, sealed.extents), (seg, n));
    (seg, wants, sealed.size_bytes)
}

fn check_segment(
    vol: &LocalVolume,
    mode: CryptoMode,
    chunk: u32,
    seg: lunix_format::SegId,
    wants: &[Want],
    size: u64,
) {
    let codec = TestCodec::new(mode, UUID);
    let footer = vol.footer(seg).expect("footer");
    assert_eq!(footer.entries.len(), wants.len());
    assert_eq!(footer.object_id, seg);
    let mut sorted = footer.entries.clone();
    sorted.sort_by_key(|e| (e.chunk_id, e.off));
    assert_eq!(sorted, footer.entries, "footer entries are sorted");
    let dicts: std::collections::BTreeSet<u32> = (0..wants.len() as u32)
        .filter(|i| i % 64 > 60)
        .map(|i| 100 + i % 3)
        .collect();
    assert_eq!(footer.dict_ids, dicts.into_iter().collect::<Vec<_>>());
    assert_eq!(
        vol.list()
            .expect("list")
            .iter()
            .find(|i| i.seg == seg)
            .map(|i| i.size_bytes),
        Some(size)
    );
    vol.verify_segment(seg).expect("segment hash");
    let mut out = Vec::new();
    for w in wants {
        assert!(footer
            .entries
            .iter()
            .any(|e| e.chunk_id == w.id && e.off == w.hint.off && e.clen == w.hint.clen));
        out.clear();
        vol.read_extent(&w.hint, &mut out).expect("read");
        assert_eq!(out, w.bytes);
        let mut plain = Vec::new();
        codec
            .open(
                lunix_format::ExtentKind::Data,
                &out,
                &OpenExpect {
                    chunk_size: chunk,
                    chunk_id: Some(&w.id),
                    id_prefix: None,
                    verify_id: false,
                    output: OpenOutput::Stored,
                },
                &mut plain,
            )
            .expect("tag verifies");
        assert_eq!(plain, w.payload);
    }
}

#[test]
fn segments_of_1_255_and_4096_extents_round_trip_in_every_mode() {
    for mode in [CryptoMode::Plain, CryptoMode::Encrypted] {
        for chunk in [16 * 1024u32, 64 * 1024] {
            for n in [1u32, 255, 4096] {
                let dir = tempdir();
                let vol = create(dir.path(), mode, chunk, test_opts());
                let (seg, wants, size) = fill_segment(&vol, mode, chunk, n);
                check_segment(&vol, mode, chunk, seg, &wants, size);
                drop(vol);
                let vol = open(dir.path(), mode, test_opts()).expect("reopen");
                check_segment(&vol, mode, chunk, seg, &wants, size);
            }
        }
    }
}

#[test]
fn packs_of_1_255_and_4096_nodes_round_trip() {
    for mode in [CryptoMode::Integrity, CryptoMode::Encrypted] {
        for n in [1u32, 255, 4096] {
            let dir = tempdir();
            let vol = create(dir.path(), mode, 16 * 1024, test_opts());
            let mut p = vol.open_pack().expect("pack");
            let mut locs = Vec::new();
            for i in 0..n {
                let hash = cid(i).0;
                let e = node_extent(mode, &hash, 20 + i as usize % 200);
                locs.push((p.append_node(&hash, &e).expect("node"), e));
            }
            let pack = p.id();
            let sealed = p.seal().expect("seal");
            assert_eq!((sealed.pack, sealed.nodes), (pack, n));
            drop(vol);
            let vol = open(dir.path(), mode, test_opts()).expect("reopen");
            vol.verify_pack(pack).expect("pack hash");
            let footer = vol.pack_footer(pack).expect("footer");
            assert_eq!(footer.entries.len(), n as usize);
            let mut out = Vec::new();
            for (loc, e) in &locs {
                out.clear();
                vol.read_node(*loc, &mut out).expect("read");
                assert_eq!(&out, e);
                assert_eq!(
                    open_extent(mode, &out).expect("tag").len(),
                    e.len() - mode.extent_ovh() as usize
                );
            }
        }
    }
}

#[test]
fn a_bad_extent_is_refused_and_two_copies_of_an_id_are_kept() {
    let dir = tempdir();
    let vol = create(dir.path(), CryptoMode::Plain, 16 * 1024, test_opts());
    let id = cid(1);
    let good = super::data_extent(CryptoMode::Plain, 16 * 1024, &id, 100);
    let mut w = vol.open_segment().expect("open");
    let kind = |e: lunix_iface::Error| e.kind();
    use lunix_iface::ErrorKind::Invalid;
    assert_eq!(
        kind(w.append_extent(&cid(2), None, &good).expect_err("id")),
        Invalid
    );
    assert_eq!(
        kind(w.append_extent(&id, None, &good[..60]).expect_err("short")),
        Invalid
    );
    let mut bad_magic = good.clone();
    bad_magic[0] = b'X';
    assert_eq!(
        kind(w.append_extent(&id, None, &bad_magic).expect_err("magic")),
        Invalid
    );
    let mut bad_clen = good.clone();
    bad_clen[48] ^= 1;
    assert_eq!(
        kind(w.append_extent(&id, None, &bad_clen).expect_err("clen")),
        Invalid
    );
    assert_eq!(
        kind(w.append_extent(&id, Some(4), &good).expect_err("dict")),
        Invalid
    );
    assert!(w.is_empty(), "refused extents leave no trace");
    let h1 = w.append_extent(&id, None, &good).expect("first");
    let h2 = w.append_extent(&id, None, &good).expect("second copy");
    assert!(h2.off > h1.off);
    let seg = w.id();
    w.seal(None).expect("seal");
    assert_eq!(vol.footer(seg).expect("footer").entries.len(), 2);
    let empty = vol.open_segment().expect("open");
    assert_eq!(kind(empty.seal(None).expect_err("empty")), Invalid);
    assert!(vol.list().expect("list").iter().all(|i| i.seg == seg));
}

#[test]
fn the_writer_reports_full_at_the_target_and_hints_stay_exact() {
    let dir = tempdir();
    let mut opts = test_opts();
    opts.segment_target = 64 * 1024;
    let vol = create(dir.path(), CryptoMode::Plain, 16 * 1024, opts);
    let mut w = vol.open_segment().expect("open");
    let mut n = 0;
    while !w.is_full() {
        let id = cid(n);
        let e = super::data_extent(CryptoMode::Plain, 16 * 1024, &id, 4000);
        let h = w.append_extent(&id, None, &e).expect("append");
        assert_eq!(u64::from(h.off), u64::from(n) * (4000 + 68));
        n += 1;
    }
    assert_eq!(n, 17);
    assert_eq!(w.len(), u64::from(n) * 4068);
}
