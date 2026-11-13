//! The pipeline against the testkit fakes with `PlainCodec`.

mod common;

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use common::*;
use lunix_destage::{destage_cut_with_cancel, DestageConfig, ErrorClass, Pressure};
use lunix_format::volume::CodecPolicy;
use lunix_format::{Codec, CryptoMode, WriteSeq};
use lunix_iface::crypto::ExtentCodec;
use lunix_iface::index::ChunkIndex;
use lunix_iface::map::BlockMap;
use lunix_iface::space::SpaceEvents;
use lunix_iface::store::SegmentStore;
use lunix_iface::ErrorKind;
use lunix_testkit::{Fault, PlainCodec};

const SIZES: [u32; 2] = [16 << 10, 64 << 10];

fn rig(chunk_size: u32) -> Rig {
    Rig::new(RigConfig::new(chunk_size, 16))
}

#[test]
fn identical_chunks_make_one_extent() {
    for cs in SIZES {
        let r = rig(cs);
        let data = text_bytes(1, cs as usize);
        for ci in 0..8 {
            r.write_chunk(ci, &data);
        }
        let out = r.run_cut(r.last_seq());
        assert_eq!(out.stats.unique_stored, 1, "cs {cs}");
        assert_eq!(out.stats.dedup_in_cut, 7);
        assert_eq!(r.extent_count(), 1);
        let snap = r.snapshot();
        let first = snap[&0];
        assert!(
            snap.values().all(|e| *e == first),
            "all point at one extent"
        );
        r.assert_equals(&r.model_at(r.last_seq()));

        // The same data again, later: a hit on the index, nothing stored.
        r.write_chunk(9, &data);
        let out = r.run_cut(r.last_seq());
        assert_eq!(out.stats.dedup_hits, 1);
        assert_eq!(out.stats.unique_stored, 0);
        assert!(out.segments.is_empty());
        assert_eq!(r.extent_count(), 1);
        r.assert_equals(&r.model_at(r.last_seq()));
    }
}

#[test]
fn zero_chunks_are_holes_and_store_nothing() {
    for cs in SIZES {
        let r = rig(cs);
        r.write_chunk(2, &vec![0u8; cs as usize]);
        r.write_chunk(3, &random_bytes(3, cs as usize));
        let out = r.run_cut(r.last_seq());
        assert_eq!(out.stats.zero_holes, 1);
        assert_eq!(r.extent_count(), 1);
        assert!(r.map.get(lunix_iface::map::ChunkNo(2)).unwrap().is_none());
        // Overwriting stored data with zeros turns the entry into a hole.
        r.write_chunk(3, &vec![0u8; cs as usize]);
        let out = r.run_cut(r.last_seq());
        assert!(out.segments.is_empty(), "no extent for zeros");
        assert!(r.map.get(lunix_iface::map::ChunkNo(3)).unwrap().is_none());
        r.assert_equals(&r.model_at(r.last_seq()));
    }
}

#[test]
fn a_partial_write_reads_the_base_and_patches_it() {
    for cs in SIZES {
        let r = rig(cs);
        let base = random_bytes(5, cs as usize);
        r.write_chunk(1, &base);
        let out = r.run_cut(r.last_seq());
        assert_eq!(out.stats.cold_rmw, 0);

        // One block in the middle.
        let patch = random_bytes(6, BLOCK);
        r.write(cs as usize + 2 * BLOCK, &patch);
        let out = r.run_cut(r.last_seq());
        assert_eq!(out.stats.cold_rmw, 1, "cs {cs}");
        let mut want = base.clone();
        want[2 * BLOCK..3 * BLOCK].copy_from_slice(&patch);
        assert_eq!(r.read_chunk(1), want);

        // A partial write into a chunk with no base needs no read.
        r.write(4 * cs as usize + BLOCK, &patch);
        let out = r.run_cut(r.last_seq());
        assert_eq!(out.stats.cold_rmw, 0);
        r.assert_equals(&r.model_at(r.last_seq()));
    }
}

