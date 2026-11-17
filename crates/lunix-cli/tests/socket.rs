//! Every socket subcommand against the fake control server: request fields,
//! rendered output, error mapping and exit codes.

mod common;

use std::os::unix::fs::PermissionsExt;

use common::{attached, status_body, Env};
use lunix_format::local::{FreeFloor, OnFull, SpaceSettings};
use lunix_iface::ctl::{
    Attach, Compress, Credential, ErrorCode, GcRequest, JournalDamagePolicy, NameParam, Request,
    Resize, Response, Secret, SpaceBody, SpaceFigures, SpaceRequest, VolumeCreate, VolumeCreated,
    VolumeMode, VolumeSummary,
};
use lunix_iface::gc::{GcPassSummary, GcState, GcStatusSnapshot};

fn created(env: &Env, mode: VolumeMode, key: Option<&std::path::Path>) {
    env.ok(
        "VolumeCreate",
        Response::VolumeCreated(VolumeCreated {
            name: "vol1".into(),
            uuid: "11111111-2222-3333-4444-555555555555".into(),
            dir: "/data/lunix/11111111-2222-3333-4444-555555555555".into(),
            mode,
            key_file: key.map(|k| k.display().to_string()),
        }),
    );
}

fn base_create() -> VolumeCreate {
    VolumeCreate {
        name: "vol1".into(),
        dir: "/data/lunix".into(),
        size_bytes: 200 << 30,
        chunk_size: 65536,
        mode: VolumeMode::Integrity,
        compress: Compress::Auto,
        id_len: 16,
        map_fanout: None,
        key_file: None,
        passphrase: None,
    }
}

#[test]
fn create_defaults_to_integrity_and_reports_the_key_file() {
    let env = Env::new();
    let key = env.dir.path().join("integrity.key");
    std::fs::write(&key, [7u8; 32]).unwrap();
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
    created(&env, VolumeMode::Integrity, Some(&key));
    let o = env.run(&[
        "volume",
        "create",
        "vol1",
        "--size",
        "200G",
        "--dir",
        "/data/lunix",
    ]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert_eq!(
        env.server.requests(),
        vec![Request::VolumeCreate(base_create())]
    );
    assert!(
        o.stdout.contains("created volume vol1 (integrity"),
        "{}",
        o.stdout
    );
    assert!(o.stdout.contains(&key.display().to_string()));
    assert_eq!(o.stderr, "", "a 0600 key file raises no warning");
}

#[test]
fn create_warns_about_a_world_readable_key_file() {
    let env = Env::new();
    let key = env.dir.path().join("integrity.key");
    std::fs::write(&key, [7u8; 32]).unwrap();
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o644)).unwrap();
    created(&env, VolumeMode::Integrity, Some(&key));
    let o = env.run(&[
        "volume",
        "create",
        "vol1",
        "--size",
        "200G",
        "--dir",
        "/data/lunix",
    ]);
    assert_eq!(o.code, 0);
    assert!(
        o.stderr.contains("mode 0644") && o.stderr.contains("0600"),
        "{}",
        o.stderr
    );
}

#[test]
fn create_makes_relative_paths_absolute() {
    let env = Env::new();
    created(&env, VolumeMode::Integrity, None);
    let cwd = env.dir.path().canonicalize().unwrap();
    let o = common::run_with(
        &env.socket(),
        &[
            "volume",
            "create",
            "vol1",
            "--size",
            "1G",
            "--dir",
            "rel/dir",
            "--key-file",
            "k.key",
        ],
        None,
        &cwd,
    );
    assert_eq!(o.code, 0, "{}", o.stderr);
    let Request::VolumeCreate(c) = &env.server.requests()[0] else {
        panic!("not a create")
    };
    assert_eq!(c.dir, format!("{}/rel/dir", cwd.display()));
    assert_eq!(
        c.key_file.as_deref(),
        Some(format!("{}/k.key", cwd.display()).as_str())
    );
}

