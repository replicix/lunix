//! 2 000 random single-byte flips over all the files of a volume: fsck never
//! panics, never hangs (60 s watchdog per run) and never reports clean when
//! the flip is in covered bytes.

mod common;

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use common::*;
use lunix_format::CryptoMode;

fn files(root: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(root).expect("read_dir").flatten() {
        let p = e.path();
        if p.is_dir() {
            files(&p, out);
        } else {
            out.push(p);
        }
    }
}

/// Files whose bytes are listed as not covered in the report (`uncovered`):
/// `LOCK` holds a pid, `volume.json` has no checksum, the checkpoint is a
/// hint, `cache/` is not read, and the journal's torn tails are legal.
fn covered(rel: &str) -> bool {
    !(rel == "LOCK"
        || rel == "volume.json"
        || rel == "state/checkpoint"
        || rel.starts_with("journal/")
        || rel.starts_with("cache/"))
}

fn run_with_watchdog(dir: PathBuf, opts: lunix_fsck::Options) -> lunix_fsck::Report {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let r = lunix_fsck::check(&dir, &opts);
        let _ = tx.send(r);
    });
    rx.recv_timeout(Duration::from_secs(60))
        .expect("fsck neither panicked nor hung within 60 s")
}

#[test]
fn two_thousand_random_flips_never_panic_hang_or_pass() {
    let mut total = 0u32;
    let mut not_clean_when_covered = 0u32;
    for (mode, flips) in [
        (CryptoMode::Plain, 700),
        (CryptoMode::Integrity, 650),
        (CryptoMode::Encrypted, 650),
    ] {
        let mut spec = Spec::new(mode, 16 * 1024);
        spec.chunks = 2048;
        spec.blobs = 120;
        spec.per_commit = 120;
        spec.journal_records = 40;
        let b = build(&spec);
        // The state a fix-up of the tree needs: a condemned list too.
        let mut all = Vec::new();
        files(b.tmp.path(), &mut all);
        all.sort();
        let mut rng = 0x1234_5678_9abc_def0u64 ^ (mode as u64);
        let mut next = move || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng
        };
        for i in 0..flips {
            let p = &all[(next() % all.len() as u64) as usize];
            let orig = std::fs::read(p).expect("read");
            if orig.is_empty() {
                continue;
            }
            let at = (next() % orig.len() as u64) as usize;
            let rel = p.strip_prefix(&b.dir).map_or_else(
                |_| p.to_string_lossy().into_owned(),
                |r| r.to_string_lossy().into_owned(),
            );
            let mut x = orig.clone();
            x[at] ^= 1 << (next() % 8);
            std::fs::write(p, &x).expect("write");
            let deep = i % 2 == 0;
            let r = run_with_watchdog(b.dir.clone(), if deep { b.deep() } else { b.opts() });
            std::fs::write(p, &orig).expect("restore");
            total += 1;
            if covered(&rel) && !rel.starts_with("../") {
                assert!(
                    r.exit_code() >= 1,
                    "[{}] flip at byte {at} of {rel} (deep {deep}) reported clean:\n{}",
                    mode_name(mode),
                    r.render_text()
                );
                not_clean_when_covered += 1;
            } else if rel.starts_with("../") || rel.contains("kmk.key") || rel.contains("keyring") {
                assert!(
                    r.exit_code() >= 1,
                    "[{}] flip in {rel} passed",
                    mode_name(mode)
                );
                not_clean_when_covered += 1;
            }
        }
        // Back to the original: clean.
        assert_eq!(b.fsck(&b.opts()).exit_code(), 0, "{}", mode_name(mode));
    }
    assert!(total >= 1990, "{total} flips ran");
    assert!(not_clean_when_covered > 1000);
}