#[test]
fn hole_pieces_cover_and_patch() {
    for cs in SIZES {
        let r = rig(cs);
        r.write_chunk(0, &random_bytes(1, cs as usize));
        r.write_chunk(1, &random_bytes(2, cs as usize));
        r.run_cut(r.last_seq());

        // A discard of a whole chunk: a hole, no read.
        r.discard(0, cs as usize);
        let out = r.run_cut(r.last_seq());
        assert_eq!(out.stats.hole_covered, 1);
        assert_eq!(out.stats.cold_rmw, 0);
        assert!(r.map.get(lunix_iface::map::ChunkNo(0)).unwrap().is_none());

        // A discard of the middle of a chunk: the base patched with zeros.
        r.discard(cs as usize + BLOCK, 2 * BLOCK);
        let out = r.run_cut(r.last_seq());
        assert_eq!(out.stats.cold_rmw, 1);
        // A write, then a discard over part of it, then a write over part of that.
        r.write(2 * cs as usize, &random_bytes(9, 4 * BLOCK));
        r.discard(2 * cs as usize + BLOCK, 2 * BLOCK);
        r.write(2 * cs as usize + 2 * BLOCK, &random_bytes(10, BLOCK));
        r.run_cut(r.last_seq());
        r.assert_equals(&r.model_at(r.last_seq()));
    }
}

#[test]
fn every_policy_round_trips() {
    for (policy, level) in [
        (CodecPolicy::None, 0),
        (CodecPolicy::Lz4, 0),
        (CodecPolicy::Zstd, 3),
        (CodecPolicy::Zstd, -3),
        (CodecPolicy::Auto, 1),
        (CodecPolicy::Auto, 9),
    ] {
        for cs in SIZES {
            let mut c = RigConfig::new(cs, 16);
            c.policy = policy;
            c.level = level;
            let r = Rig::new(c);
            for ci in 0..6u64 {
                let data = match ci % 3 {
                    0 => text_bytes(ci, cs as usize),
                    1 => random_bytes(ci, cs as usize),
                    _ => vec![0x41 + ci as u8; cs as usize],
                };
                r.write_chunk(ci, &data);
            }
            let out = r.run_cut(r.last_seq());
            r.assert_equals(&r.model_at(r.last_seq()));
            let by = out.stats.chunks_by_codec;
            match policy {
                CodecPolicy::None => assert_eq!(by[Codec::Raw.id() as usize], 6),
                CodecPolicy::Lz4 => {
                    assert_eq!(by[Codec::Lz4.id() as usize], 4, "text and 0x41 shrink");
                    assert_eq!(by[Codec::Raw.id() as usize], 2, "random stays raw");
                }
                CodecPolicy::Zstd | CodecPolicy::Auto => {
                    assert_eq!(by[Codec::Zstd.id() as usize], 4, "{policy:?}");
                    assert_eq!(by[Codec::Raw.id() as usize], 2);
                }
            }
        }
    }
}

#[test]
fn zstd_level_is_written_in_the_header() {
    let r = {
        let mut c = RigConfig::new(16 << 10, 4);
        c.policy = CodecPolicy::Zstd;
        c.level = 5;
        Rig::new(c)
    };
    r.write_chunk(0, &text_bytes(1, 16 << 10));
    r.run_cut(r.last_seq());
    let e = r.snapshot()[&0];
    let mut raw = Vec::new();
    r.store
        .read_extent(&lunix_iface::map::entry_hint(&e), &mut raw)
        .unwrap();
    let expect = lunix_iface::crypto::OpenExpect {
        chunk_size: 16 << 10,
        chunk_id: None,
        id_prefix: Some(&e.prefix),
        verify_id: false,
        output: lunix_iface::crypto::OpenOutput::Stored,
    };
    let meta = r
        .codec
        .open(
            lunix_format::ExtentKind::Data,
            &raw,
            &expect,
            &mut Vec::new(),
        )
        .unwrap();
    assert_eq!((meta.desc.codec, meta.desc.level.0), (Codec::Zstd, 5));
}

