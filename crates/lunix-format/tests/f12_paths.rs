//! Freeze item 12: bucket and local paths.

mod common;

use lunix_format::paths::{self, local};
use lunix_format::*;

fn id(epoch: u64, counter: u64) -> ObjId {
    ObjId::new(epoch, counter).expect("legal id")
}

#[test]
fn every_path_for_fixed_ids() {
    let seg = id(0, 0x2a);
    let pack = id(0, 0x2b);
    let run = id(1, 0x2c);
    let seq = CommitSeq(7);
    let uuid = VolumeUuid(common::VOLUME_B_UUID);
    let child = VolumeUuid(common::VOLUME_A_UUID);
    let fam = FamilyId(common::VOLUME_B_FAMILY);

    // (label, bucket-relative name, local path)
    let rows: Vec<(&str, String, Option<String>)> = vec![
        ("volume", paths::volume(), Some(local::volume())),
        ("lease", paths::lease(), None),
        ("head", paths::head(), None),
        ("commit", paths::commit(seq), Some(local::commit(seq))),
        (
            "segment",
            paths::segment(seg),
            Some(local::segment_sealed(seg)),
        ),
        (
            "segment-open",
            paths::segment(seg),
            Some(local::segment_open(seg)),
        ),
        (
            "map-pack",
            paths::map_pack(pack),
            Some(local::map_pack_sealed(pack)),
        ),
        (
            "map-pack-open",
            paths::map_pack(pack),
            Some(local::map_pack_open(pack)),
        ),
        (
            "index-run",
            paths::index_run(run),
            Some(local::index_run(run)),
        ),
        ("heat", paths::heat(seq), Some(local::heat(seq))),
        (
            "dict",
            paths::dict(0x1234_abcd),
            Some(local::dict(0x1234_abcd)),
        ),
        (
            "snapshot",
            paths::snapshot("nightly-2026.11").expect("name"),
            Some(local::snapshot("nightly-2026.11").expect("name")),
        ),
        (
            "reader",
            paths::reader("host-1.example", 0xfeed).expect("host"),
            Some(local::reader("host-1.example", 0xfeed).expect("host")),
        ),
        ("hold", paths::hold(&child), Some(local::hold(&child))),
        (
            "gc-pass",
            paths::gc_pass(3).expect("pass"),
            Some(local::gc_pass(3).expect("pass")),
        ),
        ("gc-space", paths::gc_space(seq), Some(local::gc_space(seq))),
        (
            "sync-progress",
            paths::sync_progress("s3-main").expect("backend"),
            None,
        ),
        ("keyring", paths::keyring(&fam), None),
    ];

    let mut t = String::new();
    t.push_str("# Bucket-relative names (D9) and local paths for fixed ids.\n");
    t.push_str(&format!(
        "# seg={seg} pack={pack} run={run} commit_seq={seq} volume={uuid} child={child} family={fam}\n"
    ));
    for (label, bucket, loc) in &rows {
        match loc {
            Some(l) => {
                t.push_str(&format!("{label:<14} bucket={bucket:<40} local={l}\n"));
                // The mapping back to the bucket-relative name (D9).
                let mapped = paths::local_to_bucket(l).expect("maps");
                if l.ends_with(".open") {
                    assert_eq!(mapped, None, "{l}");
                } else {
                    assert_eq!(mapped.as_deref(), Some(bucket.as_str()), "{l}");
                }
            }
            None => t.push_str(&format!("{label:<14} bucket={bucket}\n")),
        }
    }
    t.push_str(&format!(
        "{:<14} dir={}\n",
        "volume-dir",
        paths::local_volume_dir("/var/lib/lunix/", &uuid)
    ));
    t.push_str(&format!(
        "{:<14} prefix={}\n",
        "bucket-prefix",
        paths::bucket_volume_prefix("pfx/", &uuid)
    ));
    t.push_str(&format!(
        "{:<14} key={}\n",
        "keyring-key",
        paths::bucket_keyring_key("pfx", &fam)
    ));
    t.push_str(&format!(
        "{:<14} path={}\n",
        "keyring-local",
        paths::local_keyring_path("/var/lib/lunix", &fam)
    ));
    // Local-only paths and the maps-to-none rule.
    t.push_str("# local-only paths (map to no bucket name)\n");
    let only = [
        local::volume_json(),
        local::lock(),
        local::journal_file(0, 1),
        local::journal_file(0x1f, 0x0abc),
        local::cache_dir(),
        local::state_checkpoint(),
        local::state_device(),
        local::state_pins(),
        local::state_gc_condemned(),
        local::integrity_key(),
        local::index_manifest(),
    ];
    for p in &only {
        assert_eq!(paths::local_to_bucket(p).expect("local only"), None, "{p}");
        t.push_str(&format!("local-only     {p}\n"));
    }
    common::golden_text("12-paths.txt", &t);
}

