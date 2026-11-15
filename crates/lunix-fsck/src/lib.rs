//! Offline checker and repairer of a local Lunix volume (M14), as a library
//! and the `lunix-fsck` binary.
//!
//! `lunix-fsck` takes the volume `LOCK`, so it refuses a volume a daemon has
//! attached, and never writes unless a repair flag says so. A run is a list of
//! [`Finding`]s, each with a stable [`code`], a severity and an object; the
//! exit code is 0 for a clean volume, 1 when there are warnings only and 2
//! when there are errors.
//!
//! # Checks
//!
//! 1. **Volume and keys.** `volume` parses and its MAC (or checksum, for a
//!    plain volume) verifies; the keyring parses and, with a credential,
//!    unwraps; `volume.json` and `state/*` parse. `state/pins` is version 2 and
//!    a rejected file is an error, never "no floor".
//! 2. **Commit chain** from the tip down to `chain_floor`: contiguous
//!    `commit_seq`, `parent_hash`, non-decreasing `epoch`, the MAC or AEAD of
//!    every commit, `chain_floor` against `retain_last`. Rule R4 follows:
//!    a tip below the rollback floor is `rollback`, another commit at the
//!    floor's height is `fork`, and a walk back to the floor that cannot be
//!    completed is `fork_unverifiable` (ADR-31).
//! 3. **Map walk** from every retained root (R2): every node is opened with
//!    `kind = node`, must be codec 0, must carry the id its parent names and
//!    must hash to it. Every entry must resolve to an existing extent through
//!    its hint or, for a stale hint, through the index confirmed against the
//!    full id, and again after the index is rebuilt from the footers.
//! 4. **Segments and packs** (R3): trailer, footer opened through
//!    `MetaCodec::open`, `seg_hash` recomputed over the whole region, the
//!    footer against a scan of the extents. `--deep` opens every extent (tag
//!    or AEAD, bounded decompression, recomputed chunk id: a scan, R1).
//!    Containers nothing references are `orphan` (or `crash_leftover` after
//!    an unclean stop), `*.tmp` and `*.open` files are `leftover`.
//! 5. **Index** against the footers (missing, stale, extra); damaged runs.
//! 6. **Journal**, scanned like recovery with the tip's `cut_seq` as the floor.
//! 7. **Ratios and space**, recounted from the map and the footers.
//! 8. **Repairs**: [`Options::repair`] removes `*.tmp` and `*.open`, rebuilds a
//!    damaged index and rewrites a damaged checkpoint; `--delete-orphans`,
//!    `--rebuild-index`, `--truncate-tip` and `--reset-floor` are explicit.
//!
//! # What is not covered
//!
//! The report lists it: bytes of the journal after the last record of a lane
//! (a torn tail is legal), `volume.json` and `state/device` content that still
//! parses, `cache/`, and, without `--deep`, extent tags and ids (the extent
//! region is still covered whole by `seg_hash`).
//!
//! # Memory
//!
//! The walk streams the nodes, but the checks keep per-chunk state: the tip
//! root's entries (64 bytes per **mapped** chunk), the footers of every
//! container (about 48 bytes per extent), and sets of distinct chunk ids
//! (resolution, index comparison). Measured on a volume with 320 000
//! distinct and 400 000 mapped chunks: peak RSS 100 MiB, about **330 bytes per
//! distinct chunk**. A full 1 TiB volume of 64 KiB chunks (16 M chunks) needs
//! about 5 GiB, one of 16 KiB chunks about 20 GiB, plus the scratch copy of
//! the index on disk and one container's extent region (16 MiB) in memory.
//! A streaming rework (resolve per segment and drop, sorted vectors for the
//! id sets) is a listed follow-up.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod chain;
pub mod code;
mod containers;
mod facts;
pub mod gc;
mod indexcheck;
mod journal;
mod mapwalk;
mod repair;
mod report;
mod resolve;
mod session;
mod state;

