//! Plan M12a gates over the control socket, against a `lunixd` child with
//! `device = "none"` (no root): every request type, `Detach` of a busy
//! volume, secrets never in the audit log, the daemon log or a response
//! (canary passphrase), the downgrade cases, and the rollback floor (R4) in
//! all three modes.

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use common::{scratch, Daemon};
use lunix_format::local::OnFull;
use lunix_iface::ctl::{
    Attach, Compress, Credential, ErrorCode, GcRequest, NameParam, Outcome, Request, Resize,
    Response, Secret, SpaceRequest, VolumeCreate, VolumeMode, VolumeState,
};
use lunixd::stack::{open_volume, AttachCredential, OpenParams};

const CANARY: &str = "canary-passphrase-7f3a9c";
const SIZE: u64 = 64 << 20;
const CS: u32 = 64 << 10;

fn hex(s: &str) -> String {
    s.bytes().map(|b| format!("{b:02x}")).collect()
}

fn create_req(name: &str, dir: &Path, mode: VolumeMode, passphrase: Option<&str>) -> Request {
    Request::VolumeCreate(VolumeCreate {
        name: name.into(),
        dir: dir.to_string_lossy().into_owned(),
        size_bytes: SIZE,
        chunk_size: CS,
        mode,
        compress: Compress::Auto,
        id_len: 16,
        map_fanout: None,
        key_file: None,
        passphrase: passphrase.map(Secret::from_text),
        store_max_percent: None,
    })
}

fn attach_req(name: &str, cred: Option<Credential>) -> Request {
    Request::Attach(Attach {
        name: name.into(),
        queues: None,
        depth: None,
        cpus: None,
        max_io_buf_bytes: None,
        credential: cred,
        on_journal_damage: None,
        allow_unverifiable_fork: false,
    })
}

fn pass_cred(p: &str) -> Credential {
    Credential {
        passphrase: Some(Secret::from_text(p)),
        key_file: None,
        fingerprint: None,
        mode: None,
    }
}

fn name(n: &str) -> NameParam {
    NameParam { name: n.into() }
}

#[track_caller]
fn ok(o: Outcome) -> Response {
    match o {
        Outcome::Ok(r) => r,
        Outcome::Err(e) => panic!("expected ok, got {e:?}"),
    }
}

#[track_caller]
fn code(o: Outcome) -> (ErrorCode, String) {
    match o {
        Outcome::Err(e) => (e.code, e.message),
        Outcome::Ok(r) => panic!("expected an error, got {r:?}"),
    }
}

fn voldir_of(d: &mut Daemon, n: &str) -> PathBuf {
    match ok(d.call(Request::VolumeList)) {
        Response::VolumeList { volumes } => {
            PathBuf::from(&volumes.iter().find(|v| v.name == n).expect("listed").dir)
        }
        r => panic!("{r:?}"),
    }
}