#[test]
fn full_segments_roll_and_the_cut_lists_them() {
    let cs = 16u32 << 10;
    let mut c = RigConfig::new(cs, 32);
    c.policy = CodecPolicy::None;
    c.level = 0;
    c.segment_target = 3 * u64::from(cs);
    let r = Rig::new(c);
    for ci in 0..20u64 {
        r.write_chunk(ci, &random_bytes(100 + ci, cs as usize));
    }
    let cut = r.cut(r.last_seq());
    let out = r.destage(&cut).expect("destage");
    assert!(out.segments.len() >= 6, "{} segments", out.segments.len());
    // Every segment is already on disk and sealed when the cut returns.
    assert_eq!(r.store.list().unwrap().len(), out.segments.len());
    let total: usize = out.segments.iter().map(|s| s.entries.len()).sum();
    assert_eq!(total, 20);
    // LBA order: segment ids and offsets grow with the chunk number.
    let snap = r.snapshot();
    let mut prev = (0u64, 0u32);
    for ci in 0..20u64 {
        let e = snap[&ci];
        assert!((e.seg.raw(), e.off) > prev, "chunk {ci} out of order");
        prev = (e.seg.raw(), e.off);
    }
    for w in out.segments.windows(2) {
        assert!(w[0].writes.last < w[1].writes.first);
    }
    let out = r.finish(&cut, out);
    assert_eq!(
        out.sealed_bytes(),
        r.store
            .list()
            .unwrap()
            .iter()
            .map(|s| s.size_bytes)
            .sum::<u64>()
    );
    r.assert_equals(&r.model_at(r.last_seq()));
}

#[test]
fn a_failed_chunk_fails_the_cut_applies_nothing_and_deletes_its_segments() {
    let cs = 16u32 << 10;
    let mut c = RigConfig::new(cs, 32);
    c.policy = CodecPolicy::None;
    c.level = 0;
    c.segment_target = 3 * u64::from(cs);
    let r = Rig::new(c);
    for ci in 0..12u64 {
        r.write_chunk(ci, &random_bytes(200 + ci, cs as usize));
    }
    r.store.faults().arm("segment.append", 8, Fault::NoSpace);
    let cut = r.cut(r.last_seq());
    let err = r.destage(&cut).expect_err("must fail");
    assert_eq!(err.kind(), ErrorKind::NoSpace);
    assert_eq!(err.class, ErrorClass::AfterSpace);
    assert!(r.snapshot().is_empty(), "nothing applied to the map");
    assert!(
        !err.partial.segments.is_empty(),
        "the rolled segments are reported"
    );
    assert!(err.partial.freed_bytes > 0);
    assert!(
        r.store.list().unwrap().is_empty(),
        "the orphans were deleted"
    );
    // The overlay still holds the data; the retry succeeds and equals the model.
    let out = r.destage(&cut).expect("retry");
    r.finish(&cut, out);
    r.assert_equals(&r.model_at(r.last_seq()));
}

#[test]
fn map_full_is_found_before_anything_is_stored() {
    let cs = 16u32 << 10;
    let r = Rig::new(RigConfig::new(cs, 16));
    for ci in 0..6u64 {
        r.write_chunk(ci, &random_bytes(ci, cs as usize));
    }
    let cut = r.cut(r.last_seq());
    let index = r.index();
    // The map takes five updates per transaction; the cut has six chunks.
    let cap = CapMap {
        inner: &r.map,
        cap: 5,
    };
    let mut deps = r.deps(&index);
    deps.map = &cap;
    let err = lunix_destage::destage_cut(&cut, &deps).expect_err("map full");
    assert_eq!(err.kind(), ErrorKind::MapFull);
    assert_eq!(
        err.class,
        ErrorClass::CutTooLarge,
        "no dirty leaf: sealing cannot help"
    );
    assert!(err.partial.segments.is_empty());
    assert_eq!(
        r.store.faults().hits().get("segment.append"),
        None,
        "nothing was stored"
    );
    assert!(r.store.list().unwrap().is_empty());
    assert!(r.snapshot().is_empty());
    // A cut that fits goes through the same map.
    let cap = CapMap {
        inner: &r.map,
        cap: 6,
    };
    deps.map = &cap;
    lunix_destage::destage_cut(&cut, &deps).expect("fits");
}

