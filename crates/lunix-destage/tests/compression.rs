//! The compression policy (ADR-12): the probe rule, `auto`'s decisions, the
//! ratio on text, and the knobs.

mod common;

use common::*;
use lunix_destage::DestageConfig;
use lunix_format::volume::CodecPolicy;
use lunix_format::Codec;

const CS: u32 = 64 << 10;

fn rig() -> Rig {
    Rig::new(RigConfig::new(CS, 8))
}

fn codec_of(r: &Rig, ci: u64) -> (Codec, u32, u32) {
    use lunix_iface::crypto::{OpenExpect, OpenOutput};
    use lunix_iface::map::entry_hint;
    use lunix_iface::store::SegmentStore;
    let e = r.snapshot()[&ci];
    let mut raw = Vec::new();
    r.store.read_extent(&entry_hint(&e), &mut raw).unwrap();
    let expect = OpenExpect {
        chunk_size: CS,
        chunk_id: None,
        id_prefix: Some(&e.prefix),
        verify_id: false,
        output: OpenOutput::Stored,
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
    (meta.desc.codec, meta.desc.ulen, e.clen)
}

#[test]
fn random_data_is_stored_raw_and_zstd_is_never_called() {
    let r = rig();
    for ci in 0..4u64 {
        r.write_chunk(ci, &random_bytes(ci + 1, CS as usize));
    }
    let out = r.run_cut(r.last_seq());
    assert_eq!(out.stats.zstd_calls, 0, "the counter");
    assert_eq!(out.stats.probe_calls, 4);
    assert_eq!(out.stats.probe_raw, 4);
    for ci in 0..4 {
        assert_eq!(codec_of(&r, ci), (Codec::Raw, CS, CS), "clen = ulen");
    }
    r.assert_equals(&r.model_at(r.last_seq()));
}

#[test]
fn a_constant_chunk_is_stored_compressed() {
    let r = rig();
    r.write_chunk(0, &vec![0x41; CS as usize]);
    let out = r.run_cut(r.last_seq());
    assert_eq!(out.stats.zstd_calls, 1);
    let (codec, ulen, clen) = codec_of(&r, 0);
    assert_eq!((codec, ulen), (Codec::Zstd, CS));
    assert!(clen < 100, "clen {clen}");
}

#[test]
fn the_probe_rule_decides_a_random_head_with_a_zero_tail_and_the_reverse() {
    // The probe sees only the first 4 KiB. Random head: under 6 % saved, so
    // raw, although 15/16 of the chunk is zeros. Zero head: the probe saves
    // ~100 %, so zstd runs and wins on the 4 KiB of random... or not: the tail
    // below is random, so the chunk compresses by exactly its zero head.
    let r = rig();
    let mut head_random = vec![0u8; CS as usize];
    head_random[..4096].copy_from_slice(&random_bytes(1, 4096));
    let mut tail_random = vec![0u8; CS as usize];
    tail_random[CS as usize - 4096..].copy_from_slice(&random_bytes(2, 4096));
    r.write_chunk(0, &head_random);
    r.write_chunk(1, &tail_random);
    let out = r.run_cut(r.last_seq());
    assert_eq!(out.stats.probe_raw, 1, "only the random head is rejected");
    assert_eq!(out.stats.zstd_calls, 1);
    assert_eq!(codec_of(&r, 0).0, Codec::Raw, "decided raw by the probe");
    assert_eq!(
        codec_of(&r, 1).0,
        Codec::Zstd,
        "the zero head passes the probe"
    );
    r.assert_equals(&r.model_at(r.last_seq()));
}

#[test]
fn auto_stores_text_in_at_most_six_tenths() {
    let r = Rig::new(RigConfig::new(CS, 64));
    let mut total = 0u64;
    for ci in 0..64u64 {
        r.write_chunk(ci, &text_bytes(1000 + ci, CS as usize));
        total += u64::from(CS);
    }
    let out = r.run_cut(r.last_seq());
    let stored = out.stats.stored_clen();
    assert!(
        stored * 10 <= total * 6,
        "stored {stored} of {total}: ratio {:.3}",
        stored as f64 / total as f64
    );
    assert_eq!(out.stats.chunks_by_codec[Codec::Zstd.id() as usize], 64);
    r.assert_equals(&r.model_at(r.last_seq()));
}

#[test]
fn the_probe_knobs_move_the_decision() {
    // 0 %: every chunk goes to zstd, which then fails to shrink random data, so
    // the chunk is stored raw anyway.
    let mut r = rig();
    r.config = DestageConfig {
        probe_min_saving_percent: 0,
        ..r.config
    };
    r.write_chunk(0, &random_bytes(1, CS as usize));
    let out = r.run_cut(r.last_seq());
    assert_eq!((out.stats.zstd_calls, out.stats.probe_raw), (1, 0));
    assert_eq!(codec_of(&r, 0).0, Codec::Raw, "no gain, raw");

    // 100 %: only a probe that compresses to nothing passes.
    let mut r = rig();
    r.config = DestageConfig {
        probe_min_saving_percent: 100,
        ..r.config
    };
    r.write_chunk(0, &text_bytes(1, CS as usize));
    let out = r.run_cut(r.last_seq());
    assert_eq!(out.stats.probe_raw, 1);
    assert_eq!(out.stats.zstd_calls, 0);
}

#[test]
fn explicit_policies_ignore_the_probe() {
    for (policy, level, want) in [
        (CodecPolicy::Lz4, 0, Codec::Lz4),
        (CodecPolicy::Zstd, 7, Codec::Zstd),
        (CodecPolicy::None, 0, Codec::Raw),
    ] {
        let mut c = RigConfig::new(CS, 4);
        c.policy = policy;
        c.level = level;
        let r = Rig::new(c);
        r.write_chunk(0, &text_bytes(1, CS as usize));
        let out = r.run_cut(r.last_seq());
        assert_eq!(out.stats.probe_calls, 0, "{policy:?}");
        assert_eq!(codec_of(&r, 0).0, want, "{policy:?}");
        r.assert_equals(&r.model_at(r.last_seq()));
    }
}

#[test]
fn degradation_is_reported_per_level_and_changes_the_codec() {
    let mut r = rig();
    r.config = DestageConfig {
        workers: 1,
        batch_chunks: 1,
        // Everything but the last job is above both thresholds.
        lz4_depth: Some(0),
        raw_depth: Some(100),
        ..r.config
    };
    for ci in 0..4u64 {
        r.write_chunk(ci, &text_bytes(ci, CS as usize));
    }
    let out = r.run_cut(r.last_seq());
    assert_eq!(out.stats.degraded_lz4, 3);
    assert_eq!(out.stats.degraded_raw, 0);
    assert_eq!(out.stats.zstd_calls, 1);
    assert_eq!(codec_of(&r, 0).0, Codec::Lz4);
    assert_eq!(codec_of(&r, 3).0, Codec::Zstd);
}
