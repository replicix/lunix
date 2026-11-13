//! The real crates wired by `lunixd::stack`, driven through the engine's queue
//! handlers (no root, no block device): create, write, commit, crash, reopen,
//! in all three modes and at both chunk sizes; compression and dedup through
//! the M9 adapter; the rollback floor (R4) and journal damage at open.

mod common;

use std::path::Path;
use std::sync::{Arc, Mutex};

use common::{pattern, scratch, Driver};
use lunix_crypto::Argon2Cost;
use lunix_engine::OnJournalDamage;
use lunix_format::volume::CodecPolicy;
use lunix_format::{CryptoMode, IdLen};
use lunix_iface::block::{BlockEngine, IoOp};
use lunix_iface::ctl::Secret;
use lunix_iface::store::VolumeDir;
use lunixd::stack::{
    create_volume, open_volume, AttachCredential, CreateParams, CreatedVolume, OpenParams,
    StackError, VolumeStack,
};

const PASS: &[u8] = b"wiring test passphrase";
const SIZE: u64 = 64 << 20;

fn create(parent: &Path, mode: CryptoMode, cs: u32) -> CreatedVolume {
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

fn params(v: &CreatedVolume, mode: CryptoMode) -> OpenParams {
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

fn open(v: &CreatedVolume, mode: CryptoMode) -> VolumeStack {
    open_volume(params(v, mode)).expect("open")
}

fn open_err(v: &CreatedVolume, mode: CryptoMode) -> StackError {
    open_volume(params(v, mode)).expect_err("open must fail")
}

/// Writes `n` chunks of pattern data starting at chunk `first`, plus a partial
/// 4 KiB write into chunk `first + n`.
fn fill(d: &mut Driver, cs: u64, first: u64, n: u64, seed: u64, model: &mut [u8]) {
    for i in 0..n {
        let off = (first + i) * cs;
        let data = pattern(seed + i, cs as usize);
        assert!(d.write(off, &data, false).is_ok());
        model[off as usize..(off + cs) as usize].copy_from_slice(&data);
    }
    let off = (first + n) * cs + 4096;
    let data = pattern(seed ^ 0xff, 4096);
    assert!(d.write(off, &data, false).is_ok());
    model[off as usize..off as usize + 4096].copy_from_slice(&data);
}

fn check(d: &mut Driver, model: &[u8], upto: u64) {
    let mut off = 0u64;
    while off < upto {
        let got = d.read(off, 256 << 10);
        assert!(
            got == model[off as usize..off as usize + (256 << 10)],
            "data differs in [{off}, +256 KiB)"
        );
        off += 256 << 10;
    }
}

fn roundtrip(mode: CryptoMode, cs: u32) {
    let Some(t) = scratch("rt") else { return };
    let v = create(t.path(), mode, cs);
    let cs64 = u64::from(cs);
    let mut model = vec![0u8; SIZE as usize];
    let upto = 64 * cs64;

    // First attach: no floor yet, the volume opens at genesis.
    let s = open(&v, mode);
    assert!(s.floor_before.is_none(), "first attach has no floor");
    assert_eq!(s.engine.size_bytes(), SIZE);
    let mut d = Driver::new(&s.engine, 2, 1 << 20);
    fill(&mut d, cs64, 0, 16, 1, &mut model);
    // The same content again: dedup through the index or within the cut.
    let dup = pattern(1, cs as usize);
    assert!(d.write(40 * cs64, &dup, false).is_ok());
    model[(40 * cs64) as usize..(41 * cs64) as usize].copy_from_slice(&dup);
    assert!(d.op(IoOp::WriteZeroes, 2 * cs64, cs as u32).is_ok());
    model[(2 * cs64) as usize..(3 * cs64) as usize].fill(0);
    assert!(d.flush().is_ok());
    s.engine.commit_now(false).expect("commit");
    let st = s.engine.stats();
    assert!(st.stored_clen < st.stored_ulen, "compressed: {st:?}");
    assert!(st.dedup_hits >= 1, "dedup: {st:?}");
    check(&mut d, &model, upto);
    // More writes after the commit, flushed but not committed: the journal
    // must bring them back after a crash.
    fill(&mut d, cs64, 20, 8, 99, &mut model);
    assert!(d.flush().is_ok());
    let floor = s
        .store
        .rollback_floor()
        .unwrap()
        .expect("floor after commit");
    assert!(floor.max_commit_seq.0 >= 1);
    // Crash: drop handlers and engine without a final commit.
    drop(d);
    drop(s);

    let s = open(&v, mode);
    assert!(s.floor_before.is_some());
    assert!(s.engine.recovery_report().replayed > 0, "journal replayed");
    let mut d = Driver::new(&s.engine, 2, 1 << 20);
    check(&mut d, &model, upto);
    s.engine.commit_now(false).expect("commit after recovery");
    drop(d);
    s.engine.quiesce().expect("final commit");
    drop(s);

    let s = open(&v, mode);
    assert_eq!(s.engine.recovery_report().replayed, 0, "clean close");
    let mut d = Driver::new(&s.engine, 2, 1 << 20);
    check(&mut d, &model, upto);
}

#[test]
fn roundtrip_integrity_16k() {
    roundtrip(CryptoMode::Integrity, 16 << 10);
}

#[test]
fn roundtrip_integrity_64k() {
    roundtrip(CryptoMode::Integrity, 64 << 10);
}

#[test]
fn roundtrip_encrypted_16k() {
    roundtrip(CryptoMode::Encrypted, 16 << 10);
}

#[test]
fn roundtrip_encrypted_64k() {
    roundtrip(CryptoMode::Encrypted, 64 << 10);
}

#[test]
fn roundtrip_plain_16k() {
    roundtrip(CryptoMode::Plain, 16 << 10);
}

#[test]
fn roundtrip_plain_64k() {
    roundtrip(CryptoMode::Plain, 64 << 10);
}

/// Writes one chunk, flushes and commits; returns the new tip.
fn one_commit(s: &VolumeStack, d: &mut Driver, seed: u64) -> u64 {
    assert!(d.write(0, &pattern(seed, 64 << 10), false).is_ok());
    assert!(d.flush().is_ok());
    match s.engine.commit_now(false).expect("commit") {
        lunix_engine::CommitOutcome::Committed { commit_seq, .. } => commit_seq.0,
        lunix_engine::CommitOutcome::Idle => panic!("idle"),
    }
}

fn close(s: VolumeStack, d: Driver) {
    drop(d);
    s.engine.quiesce().expect("quiesce");
    drop(s);
}

fn copy_dir(from: &Path, to: &Path) {
    let st = std::process::Command::new("cp")
        .arg("-a")
        .arg(from)
        .arg(to)
        .status()
        .unwrap();
    assert!(st.success());
}

fn commits_dir(v: &Path) -> std::path::PathBuf {
    v.join("state/commits")
}

/// R4 at the stack level in every mode: an older valid prefix is `Rollback`,
/// another commit at the floor's height is `Fork`, a forked chain that grew
/// past the floor is `Fork` too (the daemon's pre-check), and the honest
/// volume attaches.
fn rollback_and_fork(mode: CryptoMode) {
    let Some(t) = scratch("r4") else { return };
    let v = create(t.path(), mode, 64 << 10);
    let s = open(&v, mode);
    let mut d = Driver::new(&s.engine, 2, 1 << 20);
    assert_eq!(one_commit(&s, &mut d, 1), 1);
    close(s, d);
    // A = the volume at tip 1 (floor 1).
    let a = t.path().join("snap-a");
    copy_dir(&v.voldir, &a);
    let s = open(&v, mode);
    let mut d = Driver::new(&s.engine, 2, 1 << 20);
    assert_eq!(one_commit(&s, &mut d, 2), 2);
    assert_eq!(one_commit(&s, &mut d, 3), 3);
    close(s, d);
    let honest = t.path().join("honest-commits");
    copy_dir(&commits_dir(&v.voldir), &honest);

    // Rollback: the chain replaced by its older valid prefix (tip 1 < floor 3).
    for seq in [2u64, 3] {
        std::fs::remove_file(commits_dir(&v.voldir).join(format!("{seq:016x}"))).unwrap();
    }
    match open_err(&v, mode) {
        StackError::Rollback { floor, tip } => {
            assert_eq!((floor.0, tip.0), (3, 1));
        }
        e => panic!("expected Rollback, got {e:?}"),
    }

    // Fork at the floor's height: a chain grown from A to 3 with other data.
    let fork = CreatedVolume {
        uuid: v.uuid,
        voldir: a.clone(),
        generated_key_file: None,
    };
    let fork_key = a.join("integrity.key");
    let _ = fork_key;
    {
        // A has its own state/pins (floor 1); its own volume directory name
        // differs, which the keyring path does not care about (same parent).
        let s = open(&fork, mode);
        let mut d = Driver::new(&s.engine, 2, 1 << 20);
        assert_eq!(one_commit(&s, &mut d, 20), 2);
        assert_eq!(one_commit(&s, &mut d, 30), 3);
        close(s, d);
    }
    let replace = |src: &Path| {
        std::fs::remove_dir_all(commits_dir(&v.voldir)).unwrap();
        copy_dir(src, &commits_dir(&v.voldir));
    };
    replace(&commits_dir(&a));
    match open_err(&v, mode) {
        StackError::Fork { at } => assert_eq!(at.0, 3),
        e => panic!("expected Fork at the floor height, got {e:?}"),
    }
    // The fork grows past the floor: the commit at the floor's height (3) is
    // still retained by tip 4 and differs.
    {
        let s = open(&fork, mode);
        let mut d = Driver::new(&s.engine, 2, 1 << 20);
        assert_eq!(one_commit(&s, &mut d, 40), 4);
        close(s, d);
    }
    replace(&commits_dir(&a));
    match open_err(&v, mode) {
        StackError::Fork { at } => assert_eq!(at.0, 3),
        e => panic!("expected Fork below the tip, got {e:?}"),
    }

    // The honest chain attaches and the data is the last honest write.
    replace(&honest);
    let s = open(&v, mode);
    let mut d = Driver::new(&s.engine, 2, 1 << 20);
    assert_eq!(d.read(0, 64 << 10), pattern(3, 64 << 10));
    close(s, d);
}

#[test]
fn rollback_and_fork_integrity() {
    rollback_and_fork(CryptoMode::Integrity);
}

#[test]
fn rollback_and_fork_encrypted() {
    rollback_and_fork(CryptoMode::Encrypted);
}

#[test]
fn rollback_and_fork_plain() {
    rollback_and_fork(CryptoMode::Plain);
}

/// Captures log lines and how the journal files looked when each was written.
#[derive(Default)]
struct Capture {
    lines: Mutex<Vec<String>>,
}

struct CaptureSub(Arc<Capture>);

struct Msg(String);

impl tracing::field::Visit for Msg {
    fn record_debug(&mut self, f: &tracing::field::Field, v: &dyn std::fmt::Debug) {
        self.0.push_str(&format!(" {}={v:?}", f.name()));
    }
}

impl tracing::Subscriber for CaptureSub {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, e: &tracing::Event<'_>) {
        let mut m = Msg(String::new());
        e.record(&mut m);
        self.0.lines.lock().unwrap().push(m.0);
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

fn journal_bytes(voldir: &Path) -> Vec<(String, Vec<u8>)> {
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

/// Plan M12 (journal damage at startup): a damaged record above the floor is
/// logged (with its range) before the first lane open wipes it, and
/// `on_journal_damage = refuse` leaves the journal untouched.
#[test]
fn journal_damage_is_logged_before_the_wipe_and_refuse_keeps_it() {
    let Some(t) = scratch("jd") else { return };
    let mode = CryptoMode::Integrity;
    let v = create(t.path(), mode, 64 << 10);
    let s = open(&v, mode);
    let mut d = Driver::new(&s.engine, 2, 1 << 20);
    let marks: Vec<Vec<u8>> = (0..3).map(|i| pattern(1000 + i, 4096)).collect();
    for (i, m) in marks.iter().enumerate() {
        assert!(d.write_on(0, (i as u64) * (64 << 10), m).is_ok());
        assert!(d.flush().is_ok());
    }
    drop(d);
    drop(s); // a crash: nothing committed, three records on lane 0
             // Damage the middle record's data in place (its CRC fails; the third is
             // valid and above a gap).
    let mut hit = false;
    for e in std::fs::read_dir(v.voldir.join("journal"))
        .unwrap()
        .flatten()
    {
        let mut b = std::fs::read(e.path()).unwrap();
        if let Some(pos) = b.windows(64).position(|w| w == &marks[1][..64]) {
            b[pos + 100] ^= 0xff;
            std::fs::write(e.path(), &b).unwrap();
            hit = true;
        }
    }
    assert!(hit, "the second record was found in the journal");
    let before = journal_bytes(&v.voldir);

    let mut p = params(&v, mode);
    p.on_journal_damage = OnJournalDamage::Refuse;
    match open_volume(p) {
        Err(StackError::JournalDamage) => {}
        other => panic!("expected JournalDamage, got {other:?}"),
    }
    assert!(
        journal_bytes(&v.voldir) == before,
        "refuse touched the journal"
    );

    let cap = Arc::new(Capture::default());
    let sub = CaptureSub(Arc::clone(&cap));
    let s = tracing::subscriber::with_default(sub, || open(&v, mode));
    let logged = cap.lines.lock().unwrap().iter().any(|l| {
        l.contains("journal recovery stopped at a gap or skipped damage") && l.contains("damaged")
    });
    assert!(logged, "damage logged: {:?}", cap.lines.lock().unwrap());
    let rep = s.engine.recovery_report();
    assert!(
        !rep.damaged.is_empty() || rep.ignored_after_gap.is_some(),
        "{rep:?}"
    );
    // Logged before the wipe: the files are still as they were until the
    // first lane opens.
    assert!(
        journal_bytes(&v.voldir) == before,
        "wiped before the log line"
    );
    let mut d = Driver::new(&s.engine, 2, 1 << 20);
    assert!(
        journal_bytes(&v.voldir) != before,
        "the first lane open wipes"
    );
    // The first record survives, the damaged one and everything after it do not.
    assert_eq!(&d.read(0, 4096), &marks[0]);
    assert_eq!(d.read(2 * (64 << 10), 4096), vec![0u8; 4096]);
}

/// The descriptor's map fanout reaches the engine and the map (M11 re-review
/// minor 8): a fanout-256 volume round-trips across a leaf boundary.
#[test]
fn fanout_256_volume_round_trips() {
    let Some(t) = scratch("f256") else { return };
    let tweak = |j: &mut lunix_format::local::VolumeJson| j.journal_max = 256 << 20;
    let v = create_volume(&CreateParams {
        name: "f",
        parent: t.path(),
        size_bytes: 1024 * (16 << 10),
        chunk_size: 16 << 10,
        mode: CryptoMode::Plain,
        policy: CodecPolicy::Lz4,
        level: 0,
        id_len: IdLen::ThirtyTwo,
        map_fanout: 256,
        key_file: None,
        passphrase: None,
        argon2: Argon2Cost::DEFAULT,
        json: Some(&tweak),
    })
    .unwrap();
    let s = open(&v, CryptoMode::Plain);
    let mut d = Driver::new(&s.engine, 2, 1 << 20);
    for ci in [0u64, 255, 256, 700] {
        assert!(d
            .write(ci * (16 << 10), &pattern(ci, 16 << 10), false)
            .is_ok());
    }
    assert!(d.flush().is_ok());
    s.engine.commit_now(false).unwrap();
    close(s, d);
    let s = open(&v, CryptoMode::Plain);
    let mut d = Driver::new(&s.engine, 2, 1 << 20);
    for ci in [0u64, 255, 256, 700] {
        assert_eq!(d.read(ci * (16 << 10), 16 << 10), pattern(ci, 16 << 10));
    }
}