#[test]
fn a_commit_time_map_full_deletes_what_the_cut_stored() {
    // FlatMap checks its cap at commit, so this one is found after the extents
    // are on disk: they must not stay behind.
    let cs = 16u32 << 10;
    let r = Rig::new(RigConfig::new(cs, 1200));
    let map = lunix_testkit::FlatMap::new(
        r.codec.clone(),
        Arc::new(r.store.clone()),
        1200,
        lunix_format::IdLen::ThirtyTwo,
    )
    .with_dirty_leaf_cap(1);
    // Chunks 0 and 600 sit in two different leaves of 512 chunks.
    r.write_chunk(0, &random_bytes(1, cs as usize));
    r.write_chunk(600, &random_bytes(2, cs as usize));
    let cut = r.cut(r.last_seq());
    let index = r.index();
    let mut deps = r.deps(&index);
    deps.map = &map;
    let err = lunix_destage::destage_cut(&cut, &deps).expect_err("map full");
    assert_eq!(err.kind(), ErrorKind::MapFull);
    assert_eq!(err.class, ErrorClass::CutTooLarge);
    assert!(!err.partial.segments.is_empty() && err.partial.freed_bytes > 0);
    assert!(
        r.store.list().unwrap().is_empty(),
        "the cut's segments are gone"
    );
    assert!(map.snapshot().is_empty());
}

#[test]
fn a_map_with_dirty_leaves_asks_for_a_seal_not_a_smaller_cut() {
    let cs = 16u32 << 10;
    let r = Rig::new(RigConfig::new(cs, 16));
    let map = lunix_testkit::FlatMap::new(
        r.codec.clone(),
        Arc::new(r.store.clone()),
        16,
        lunix_format::IdLen::ThirtyTwo,
    )
    .with_dirty_leaf_cap(1);
    r.write_chunk(0, &random_bytes(1, cs as usize));
    let index = r.index();
    let mut deps = r.deps(&index);
    deps.map = &map;
    let cut = r.cut(r.last_seq());
    lunix_destage::destage_cut(&cut, &deps).expect("first cut");
    r.write_chunk(1, &random_bytes(2, cs as usize));
    let cut = r.cut(r.last_seq());
    let err = lunix_destage::destage_cut(&cut, &deps).expect_err("cap reached");
    assert_eq!(
        (err.kind(), err.class),
        (ErrorKind::MapFull, ErrorClass::AfterMapSeal)
    );
    assert!(err.class.is_retryable());
}

#[test]
fn every_map_entry_is_readable_the_moment_the_cut_returns() {
    // A partial overwrite stores a new extent in the last segment of the cut;
    // the untouched blocks of that chunk are served by the map alone.
    for cs in SIZES {
        let r = rig(cs);
        let base = random_bytes(5, cs as usize);
        r.write_chunk(1, &base);
        r.run_cut(r.last_seq());
        r.write(cs as usize, &random_bytes(6, BLOCK));
        let cut = r.cut(r.last_seq());
        let out = r.destage(&cut).expect("destage");
        assert_eq!(out.stats.cold_rmw, 1);
        // Before the coordinator does anything else: read through the map.
        let got = r.read_chunk(1);
        assert_eq!(&got[BLOCK..], &base[BLOCK..], "untouched blocks of {cs}");
        for e in r.snapshot().values() {
            let mut raw = Vec::new();
            r.store
                .read_extent(&lunix_iface::map::entry_hint(e), &mut raw)
                .expect("sealed and readable");
        }
        r.finish(&cut, out);
    }
}

