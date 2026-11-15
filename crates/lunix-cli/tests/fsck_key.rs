//! `lunix fsck` and `lunix key` against volumes built by the real crates
//! (the M14 builder: `lunix-store`, `lunix-map`, `lunix-index`, `lunix-crypto`),
//! through the `lunix` binary and no daemon.

#[path = "../../lunix-fsck/tests/common/mod.rs"]
#[allow(clippy::all)]
mod build;
mod common;

use build::{build, set_floor, Built, Spec};
use common::{run_with, Out};
use lunix_format::local::RollbackFloor;
use lunix_format::{CommitSeq, CryptoMode};

fn spec(mode: CryptoMode) -> Spec {
    let mut s = Spec::new(mode, 16 * 1024);
    s.chunks = 2048;
    s.blobs = 120;
    s.per_commit = 120;
    s.journal_records = 40;
    s
}

/// Runs `lunix <args>` with no daemon (the socket does not exist).
fn lunix(b: &Built, args: &[&str]) -> Out {
    let sock = b.tmp.path().join("no-daemon.sock");
    run_with(&sock, args, None, b.tmp.path())
}

fn fsck(b: &Built, extra: &[&str]) -> Out {
    let dir = b.tmp.path().to_string_lossy().into_owned();
    let key = b
        .key_file
        .as_ref()
        .map(|k| k.to_string_lossy().into_owned());
    let mut args = vec!["fsck", "fsck-test", "--dir", &dir];
    if let Some(k) = &key {
        args.push("--key-file");
        args.push(k);
    }
    args.extend_from_slice(extra);
    lunix(b, &args)
}

fn json(o: &Out) -> serde_json::Value {
    serde_json::from_str(&o.stdout).unwrap_or_else(|e| panic!("{e}: {}", o.stdout))
}

const MODES: [CryptoMode; 3] = [
    CryptoMode::Plain,
    CryptoMode::Integrity,
    CryptoMode::Encrypted,
];

#[test]
fn clean_volumes_of_all_modes_exit_0_with_the_builders_ratios() {
    for mode in MODES {
        let b = build(&spec(mode));
        let o = fsck(&b, &[]);
        assert_eq!(o.code, 0, "{mode:?}: {}{}", o.stdout, o.stderr);
        assert!(o.stdout.contains("result: clean"), "{}", o.stdout);
        let o = fsck(&b, &["--deep", "--json"]);
        assert_eq!(o.code, 0, "{mode:?}: {}{}", o.stdout, o.stderr);
        let v = json(&o);
        assert_eq!(v["exit"], 0);
        assert_eq!(v["tip"], b.known.tip);
        let ratio = v["ratios"]["dedup_ratio"].as_f64().unwrap();
        assert!((ratio - b.known.dedup_ratio()).abs() < 1e-9, "{ratio}");
        let comp = v["ratios"]["compression_ratio"].as_f64().unwrap();
        assert!((comp - b.known.compression_ratio()).abs() < 1e-9, "{comp}");
    }
}

#[test]
fn a_corrupted_volume_exits_2_and_names_the_repair() {
    for mode in [CryptoMode::Integrity, CryptoMode::Encrypted] {
        let b = build(&spec(mode));
        b.flip(&b.commit_rel(b.known.tip), 10);
        let o = fsck(&b, &[]);
        assert_eq!(o.code, 2, "{mode:?}: {}", o.stdout);
        assert!(o.stdout.contains("tip_auth"), "{}", o.stdout);
        assert!(o.stdout.contains("--truncate-tip"), "{}", o.stdout);
        // Nothing was changed by a plain run.
        assert_eq!(fsck(&b, &[]).code, 2);
    }
}

#[test]
fn truncate_tip_through_the_cli_makes_the_previous_commit_the_tip() {
    for mode in [CryptoMode::Integrity, CryptoMode::Encrypted] {
        let b = build(&spec(mode));
        let tip = b.known.tip;
        b.flip(&b.commit_rel(tip), 10);
        let o = fsck(&b, &["--repair", "--truncate-tip"]);
        assert!(o.code <= 1, "{mode:?}: {}{}", o.stdout, o.stderr);
        assert!(o.stdout.contains("truncate_tip"), "{}", o.stdout);
        let after = fsck(&b, &["--json"]);
        assert!(after.code <= 1, "{}", after.stdout);
        assert_eq!(json(&after)["tip"], tip - 1);
        let aside = std::fs::read_dir(b.path("state/commits"))
            .unwrap()
            .filter_map(|e| e.ok())
            .any(|e| e.file_name().to_string_lossy().ends_with(".damaged"));
        assert!(aside, "the damaged commit is kept aside");
    }
}

