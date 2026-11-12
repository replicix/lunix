//! The attach downgrade check against `docs/reference/attach-matrix.md`.
//!
//! The expected results are **loaded from that file** (written in M1.A0), not
//! derived here: the test parses its four tables, expands the merged rows, and
//! runs `check_attach` on a real volume for each combination.

mod common;

use std::collections::BTreeSet;

use common::*;
use lunix_crypto::keyring::{self, Argon2Cost, KeyFile, NewSlot, Unlock, UnlockOptions};
use lunix_crypto::{check_attach, seal_descriptor, AttachError, AttachRequest, Credential};
use lunix_format::keyring::RootKind;
use lunix_format::local::Pin;
use lunix_format::volume::VolumeDescriptor;
use lunix_format::{CryptoMode, DekGen, IdLen, Profile};

const MATRIX: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../docs/reference/attach-matrix.md"
);

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Cred {
    Key,
    Integrity,
    None,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Fp {
    Right,
    Wrong,
    Absent,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum PinSpec {
    Right,
    Wrong,
    Absent,
    Records(CryptoMode),
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
struct Case {
    desc: CryptoMode,
    cred: Cred,
    mac_valid: bool,
    fp: Fp,
    pin: PinSpec,
}

struct Expected {
    row: u32,
    case: Case,
    result: String,
}

fn clean(cell: &str) -> String {
    cell.replace('`', "").trim().to_owned()
}

fn parse_desc(s: &str) -> CryptoMode {
    let s = clean(s);
    if s.starts_with("unencrypted") || s == "plain" {
        CryptoMode::Plain
    } else if s == "integrity" {
        CryptoMode::Integrity
    } else if s == "encrypted" {
        CryptoMode::Encrypted
    } else {
        panic!("unknown descriptor {s:?}")
    }
}

fn parse_cred(s: &str) -> Cred {
    match clean(s).as_str() {
        "key" => Cred::Key,
        "integrity" => Cred::Integrity,
        "none" => Cred::None,
        other => panic!("unknown credential {other:?}"),
    }
}

fn parse_fps(s: &str) -> Vec<Fp> {
    let s = clean(s);
    match s.as_str() {
        "right" => vec![Fp::Right],
        "wrong" => vec![Fp::Wrong],
        "absent" => vec![Fp::Absent],
        _ if s.starts_with("any") => vec![Fp::Right, Fp::Wrong, Fp::Absent],
        other => panic!("unknown fingerprint {other:?}"),
    }
}

fn parse_pins(s: &str) -> Vec<PinSpec> {
    let s = clean(s);
    match s.as_str() {
        "right" => vec![PinSpec::Right],
        "wrong" => vec![PinSpec::Wrong],
        "absent" => vec![PinSpec::Absent],
        "records encrypted" => vec![PinSpec::Records(CryptoMode::Encrypted)],
        "records integrity" => vec![PinSpec::Records(CryptoMode::Integrity)],
        "records plain" => vec![PinSpec::Records(CryptoMode::Plain)],
        _ if s.starts_with("any") => vec![PinSpec::Right, PinSpec::Wrong, PinSpec::Absent],
        other => panic!("unknown pin {other:?}"),
    }
}

fn parse_macs(s: &str) -> Vec<bool> {
    match clean(s).as_str() {
        "valid" => vec![true],
        "invalid" => vec![false],
        other => panic!("unknown MAC {other:?}"),
    }
}

/// Rows of the four tables, merged rows expanded.
fn load() -> (usize, Vec<Expected>) {
    let text = std::fs::read_to_string(MATRIX).expect("attach-matrix.md");
    let mut rows = 0;
    let mut out = Vec::new();
    for line in text.lines() {
        // Table E (amendment 2026-11-12) is the rollback-anchor stage that
        // runs after `verify_chain`; it is not a result of `check_attach`.
        if line.starts_with("## Table E") {
            break;
        }
        let Some(rest) = line.strip_prefix("| ") else {
            continue;
        };
        let cells: Vec<&str> = rest.trim_end().trim_end_matches('|').split('|').collect();
        let Ok(row) = cells[0].trim().parse::<u32>() else {
            continue;
        };
        rows += 1;
        match cells.len() {
            // Tables A, B, D: # descriptor credential MAC fingerprint pin result rule
            8 => {
                let desc = parse_desc(cells[1]);
                let cred = parse_cred(cells[2]);
                for mac_valid in parse_macs(cells[3]) {
                    for fp in parse_fps(cells[4]) {
                        for pin in parse_pins(cells[5]) {
                            out.push(Expected {
                                row,
                                case: Case {
                                    desc,
                                    cred,
                                    mac_valid,
                                    fp,
                                    pin,
                                },
                                result: clean(cells[6]),
                            });
                        }
                    }
                }
            }
            // Table C: # descriptor credential "any (18)" result rule reason
            7 => {
                assert!(clean(cells[3]).starts_with("any (18)"), "row {row}");
                let desc = parse_desc(cells[1]);
                let cred = parse_cred(cells[2]);
                for mac_valid in [true, false] {
                    for fp in [Fp::Right, Fp::Wrong, Fp::Absent] {
                        for pin in [PinSpec::Right, PinSpec::Wrong, PinSpec::Absent] {
                            out.push(Expected {
                                row,
                                case: Case {
                                    desc,
                                    cred,
                                    mac_valid,
                                    fp,
                                    pin,
                                },
                                result: clean(cells[4]),
                            });
                        }
                    }
                }
            }
            n => panic!("row {row}: {n} cells"),
        }
    }
    (rows, out)
}

struct Fixture {
    desc: VolumeDescriptor,
    keyring: Option<Vec<u8>>,
    key_file: KeyFile,
}

fn fixture(mode: CryptoMode) -> Fixture {
    let mut desc = descriptor(
        mode,
        Profile::Default,
        65_536,
        IdLen::ThirtyTwo,
        [0x44; 16],
        [0x45; 16],
    );
    let key_file = KeyFile::generate().expect("kf");
    match mode {
        CryptoMode::Plain => {
            seal_descriptor(&mut desc, None).expect("seal");
            Fixture {
                desc,
                keyring: None,
                key_file,
            }
        }
        _ => {
            let kind = if mode == CryptoMode::Encrypted {
                RootKind::Kmk
            } else {
                RootKind::Ik
            };
            let c = keyring::create(
                Profile::Default,
                desc.family_id,
                kind,
                &NewSlot::KeyFile(&key_file),
            )
            .expect("keyring");
            seal_descriptor(&mut desc, Some(&c.root)).expect("seal");
            Fixture {
                desc,
                keyring: Some(c.bytes),
                key_file,
            }
        }
    }
}

fn flipped(fp: [u8; 16]) -> [u8; 16] {
    let mut f = fp;
    f[3] ^= 0x40;
    f
}

fn run(fx: &Fixture, case: Case) -> Result<lunix_crypto::AttachOk, AttachError> {
    let mut desc = fx.desc.clone();
    if !case.mac_valid {
        desc.mac[7] ^= 0x01;
    }
    let right = fx.desc.kmk_fingerprint;
    let keyed = case.desc != CryptoMode::Plain;
    let fingerprint = match case.fp {
        Fp::Absent => None,
        // A plain volume has no fingerprint: "right" and "wrong" both mean
        // that one was supplied.
        Fp::Right => Some(right),
        Fp::Wrong => Some(if keyed { flipped(right) } else { [1; 16] }),
    };
    let pin = match case.pin {
        PinSpec::Absent => None,
        PinSpec::Right => Some(Pin {
            mode: case.desc,
            fingerprint: right,
        }),
        PinSpec::Wrong => Some(if keyed {
            Pin {
                mode: case.desc,
                fingerprint: flipped(right),
            }
        } else {
            Pin {
                mode: CryptoMode::Integrity,
                fingerprint: [7; 16],
            }
        }),
        PinSpec::Records(mode) => Some(Pin {
            mode,
            fingerprint: if mode == CryptoMode::Plain {
                [0; 16]
            } else {
                right
            },
        }),
    };
    let unlock = Unlock::KeyFile(&fx.key_file);
    let credential = match case.cred {
        Cred::Key => Some(Credential::Key(unlock)),
        Cred::Integrity => Some(Credential::Integrity(unlock)),
        Cred::None => None,
    };
    check_attach(
        &desc,
        fx.keyring.as_deref(),
        &AttachRequest {
            credential,
            fingerprint,
            pin,
            keyring_seq_seen: 0,
            unlock: Default::default(),
        },
    )
}

fn name(r: &Result<lunix_crypto::AttachOk, AttachError>) -> String {
    match r {
        Ok(ok) if ok.record_pin.is_some() => "Ok+pin".into(),
        Ok(_) => "Ok".into(),
        Err(AttachError::Downgrade) => "Downgrade".into(),
        Err(AttachError::NeedCredential) => "NeedCredential".into(),
        Err(AttachError::ModeMismatch) => "ModeMismatch".into(),
        Err(AttachError::BadMac) => "BadMac".into(),
        Err(AttachError::WrongKmk) => "WrongKmk".into(),
        Err(other) => format!("{other:?}"),
    }
}

/// Table E is outside `check_attach`: pin the shape the chain step will load
/// (rows 120-126, results `pass`, `Rollback` and `Fork`).
#[test]
fn table_e_rows_are_there_but_not_check_attach_cases() {
    let text = std::fs::read_to_string(MATRIX).expect("attach-matrix.md");
    let table_e = text.split("## Table E").nth(1).expect("Table E section");
    let rows: Vec<Vec<&str>> = table_e
        .lines()
        .filter_map(|l| l.strip_prefix("| "))
        .map(|r| {
            r.trim_end()
                .trim_end_matches('|')
                .split('|')
                .map(str::trim)
                .collect()
        })
        .filter(|c: &Vec<&str>| c[0].parse::<u32>().is_ok())
        .collect();
    let ids: Vec<u32> = rows.iter().map(|c| c[0].parse().unwrap()).collect();
    assert_eq!(ids, (120..=126).collect::<Vec<_>>());
    assert!(rows.iter().all(|c| c.len() == 6));
    let results: Vec<&str> = rows.iter().map(|c| c[4]).collect();
    assert_eq!(results.iter().filter(|r| r.contains("Rollback")).count(), 1);
    assert_eq!(results.iter().filter(|r| r.contains("Fork")).count(), 2);
    // None of them is a case of `load`.
    let (_, cases) = load();
    assert!(cases.iter().all(|c| c.row < 120));
}

#[test]
fn the_matrix_file_has_the_rows_it_says() {
    let (rows, cases) = load();
    assert_eq!(rows, 119, "rows of tables A, B, C and D");
    let ids: BTreeSet<u32> = cases.iter().map(|c| c.row).collect();
    assert_eq!(ids.len(), 119);
    assert_eq!(ids.iter().next(), Some(&1));
    assert_eq!(ids.iter().next_back(), Some(&119));
    assert_eq!(cases.len(), 186, "combinations covered by the 119 rows");
    // Tables A, B and C cover all 3 x 3 x 2 x 3 x 3 = 162 combinations once.
    let base: BTreeSet<Case> = cases
        .iter()
        .filter(|c| c.row <= 111)
        .map(|c| c.case)
        .collect();
    assert_eq!(base.len(), 162);
    assert_eq!(cases.iter().filter(|c| c.row <= 111).count(), 162);
    // Table A alone is the plan's 72.
    assert_eq!(cases.iter().filter(|c| c.row <= 72).count(), 72);
    for e in &cases {
        assert!(
            [
                "Ok",
                "Ok+pin",
                "Downgrade",
                "NeedCredential",
                "ModeMismatch",
                "BadMac",
                "WrongKmk"
            ]
            .contains(&e.result.as_str()),
            "row {}: {}",
            e.row,
            e.result
        );
    }
}

#[test]
fn check_attach_equals_the_matrix() {
    let (_, cases) = load();
    let fixtures = [
        fixture(CryptoMode::Encrypted),
        fixture(CryptoMode::Integrity),
        fixture(CryptoMode::Plain),
    ];
    let mut failures = Vec::new();
    for e in &cases {
        let fx = fixtures
            .iter()
            .find(|f| f.desc.crypto_mode == e.case.desc)
            .expect("fixture");
        let got = run(fx, e.case);
        let got_name = name(&got);
        if got_name != e.result {
            failures.push(format!(
                "row {}: {:?} expected {}, got {}",
                e.row, e.case, e.result, got_name
            ));
        }
        if let Ok(ok) = &got {
            // `Ok+pin` records exactly the pin of the descriptor.
            if let Some(p) = ok.record_pin {
                assert_eq!(p.mode, e.case.desc, "row {}", e.row);
                assert_eq!(p.fingerprint, fx.desc.kmk_fingerprint, "row {}", e.row);
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn a_successful_attach_unlocks_codecs_that_work() {
    for mode in [
        CryptoMode::Encrypted,
        CryptoMode::Integrity,
        CryptoMode::Plain,
    ] {
        let fx = fixture(mode);
        let cred = match mode {
            CryptoMode::Encrypted => Some(Credential::Key(Unlock::KeyFile(&fx.key_file))),
            CryptoMode::Integrity => Some(Credential::Integrity(Unlock::KeyFile(&fx.key_file))),
            CryptoMode::Plain => None,
        };
        let ok = check_attach(
            &fx.desc,
            fx.keyring.as_deref(),
            &AttachRequest {
                credential: cred,
                ..AttachRequest::default()
            },
        )
        .expect("attach");
        assert_eq!(ok.crypto.keys().mode(), mode);
        let c = ok.crypto.extents();
        let data = pattern(65_536, 1);
        let (id, _, bytes) = seal_data(&**c, lunix_format::Codec::Lz4, &data);
        let mut out = Vec::new();
        c.open(
            lunix_format::ExtentKind::Data,
            &bytes,
            &lunix_iface::crypto::OpenExpect {
                chunk_size: 65_536,
                chunk_id: Some(&id),
                id_prefix: None,
                verify_id: true,
                output: lunix_iface::crypto::OpenOutput::Plaintext,
            },
            &mut out,
        )
        .expect("open");
        assert_eq!(out, data);
        assert_eq!(ok.keyring_seq.is_some(), mode != CryptoMode::Plain);
    }
}

#[test]
fn failures_outside_the_matrix() {
    let enc = fixture(CryptoMode::Encrypted);
    fn key(fx: &Fixture) -> Option<Credential<'_>> {
        Some(Credential::Key(Unlock::KeyFile(&fx.key_file)))
    }
    let attach = |fx: &Fixture, cred, seen: u64, kr: Option<&[u8]>| {
        check_attach(
            &fx.desc,
            kr,
            &AttachRequest {
                credential: cred,
                keyring_seq_seen: seen,
                ..AttachRequest::default()
            },
        )
    };
    // A key that unwraps nothing, a missing key file, a missing keyring.
    let stranger = KeyFile::generate().expect("kf");
    let err = attach(
        &enc,
        Some(Credential::Key(Unlock::KeyFile(&stranger))),
        0,
        enc.keyring.as_deref(),
    )
    .err()
    .expect("wrong key");
    assert!(matches!(err, AttachError::WrongCredential));
    assert!(err.is_wrong_key_class());
    let missing = std::path::Path::new("/nonexistent/lunix/integrity.key");
    let err = attach(
        &enc,
        Some(Credential::Key(Unlock::KeyFilePath(missing))),
        0,
        enc.keyring.as_deref(),
    )
    .err()
    .expect("missing key file");
    assert!(err.is_wrong_key_class());
    assert!(attach(&enc, key(&enc), 0, None).is_err());
    // A keyring older than the pin's floor.
    let (kr2, _) = {
        let kr = lunix_format::keyring::Keyring::decode(enc.keyring.as_deref().expect("kr"))
            .expect("decode");
        // Re-wrap the root through the public API to raise the sequence.
        let u = keyring::unlock(
            enc.keyring.as_deref().expect("kr"),
            Profile::Default,
            &enc.desc.family_id,
            &Unlock::KeyFile(&enc.key_file),
            0,
            &UnlockOptions::default(),
        )
        .expect("unlock");
        let extra = KeyFile::generate().expect("kf");
        keyring::add_slot(&kr, Profile::Default, &u.root, &NewSlot::KeyFile(&extra)).expect("add")
    };
    let kr2_bytes = keyring::encode(&kr2).expect("encode");
    attach(&enc, key(&enc), 2, Some(&kr2_bytes)).expect("current keyring");
    let err = attach(&enc, key(&enc), 3, Some(&kr2_bytes))
        .err()
        .expect("rollback");
    assert!(matches!(err, AttachError::KeyringRollback));
    assert!(attach(&enc, key(&enc), 3, enc.keyring.as_deref()).is_err());
    // A KMS credential.
    let err = attach(
        &enc,
        Some(Credential::Key(Unlock::Kms("arn:x"))),
        0,
        enc.keyring.as_deref(),
    )
    .err()
    .expect("kms");
    assert!(matches!(err, AttachError::Unsupported(_)));
    // An integrity keyring behind an encrypted descriptor is not a key for it.
    let int = fixture(CryptoMode::Integrity);
    let err = check_attach(
        &enc.desc,
        int.keyring.as_deref(),
        &AttachRequest {
            credential: Some(Credential::Key(Unlock::KeyFile(&int.key_file))),
            ..AttachRequest::default()
        },
    )
    .err()
    .expect("wrong keyring");
    assert!(err.is_wrong_key_class());
    // A rotated keyring hands its generation to the codecs.
    let u = keyring::unlock(
        enc.keyring.as_deref().expect("kr"),
        Profile::Default,
        &enc.desc.family_id,
        &Unlock::KeyFile(&enc.key_file),
        0,
        &UnlockOptions::default(),
    )
    .expect("unlock");
    let rotated = keyring::rotate_dek(&u.keyring, Profile::Default, &u.root).expect("rotate");
    let ok = attach(
        &enc,
        key(&enc),
        0,
        Some(&keyring::encode(&rotated).expect("e")),
    )
    .expect("attach");
    assert_eq!(ok.crypto.volume_keys().dek_gen(), DekGen(1));
    assert_eq!(ok.keyring_seq, Some(2));
}

/// The descriptor MAC covers every plaintext field: flipping any byte either
/// breaks the structure or fails the MAC; it is never accepted.
#[test]
fn the_descriptor_mac_covers_every_byte() {
    for mode in [
        CryptoMode::Encrypted,
        CryptoMode::Integrity,
        CryptoMode::Plain,
    ] {
        let fx = fixture(mode);
        let mut bytes = Vec::new();
        fx.desc.encode(&mut bytes).expect("encode");
        fn cred(mode: CryptoMode, fx: &Fixture) -> Option<Credential<'_>> {
            match mode {
                CryptoMode::Encrypted => Some(Credential::Key(Unlock::KeyFile(&fx.key_file))),
                CryptoMode::Integrity => Some(Credential::Integrity(Unlock::KeyFile(&fx.key_file))),
                CryptoMode::Plain => None,
            }
        }
        for i in 0..bytes.len() {
            let mut bad = bytes.clone();
            bad[i] ^= 0x01;
            let Ok(d) = VolumeDescriptor::decode(&bad) else {
                continue;
            };
            let r = check_attach(
                &d,
                fx.keyring.as_deref(),
                &AttachRequest {
                    credential: cred(mode, &fx),
                    ..AttachRequest::default()
                },
            );
            assert!(r.is_err(), "{mode:?}: flipping byte {i} was accepted");
            if d.crypto_mode == mode {
                // The profile and family bytes also select the keyring MAC key
                // and the keyring itself, so they fail one step earlier.
                let earlier = i == 11 || (48..64).contains(&i);
                assert!(
                    matches!(r, Err(AttachError::BadMac))
                        || (earlier && matches!(r, Err(AttachError::WrongCredential))),
                    "{mode:?} byte {i}: {:?}",
                    r.err()
                );
            }
        }
    }
}

#[test]
fn passphrase_attach_is_the_same_check() {
    let mut desc = descriptor(
        CryptoMode::Integrity,
        Profile::Default,
        16_384,
        IdLen::Sixteen,
        [0x10; 16],
        [0x11; 16],
    );
    let c = keyring::create(
        Profile::Default,
        desc.family_id,
        RootKind::Ik,
        &NewSlot::Passphrase {
            passphrase: b"a passphrase",
            cost: Argon2Cost {
                m_kib: 19_456,
                t: 2,
                p: 1,
            },
        },
    )
    .expect("create");
    seal_descriptor(&mut desc, Some(&c.root)).expect("seal");
    let ok = check_attach(
        &desc,
        Some(&c.bytes),
        &AttachRequest {
            credential: Some(Credential::Integrity(Unlock::Passphrase(b"a passphrase"))),
            ..AttachRequest::default()
        },
    )
    .expect("attach");
    assert_eq!(
        ok.record_pin.expect("first use").mode,
        CryptoMode::Integrity
    );
    let err = check_attach(
        &desc,
        Some(&c.bytes),
        &AttachRequest {
            credential: Some(Credential::Integrity(Unlock::Passphrase(b"another"))),
            ..AttachRequest::default()
        },
    )
    .err()
    .expect("wrong passphrase");
    assert!(err.is_wrong_key_class());
}