#[test]
fn read_only_and_cancelled_and_malformed_cuts_are_refused() {
    let cs = 16u32 << 10;
    let r = Rig::new(RigConfig::new(cs, 4));
    r.write_chunk(0, &random_bytes(1, cs as usize));
    let cut = r.cut(r.last_seq());

    let flag = AtomicBool::new(true);
    let index = r.index();
    let err = destage_cut_with_cancel(&cut, &r.deps(&index), Some(&flag)).expect_err("cancelled");
    assert_eq!(err.kind(), ErrorKind::Closed);
    assert!(r.snapshot().is_empty());

    let mut bad = cut.clone();
    bad.chunks[0].chunk = 99;
    let err = r.destage(&bad).expect_err("beyond the map");
    assert_eq!(err.kind(), ErrorKind::Invalid);

    let mut bad = cut.clone();
    bad.chunks.push(bad.chunks[0].clone());
    assert_eq!(
        r.destage(&bad).expect_err("unsorted").kind(),
        ErrorKind::Invalid
    );

    r.space.fatal_io("test");
    assert!(r.space.is_read_only());
    assert_eq!(
        r.destage(&cut).expect_err("read-only").kind(),
        ErrorKind::ReadOnly
    );
    assert!(r.snapshot().is_empty());
}

#[test]
fn an_empty_cut_does_nothing() {
    let r = rig(16 << 10);
    let out = r.destage(&r.cut(WriteSeq(0))).expect("empty");
    assert!(out.segments.is_empty());
    assert_eq!(out.stats.chunks_total, 0);
}

#[test]
fn dedup_never_targets_a_condemned_segment() {
    for cs in SIZES {
        let r = rig(cs);
        let data = text_bytes(7, cs as usize);
        r.write_chunk(0, &data);
        let first = r.run_cut(r.last_seq());
        let old = first.segments[0].seg;
        r.index().mark_condemned(old).unwrap();

        r.write_chunk(1, &data);
        let out = r.run_cut(r.last_seq());
        assert_eq!(out.stats.dedup_hits, 0);
        assert_eq!(out.stats.unique_stored, 1);
        assert_eq!(out.segments.len(), 1);
        assert_ne!(out.segments[0].seg, old, "the new copy lives elsewhere");
        let snap = r.snapshot();
        assert_ne!(snap[&1].seg, old);
        r.assert_equals(&r.model_at(r.last_seq()));
    }
}

#[test]
fn identical_data_gets_different_ids_under_different_keys() {
    let cs = 16u32 << 10;
    let data = text_bytes(3, cs as usize);
    let mut ids = Vec::new();
    for codec in [
        PlainCodec::with_key(*b"key-number-one!!"),
        PlainCodec::with_key(*b"key-number-two!!"),
        PlainCodec::unkeyed(),
    ] {
        let mut c = RigConfig::new(cs, 4);
        c.codec = Arc::new(codec);
        let r = Rig::new(c);
        r.write_chunk(0, &data);
        r.run_cut(r.last_seq());
        let e = r.snapshot()[&0];
        r.assert_equals(&r.model_at(r.last_seq()));
        ids.push(e.prefix);
        if ExtentCodec::mode(&codec) == CryptoMode::Plain {
            assert_eq!(
                r.id_of(&data).0,
                *blake3::hash(&data).as_bytes(),
                "plain ids are unkeyed BLAKE3"
            );
        }
    }
    assert_ne!(
        ids[0], ids[1],
        "two keys, two id spaces: no dedup between volumes"
    );
    assert_ne!(ids[0], ids[2]);
}

#[test]
fn the_layout_does_not_depend_on_the_worker_count() {
    let cs = 16u32 << 10;
    let mut snaps = Vec::new();
    for workers in [1usize, 2, 3, 8] {
        let mut c = RigConfig::new(cs, 40);
        c.workers = workers;
        c.segment_target = 5 * u64::from(cs);
        let r = Rig::new(c);
        // Repeats at scattered places, so a dup is often reached before its producer.
        for ci in 0..40u64 {
            let kind = [3u64, 7, 3, 11, 7, 3, 99, 5][ci as usize % 8];
            r.write_chunk(
                ci,
                &text_bytes(kind * 1000 + (ci % 5 == 0) as u64, cs as usize),
            );
        }
        r.run_cut(r.last_seq());
        r.assert_equals(&r.model_at(r.last_seq()));
        snaps.push(r.snapshot());
    }
    assert!(
        snaps.windows(2).all(|w| w[0] == w[1]),
        "same map for every worker count"
    );
}

