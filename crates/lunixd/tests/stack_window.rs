//! ADR-33 T2 through the real stack (no root, no block device): a ring record
//! damaged on disk after the write, and not resident, fails the cut that
//! reads it back; no chunk holds its bytes (W6).
//!
//! What follows is the engine's rule for a failure that cannot be retried
//! (`Corrupt` is `Permanent` in `lunix-destage`): the cut is abandoned with
//! the map untouched and its segments deleted, **no more commits happen in
//! this process**, writes fail and reads continue. The journal keeps the
//! flushed records, so the next start finds the damage with the recovery scan
//! (the damaged record ends the recovered prefix; `on_journal_damage` decides
//! whether the volume opens) instead of ever sealing it into a chunk.

mod common;

use std::path::Path;
use std::sync::Arc;

use common::volume::{damage_record, params, SIZE};
use common::{scratch, Driver};
use lunix_crypto::Argon2Cost;
use lunix_format::volume::CodecPolicy;
use lunix_format::{CryptoMode, IdLen};
use lunix_iface::ErrorKind;
use lunixd::stack::{create_volume, open_volume, CreateParams, CreatedVolume};

const CS: u32 = 64 << 10;
/// One maximal batch: the smallest cap the journal accepts.
const CAP: u64 = (1 << 20) + 4096;

/// A volume whose chunks are stored verbatim (`CodecPolicy::None`), so a
/// stored copy of written data is found byte for byte in its segments.
fn create_raw(parent: &Path) -> CreatedVolume {
    let tweak = |j: &mut lunix_format::local::VolumeJson| {
        j.journal_max = 256 << 20;
    };
    create_volume(&CreateParams {
        name: "w",
        parent,
        size_bytes: SIZE,
        chunk_size: CS,
        mode: CryptoMode::Plain,
        policy: CodecPolicy::None,
        level: 0,
        id_len: IdLen::Sixteen,
        map_fanout: 512,
        key_file: None,
        passphrase: None,
        argon2: Argon2Cost {
            m_kib: 19_456,
            t: 2,
            p: 1,
        },
        json: Some(&tweak),
    })
    .expect("create")
}

/// Incompressible bytes.
fn random(seed: u64, len: usize) -> Vec<u8> {
    let mut x = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 24) as u8
        })
        .collect()
}

/// Every file of the volume outside the journal, with its bytes.
fn store_files(voldir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    let mut stack = vec![voldir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                if p.file_name().is_some_and(|n| n == "journal") {
                    continue;
                }
                stack.push(p);
            } else if let Ok(b) = std::fs::read(&p) {
                out.push((p.display().to_string(), b));
            }
        }
    }
    out.sort();
    out
}

fn holds(files: &[(String, Vec<u8>)], needle: &[u8]) -> Option<String> {
    files
        .iter()
        .find(|(_, b)| b.windows(needle.len()).any(|w| w == needle))
        .map(|(n, _)| n.clone())
}

/// 24 records of 64 KiB on lane 0 of a raw-codec volume with a one-batch
/// window cap: the first 16 fill the window, the rest are not admitted and are
/// read back from the ring file.
fn rig(
    t: &Path,
) -> (
    CreatedVolume,
    lunixd::stack::VolumeStack,
    Driver,
    Vec<Vec<u8>>,
) {
    let v = create_raw(t);
    let mut p = params(&v, CryptoMode::Plain);
    p.journal_tweak = Some(Arc::new(|j| {
        j.mark_idle = None;
        j.resident_max = Some(CAP);
    }));
    let s = open_volume(p).expect("open");
    let mut d = Driver::new(&s.engine, 2, 1 << 20);
    let data: Vec<Vec<u8>> = (0..24).map(|i| random(500 + i, CS as usize)).collect();
    for (i, b) in data.iter().enumerate() {
        assert!(d.write_on(0, i as u64 * u64::from(CS), b).is_ok());
    }
    assert!(d.flush().is_ok());
    (v, s, d, data)
}

/// The positive control: the same writes without damage commit, and the
/// stored chunks hold the written bytes verbatim, so the search of the failing
/// run below can find them.
#[test]
fn without_damage_the_cut_commits_and_the_store_holds_the_bytes_verbatim() {
    let Some(t) = scratch("jwin-ok") else { return };
    let (v, s, _d, data) = rig(t.path());
    let before = s.engine.tip();
    s.engine
        .commit_now(false)
        .expect("an undamaged cut commits");
    assert_ne!(s.engine.tip(), before);
    let files = store_files(&v.voldir);
    for (i, b) in data.iter().enumerate() {
        assert!(
            holds(&files, &b[..64]).is_some() && holds(&files, &b[CS as usize - 64..]).is_some(),
            "record {i} is not stored verbatim"
        );
    }
}

#[test]
fn a_damaged_ring_record_that_is_not_resident_fails_the_cut_and_no_chunk_holds_it() {
    let Some(t) = scratch("jwin") else { return };
    let (v, s, mut d, data) = rig(t.path());
    let tip = s.engine.tip();
    let list_before: Vec<(String, usize)> = store_files(&v.voldir)
        .into_iter()
        .map(|(n, b)| (n, b.len()))
        .collect();

    // One byte of a record that is not resident is flipped on disk.
    let victim = 20;
    damage_record(&v.voldir, &data[victim][..64]);

    let err = s.engine.commit_now(false).expect_err("the cut must fail");
    assert_eq!(err.kind(), ErrorKind::Corrupt, "{err}");
    assert!(s.engine.is_stopped(), "no more commits in this process");
    assert_eq!(s.engine.tip(), tip, "the map and the tip are untouched");
    assert!(s.engine.commit_now(false).is_err(), "and none follows");

    // The failed cut left nothing behind: the files outside the journal are
    // the ones there were, and none holds any of the data.
    let files = store_files(&v.voldir);
    let list_after: Vec<(String, usize)> =
        files.iter().map(|(n, b)| (n.clone(), b.len())).collect();
    assert_eq!(list_after, list_before, "the cut's segments were deleted");
    for (i, b) in data.iter().enumerate() {
        assert!(
            holds(&files, &b[..64]).is_none() && holds(&files, &b[CS as usize - 64..]).is_none(),
            "record {i} reached the store: {:?}",
            holds(&files, &b[..64])
        );
    }
    let mut damaged = data[victim].clone();
    damaged[100] ^= 0xff;
    assert!(holds(&files, &damaged[64..128]).is_none());

    // Reads continue: a resident record reads back.
    assert_eq!(d.read(0, CS), data[0]);
}
