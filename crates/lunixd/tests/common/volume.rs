//! Volume helpers shared by the wiring and logging tests.

use std::path::Path;
use std::sync::Arc;

use lunix_crypto::Argon2Cost;
use lunix_format::volume::CodecPolicy;
use lunix_format::{CryptoMode, IdLen};
use lunix_iface::ctl::Secret;
use lunixd::stack::{
    create_volume, open_volume, AttachCredential, CreateParams, CreatedVolume, OpenParams,
    StackError, VolumeStack,
};

use super::pattern;
use super::Driver;

pub const PASS: &[u8] = b"wiring test passphrase";
pub const SIZE: u64 = 64 << 20;

pub fn create(parent: &Path, mode: CryptoMode, cs: u32) -> CreatedVolume {
    let tweak = |j: &mut lunix_format::local::VolumeJson| {
        j.journal_max = 256 << 20;
    };
    create_volume(&CreateParams {
        name: "w",
        parent,
        size_bytes: SIZE,
        chunk_size: cs,
        mode,
        policy: CodecPolicy::Auto,
        level: 1,
        id_len: IdLen::Sixteen,
        map_fanout: 512,
        key_file: None,
        passphrase: (mode == CryptoMode::Encrypted).then_some(PASS),
        argon2: Argon2Cost {
            m_kib: 19_456,
            t: 2,
            p: 1,
        },
        json: Some(&tweak),
    })
    .expect("create")
}

pub fn params(v: &CreatedVolume, mode: CryptoMode) -> OpenParams {
    let mut p = OpenParams::new(v.voldir.clone(), 2);
    if mode == CryptoMode::Encrypted {
        p.credential = AttachCredential {
            passphrase: Some(Secret::new(PASS.to_vec())),
            ..AttachCredential::default()
        };
    }
    p.tweak = Some(Arc::new(|c| {
        c.background = false;
        c.read_helpers = 2;
    }));
    p.destage_workers = Some(2);
    p
}

pub fn open(v: &CreatedVolume, mode: CryptoMode) -> VolumeStack {
    open_volume(params(v, mode)).expect("open")
}

/// Opens without journal marks: a mark written after a slow cut (a loaded
/// host) takes a number above the cut, and the close then commits once more,
/// which shifts the commit numbers the R4 tests count.
pub fn open_unmarked(v: &CreatedVolume, mode: CryptoMode) -> VolumeStack {
    let mut p = params(v, mode);
    p.journal_tweak = Some(Arc::new(|j| j.mark_idle = None));
    open_volume(p).expect("open")
}

pub fn open_err(v: &CreatedVolume, mode: CryptoMode) -> StackError {
    open_volume(params(v, mode)).expect_err("open must fail")
}

pub fn journal_bytes(voldir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut v: Vec<(String, Vec<u8>)> = std::fs::read_dir(voldir.join("journal"))
        .unwrap()
        .flatten()
        .map(|e| {
            (
                e.file_name().to_string_lossy().into_owned(),
                std::fs::read(e.path()).unwrap(),
            )
        })
        .collect();
    v.sort();
    v
}

/// Three flushed 4 KiB records on lane 0 of a plain volume, then a crash.
pub fn torn_tail_volume(t: &Path) -> (CreatedVolume, Vec<Vec<u8>>) {
    let mode = CryptoMode::Plain;
    let v = create(t, mode, 64 << 10);
    // No journal marks: one written while the host is slow between a flush
    // and the next write takes a number and shifts the ones tests assert.
    let s = open_unmarked(&v, mode);
    let mut d = Driver::new(&s.engine, 2, 1 << 20);
    let marks: Vec<Vec<u8>> = (0..3).map(|i| pattern(2000 + i, 4096)).collect();
    for (i, m) in marks.iter().enumerate() {
        assert!(d.write_on(0, (i as u64) * (64 << 10), m).is_ok());
        assert!(d.flush().is_ok());
    }
    drop(d);
    drop(s);
    (v, marks)
}

/// Flips a data byte of the record whose data starts with `mark`.
pub fn damage_record(voldir: &Path, mark: &[u8]) {
    let mut hit = false;
    for e in std::fs::read_dir(voldir.join("journal")).unwrap().flatten() {
        let mut b = std::fs::read(e.path()).unwrap();
        if let Some(pos) = b.windows(64).position(|w| w == &mark[..64]) {
            b[pos + 100] ^= 0xff;
            std::fs::write(e.path(), &b).unwrap();
            hit = true;
        }
    }
    assert!(hit, "the record was found in the journal");
}