use std::path::{Path, PathBuf};

pub use report::{
    CategoryBytes, Finding, Ratios, RepairAction, Report, Severity, Stats, PER_CODE_CAP,
};

use facts::Facts;
use lunix_iface::ctl::Secret;
use report::Sink;

/// How to unlock a keyed volume.
#[derive(Default)]
pub struct Credentials {
    /// A key file (the raw 32 bytes). For an `integrity` volume made by
    /// `volume create` without a passphrase, `<volume dir>/integrity.key` is
    /// tried when nothing else is given.
    pub key_file: Option<PathBuf>,
    /// A passphrase, any bytes. Zeroized on drop; never printed.
    pub passphrase: Option<Secret>,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials")
            .field("key_file", &self.key_file)
            .field(
                "passphrase",
                &self.passphrase.as_ref().map(|_| "<redacted>"),
            )
            .finish()
    }
}

/// What to do.
#[derive(Default)]
pub struct Options {
    /// `--deep`: open every extent.
    pub deep: bool,
    /// `--rebuild-index`: rewrite the index from the footers.
    pub rebuild_index: bool,
    /// `--repair`: the safe repairs, and the gate for the flags below.
    pub repair: bool,
    /// `--delete-orphans` (with `--repair`).
    pub delete_orphans: bool,
    /// `--gc` (with `--repair`): finish a pending GC pass; filled in by M17.B.
    pub gc: bool,
    /// `--truncate-tip` (with `--repair`).
    pub truncate_tip: bool,
    /// `--reset-floor` (with `--repair`): the explicit operator reset.
    pub reset_floor: bool,
    /// Credentials for a keyed volume.
    pub credentials: Credentials,
    /// `--repair-pins` (with `--repair`): replace a rejected `state/pins` by a
    /// fresh file whose floor is the verified tip. Gives up the recorded
    /// `max_keyring_seq`, `max_epoch` and pin; audited.
    pub repair_pins: bool,
    /// Where the scratch copy of the index goes: a private directory (mode
    /// 0700, created exclusively) is made inside it. Default: the directory
    /// that holds the volume directory, so it is on the same file system.
    pub scratch_dir: Option<PathBuf>,
}

impl std::fmt::Debug for Options {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Options")
            .field("deep", &self.deep)
            .field("rebuild_index", &self.rebuild_index)
            .field("repair", &self.repair)
            .field("delete_orphans", &self.delete_orphans)
            .field("gc", &self.gc)
            .field("truncate_tip", &self.truncate_tip)
            .field("reset_floor", &self.reset_floor)
            .field("repair_pins", &self.repair_pins)
            .finish_non_exhaustive()
    }
}

fn peak_rss_kib() -> Option<u64> {
    let text = std::fs::read_to_string("/proc/self/status").ok()?;
    text.lines()
        .find_map(|l| l.strip_prefix("VmHWM:"))
        .and_then(|v| v.trim().trim_end_matches("kB").trim().parse().ok())
}

fn run_checks(s: &session::Session, opts: &Options, sink: &mut Sink) -> Facts {
    let mut facts = Facts::default();
    state::check(s, sink, &mut facts);
    chain::check(s, sink, &mut facts);
    let tip = facts.tip.as_ref().map(|t| t.seq.0);
    state::check_checkpoint(s, tip, sink);
    if let Some(c) = &facts.condemned {
        facts.condemned_ids = gc::condemned_ids(c);
    }
    containers::check(s, opts.deep, sink, &mut facts);
    let view = indexcheck::open_view(s, sink);
    facts.index_ok = view.is_some();
    mapwalk::walk(s, sink, &mut facts);
    resolve::run(s, view.as_ref(), sink, &mut facts);
    resolve::orphans(sink, &facts);
    if let Some(v) = &view {
        indexcheck::compare(v, &facts, sink);
    }
    journal::check(s, sink, &mut facts);
    if s.desc.crypto_mode == lunix_format::CryptoMode::Plain {
        sink.push(Finding::new(
            code::PLAIN_NOTICE,
            "volume",
            "a plain volume detects corruption only: no check can detect a writer who recomputed the checksums",
        ));
    }
    let ctx = gc::GcContext {
        session: s,
        facts: &facts,
        repair: false,
    };
    for f in gc::check_gc(&ctx) {
        sink.push(f);
    }
    let bytes = resolve::category_bytes(s, &facts);
    if let Some(r) = &mut facts.ratios {
        r.bytes = bytes;
    }
    facts
}

