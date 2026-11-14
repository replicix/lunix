//! Clean volumes built by the real crates: exit 0 and the known ratios.

mod common;

use common::*;
use lunix_format::CryptoMode;

fn assert_clean(b: &Built, deep: bool) {
    let r = b.fsck(&if deep { b.deep() } else { b.opts() });
    assert!(
        r.exit_code() == 0,
        "mode {} chunk {} deep {deep}:\n{}",
        mode_name(b.spec.mode),
        b.spec.chunk_size,
        r.render_text()
    );
    let ratios = r.ratios.expect("ratios of a clean volume");
    assert_eq!(ratios.mapped_chunks, b.known.mapped, "mapped");
    assert_eq!(ratios.hole_chunks, b.known.holes, "holes");
    assert_eq!(ratios.unique_chunks, b.known.unique, "unique");
    assert_eq!(ratios.unique_ulen, b.known.unique_ulen, "ulen");
    assert_eq!(ratios.unique_clen, b.known.unique_clen, "clen");
    assert!((ratios.dedup_ratio - b.known.dedup_ratio()).abs() < 1e-9);
    assert!((ratios.compression_ratio - b.known.compression_ratio()).abs() < 1e-9);
    assert_eq!(r.tip, Some(b.known.tip));
    assert!(ratios.dedup_ratio > 1.0, "the builder dedups");
    assert!(ratios.compression_ratio > 1.0, "the builder compresses");
    assert!(ratios.bytes.segments > 0 && ratios.bytes.map_packs > 0 && ratios.bytes.journal > 0);
}

#[test]
fn clean_volumes_of_all_modes_and_both_chunk_sizes_exit_0() {
    for mode in ALL_MODES {
        for chunk in [16 * 1024, 64 * 1024] {
            let mut spec = Spec::new(mode, chunk);
            if chunk == 64 * 1024 {
                spec.segment_target = 4 << 20;
                spec.blobs = 400;
                spec.per_commit = 400;
            }
            let b = build(&spec);
            assert!(b.known.mapped >= 1000, "mapped {}", b.known.mapped);
            assert!(b.known.segments.len() >= 2, "several segments");
            assert_clean(&b, false);
            assert_clean(&b, true);
        }
    }
}

#[test]
fn a_plain_volume_prints_the_notice_and_the_others_do_not() {
    for mode in ALL_MODES {
        let b = build(&Spec::new(mode, 16 * 1024));
        let r = b.fsck(&b.opts());
        assert_eq!(
            r.has(lunix_fsck::code::PLAIN_NOTICE),
            mode == CryptoMode::Plain
        );
        assert_eq!(r.exit_code(), 0);
    }
}

#[test]
fn json_output_carries_the_exit_code_and_the_codes() {
    let b = build(&Spec::new(CryptoMode::Integrity, 16 * 1024));
    b.flip(&b.seg_rel(b.known.segments[0]), 100);
    let r = b.fsck(&b.opts());
    let v: serde_json::Value = serde_json::from_str(&r.to_json()).expect("json");
    assert_eq!(v["exit"], 2);
    assert!(v["findings"]
        .as_array()
        .expect("findings")
        .iter()
        .any(|f| f["code"] == "seg_hash"));
    assert_eq!(v["deep"], false);
}