#[test]
fn every_request_type_returns_its_documented_result_and_secrets_stay_out() {
    let Some(t) = scratch("ctl") else { return };
    let vols = t.path().join("vols");
    let mut d = Daemon::start(t.path(), "");
    let mut seen = String::new();
    let mut call = |d: &mut Daemon, r: Request| {
        let o = d.call(r);
        seen.push_str(&format!("{o:?}\n"));
        seen.push_str(&serde_json::to_string(&o).unwrap());
        o
    };

    match ok(call(&mut d, Request::Ping)) {
        Response::Pong { version, .. } => assert_eq!(version, 1),
        r => panic!("{r:?}"),
    }
    assert!(
        matches!(ok(call(&mut d, Request::VolumeList)), Response::VolumeList { volumes } if volumes.is_empty())
    );

    // Create: the three modes; a duplicate name and plain key material refused.
    for (n, m, p) in [
        ("p", VolumeMode::Plain, None),
        ("i", VolumeMode::Integrity, None),
        ("e", VolumeMode::Encrypted, Some(CANARY)),
    ] {
        match ok(call(&mut d, create_req(n, &vols, m, p))) {
            Response::VolumeCreated(c) => {
                assert_eq!(c.mode, m);
                assert_eq!(c.key_file.is_some(), m == VolumeMode::Integrity, "{c:?}");
                assert!(Path::new(&c.dir).join("volume").exists());
            }
            r => panic!("{r:?}"),
        }
    }
    assert_eq!(
        code(call(
            &mut d,
            create_req("p", &vols, VolumeMode::Plain, None)
        ))
        .0,
        ErrorCode::Invalid
    );
    assert_eq!(
        code(call(
            &mut d,
            create_req("q", &vols, VolumeMode::Plain, Some("x"))
        ))
        .0,
        ErrorCode::Invalid
    );
    assert_eq!(
        code(call(
            &mut d,
            create_req("bad/name", &vols, VolumeMode::Plain, None)
        ))
        .0,
        ErrorCode::Invalid
    );
    match ok(call(&mut d, Request::VolumeList)) {
        Response::VolumeList { volumes } => {
            assert_eq!(volumes.len(), 3);
            for v in &volumes {
                // An encrypted volume's size is sealed in its commits.
                let want = if v.mode == VolumeMode::Encrypted {
                    0
                } else {
                    SIZE
                };
                assert_eq!(v.size_bytes, want, "{v:?}");
                assert!(v.device.is_none());
            }
        }
        r => panic!("{r:?}"),
    }

    // Status of a detached volume.
    match ok(call(&mut d, Request::VolumeStatus(name("i")))) {
        Response::VolumeStatus(s) => {
            assert_eq!(s.state, VolumeState::Detached);
            assert_eq!(
                (s.size_bytes, s.chunk_size, s.mode),
                (SIZE, CS, VolumeMode::Integrity)
            );
            assert_eq!(s.last_commit_seq, 0, "genesis");
        }
        r => panic!("{r:?}"),
    }
    assert_eq!(
        code(call(&mut d, Request::VolumeStatus(name("none")))).0,
        ErrorCode::NotFound
    );

    // Attach (the default integrity volume needs no credential).
    match ok(call(&mut d, attach_req("i", None))) {
        Response::Attached(a) => {
            assert_eq!((a.queues, a.recovered), (2, false));
        }
        r => panic!("{r:?}"),
    }
    assert_eq!(code(call(&mut d, attach_req("i", None))).0, ErrorCode::Busy);
    assert_eq!(
        code(call(&mut d, attach_req("none", None))).0,
        ErrorCode::NotFound
    );
    match ok(call(&mut d, Request::VolumeStatus(name("i")))) {
        Response::VolumeStatus(s) => {
            assert_eq!(s.state, VolumeState::Attached);
            assert!(s.journal_capacity >= 256 << 20, "{s:?}");
            // A fresh attach recovers nothing above the floor: no mark.
            assert_eq!((s.journal_marks_written, s.journal_marks_skipped), (0, 0));
            assert!(s.space.limit > 0);
            assert!(s.device.is_some());
        }
        r => panic!("{r:?}"),
    }

    // Resize: grow online (a forced FULL commit), shrink refused.
    match ok(call(
        &mut d,
        Request::Resize(Resize {
            name: "i".into(),
            size_bytes: 2 * SIZE,
        }),
    )) {
        Response::Resized { size_bytes } => assert_eq!(size_bytes, 2 * SIZE),
        r => panic!("{r:?}"),
    }
    assert_eq!(
        code(call(
            &mut d,
            Request::Resize(Resize {
                name: "i".into(),
                size_bytes: SIZE
            })
        ))
        .0,
        ErrorCode::Invalid
    );
    assert_eq!(
        code(call(
            &mut d,
            Request::Resize(Resize {
                name: "p".into(),
                size_bytes: 2 * SIZE
            })
        ))
        .0,
        ErrorCode::Invalid,
        "detached"
    );
    match ok(call(&mut d, Request::VolumeStatus(name("i")))) {
        Response::VolumeStatus(s) => assert!(s.last_commit_seq >= 1 && s.size_bytes == 2 * SIZE),
        r => panic!("{r:?}"),
    }

    // ADR-32 M2 and review M6: `create` wrote the default cap (200 %, at
    // least the size plus twice `commit.dirty_bytes`); a grow raises a
    // default cap to the new size and keeps an explicit one.
    let cap_req = |cache_max: Option<u64>| {
        Request::Space(SpaceRequest {
            name: "i".into(),
            free_floor: None,
            on_full: None,
            full_timeout_ms: None,
            cache_max,
        })
    };
    let cap = |r: Response| match r {
        Response::Space(b) => b.settings.cache_max,
        r => panic!("{r:?}"),
    };
    let floor = 2u64 << 30;
    assert_eq!(
        cap(ok(call(&mut d, cap_req(None)))),
        Some(2 * SIZE + floor),
        "raised with the grow"
    );
    let explicit = 10u64 << 30;
    assert_eq!(
        cap(ok(call(&mut d, cap_req(Some(explicit))))),
        Some(explicit)
    );
    match ok(call(
        &mut d,
        Request::Resize(Resize {
            name: "i".into(),
            size_bytes: 3 * SIZE,
        }),
    )) {
        Response::Resized { size_bytes } => assert_eq!(size_bytes, 3 * SIZE),
        r => panic!("{r:?}"),
    }
    assert_eq!(
        cap(ok(call(&mut d, cap_req(None)))),
        Some(explicit),
        "an explicit cap stays"
    );

    // Space: query, change (attached and detached).
    let space = |n: &str, on_full: Option<OnFull>| {
        Request::Space(SpaceRequest {
            name: n.into(),
            free_floor: None,
            on_full,
            full_timeout_ms: None,
            cache_max: None,
        })
    };
    match ok(call(&mut d, space("i", None))) {
        Response::Space(b) => assert_eq!(b.settings.on_full, OnFull::Wait),
        r => panic!("{r:?}"),
    }
    match ok(call(&mut d, space("i", Some(OnFull::Error)))) {
        Response::Space(b) => assert_eq!(b.settings.on_full, OnFull::Error),
        r => panic!("{r:?}"),
    }
    match ok(call(&mut d, space("p", Some(OnFull::Error)))) {
        Response::Space(b) => assert_eq!(b.settings.on_full, OnFull::Error),
        r => panic!("{r:?}"),
    }
    let ij = std::fs::read_to_string(voldir_of(&mut d, "i").join("volume.json")).unwrap();
    assert!(ij.contains("\"error\""), "{ij}");

    // GC: a dry run on the attached volume changes nothing; status answers
    // (control_gc.rs has the churn volume). Not attached: `gc` is refused,
    // status is idle.
    match ok(call(
        &mut d,
        Request::Gc(GcRequest {
            name: "i".into(),
            dry_run: true,
        }),
    )) {
        Response::Gc(s) => assert!(s.dry_run && s.pass == 0, "{s:?}"),
        r => panic!("{r:?}"),
    }
    assert!(matches!(
        ok(call(&mut d, Request::GcStatus(name("i")))),
        Response::GcStatus(_)
    ));
    assert_eq!(
        code(call(
            &mut d,
            Request::Gc(GcRequest {
                name: "p".into(),
                dry_run: false,
            })
        ))
        .0,
        ErrorCode::Invalid
    );
    assert!(
        matches!(ok(call(&mut d, Request::GcStatus(name("p")))), Response::GcStatus(s) if s.state.is_none() && s.last_pass.is_none())
    );

    // Detach of a busy volume is Busy and changes nothing.
    let idir = voldir_of(&mut d, "i");
    std::fs::write(idir.join("null-device-busy"), b"").unwrap();
    assert_eq!(
        code(call(&mut d, Request::Detach(name("i")))).0,
        ErrorCode::Busy
    );
    assert!(
        matches!(ok(call(&mut d, Request::VolumeStatus(name("i")))), Response::VolumeStatus(s) if s.state == VolumeState::Attached)
    );
    std::fs::remove_file(idir.join("null-device-busy")).unwrap();
    assert!(matches!(
        ok(call(&mut d, Request::Detach(name("i")))),
        Response::Detached
    ));
    assert_eq!(
        code(call(&mut d, Request::Detach(name("i")))).0,
        ErrorCode::Invalid
    );

    // The encrypted volume with the canary passphrase.
    assert!(matches!(
        ok(call(&mut d, attach_req("e", Some(pass_cred(CANARY))))),
        Response::Attached(_)
    ));
    assert!(matches!(
        ok(call(&mut d, Request::Detach(name("e")))),
        Response::Detached
    ));

    // Framing and version errors.
    let mut bad =
        serde_json::to_vec(&serde_json::json!({"v": 2, "id": 9, "method": "ping"})).unwrap();
    let mut framed = (bad.len() as u32).to_be_bytes().to_vec();
    framed.append(&mut bad);
    let r = String::from_utf8_lossy(&d.raw(&framed)).into_owned();
    assert!(r.contains("\"invalid\"") && r.contains("\"id\":9"), "{r}");
    let r = String::from_utf8_lossy(&d.raw(&[0x7f, 0xff, 0xff, 0xff])).into_owned();
    assert!(r.contains("\"invalid\"") && r.contains("exceeds"), "{r}");
    let mut junk = b"{nope".to_vec();
    let mut framed = (junk.len() as u32).to_be_bytes().to_vec();
    framed.append(&mut junk);
    let r = String::from_utf8_lossy(&d.raw(&framed)).into_owned();
    assert!(r.contains("malformed"), "{r}");

    // QuiesceAll: every device quiesced, then the daemon exits 0.
    assert!(matches!(
        ok(call(&mut d, attach_req("i", None))),
        Response::Attached(_)
    ));
    assert!(matches!(
        ok(call(&mut d, Request::QuiesceAll)),
        Response::Quiesced { devices: 1 }
    ));
    assert_eq!(d.wait_exit(), 0);

    // The registry survives a restart.
    let mut d = Daemon::start(t.path(), "");
    match ok(d.call(Request::VolumeList)) {
        Response::VolumeList { volumes } => assert_eq!(volumes.len(), 3),
        r => panic!("{r:?}"),
    }
    // SIGTERM is QuiesceAll.
    assert!(matches!(
        ok(d.call(attach_req("p", None))),
        Response::Attached(_)
    ));
    let pid = d.child.as_ref().unwrap().id();
    assert!(std::process::Command::new("kill")
        .arg("-TERM")
        .arg(pid.to_string())
        .status()
        .unwrap()
        .success());
    assert_eq!(d.wait_exit(), 0);

    // Secrets: not in the audit log, the daemon logs or any response.
    let audit = std::fs::read_to_string(t.path().join("state/audit.log")).unwrap();
    assert!(
        audit.contains("\"method\":\"attach\"") && audit.contains("\"volume\":\"e\""),
        "{audit}"
    );
    let mut logs = String::new();
    for e in std::fs::read_dir(t.path()).unwrap().flatten() {
        if e.file_name().to_string_lossy().starts_with("lunixd-") {
            logs.push_str(&std::fs::read_to_string(e.path()).unwrap());
        }
    }
    for (what, text) in [
        ("audit log", &audit),
        ("daemon log", &logs),
        ("responses", &seen),
    ] {
        for needle in [CANARY.to_string(), hex(CANARY)] {
            assert!(!text.contains(&needle), "secret found in the {what}");
        }
    }
}