#[test]
fn exact_shapes() {
    assert_eq!(paths::commit(CommitSeq(0)), "commits/0000000000000000");
    assert_eq!(paths::segment(id(0, 1)), "segments/0000000000000001");
    assert_eq!(paths::map_pack(id(0, 2)), "map/0000000000000002");
    assert_eq!(paths::index_run(id(1, 3)), "index/0000010000000003");
    assert_eq!(paths::dict(0xab), "dicts/000000ab");
    assert_eq!(paths::gc_space(CommitSeq(255)), "gc/space/00000000000000ff");
    assert_eq!(
        local::commit(CommitSeq(9)),
        "state/commits/0000000000000009"
    );
    assert_eq!(
        local::segment_sealed(id(0, 1)),
        "segments/0000000000000001.seg"
    );
    assert_eq!(
        local::segment_open(id(0, 1)),
        "segments/0000000000000001.open"
    );
    assert_eq!(local::journal_file(0, 1), "journal/q00-00000001.j");
    assert_eq!(
        local::journal_file(0xff, 0xdead_beef),
        "journal/qff-deadbeef.j"
    );
    let u = VolumeUuid(common::VOLUME_B_UUID);
    assert_eq!(
        paths::bucket_volume_prefix("p", &u),
        "p/lunix/v1/b0b1b2b3-b4b5-46b7-88b9-babbbcbdbebf/"
    );
    assert_eq!(
        paths::bucket_volume_prefix("", &u),
        "lunix/v1/b0b1b2b3-b4b5-46b7-88b9-babbbcbdbebf/"
    );
    assert_eq!(
        paths::bucket_keyring_key("p", &FamilyId(common::VOLUME_B_FAMILY)),
        "p/lunix/v1/families/505152535455565758595a5b5c5d5e5f/keyring"
    );
}

#[test]
fn name_rules() {
    assert!(paths::valid_backend_id("s3-main_1.x"));
    assert!(!paths::valid_backend_id(""));
    assert!(!paths::valid_backend_id("UPPER"));
    assert!(!paths::valid_backend_id("a/b"));
    assert!(!paths::valid_backend_id(&"a".repeat(65)));
    assert!(paths::valid_backend_id(&"a".repeat(64)));
    assert!(paths::valid_snapshot_name("Snap-1.x_y"));
    assert!(!paths::valid_snapshot_name(".hidden"));
    assert!(!paths::valid_snapshot_name(""));
    assert!(!paths::valid_snapshot_name("a b"));
    assert!(!paths::valid_snapshot_name("../x"));
    assert!(!paths::valid_snapshot_name(&"a".repeat(129)));
    assert!(paths::valid_snapshot_name(&"a".repeat(128)));
    assert!(paths::valid_host("host-1.example"));
    assert!(!paths::valid_host("Host"));
    assert!(!paths::valid_host("a_b"));
    assert!(paths::snapshot("../etc").is_err());
    assert!(paths::reader("BAD", 1).is_err());
    assert!(paths::sync_progress("A").is_err());
    assert!(paths::gc_pass(0).is_err());
    assert!(local::gc_pass(0).is_err());
}

#[test]
fn local_to_bucket_rejections() {
    for bad in [
        "segments/zz.seg",
        "segments/0000000000000001",
        "segments/0000000000000001.SEG",
        "map/1",
        "state/commits/xyz",
        "index/0000000000000001.x",
        "heat/g000000000000000",
        "holds/not-a-uuid",
        "readers/nodash",
        "readers/host-short",
        "gc/0000000000000000",
        "dicts/123",
        "dicts/1234567G",
        "journal/q00-1.j",
        "unknown/file",
        "",
        "state/unknown",
        "families/short/keyring",
        "../escape",
    ] {
        assert!(paths::local_to_bucket(bad).is_err(), "{bad}");
    }
    assert_eq!(paths::local_to_bucket("commit.tmp").expect("tmp"), None);
    assert_eq!(
        paths::local_to_bucket("families/505152535455565758595a5b5c5d5e5f/keyring")
            .expect("keyring")
            .as_deref(),
        Some("families/505152535455565758595a5b5c5d5e5f/keyring")
    );
}
