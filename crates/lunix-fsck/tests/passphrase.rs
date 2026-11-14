//! Passphrases: any bytes, from a descriptor, never from the environment.

mod common;

use std::process::Command;

use common::*;
use lunix_fsck::{code, Credentials, Options};
use lunix_iface::ctl::Secret;

const PASS: &[u8] = b"\xff\xfe not utf-8 \x80 pass";

fn spec() -> Spec {
    let mut s = Spec::new(lunix_format::CryptoMode::Encrypted, 16 * 1024);
    s.passphrase = Some(PASS.to_vec());
    s.chunks = 1024;
    s.blobs = 50;
    s.per_commit = 50;
    s
}

#[test]
fn a_non_utf8_passphrase_unlocks_and_a_wrong_one_does_not() {
    let b = build(&spec());
    let ok = lunix_fsck::check(
        &b.dir,
        &Options {
            credentials: Credentials {
                key_file: None,
                passphrase: Some(Secret::new(PASS.to_vec())),
            },
            ..Options::default()
        },
    );
    assert_eq!(ok.exit_code(), 0, "{}", ok.render_text());
    let bad = lunix_fsck::check(
        &b.dir,
        &Options {
            credentials: Credentials {
                key_file: None,
                passphrase: Some(Secret::new(b"wrong".to_vec())),
            },
            ..Options::default()
        },
    );
    assert!(bad.has(code::CREDENTIAL));
}

#[test]
fn the_binary_reads_the_passphrase_from_a_descriptor_not_the_environment() {
    let b = build(&spec());
    let bin = env!("CARGO_BIN_EXE_lunix-fsck");
    let pw = b.tmp.path().join("pw");
    let mut bytes = PASS.to_vec();
    bytes.push(b'\n');
    std::fs::write(&pw, bytes).expect("write");
    let run = |extra: &str| {
        Command::new("sh")
            .arg("-c")
            .arg(format!("exec \"$0\" {extra} \"$1\" 3<\"$2\""))
            .arg(bin)
            .arg(&b.dir)
            .arg(&pw)
            .env("LUNIX_PASSPHRASE", "ignored")
            .output()
            .expect("run")
    };
    let ok = run("--passphrase-fd 3");
    assert_eq!(
        ok.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&ok.stdout)
    );
    let none = run("");
    assert_eq!(
        none.status.code(),
        Some(2),
        "no credential from the environment"
    );
    let env = Command::new(bin)
        .arg("--passphrase-env")
        .arg("LUNIX_PASSPHRASE")
        .arg(&b.dir)
        .output()
        .expect("run");
    assert_eq!(env.status.code(), Some(64), "the option is gone");
}
