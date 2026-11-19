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

use common::volume::{create, damage_record, params};
use common::{scratch, Driver};
use lunix_format::CryptoMode;
use lunix_iface::ErrorKind;
use lunixd::stack::open_volume;

const CS: u32 = 64 << 10;
/// One maximal batch: the smallest cap the journal accepts.
const CAP: u64 = (1 << 20) + 4096;

/// Incompressible bytes (a chunk stores them raw, so a stored copy would be
/// found verbatim).
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

#[test]
fn a_damaged_ring_record_that_is_not_resident_fails_the_cut_and_no_chunk_holds_it() {
    let Some(t) = scratch("jwin") else { return };
    let mode = CryptoMode::Plain;
    let v = create(t.path(), mode, CS);
    let mut p = params(&v, mode);
    p.journal_tweak = Some(Arc::new(|j| {
        j.mark_idle = None;
        j.resident_max = Some(CAP);
    }));
    let s = open_volume(p).expect("open");
    let mut d = Driver::new(&s.engine, 2, 1 << 20);

    // 24 records of 64 KiB on lane 0: the first 16 fill the window, the rest
    // are not admitted and are read back from the ring file.
    let data: Vec<Vec<u8>> = (0..24).map(|i| random(500 + i, CS as usize)).collect();
    for (i, b) in data.iter().enumerate() {
        assert!(d.write_on(0, i as u64 * u64::from(CS), b).is_ok());
    }
    assert!(d.flush().is_ok());
    let tip = s.engine.tip();

    // One byte of a record that is not resident is flipped on disk.
    let victim = 20;
    damage_record(&v.voldir, &data[victim][..64]);

    let err = s.engine.commit_now(false).expect_err("the cut must fail");
    assert_eq!(err.kind(), ErrorKind::Corrupt, "{err}");
    assert!(s.engine.is_stopped(), "no more commits in this process");
    assert_eq!(s.engine.tip(), tip, "the map and the tip are untouched");
    assert!(s.engine.commit_now(false).is_err(), "and none follows");

    // No chunk, segment or map object of the volume holds any of the data
    // (the segments of the failed cut were deleted).
    let files = store_files(&v.voldir);
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
