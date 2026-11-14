//! Findings, the report and its two renderings (human text and `--json`).

use std::collections::BTreeMap;
use std::fmt::Write as _;

use serde::Serialize;

use crate::code;

/// How bad a finding is. The exit code is the highest severity found.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// Information; exit code 0.
    Note,
    /// Exit code 1.
    Warning,
    /// Exit code 2.
    Error,
}

impl Severity {
    fn label(self) -> &'static str {
        match self {
            Self::Note => "note",
            Self::Warning => "warning",
            Self::Error => "error",
        }
    }
}

/// One finding.
#[derive(Clone, PartialEq, Eq, Debug, Serialize)]
pub struct Finding {
    /// The stable code, see [`crate::code`].
    pub code: &'static str,
    /// Its severity.
    pub severity: Severity,
    /// The object it is about, relative to the volume directory when it is a
    /// file.
    pub object: String,
    /// A fixed sentence plus the numbers that matter; never file contents.
    pub message: String,
    /// A byte range inside `object`, when the finding has one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub range: Option<(u64, u64)>,
}

impl Finding {
    /// A finding with the severity the code table gives.
    pub fn new(code: &'static str, object: impl Into<String>, message: impl Into<String>) -> Self {
        debug_assert!(code::info(code).is_some(), "undocumented code {code}");
        let severity = code::info(code).map_or(Severity::Error, |c| c.severity);
        Self {
            code,
            severity,
            object: object.into(),
            message: message.into(),
            range: None,
        }
    }

    /// Adds a byte range.
    #[must_use]
    pub fn with_range(mut self, start: u64, end: u64) -> Self {
        self.range = Some((start, end));
        self
    }
}

/// How many findings of one code are kept in full; the rest are counted.
pub const PER_CODE_CAP: usize = 24;

/// Collects findings, keeping at most [`PER_CODE_CAP`] per code.
#[derive(Default, Debug)]
pub(crate) struct Sink {
    pub(crate) findings: Vec<Finding>,
    counts: BTreeMap<&'static str, u64>,
    kept: BTreeMap<&'static str, usize>,
}

impl Sink {
    pub(crate) fn push(&mut self, f: Finding) {
        *self.counts.entry(f.code).or_default() += 1;
        let kept = self.kept.entry(f.code).or_default();
        if *kept < PER_CODE_CAP {
            *kept += 1;
            self.findings.push(f);
        }
    }

    pub(crate) fn has(&self, code: &str) -> bool {
        self.counts.contains_key(code)
    }

    pub(crate) fn into_parts(self) -> (Vec<Finding>, BTreeMap<&'static str, u64>) {
        (self.findings, self.counts)
    }
}

/// Bytes per category, recounted from the files.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug, Serialize)]
pub struct CategoryBytes {
    /// Sealed segments.
    pub segments: u64,
    /// Sealed map packs.
    pub map_packs: u64,
    /// Commit objects (including aside copies).
    pub commits: u64,
    /// Index runs and the manifest.
    pub index: u64,
    /// Journal ring files.
    pub journal: u64,
    /// Everything else below the volume directory except `cache/`.
    pub other: u64,
}

/// Ratios recounted from the tip's map and the footers.
#[derive(Clone, Copy, Default, PartialEq, Debug, Serialize)]
pub struct Ratios {
    /// The volume's chunk size.
    pub chunk_size: u32,
    /// Chunks the volume has (`size / chunk_size`).
    pub chunk_count: u64,
    /// Chunks with a map entry.
    pub mapped_chunks: u64,
    /// Chunks that are map holes (never written, zero or discarded).
    pub hole_chunks: u64,
    /// Distinct chunk ids behind the mapped chunks.
    pub unique_chunks: u64,
    /// Mapped entries that resolved to no footer entry (counted out of the
    /// figures below; each is also an error finding).
    pub unresolved_entries: u64,
    /// `mapped_chunks / unique_chunks`; 0 without mapped chunks.
    pub dedup_ratio: f64,
    /// Sum of `ulen` over the distinct chunks.
    pub unique_ulen: u64,
    /// Sum of `clen` over the distinct chunks.
    pub unique_clen: u64,
    /// `unique_ulen / unique_clen`; 0 without chunks.
    pub compression_ratio: f64,
    /// `hole_chunks / chunk_count`; 0 for an empty volume.
    pub hole_share: f64,
    /// Bytes per category.
    pub bytes: CategoryBytes,
}

/// What was looked at, for the summary line.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug, Serialize)]
pub struct Stats {
    /// Commit objects authenticated.
    pub commits: u64,
    /// Segments verified.
    pub segments: u64,
    /// Map packs verified.
    pub packs: u64,
    /// Extents opened (`--deep`) or scanned.
    pub extents: u64,
    /// Map nodes read.
    pub map_nodes: u64,
    /// Journal records valid above the floor.
    pub journal_records: u64,
}