#[test]
fn downgrade_cases_over_the_socket() {
    let Some(t) = scratch("dg") else { return };
    let vols = t.path().join("vols");
    let mut d = Daemon::start(t.path(), "");
    for (n, m, p) in [
        ("p", VolumeMode::Plain, None),
        ("i", VolumeMode::Integrity, None),
        ("e", VolumeMode::Encrypted, Some("right")),
    ] {
        ok(d.call(create_req(n, &vols, m, p)));
    }
    // Encrypted without a credential: allowed to fail (it does: needs one).
    let (c, m) = code(d.call(attach_req("e", None)));
    assert_eq!(c, ErrorCode::Invalid, "{m}");
    // A wrong passphrase is the wrong-key class.
    assert_eq!(
        code(d.call(attach_req("e", Some(pass_cred("wrong"))))).0,
        ErrorCode::WrongKmk
    );
    // The right passphrase with a wrong fingerprint: WrongKmk.
    let mut cred = pass_cred("right");
    cred.fingerprint = Some("00".repeat(16));
    assert_eq!(
        code(d.call(attach_req("e", Some(cred)))).0,
        ErrorCode::WrongKmk
    );
    assert!(matches!(
        ok(d.call(attach_req("e", Some(pass_cred("right"))))),
        Response::Attached(_)
    ));
    ok(d.call(Request::Detach(name("e"))));

    // The default integrity volume (key file beside it) needs nothing; a
    // wrong fingerprint is WrongKmk.
    let cred = Credential {
        passphrase: None,
        key_file: None,
        fingerprint: Some("ab".repeat(16)),
        mode: None,
    };
    assert_eq!(
        code(d.call(attach_req("i", Some(cred)))).0,
        ErrorCode::WrongKmk
    );
    assert!(matches!(
        ok(d.call(attach_req("i", None))),
        Response::Attached(_)
    ));
    ok(d.call(Request::Detach(name("i"))));

    // Plain: no credential works; any credential is Downgrade (key material
    // declares an encrypted volume; a fingerprint a keyed one).
    let key = t.path().join("some.key");
    std::fs::write(&key, [7u8; 32]).unwrap();
    for cred in [
        pass_cred("anything"),
        Credential {
            passphrase: None,
            key_file: Some(key.to_string_lossy().into_owned()),
            fingerprint: None,
            mode: None,
        },
        Credential {
            passphrase: None,
            key_file: None,
            fingerprint: Some("cd".repeat(16)),
            mode: None,
        },
    ] {
        assert_eq!(
            code(d.call(attach_req("p", Some(cred)))).0,
            ErrorCode::Downgrade
        );
    }
    assert!(matches!(
        ok(d.call(attach_req("p", None))),
        Response::Attached(_)
    ));
    ok(d.call(Request::Detach(name("p"))));

    // An integrity volume given an encryption credential (key material
    // without a declared mode means "encrypted"): Downgrade; the same key
    // declared as an integrity credential attaches.
    let ikey = voldir_of(&mut d, "i").join("integrity.key");
    let kcred = |mode| Credential {
        passphrase: None,
        key_file: Some(ikey.to_string_lossy().into_owned()),
        fingerprint: None,
        mode,
    };
    assert_eq!(
        code(d.call(attach_req("i", Some(kcred(None))))).0,
        ErrorCode::Downgrade
    );
    assert!(matches!(
        ok(d.call(attach_req("i", Some(kcred(Some(VolumeMode::Integrity)))))),
        Response::Attached(_)
    ));
    ok(d.call(Request::Detach(name("i"))));
    // An integrity volume with a passphrase slot, attached by declaring the
    // mode (request D), on any host.
    ok(d.call(create_req(
        "ip",
        &vols,
        VolumeMode::Integrity,
        Some("ipass"),
    )));
    assert_eq!(
        code(d.call(attach_req("ip", Some(pass_cred("ipass"))))).0,
        ErrorCode::Downgrade
    );
    let mut ic = pass_cred("ipass");
    ic.mode = Some(VolumeMode::Integrity);
    assert!(matches!(
        ok(d.call(attach_req("ip", Some(ic)))),
        Response::Attached(_)
    ));
    ok(d.call(Request::Detach(name("ip"))));

    // A plain volume put where an integrity volume was (same uuid, same
    // directory, its pin kept): the pin records a stronger mode, Downgrade.
    let idir = voldir_of(&mut d, "i");
    let pdir = voldir_of(&mut d, "p");
    std::fs::copy(pdir.join("volume"), idir.join("volume")).unwrap();
    assert_eq!(code(d.call(attach_req("i", None))).0, ErrorCode::Downgrade);
}

