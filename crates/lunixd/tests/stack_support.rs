//! `lunixd::testsupport` itself: the builder in every mode, the oracle's
//! prefix search, the crash's damage staying above the flush point; and the
//! defect the stack model test found in the engine's `refuse` rule (E1,
//! ignored until the engine is fixed; see the M12b report).
//!
//! `cargo test -p lunixd --features testsupport --test stack_support`

#![cfg(feature = "testsupport")]

use std::path::Path;

use lunix_engine::OnJournalDamage;
use lunix_format::journal::{scan_file, RECORD_HEADER_LEN};
use lunix_format::{CryptoMode, WriteSeq};
use lunix_iface::block::BlockEngine;
use lunixd::stack::StackError;
use lunixd::testsupport::{
    close, damage_journal, damage_unsynced, payload, DeviceModel, Driver, ModelOp, Scratch,
    TestRng, VolumeBuilder,
};

/// Every record of every journal file: (file, write_seq, flushed, bytes).
fn records(voldir: &Path) -> Vec<(String, u64, u64, Vec<u8>)> {
    let mut out = Vec::new();
    let mut names: Vec<_> = std::fs::read_dir(voldir.join("journal"))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "j"))
        .collect();
    names.sort();
    for p in names {
        let bytes = std::fs::read(&p).unwrap();
        let Ok(scan) = scan_file(&bytes, WriteSeq(0)) else {
            continue;
        };
        for r in scan.records {
            let h = r.header;
            let off = h.off as usize;
            let len = RECORD_HEADER_LEN + r.data.len();
            out.push((
                p.file_name().unwrap().to_string_lossy().into_owned(),
                h.write_seq.0,
                h.flushed.0,
                bytes[off..off + len].to_vec(),
            ));
        }
    }
    out
}

#[test]
fn every_mode_creates_opens_and_round_trips() {
    let t = Scratch::new("m12b-support-modes");
    for (i, (mode, pass)) in [
        (CryptoMode::Plain, None),
        (CryptoMode::Integrity, None),
        (CryptoMode::Integrity, Some("integrity pass")),
        (CryptoMode::Encrypted, None),
        (CryptoMode::Encrypted, Some("encrypted pass")),
    ]
    .into_iter()
    .enumerate()
    {
        let mut b = VolumeBuilder::new(t.path().join(format!("v{i}")))
            .manual()
            .mode(mode)
            .chunk_size(16 << 10)
            .size_bytes(4 << 20)
            .queues(2);
        if let Some(p) = pass {
            b = b.passphrase(p);
        }
        let vol = b.create().expect("create");
        assert_eq!(
            vol.key_file.is_some(),
            mode == CryptoMode::Encrypted && pass.is_none()
        );
        let s = vol.open().expect("open");
        let mut d = Driver::new(&s).expect("driver");
        assert_eq!(d.queues(), 2);
        let data = payload(5, 64 << 10);
        assert!(d.write(1, 128 << 10, &data, true).is_ok());
        assert!(d.durable().0 >= 1, "a FUA write is durable");
        drop(d);
        close(s).expect("close");
        let copy = vol.copy_as(&format!("copy{i}")).expect("copy");
        let s = copy.open().expect("open the copy");
        let mut d = Driver::new(&s).expect("driver");
        assert_eq!(d.read(0, 128 << 10, 64 << 10).expect("read"), data);
        drop(d);
        close(s).expect("close");
        assert!(vol.check().expect("check").is_clean());
        assert_eq!(vol.fsck_args().is_some(), pass.is_none());
    }
}

#[test]
fn the_oracle_finds_the_shortest_prefix_at_or_above_the_durable_one() {
    let mut m = DeviceModel::new(64 << 10);
    m.apply(ModelOp::Write {
        offset: 0,
        len: 8192,
        seed: 1,
    });
    let after1 = m.bytes().to_vec();
    m.all_durable();
    m.apply(ModelOp::Zero {
        offset: 4096,
        len: 4096,
    });
    m.apply(ModelOp::Grow(128 << 10));
    m.apply(ModelOp::Write {
        offset: 64 << 10,
        len: 4096,
        seed: 2,
    });
    assert_eq!(m.first_match(&after1), Some(1));
    assert_eq!(m.first_match(m.bytes()), Some(4));
    let grown = m.state_after(3);
    assert_eq!(grown.len(), 128 << 10);
    assert_eq!(m.first_match(&grown), Some(3));
    assert_eq!(m.first_match(&vec![0u8; 64 << 10]), None, "below durable");
    m.truncate(3);
    assert_eq!(m.len(), 3);
    assert_eq!(m.durable(), 3);
    assert_eq!(m.bytes(), &grown[..]);
}