#[test]
fn reset_floor_through_the_cli_accepts_a_rolled_back_copy() {
    let b = build(&spec(CryptoMode::Integrity));
    set_floor(
        &b,
        Some(RollbackFloor {
            max_commit_seq: CommitSeq(b.known.tip + 5),
            tip_hash: [1; 32],
        }),
    );
    let o = fsck(&b, &[]);
    assert_eq!(o.code, 2, "{}", o.stdout);
    assert!(o.stdout.contains("rollback"), "{}", o.stdout);
    assert!(o.stdout.contains("--repair --reset-floor"), "{}", o.stdout);
    // Without --repair the flag is a usage error and nothing changes.
    assert_eq!(fsck(&b, &["--reset-floor"]).code, 2);
    assert_eq!(fsck(&b, &[]).code, 2);
    let o = fsck(&b, &["--repair", "--reset-floor"]);
    assert!(o.code <= 1, "{}{}", o.stdout, o.stderr);
    assert!(o.stdout.contains("reset_floor"), "{}", o.stdout);
    let after = fsck(&b, &[]);
    assert!(after.code <= 1, "{}", after.stdout);
}

#[test]
fn repair_gc_finishes_and_removes_leftovers() {
    // The condemned-list repair itself is M17.B's (the library's gc hook is
    // still empty): here the pass must run to the end next to the safe repairs.
    let b = build(&spec(CryptoMode::Integrity));
    std::fs::write(b.path("segments/ffffffffffffff00.seg.tmp"), b"junk").unwrap();
    let o = fsck(&b, &["--repair", "--gc", "--json"]);
    assert!(o.code <= 1, "{}{}", o.stdout, o.stderr);
    assert!(!b.path("segments/ffffffffffffff00.seg.tmp").exists());
    let after = fsck(&b, &["--deep"]);
    assert_eq!(after.code, 0, "{}", after.stdout);
}

#[test]
fn a_locked_volume_is_refused_as_busy() {
    let b = build(&spec(CryptoMode::Integrity));
    let held = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(b.path("LOCK"))
        .unwrap();
    held.try_lock().expect("the lock is free after the build");
    let o = fsck(&b, &[]);
    assert_eq!(o.code, 2, "{}", o.stdout);
    assert!(o.stdout.contains("busy"), "{}", o.stdout);
    let o = lunix(
        &b,
        &[
            "key",
            "ls",
            "fsck-test",
            "--dir",
            &b.tmp.path().to_string_lossy(),
        ],
    );
    assert_eq!(o.code, 0, "{}", o.stderr);
    drop(held);
}

#[test]
fn key_commands_change_the_keyring_and_fsck_still_opens_the_volume() {
    for mode in [CryptoMode::Integrity, CryptoMode::Encrypted] {
        let b = build(&spec(mode));
        let dir = b.tmp.path().to_string_lossy().into_owned();
        let old = b.key_file.as_ref().unwrap().to_string_lossy().into_owned();
        let new = b
            .tmp
            .path()
            .join("second.key")
            .to_string_lossy()
            .into_owned();
        let o = lunix(&b, &["key", "ls", "fsck-test", "--dir", &dir, "--json"]);
        assert_eq!(o.code, 0, "{}", o.stderr);
        assert_eq!(json(&o)["slots"].as_array().unwrap().len(), 1);
        let o = lunix(
            &b,
            &[
                "key",
                "add-slot",
                "fsck-test",
                "--dir",
                &dir,
                "--key-file",
                &new,
                "--unlock-key-file",
                &old,
            ],
        );
        assert_eq!(o.code, 0, "{}", o.stderr);
        let o = lunix(&b, &["key", "ls", "fsck-test", "--dir", &dir, "--json"]);
        assert_eq!(json(&o)["slots"].as_array().unwrap().len(), 2);
        // The volume opens with the new key file, and fsck is still clean.
        let o = lunix(
            &b,
            &["fsck", "fsck-test", "--dir", &dir, "--key-file", &new],
        );
        assert_eq!(o.code, 0, "{}{}", o.stdout, o.stderr);
        // Remove the first slot with the new key; the old file no longer opens it.
        let o = lunix(
            &b,
            &[
                "key",
                "rm-slot",
                "fsck-test",
                "--dir",
                &dir,
                "--slot",
                "1",
                "--unlock-key-file",
                &new,
            ],
        );
        assert_eq!(o.code, 0, "{}", o.stderr);
        let o = lunix(
            &b,
            &["fsck", "fsck-test", "--dir", &dir, "--key-file", &old],
        );
        assert_eq!(o.code, 2, "{}", o.stdout);
        let o = lunix(
            &b,
            &["fsck", "fsck-test", "--dir", &dir, "--key-file", &new],
        );
        assert_eq!(o.code, 0, "{}", o.stdout);
    }
}

#[test]
fn a_plain_volume_has_no_slots_through_the_binary() {
    let b = build(&spec(CryptoMode::Plain));
    let dir = b.tmp.path().to_string_lossy().into_owned();
    let o = lunix(&b, &["key", "ls", "fsck-test", "--dir", &dir]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert!(o.stdout.contains("no key slots"), "{}", o.stdout);
    let o = lunix(
        &b,
        &["key", "rm-slot", "fsck-test", "--dir", &dir, "--slot", "1"],
    );
    assert_eq!(o.code, 2, "{}", o.stderr);
}