fn commits(dir: &Path) -> PathBuf {
    dir.join("state/commits")
}

fn cp(from: &Path, to: &Path) {
    assert!(std::process::Command::new("cp")
        .arg("-a")
        .arg(from)
        .arg(to)
        .status()
        .unwrap()
        .success());
}

fn grow(d: &mut Daemon, n: &str, size: u64) {
    match ok(d.call(Request::Resize(Resize {
        name: n.into(),
        size_bytes: size,
    }))) {
        Response::Resized { size_bytes } => assert_eq!(size_bytes, size),
        r => panic!("{r:?}"),
    }
}

/// Grows a copy of a volume directory through the library (the fork's own
/// history), `n` forced commits.
fn grow_copy(dir: &Path, mode: VolumeMode, pass: &str, from: u64, n: u64) {
    let mut p = OpenParams::new(dir.to_path_buf(), 2);
    if mode == VolumeMode::Encrypted {
        p.credential = AttachCredential {
            passphrase: Some(Secret::from_text(pass)),
            ..AttachCredential::default()
        };
    }
    p.tweak = Some(Arc::new(|c| c.background = false));
    let s = open_volume(p).expect("open the copy");
    for k in 0..n {
        lunix_iface::block::BlockEngine::grow(&*s.engine, from + (k + 1) * u64::from(CS) * 8)
            .unwrap();
    }
}