#[test]
fn a_crash_damages_only_records_above_the_flush_point() {
    let t = Scratch::new("m12b-support-damage");
    let vol = VolumeBuilder::new(t.path())
        .manual()
        .mode(CryptoMode::Plain)
        .chunk_size(16 << 10)
        .size_bytes(4 << 20)
        .queues(2)
        .create()
        .expect("create");
    let mut damaged = 0;
    for round in 0..8u64 {
        let s = vol.open().expect("open");
        let mut d = Driver::new(&s).expect("driver");
        let mut rng = TestRng::new(round);
        for i in 0..40u64 {
            let off = rng.below(256) * 4096;
            assert!(d
                .write(i as usize, off, &payload(rng.next_u64(), 4096), false)
                .is_ok());
            if i == 20 {
                assert!(d.flush(0).is_ok());
            }
        }
        let bound = d.durable();
        assert!(bound.0 > 0);
        drop(d);
        drop(s);
        let before = records(&vol.voldir);
        let mut rng = TestRng::new(round ^ 0xd);
        let c = damage_journal(&vol.voldir, bound, &mut rng);
        assert!(c.durable_floor >= bound);
        let after = records(&vol.voldir);
        for r in before.iter().filter(|r| r.1 <= c.durable_floor.0) {
            assert!(
                after.iter().any(|a| a == r),
                "record {} at or below the flush point {} changed",
                r.1,
                c.durable_floor.0
            );
        }
        let above = before.iter().filter(|r| r.1 > c.durable_floor.0).count();
        assert_eq!(above, c.records_kept + c.records_damaged);
        damaged += c.records_damaged;
        // The volume opens under `refuse`: every damage is crash residue.
        let s = vol
            .open_with(|p| p.on_journal_damage = OnJournalDamage::Refuse)
            .expect("refuse opens over crash residue");
        close(s).expect("close");
    }
    assert!(damaged > 0, "no record was ever damaged");
}

/// Tears the record whose data starts with `mark`: one data byte flipped
/// (its CRC fails; the reader resyncs to the next record of the lane).
fn tear_record(voldir: &Path, mark: &[u8]) {
    for e in std::fs::read_dir(voldir.join("journal")).unwrap().flatten() {
        let mut b = std::fs::read(e.path()).unwrap();
        if let Some(pos) = b.windows(64).position(|w| w == &mark[..64]) {
            b[pos + 100] ^= 0xff;
            std::fs::write(e.path(), &b).unwrap();
            return;
        }
    }
    panic!("record not found");
}