/// A repair that was applied, for the output.
#[derive(Clone, PartialEq, Eq, Debug, Serialize)]
pub struct RepairAction {
    /// A short stable word: `remove_leftovers`, `delete_orphans`,
    /// `rebuild_index`, `rewrite_checkpoint`, `truncate_tip`, `reset_floor`,
    /// `gc`.
    pub action: &'static str,
    /// What happened, with the numbers.
    pub detail: String,
}

/// The result of a run.
#[derive(Clone, PartialEq, Debug, Serialize)]
pub struct Report {
    /// The volume uuid, once the descriptor parsed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub volume: Option<String>,
    /// `plain`, `integrity` or `encrypted`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<&'static str>,
    /// The verified tip's `commit_seq`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tip: Option<u64>,
    /// Whether `--deep` ran.
    pub deep: bool,
    /// The findings, at most [`PER_CODE_CAP`] per code.
    pub findings: Vec<Finding>,
    /// How many findings each code had in all.
    pub counts: BTreeMap<&'static str, u64>,
    /// The figures of step 7, when the tip's map could be walked.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ratios: Option<Ratios>,
    /// What was looked at.
    pub stats: Stats,
    /// Repairs applied, in order.
    pub repairs: Vec<RepairAction>,
    /// What this run does not cover, in words.
    pub uncovered: Vec<String>,
    /// The process's peak resident set in KiB (Linux `VmHWM`), when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peak_rss_kib: Option<u64>,
}

impl Report {
    /// The highest severity found, or `None` for a clean volume.
    pub fn worst(&self) -> Option<Severity> {
        self.counts
            .keys()
            .filter_map(|c| code::info(c).map(|i| i.severity))
            .filter(|s| *s > Severity::Note)
            .max()
    }

    /// 0 clean (notes allowed), 1 warnings only, 2 errors.
    pub fn exit_code(&self) -> i32 {
        match self.worst() {
            None => 0,
            Some(Severity::Warning) => 1,
            Some(_) => 2,
        }
    }

    /// True when a finding with `code` exists.
    pub fn has(&self, code: &str) -> bool {
        self.counts.contains_key(code)
    }

    /// The findings with `code`.
    pub fn with_code<'a>(&'a self, code: &'a str) -> impl Iterator<Item = &'a Finding> {
        self.findings.iter().filter(move |f| f.code == code)
    }

    /// The JSON document (`--json`).
    pub fn to_json(&self) -> String {
        #[derive(Serialize)]
        struct Doc<'a> {
            exit: i32,
            #[serde(flatten)]
            report: &'a Report,
        }
        serde_json::to_string_pretty(&Doc {
            exit: self.exit_code(),
            report: self,
        })
        .unwrap_or_else(|_| String::from("{}"))
    }

    /// The human text.
    pub fn render_text(&self) -> String {
        let mut s = String::new();
        let _ = writeln!(
            s,
            "lunix-fsck: volume {} ({}){}",
            self.volume.as_deref().unwrap_or("?"),
            self.mode.unwrap_or("mode unknown"),
            if self.deep { ", deep" } else { "" }
        );
        for f in &self.findings {
            let _ = write!(
                s,
                "{:<8} {:<26} {}: {}",
                f.severity.label(),
                f.code,
                f.object,
                f.message
            );
            if let Some((a, b)) = f.range {
                let _ = write!(s, " [bytes {a}..{b}]");
            }
            s.push('\n');
        }
        for (code, n) in &self.counts {
            let kept = self.findings.iter().filter(|f| f.code == *code).count() as u64;
            if *n > kept {
                let _ = writeln!(s, "         {code}: {} more not listed", n - kept);
            }
        }
        for r in &self.repairs {
            let _ = writeln!(s, "repaired {:<22} {}", r.action, r.detail);
        }
        if let Some(r) = &self.ratios {
            let _ = writeln!(
                s,
                "chunks: {} of {} mapped, {} holes ({:.1}% hole), {} distinct; dedup {:.3}, compression {:.3}",
                r.mapped_chunks,
                r.chunk_count,
                r.hole_chunks,
                r.hole_share * 100.0,
                r.unique_chunks,
                r.dedup_ratio,
                r.compression_ratio
            );
            let b = &r.bytes;
            let _ = writeln!(
                s,
                "bytes: segments {} map {} commits {} index {} journal {} other {}",
                b.segments, b.map_packs, b.commits, b.index, b.journal, b.other
            );
        }
        let st = &self.stats;
        let _ = writeln!(
            s,
            "checked: {} commits, {} segments, {} packs, {} map nodes, {} extents, {} journal records",
            st.commits, st.segments, st.packs, st.map_nodes, st.extents, st.journal_records
        );
        if !self.uncovered.is_empty() {
            let _ = writeln!(s, "not covered by this run:");
            for u in &self.uncovered {
                let _ = writeln!(s, "  {u}");
            }
        }
        let _ = writeln!(
            s,
            "result: {}",
            match self.exit_code() {
                0 => "clean",
                1 => "warnings",
                _ => "errors",
            }
        );
        s
    }
}