#[test]
fn rollback_and_fork_over_the_socket_in_all_modes() {
    let Some(t) = scratch("r4s") else { return };
    let vols = t.path().join("vols");
    let mut d = Daemon::start(t.path(), "");
    for (n, m) in [
        ("p", VolumeMode::Plain),
        ("i", VolumeMode::Integrity),
        ("e", VolumeMode::Encrypted),
    ] {
        let pass = "r4 pass";
        ok(d.call(create_req(
            n,
            &vols,
            m,
            (m == VolumeMode::Encrypted).then_some(pass),
        )));
        let cred = || (m == VolumeMode::Encrypted).then(|| pass_cred(pass));
        let dir = voldir_of(&mut d, n);
        // First attach on this host: succeeds, with the notice in the log.
        ok(d.call(attach_req(n, cred())));
        grow(&mut d, n, SIZE + 16 * u64::from(CS)); // commit 1
        ok(d.call(Request::Detach(name(n))));
        let a = vols.join(format!("fork-{n}"));
        cp(&dir, &a);
        ok(d.call(attach_req(n, cred())));
        grow(&mut d, n, SIZE + 32 * u64::from(CS)); // commit 2
        grow(&mut d, n, SIZE + 48 * u64::from(CS)); // commit 3 = floor
        ok(d.call(Request::Detach(name(n))));
        let honest = t.path().join(format!("honest-{n}"));
        cp(&commits(&dir), &honest);

        // An older valid prefix: Rollback.
        for s in [2u64, 3] {
            std::fs::remove_file(commits(&dir).join(format!("{s:016x}"))).unwrap();
        }
        let (c, m1) = code(d.call(attach_req(n, cred())));
        assert_eq!(c, ErrorCode::Rollback, "{n}: {m1}");

        // Forked at the floor's height.
        grow_copy(&a, m, pass, SIZE + 16 * u64::from(CS), 2);
        std::fs::remove_dir_all(commits(&dir)).unwrap();
        cp(&commits(&a), &commits(&dir));
        let (c, m2) = code(d.call(attach_req(n, cred())));
        assert_eq!(c, ErrorCode::Fork, "{n}: {m2}");

        // The honest chain attaches.
        std::fs::remove_dir_all(commits(&dir)).unwrap();
        cp(&honest, &commits(&dir));
        match ok(d.call(attach_req(n, cred()))) {
            Response::Attached(a) => assert!(!a.first_attach, "the floor exists since create"),
            r => panic!("{r:?}"),
        }
        match ok(d.call(Request::VolumeStatus(name(n)))) {
            Response::VolumeStatus(s) => {
                assert_eq!(s.last_commit_seq, 3);
                assert_eq!(s.size_bytes, SIZE + 48 * u64::from(CS));
                let f = s.rollback_floor.expect("floor shown");
                assert_eq!(f.max_commit_seq, 3);
                assert_eq!(f.tip_hash.len(), 64);
                assert!(s.last_commit_ms.is_some());
            }
            r => panic!("{r:?}"),
        }
        ok(d.call(Request::Detach(name(n))));
        // The floor is shown for a detached volume too.
        match ok(d.call(Request::VolumeStatus(name(n)))) {
            Response::VolumeStatus(s) => {
                assert_eq!(s.state, VolumeState::Detached);
                assert_eq!(s.rollback_floor.expect("floor").max_commit_seq, 3);
            }
            r => panic!("{r:?}"),
        }
        // A host with no floor (state/pins gone): the attach says first_attach.
        std::fs::remove_file(dir.join("state/pins")).unwrap();
        match ok(d.call(attach_req(n, cred()))) {
            Response::Attached(a) => assert!(a.first_attach, "{n}"),
            r => panic!("{r:?}"),
        }
        ok(d.call(Request::Detach(name(n))));
    }
    assert!(
        d.log_text().contains("first attach on this host"),
        "notice logged"
    );
}