#[test]
fn create_mode_flags_select_the_mode() {
    for (flag, mode) in [
        ("--encrypt", VolumeMode::Encrypted),
        ("--integrity", VolumeMode::Integrity),
        ("--plain", VolumeMode::Plain),
    ] {
        let env = Env::new();
        created(&env, mode, None);
        let o = env.run(&[
            "volume", "create", "vol1", "--size", "1G", "--dir", "/d", flag,
        ]);
        assert_eq!(o.code, 0, "{flag}: {}", o.stderr);
        let Request::VolumeCreate(c) = &env.server.requests()[0] else {
            panic!("not a create")
        };
        assert_eq!(c.mode, mode, "{flag}");
    }
}

#[test]
fn mode_flags_together_are_a_usage_error() {
    for flags in [
        &["--encrypt", "--integrity"][..],
        &["--encrypt", "--plain"],
        &["--integrity", "--plain"],
        &["--encrypt", "--integrity", "--plain"],
    ] {
        let env = Env::new();
        let mut args = vec!["volume", "create", "vol1", "--size", "1G", "--dir", "/d"];
        args.extend_from_slice(flags);
        let o = env.run(&args);
        assert_eq!(o.code, 2, "{flags:?}: {}", o.stderr);
        assert!(env.server.requests().is_empty(), "{flags:?} sent a request");
    }
}

#[test]
fn plain_prints_the_warning_and_takes_no_key_material() {
    let env = Env::new();
    created(&env, VolumeMode::Plain, None);
    let o = env.run(&[
        "volume", "create", "vol1", "--size", "1G", "--dir", "/d", "--plain",
    ]);
    assert_eq!(o.code, 0);
    common::assert_snapshot("plain-create-warning.txt", &o.stderr);
    assert!(o.stderr.contains("corruption only"));
    for extra in [
        &["--key-file", "/k"][..],
        &["--passphrase"],
        &["--passphrase-fd", "0"],
    ] {
        let mut args = vec![
            "volume", "create", "vol2", "--size", "1G", "--dir", "/d", "--plain",
        ];
        args.extend_from_slice(extra);
        let o = env.run(&args);
        assert_eq!(o.code, 2, "{extra:?}");
    }
    assert_eq!(env.server.requests().len(), 1);
}

#[test]
fn the_default_mode_prints_no_plain_warning() {
    let env = Env::new();
    created(&env, VolumeMode::Integrity, None);
    let o = env.run(&["volume", "create", "vol1", "--size", "1G", "--dir", "/d"]);
    assert!(!o.stderr.contains("plain"));
}

#[test]
fn create_options_and_passphrase_from_stdin() {
    let env = Env::new();
    created(&env, VolumeMode::Encrypted, None);
    let o = env.run_stdin(
        &[
            "volume",
            "create",
            "vol1",
            "--size",
            "4096",
            "--dir",
            "/data/x",
            "--encrypt",
            "--chunk",
            "128K",
            "--compress",
            "zstd:5",
            "--id-len",
            "32",
            "--passphrase-fd",
            "0",
        ],
        b"correct horse\n",
    );
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert_eq!(
        env.server.requests(),
        vec![Request::VolumeCreate(VolumeCreate {
            name: "vol1".into(),
            dir: "/data/x".into(),
            size_bytes: 4096,
            chunk_size: 128 * 1024,
            mode: VolumeMode::Encrypted,
            compress: Compress::Zstd { level: 5 },
            id_len: 32,
            map_fanout: None,
            key_file: None,
            passphrase: Some(Secret::from_text("correct horse")),
        })]
    );
    assert!(!o.stdout.contains("correct horse") && !o.stderr.contains("correct horse"));
}

#[test]
fn bad_create_values_are_usage_errors_without_a_request() {
    let env = Env::new();
    let base = ["volume", "create", "vol1", "--dir", "/d"];
    for tail in [
        &["--size", "0"][..],
        &["--size", "1.5G"],
        &["--size", "-1"],
        &["--size", "99999999999T"],
        &["--size", "5000"],                  // not a multiple of 4096
        &["--size", "1G", "--chunk", "100K"], // not a power of two
        &["--size", "1G", "--chunk", "8K"],   // below 16K
        &["--size", "1G", "--id-len", "8"],
        &["--size", "1G", "--compress", "zstd"],
        &["--size", "1G", "--compress", "zstd:99"],
        &[], // no size
    ] {
        let mut args = base.to_vec();
        args.extend_from_slice(tail);
        let o = env.run(&args);
        assert_eq!(o.code, 2, "{tail:?}: {}", o.stderr);
    }
    let o = env.run(&[
        "volume", "create", "bad/name", "--size", "1G", "--dir", "/d",
    ]);
    assert_eq!(o.code, 2);
    assert!(env.server.requests().is_empty());
}