#[test]
fn accounting_adds_up() {
    let cs = 64u32 << 10;
    let r = rig(cs);
    r.write_chunk(0, &text_bytes(1, cs as usize));
    r.write_chunk(1, &text_bytes(1, cs as usize));
    r.write_chunk(2, &random_bytes(2, cs as usize));
    r.write_chunk(3, &vec![0; cs as usize]);
    r.write(4 * cs as usize, &random_bytes(3, 2 * BLOCK));
    let out = r.run_cut(r.last_seq());
    let s = out.stats;
    assert_eq!(s.chunks_total, 5);
    assert_eq!(s.bytes_in, 4 * u64::from(cs) + 2 * BLOCK as u64);
    assert_eq!(s.zero_holes, 1);
    assert_eq!(s.unique_stored, 3);
    assert_eq!(s.dedup_in_cut, 1);
    assert_eq!(s.stored_ulen(), 3 * u64::from(cs));
    assert!(s.stored_clen() < s.stored_ulen());
    assert!(s.hash_ns > 0 && s.compress_ns > 0 && s.seal_ns > 0 && s.wall_ns > 0);
    assert_eq!(out.map.sets, 5, "four sets and one hole");
    let mut e = lunix_iface::stats::EngineStats::default();
    s.add_to_engine(&mut e);
    assert_eq!((e.chunks_total, e.chunks_zero, e.dedup_hits), (5, 1, 1));
}

#[test]
fn backpressure_degrades_to_lz4_then_raw_one_level_per_cut() {
    let cs = 16u32 << 10;
    for (pressure, lz4_bytes, raw_bytes) in [
        (Pressure::Lz4, None, None),
        (Pressure::Raw, None, None),
        (Pressure::None, Some(u64::from(cs) * 5), None),
        (Pressure::None, Some(1), Some(u64::from(cs) * 5)),
    ] {
        let mut r = Rig::new(RigConfig::new(cs, 16));
        r.config = DestageConfig {
            pressure,
            lz4_cut_bytes: lz4_bytes,
            raw_cut_bytes: raw_bytes,
            ..r.config
        };
        for ci in 0..12u64 {
            r.write_chunk(ci, &text_bytes(500 + ci, cs as usize));
        }
        let out = r.run_cut(r.last_seq());
        let s = out.stats;
        if pressure == Pressure::Raw || raw_bytes.is_some() {
            assert_eq!((s.degraded_raw, s.degraded_lz4, s.zstd_calls), (12, 0, 0));
            assert_eq!(s.chunks_by_codec[Codec::Raw.id() as usize], 12);
        } else {
            assert_eq!((s.degraded_raw, s.degraded_lz4, s.zstd_calls), (0, 12, 0));
            assert_eq!(s.chunks_by_codec[Codec::Lz4.id() as usize], 12);
        }
        r.assert_equals(&r.model_at(r.last_seq()));
    }
}

#[test]
fn the_layout_under_pressure_does_not_depend_on_which_job_claims_a_duplicate() {
    let cs = 16u32 << 10;
    let mut snaps = Vec::new();
    for workers in [1usize, 2, 4, 8] {
        let mut c = RigConfig::new(cs, 40);
        c.workers = workers;
        let mut r = Rig::new(c);
        r.config = DestageConfig {
            workers,
            batch_chunks: 1,
            pressure: Pressure::Lz4,
            ..r.config
        };
        for ci in 0..40u64 {
            let kind = [1u64, 2, 1, 3, 2, 1, 9, 4][ci as usize % 8];
            r.write_chunk(ci, &text_bytes(kind, cs as usize));
        }
        r.run_cut(r.last_seq());
        r.assert_equals(&r.model_at(r.last_seq()));
        snaps.push(r.snapshot());
    }
    assert!(snaps.windows(2).all(|w| w[0] == w[1]));
}

