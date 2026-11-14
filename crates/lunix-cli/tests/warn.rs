//! The plain-volume backend warning (phase 2 wires the command), against the
//! fake server.

mod common;

use common::{status_body, Env};
use lunix_cli::client::Client;
use lunix_cli::warn::{check_plain_backend, confirm_plain_backend, PLAIN_BACKEND_WARNING};
use lunix_iface::ctl::{Response, VolumeMode};

#[test]
fn a_plain_volume_gets_the_backend_warning() {
    let env = Env::new();
    env.ok(
        "VolumeStatus",
        Response::VolumeStatus(Box::new(status_body("p", VolumeMode::Plain))),
    );
    let mut c = Client::connect(&env.socket()).unwrap();
    let w = check_plain_backend(&mut c, "p").unwrap();
    assert_eq!(w, Some(PLAIN_BACKEND_WARNING));
    common::assert_snapshot("plain-backend-warning.txt", PLAIN_BACKEND_WARNING);
}

#[test]
fn keyed_volumes_get_none() {
    for mode in [VolumeMode::Integrity, VolumeMode::Encrypted] {
        let env = Env::new();
        env.ok(
            "VolumeStatus",
            Response::VolumeStatus(Box::new(status_body("k", mode))),
        );
        let mut c = Client::connect(&env.socket()).unwrap();
        assert_eq!(check_plain_backend(&mut c, "k").unwrap(), None);
    }
}

#[test]
fn the_confirmation_needs_a_yes_unless_skipped() {
    let mut err = Vec::new();
    let mut no = std::io::Cursor::new(&b"\n"[..]);
    assert!(!confirm_plain_backend(PLAIN_BACKEND_WARNING, false, &mut no, &mut err).unwrap());
    let mut err = Vec::new();
    let mut yes = std::io::Cursor::new(&b"y\n"[..]);
    assert!(confirm_plain_backend(PLAIN_BACKEND_WARNING, false, &mut yes, &mut err).unwrap());
    let mut err = Vec::new();
    let mut none = std::io::Cursor::new(&b""[..]);
    assert!(confirm_plain_backend(PLAIN_BACKEND_WARNING, true, &mut none, &mut err).unwrap());
    assert!(String::from_utf8(err).unwrap().contains("corruption only"));
}