#[test]
fn list_renders_a_table_and_json() {
    let env = Env::new();
    env.ok(
        "VolumeList",
        Response::VolumeList {
            volumes: vec![
                VolumeSummary {
                    name: "alpha".into(),
                    uuid: "u1".into(),
                    dir: "/data/a".into(),
                    size_bytes: 200 << 30,
                    mode: VolumeMode::Integrity,
                    device: Some("/dev/lunix/alpha".into()),
                },
                VolumeSummary {
                    name: "b".into(),
                    uuid: "u2".into(),
                    dir: "/data/b".into(),
                    size_bytes: 0,
                    mode: VolumeMode::Encrypted,
                    device: None,
                },
            ],
        },
    );
    let o = env.run(&["volume", "list"]);
    assert_eq!(o.code, 0);
    common::assert_snapshot("volume-list.txt", &o.stdout);
    let j = env.run(&["volume", "list", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&j.stdout).unwrap();
    assert_eq!(v[0]["name"], "alpha");
    assert_eq!(v[1]["mode"], "encrypted");
    assert_eq!(
        env.server.requests(),
        vec![Request::VolumeList, Request::VolumeList]
    );
}

#[test]
fn attach_maps_every_option() {
    let env = Env::new();
    env.ok("Attach", Response::Attached(attached()));
    let o = env.run(&[
        "attach",
        "vol1",
        "--queues",
        "4",
        "--depth",
        "128",
        "--cpus",
        "12-15",
        "--max-io-buf",
        "1M",
        "--on-journal-damage",
        "refuse",
        "--allow-unverifiable-fork",
    ]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert!(o
        .stdout
        .contains("attached vol1 as /dev/lunix/vol1 (4 queues, depth 128)"));
    assert_eq!(
        env.server.requests(),
        vec![Request::Attach(Attach {
            name: "vol1".into(),
            queues: Some(4),
            depth: Some(128),
            cpus: Some("12-15".into()),
            max_io_buf_bytes: Some(1 << 20),
            credential: None,
            on_journal_damage: Some(JournalDamagePolicy::Refuse),
            allow_unverifiable_fork: true,
        })]
    );
}

#[test]
fn attach_with_no_options_sends_none() {
    let env = Env::new();
    env.ok("Attach", Response::Attached(attached()));
    assert_eq!(env.run(&["attach", "vol1"]).code, 0);
    assert_eq!(
        env.server.requests(),
        vec![Request::Attach(Attach {
            name: "vol1".into(),
            queues: None,
            depth: None,
            cpus: None,
            max_io_buf_bytes: None,
            credential: None,
            on_journal_damage: None,
            allow_unverifiable_fork: false,
        })]
    );
}

#[test]
fn attach_credentials() {
    let env = Env::new();
    env.ok("Attach", Response::Attached(attached()));
    let fp = "0123456789abcdef0123456789abcdef";
    let o = env.run_stdin(
        &[
            "attach",
            "vol1",
            "--passphrase-fd",
            "0",
            "--credential-mode",
            "integrity",
            "--fingerprint",
            fp,
        ],
        b"s3cret\n",
    );
    assert_eq!(o.code, 0, "{}", o.stderr);
    let o = env.run(&["attach", "vol1", "--key-file", "/keys/vol1.key"]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    let reqs = env.server.requests();
    let cred = |r: &Request| match r {
        Request::Attach(a) => a.credential.clone().unwrap(),
        _ => panic!("not an attach"),
    };
    assert_eq!(
        cred(&reqs[0]),
        Credential {
            passphrase: Some(Secret::from_text("s3cret")),
            key_file: None,
            fingerprint: Some(fp.into()),
            mode: Some(VolumeMode::Integrity),
        }
    );
    assert_eq!(
        cred(&reqs[1]),
        Credential {
            passphrase: None,
            key_file: Some("/keys/vol1.key".into()),
            fingerprint: None,
            mode: None,
        }
    );
    // A key file and a passphrase together, and a bad fingerprint, are usage errors.
    let o = env.run(&["attach", "vol1", "--key-file", "/k", "--passphrase-fd", "0"]);
    assert_eq!(o.code, 2);
    let o = env.run(&["attach", "vol1", "--key-file", "/k", "--fingerprint", "xyz"]);
    assert_eq!(o.code, 2);
    assert_eq!(env.server.requests().len(), 2);
}

#[test]
fn attach_notes_a_first_attach_and_a_recovered_device() {
    let env = Env::new();
    let mut a = attached();
    a.first_attach = true;
    a.recovered = true;
    env.ok("Attach", Response::Attached(a));
    let o = env.run(&["attach", "vol1"]);
    assert_eq!(o.code, 0);
    assert!(o.stdout.contains("recovered an existing device"));
    common::assert_snapshot("first-attach-note.txt", &o.stderr);
}

#[test]
fn attach_range_errors_are_usage_errors() {
    let env = Env::new();
    for bad in [
        &["--queues", "0"][..],
        &["--queues", "17"],
        &["--depth", "0"],
        &["--depth", "4097"],
        &["--max-io-buf", "1K"],
        &["--cpus", "a-b"],
        &["--on-journal-damage", "ignore"],
    ] {
        let mut args = vec!["attach", "vol1"];
        args.extend_from_slice(bad);
        assert_eq!(env.run(&args).code, 2, "{bad:?}");
    }
    assert!(env.server.requests().is_empty());
}

#[test]
fn detach_and_resize() {
    let env = Env::new();
    env.ok("Detach", Response::Detached);
    env.ok(
        "Resize",
        Response::Resized {
            size_bytes: 300 << 30,
        },
    );
    let o = env.run(&["detach", "vol1"]);
    assert_eq!((o.code, o.stdout.as_str()), (0, "detached vol1\n"));
    let o = env.run(&["resize", "vol1", "300G"]);
    assert_eq!(
        (o.code, o.stdout.as_str()),
        (0, "resized vol1 to 300 GiB\n")
    );
    assert_eq!(
        env.server.requests(),
        vec![
            Request::Detach(NameParam {
                name: "vol1".into()
            }),
            Request::Resize(Resize {
                name: "vol1".into(),
                size_bytes: 300 << 30
            }),
        ]
    );
    assert_eq!(env.run(&["resize", "vol1", "0"]).code, 2);
    assert_eq!(env.run(&["resize", "vol1", "5000"]).code, 2);
}

#[test]
fn status_human_json_and_all() {
    let env = Env::new();
    env.ok(
        "VolumeStatus",
        Response::VolumeStatus(Box::new(status_body("vol1", VolumeMode::Integrity))),
    );
    let o = env.run(&["status", "vol1"]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    common::assert_snapshot("status.txt", &o.stdout);
    let j = env.run(&["status", "vol1", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&j.stdout).unwrap();
    assert_eq!(v["name"], "vol1");
    assert_eq!(v["rollback_floor"]["max_commit_seq"], 12);

    env.ok(
        "VolumeList",
        Response::VolumeList {
            volumes: vec![VolumeSummary {
                name: "vol1".into(),
                uuid: "u".into(),
                dir: "/d".into(),
                size_bytes: 1,
                mode: VolumeMode::Integrity,
                device: None,
            }],
        },
    );
    let all = env.run(&["status"]);
    assert_eq!(all.code, 0, "{}", all.stderr);
    assert!(all.stdout.contains("vol1"));
    let all = env.run(&["status", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&all.stdout).unwrap();
    assert_eq!(v.as_array().map(Vec::len), Some(1));
}

#[test]
fn status_without_a_rollback_floor_says_so() {
    let env = Env::new();
    let mut s = status_body("vol1", VolumeMode::Plain);
    s.rollback_floor = None;
    s.stats.extent_auth_failures = 3;
    env.ok("VolumeStatus", Response::VolumeStatus(Box::new(s)));
    let o = env.run(&["status", "vol1"]);
    assert!(
        o.stdout.contains("none recorded on this host"),
        "{}",
        o.stdout
    );
    assert!(o.stdout.contains("AUTH FAILURES"));
}

fn space_body() -> SpaceBody {
    SpaceBody {
        settings: SpaceSettings {
            free_floor: FreeFloor::Percent("10%".into()),
            on_full: OnFull::Error,
            full_timeout_ms: Some(60_000),
            cache_max: Some(100 << 30),
            headroom: 256 << 20,
        },
        figures: SpaceFigures::default(),
    }
}

#[test]
fn space_query_and_update() {
    let env = Env::new();
    env.ok("Space", Response::Space(space_body()));
    let o = env.run(&["space", "vol1"]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    common::assert_snapshot("space.txt", &o.stdout);
    let o = env.run(&[
        "space",
        "vol1",
        "--floor",
        "10%",
        "--on-full",
        "error",
        "--full-timeout",
        "60s",
        "--cache-max",
        "100G",
    ]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    let o = env.run(&["space", "vol1", "--floor", "5G"]);
    assert_eq!(o.code, 0);
    assert_eq!(
        env.server.requests(),
        vec![
            Request::Space(SpaceRequest {
                name: "vol1".into(),
                free_floor: None,
                on_full: None,
                full_timeout_ms: None,
                cache_max: None
            }),
            Request::Space(SpaceRequest {
                name: "vol1".into(),
                free_floor: Some(FreeFloor::Percent("10%".into())),
                on_full: Some(OnFull::Error),
                full_timeout_ms: Some(60_000),
                cache_max: Some(100 << 30)
            }),
            Request::Space(SpaceRequest {
                name: "vol1".into(),
                free_floor: Some(FreeFloor::Bytes(5 << 30)),
                on_full: None,
                full_timeout_ms: None,
                cache_max: None
            }),
        ]
    );
    for bad in [
        &["--floor", "101%"][..],
        &["--on-full", "block"],
        &["--full-timeout", "60"],
        &["--full-timeout", "0s"],
        &["--cache-max", "0"],
    ] {
        let mut args = vec!["space", "vol1"];
        args.extend_from_slice(bad);
        assert_eq!(env.run(&args).code, 2, "{bad:?}");
    }
}

#[test]
fn gc_pass_dry_run_and_status() {
    let env = Env::new();
    let pass = GcPassSummary {
        pass: 7,
        segments_condemned: 3,
        segments_deleted: 2,
        bytes_freed: 5 << 20,
        ..GcPassSummary::default()
    };
    env.ok("Gc", Response::Gc(pass.clone()));
    let o = env.run(&["gc", "vol1"]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    common::assert_snapshot("gc-pass.txt", &o.stdout);

    let dry = GcPassSummary {
        dry_run: true,
        ..pass
    };
    env.ok("Gc", Response::Gc(dry));
    let o = env.run(&["gc", "vol1", "--dry-run"]);
    assert_eq!(o.code, 0);
    common::assert_snapshot("gc-dry-run.txt", &o.stdout);

    env.ok(
        "GcStatus",
        Response::GcStatus(GcStatusSnapshot {
            state: Some(GcState::Grace),
            condemned_pending: 4,
            ..GcStatusSnapshot::default()
        }),
    );
    let o = env.run(&["gc", "vol1", "--status"]);
    assert_eq!(o.code, 0);
    assert!(o.stdout.contains("grace"), "{}", o.stdout);
    let j = env.run(&["gc", "vol1", "--status", "--json"]);
    assert!(serde_json::from_str::<serde_json::Value>(&j.stdout).is_ok());
    // `--json` of a pass, a dry run and the status: the response bodies as
    // the daemon sends them (M12c), every field present.
    env.ok("Gc", Response::Gc(pass.clone()));
    let j = env.run(&["gc", "vol1", "--json"]);
    assert_eq!(j.code, 0, "{}", j.stderr);
    let v: serde_json::Value = serde_json::from_str(&j.stdout).unwrap();
    assert_eq!(v["pass"], 7);
    assert_eq!(v["dry_run"], false);
    assert_eq!(v["segments_deleted"], 2);
    assert_eq!(v["bytes_freed"], 5 << 20);
    for k in [
        "segments_condemned",
        "packs_deleted",
        "runs_deleted",
        "commits_deleted",
        "segments_compacted",
        "bytes_moved",
    ] {
        assert!(v[k].is_u64(), "{k}: {v}");
    }
    assert_eq!(serde_json::from_value::<GcPassSummary>(v).unwrap(), pass);
    env.ok(
        "Gc",
        Response::Gc(GcPassSummary {
            dry_run: true,
            ..pass.clone()
        }),
    );
    let j = env.run(&["gc", "vol1", "--dry-run", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&j.stdout).unwrap();
    assert_eq!(v["dry_run"], true);
    assert_eq!(v["pass"], 7);
    let status = GcStatusSnapshot {
        state: None,
        condemned_pending: 1,
        moved_pending: 2,
        last_pass: Some(pass.clone()),
        stats: lunix_iface::gc::GcStats {
            passes: 3,
            live_bytes: 40 << 20,
            dead_bytes: 1 << 20,
            bytes_freed: 9 << 20,
            segments_compacted: 1,
            compaction_read: 2 << 20,
            compaction_written: 1 << 20,
        },
        last_finished_ms: Some(1_767_225_600_000),
    };
    env.ok("GcStatus", Response::GcStatus(status.clone()));
    let o = env.run(&["gc", "vol1", "--status"]);
    assert_eq!(o.code, 0);
    common::assert_snapshot("gc-status.txt", &o.stdout);
    env.ok("GcStatus", Response::GcStatus(status.clone()));
    let j = env.run(&["gc", "vol1", "--status", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&j.stdout).unwrap();
    assert!(v["state"].is_null(), "{v}");
    assert_eq!(v["stats"]["passes"], 3);
    assert_eq!(v["last_pass"]["pass"], 7);
    assert_eq!(
        serde_json::from_value::<GcStatusSnapshot>(v).unwrap(),
        status
    );
    assert_eq!(env.run(&["gc", "vol1", "--status", "--dry-run"]).code, 2);
    let reqs = env.server.requests();
    assert_eq!(
        reqs[0],
        Request::Gc(GcRequest {
            name: "vol1".into(),
            dry_run: false
        })
    );
    assert_eq!(
        reqs[1],
        Request::Gc(GcRequest {
            name: "vol1".into(),
            dry_run: true
        })
    );
}

#[test]
fn error_codes_map_to_exit_codes() {
    let cases = [
        (ErrorCode::Busy, 3),
        (ErrorCode::NotFound, 4),
        (ErrorCode::NoSpace, 5),
        (ErrorCode::Downgrade, 6),
        (ErrorCode::WrongKmk, 6),
        (ErrorCode::Rollback, 6),
        (ErrorCode::Fork, 6),
        (ErrorCode::ForkUnverifiable, 6),
        (ErrorCode::JournalDamage, 7),
        (ErrorCode::Invalid, 2),
        (ErrorCode::Io, 1),
        (ErrorCode::Internal, 1),
        (ErrorCode::Unknown, 1),
    ];
    for (code, exit) in cases {
        let env = Env::new();
        env.err("Detach", code, "the daemon says no");
        let o = env.run(&["detach", "vol1"]);
        assert_eq!(o.code, exit, "{code:?}");
        assert!(
            o.stderr.contains("the daemon says no"),
            "{code:?}: {}",
            o.stderr
        );
        assert!(o.stderr.starts_with("lunix: "), "{}", o.stderr);
        assert_eq!(o.stdout, "");
        // The same mapping for every other command that can fail.
        for (method, args) in [
            ("VolumeList", vec!["volume", "list"]),
            ("VolumeStatus", vec!["status", "vol1"]),
            ("Resize", vec!["resize", "vol1", "1G"]),
            ("Space", vec!["space", "vol1"]),
            ("Gc", vec!["gc", "vol1"]),
            ("GcStatus", vec!["gc", "vol1", "--status"]),
            ("Attach", vec!["attach", "vol1"]),
            (
                "VolumeCreate",
                vec!["volume", "create", "vol1", "--size", "1G", "--dir", "/d"],
            ),
        ] {
            env.err(method, code, "no");
            assert_eq!(env.run(&args).code, exit, "{method} {code:?}");
        }
    }
}

#[test]
fn attach_failures_carry_a_hint() {
    for (code, needle) in [
        (ErrorCode::ForkUnverifiable, "--allow-unverifiable-fork"),
        (ErrorCode::Rollback, "--reset-floor"),
        (ErrorCode::Fork, "lunix fsck vol1"),
        (ErrorCode::JournalDamage, "lunix fsck vol1"),
        (ErrorCode::WrongKmk, "--credential-mode"),
        (ErrorCode::Downgrade, "--credential-mode"),
    ] {
        let env = Env::new();
        env.err("Attach", code, "refused");
        let o = env.run(&["attach", "vol1"]);
        assert!(
            o.stderr.contains("hint: ") && o.stderr.contains(needle),
            "{code:?}: {}",
            o.stderr
        );
    }
    let env = Env::new();
    env.err("Attach", ErrorCode::Busy, "in use");
    assert!(!env.run(&["attach", "vol1"]).stderr.contains("hint"));
}

#[test]
fn daemon_messages_cannot_drive_the_terminal() {
    let env = Env::new();
    env.err("Detach", ErrorCode::Internal, "a\x1b[31mred\x07");
    let o = env.run(&["detach", "vol1"]);
    assert!(!o.stderr.contains('\x1b') && !o.stderr.contains('\x07'));
}

#[test]
fn an_unreachable_daemon_is_a_generic_error() {
    let dir = tempfile::tempdir().unwrap();
    let o = common::run_with(
        &dir.path().join("none.sock"),
        &["volume", "list"],
        None,
        dir.path(),
    );
    assert_eq!(o.code, 1);
    assert!(
        o.stderr.contains("cannot connect to lunixd"),
        "{}",
        o.stderr
    );
}

#[test]
fn a_daemon_that_hangs_up_is_a_generic_error() {
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("hup.sock");
    let l = std::os::unix::net::UnixListener::bind(&sock).unwrap();
    let h = std::thread::spawn(move || {
        let _ = l.accept();
    });
    let o = common::run_with(&sock, &["volume", "list"], None, dir.path());
    h.join().unwrap();
    assert_eq!(o.code, 1);
    assert!(o.stderr.contains("closed the connection"), "{}", o.stderr);
}

#[test]
fn fsck_repair_flags_need_repair() {
    let env = Env::new();
    // --gc needs --repair (a usage error of fsck is 64, not "errors found").
    assert_eq!(env.run(&["fsck", "vol1", "--gc"]).code, 64);
    assert_eq!(env.run(&["fsck", "vol1", "--reset-floor"]).code, 64);
    assert_eq!(env.run(&["fsck", "vol1", "--delete-orphans"]).code, 64);
}

#[test]
fn usage_errors_exit_2() {
    let cases: Vec<Vec<&str>> = vec![
        vec![],
        vec!["frobnicate"],
        vec!["attach"],
        vec!["volume"],
        vec!["volume", "create"],
        vec!["detach", "a", "b"],
    ];
    for args in &cases {
        assert_eq!(common::run_bare(args).code, 2, "{args:?}");
    }
    assert_eq!(common::run_bare(&["--help"]).code, 0);
    assert_eq!(common::run_bare(&["--version"]).code, 0);
}

#[test]
fn a_silent_daemon_times_out() {
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("mute.sock");
    let l = std::os::unix::net::UnixListener::bind(&sock).unwrap();
    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
    let h = std::thread::spawn(move || {
        let conn = l.accept();
        // Keep the connection open (and silent) until the client has given up.
        let _ = done_rx.recv();
        drop(conn);
    });
    let started = std::time::Instant::now();
    let o = common::run_with(
        &sock,
        &["--timeout", "300ms", "volume", "list"],
        None,
        dir.path(),
    );
    let waited = started.elapsed();
    done_tx.send(()).unwrap();
    h.join().unwrap();
    assert_eq!(o.code, 1, "{}", o.stderr);
    assert!(o.stderr.contains("did not answer in time"), "{}", o.stderr);
    assert!(waited < std::time::Duration::from_secs(20), "{waited:?}");
    assert_eq!(
        common::run_bare(&["--timeout", "0s", "volume", "list"]).code,
        2
    );
    assert_eq!(
        common::run_bare(&["--timeout", "10", "volume", "list"]).code,
        2
    );
}