#[test]
fn a_lean_volume_reads_its_base_as_a_scan() {
    let cs = 16u32 << 10;
    let mut c = RigConfig::new(cs, 8);
    c.id_len = lunix_format::IdLen::Zero;
    let r = Rig::new(c);
    r.write_chunk(0, &random_bytes(1, cs as usize));
    r.run_cut(r.last_seq());
    r.write(BLOCK, &random_bytes(2, BLOCK));
    let out = r.run_cut(r.last_seq());
    assert_eq!(out.stats.cold_rmw, 1);
    r.assert_equals(&r.model_at(r.last_seq()));
}

#[test]
fn a_piece_above_the_cut_is_refused() {
    let cs = 16u32 << 10;
    let r = Rig::new(RigConfig::new(cs, 4));
    r.write_chunk(0, &random_bytes(1, cs as usize));
    let mut cut = r.cut(r.last_seq());
    cut.cut_seq = WriteSeq(0);
    let err = r.destage(&cut).expect_err("overlay bug");
    assert_eq!(
        (err.kind(), err.class),
        (ErrorKind::Invalid, ErrorClass::Permanent)
    );
}

#[test]
fn a_destage_error_leaves_overlay_and_index_alone() {
    let cs = 16u32 << 10;
    let r = Rig::new(RigConfig::new(cs, 4));
    r.write_chunk(0, &random_bytes(1, cs as usize));
    r.store.faults().arm("segment.append", 1, Fault::Io);
    let cut = r.cut(r.last_seq());
    let err = r.destage(&cut).expect_err("io");
    assert_eq!(err.kind(), ErrorKind::Io);
    assert_eq!(err.class, ErrorClass::Transient);
    assert_eq!(r.index().dirty_entries(), 0);
    assert_eq!(r.overlay.len_blocks(), (cs as u64) / 4096);
}

#[cfg(feature = "crashpoints")]
#[test]
fn the_crash_points_are_registered() {
    let names = lunix_iface::crashpoint::registered();
    for n in ["destage.after_assemble", "destage.after_segment_append"] {
        assert!(names.contains(&n), "{n} missing");
    }
    lunix_iface::crashpoint::check_registry().expect("names are unique and well formed");
}

#[test]
fn a_host_eio_makes_the_volume_read_only_and_names_the_cause() {
    let cs = 16u32 << 10;
    let r = Rig::new(RigConfig::new(cs, 4));
    r.write_chunk(0, &random_bytes(1, cs as usize));
    r.run_cut(r.last_seq());
    // A partial overwrite needs the base; the host fails the read with EIO.
    r.write(0, &random_bytes(2, BLOCK));
    let t = TamperStore::new(r.store.clone());
    t.eio.store(true, std::sync::atomic::Ordering::SeqCst);
    let cut = r.cut(r.last_seq());
    let err = r.destage_via(&cut, &t).expect_err("eio");
    assert_eq!(err.kind(), ErrorKind::Io, "the cause, not a bare Closed");
    assert_eq!((err.class, err.chunk), (ErrorClass::Stop, Some(0)));
    assert!(r.space.is_read_only(), "fatal_io was called");
}

#[test]
fn a_worker_error_is_reported_as_itself_with_many_workers() {
    let cs = 16u32 << 10;
    let mut c = RigConfig::new(cs, 32);
    c.workers = 4;
    let mut r = Rig::new(c);
    r.config.batch_chunks = 1;
    for ci in 0..24u64 {
        r.write_chunk(ci, &text_bytes(ci % 3, cs as usize));
    }
    r.run_cut(r.last_seq());
    let before = r.snapshot();
    // Partial overwrites everywhere, then a failing store.
    for ci in 0..24u64 {
        r.write(ci as usize * cs as usize, &random_bytes(ci, BLOCK));
    }
    let t = TamperStore::new(r.store.clone());
    t.eio.store(true, std::sync::atomic::Ordering::SeqCst);
    let cut = r.cut(r.last_seq());
    let err = r.destage_via(&cut, &t).expect_err("eio");
    assert_eq!(err.kind(), ErrorKind::Io);
    assert_eq!(r.snapshot(), before, "map untouched");
}