fn uncovered(deep: bool) -> Vec<String> {
    let mut v = vec![
        "journal: bytes after the last record of a lane and the zero padding of a batch (a torn tail is legal)".to_owned(),
        "volume.json and state/device: content that still parses (no checksum)".to_owned(),
        "state/checkpoint: a hint; damage is a warning, never an error".to_owned(),
        "cache/: not looked at".to_owned(),
    ];
    if !deep {
        v.push("extents: tags and ids are not opened without --deep (the extent region is covered whole by seg_hash)".to_owned());
    }
    v
}

/// Checks (and, with the repair flags, repairs) the volume in `dir`.
///
/// Never panics on a damaged volume and never returns an error: every
/// failure, including a volume that cannot be opened, is a finding.
pub fn check(dir: &Path, opts: &Options) -> Report {
    // The keyring lives beside the volume directory: resolve a relative path
    // first (`Path::new("vol").parent()` is the empty path, not `.`).
    let abs = std::path::absolute(dir).unwrap_or_else(|_| dir.to_path_buf());
    let dir = abs.as_path();
    let mut sink = Sink::default();
    let mut ident = None;
    let session = session::open(dir, opts, &mut sink, &mut ident);
    let mut facts = Facts::default();
    let mut repairs = Vec::new();
    if let Some(s) = &session {
        facts = run_checks(s, opts, &mut sink);
        let wants_repair = opts.repair
            || opts.rebuild_index
            || opts.truncate_tip
            || opts.reset_floor
            || opts.repair_pins
            || opts.delete_orphans
            || opts.gc;
        if wants_repair {
            let mut refusals = Sink::default();
            let recheck = |sink: &mut Sink| run_checks(s, opts, sink);
            repair::run(
                s,
                opts,
                &mut facts,
                &mut sink,
                &mut refusals,
                &mut repairs,
                &recheck,
            );
            if !repairs.is_empty() {
                // Report the state after the repairs.
                sink = Sink::default();
                facts = run_checks(s, opts, &mut sink);
            }
            for f in refusals.findings {
                sink.push(f);
            }
        }
    }
    let (findings, counts) = sink.into_parts();
    Report {
        volume: ident.as_ref().map(|i| i.volume.clone()),
        mode: ident.as_ref().map(|i| i.mode),
        tip: facts.tip.as_ref().map(|t| t.seq.0),
        deep: opts.deep,
        findings,
        counts,
        ratios: facts.ratios,
        stats: facts.stats,
        repairs,
        uncovered: {
            let mut u = uncovered(opts.deep);
            u.extend(facts.uncovered.iter().cloned());
            u
        },
        peak_rss_kib: peak_rss_kib(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_code_is_unique_and_documented() {
        let mut seen = std::collections::HashSet::new();
        for c in code::ALL {
            assert!(seen.insert(c.code), "duplicate code {}", c.code);
            assert!(!c.doc.is_empty());
            assert!(
                c.code.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'),
                "code {} is not snake_case",
                c.code
            );
        }
    }

    #[test]
    fn a_missing_directory_is_a_finding_not_a_panic() {
        let r = check(
            Path::new("/nonexistent/lunix-fsck-test"),
            &Options::default(),
        );
        assert!(r.has(code::VOLUME_PARSE) || r.has(code::IO));
        assert_eq!(r.exit_code(), 2);
    }
}