/// Defect E1 (lunix-engine `recover::refuse_worthy`). A crash right after a
/// commit can tear journal records the commit's cut covers but no flush
/// synced. Damage at or below the floor costs nothing (on-disk-formats
/// §5.3), and recovery wipes only what follows the last record below its
/// stop point, so a torn record followed by a later record of its lane stays
/// in the ring file. Once a later flush raises the watermark above it, the
/// next recovery reports it as a damaged range whose `next_seq` is at or
/// below the watermark, and `refuse` refuses the volume although nothing
/// above the floor is missing. Expected: the open succeeds. Run with
/// `--ignored`; it fails until `refuse_worthy` ignores ranges whose records
/// all lie at or below the floor (`next_seq <= cut_seq + 1`).
#[test]
#[ignore = "defect E1 in lunix-engine: refuse refuses damage below the floor"]
fn refuse_ignores_torn_records_below_the_floor() {
    let t = Scratch::new("m12b-support-e1");
    let mut b = VolumeBuilder::new(t.path())
        .manual()
        .mode(CryptoMode::Plain)
        .chunk_size(16 << 10)
        .size_bytes(4 << 20)
        .queues(1);
    b.on_journal_damage = OnJournalDamage::Refuse;
    let vol = b.create().expect("create");
    let s = vol.open().expect("open");
    let mut d = Driver::new(&s).expect("driver");
    let a = payload(101, 4096);
    let torn = payload(102, 4096);
    let c = payload(103, 4096);
    assert!(d.write(0, 0, &a, false).is_ok());
    assert!(d.flush(0).is_ok());
    std::thread::sleep(std::time::Duration::from_millis(200));
    assert!(d.write(0, 4096, &torn, false).is_ok());
    assert!(d.write(0, 8192, &c, false).is_ok());
    // The commit covers both writes; neither is synced in the journal.
    s.engine.commit_now(false).expect("commit");
    drop(d);
    drop(s);
    tear_record(&vol.voldir, &torn);
    // Below the floor: the open succeeds and the committed data is there.
    let s = vol.open().expect("damage below the floor is harmless");
    assert!(!s.engine.recovery_report().damaged.is_empty());
    let mut d = Driver::new(&s).expect("driver");
    assert_eq!(d.read(0, 4096, 4096).expect("read"), torn);
    // A flush raises the watermark past the torn record; then a crash.
    assert!(d.write(0, 12288, &payload(104, 4096), false).is_ok());
    assert!(d.flush(0).is_ok());
    std::thread::sleep(std::time::Duration::from_millis(200));
    assert!(d.write(0, 16384, &payload(105, 4096), false).is_ok());
    drop(d);
    drop(s);
    match vol.open() {
        Ok(s) => {
            assert_eq!(s.engine.size_bytes(), 4 << 20);
        }
        Err(StackError::JournalDamage) => {
            panic!("refused damage that lies at or below the floor (defect E1)")
        }
        Err(e) => panic!("open: {e}"),
    }
}

#[test]
fn damage_unsynced_touches_only_unsealed_files_outside_journal_and_cache() {
    let t = Scratch::new("m12b-support-unsynced");
    let (mut removed, mut truncated, mut kept) = (0, 0, 0);
    for seed in 0..24u64 {
        let v = t.path().join(format!("v{seed}"));
        for d in ["segments", "map", "journal", "cache", "index"] {
            std::fs::create_dir_all(v.join(d)).unwrap();
        }
        let planted = [
            "segments/0000000000000009.open",
            "map/0000000000000004.tmp",
            "index/manifest.tmp",
        ];
        let spared = [
            "journal/q00-00000001.j.tmp",
            "cache/x.tmp",
            "segments/0000000000000001.seg",
        ];
        for f in planted.iter().chain(spared.iter()) {
            std::fs::write(v.join(f), vec![7u8; 8192]).unwrap();
        }
        let mut rng = TestRng::new(seed);
        let (r, tr) = damage_unsynced(&v, &mut rng);
        removed += r;
        truncated += tr;
        let mut seen = 0;
        for f in planted {
            match std::fs::metadata(v.join(f)) {
                Err(_) => seen += 1,
                Ok(m) if m.len() < 8192 => seen += 1,
                Ok(_) => kept += 1,
            }
        }
        assert_eq!(
            seen,
            r + tr,
            "removed and truncated files are the planted ones"
        );
        for f in spared {
            assert_eq!(
                std::fs::read(v.join(f)).unwrap(),
                vec![7u8; 8192],
                "{f} must not be touched"
            );
        }
    }
    assert!(
        removed > 0 && truncated > 0 && kept > 0,
        "{removed} {truncated} {kept}"
    );
}

#[test]
#[should_panic(expected = "drop every Driver")]
fn close_refuses_a_stack_with_a_live_driver() {
    let t = Scratch::new("m12b-support-live");
    let vol = VolumeBuilder::new(t.path())
        .manual()
        .mode(CryptoMode::Plain)
        .chunk_size(16 << 10)
        .size_bytes(1 << 20)
        .create()
        .expect("create");
    let s = vol.open().expect("open");
    let d = Driver::new(&s).expect("driver");
    let _keep = d;
    let _ = close(s);
}
