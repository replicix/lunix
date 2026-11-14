//! Step 5: the chunk index.
//!
//! The index is opened on a scratch copy of `index/`, so a check never writes
//! to the volume (opening an index removes `*.tmp` files and replaces a
//! damaged manifest). `--rebuild-index` is the only thing that touches the
//! real directory, in [`rebuild`].

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use lunix_format::{ChunkId, ObjId};
use lunix_iface::index::ChunkIndex;
use lunix_iface::store::{SegmentFooters, VolumeDir};
use lunix_index::LsmIndex;

use crate::code;
use crate::facts::Facts;
use crate::report::{Finding, Sink};
use crate::session::Session;

static SCRATCH_SEQ: AtomicU64 = AtomicU64::new(0);

/// A scratch directory removed on drop.
pub(crate) struct Scratch {
    path: PathBuf,
}

impl Scratch {
    /// A private directory (mode 0700) created exclusively inside `base`; a
    /// name that exists is never reused. Falls back to the system temporary
    /// directory when `base` is not writable.
    pub(crate) fn new(base: &Path, tag: &str) -> std::io::Result<Self> {
        match Self::make(base, tag) {
            Ok(s) => Ok(s),
            Err(_) => Self::make(&std::env::temp_dir(), tag),
        }
    }

    fn make(base: &Path, tag: &str) -> std::io::Result<Self> {
        use std::os::unix::fs::DirBuilderExt;
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        let mut last = None;
        for _ in 0..64 {
            let n = SCRATCH_SEQ.fetch_add(1, Ordering::Relaxed);
            let path = base.join(format!(
                ".lunix-fsck-{}-{nanos:08x}-{n:x}-{tag}",
                std::process::id()
            ));
            match fs::DirBuilder::new().mode(0o700).create(&path) {
                Ok(()) => return Ok(Self { path }),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => last = Some(e),
                Err(e) => return Err(e),
            }
        }
        Err(last.unwrap_or_else(|| std::io::Error::other("no scratch name")))
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// The on-disk index opened over a copy.
pub(crate) struct IndexView {
    pub(crate) idx: LsmIndex,
    _scratch: Scratch,
}

/// Opens the scratch copy and reports damage.
pub(crate) fn open_view(s: &Session, sink: &mut Sink) -> Option<IndexView> {
    let scratch = match Scratch::new(&s.scratch_base, "index") {
        Ok(x) => x,
        Err(e) => {
            sink.push(Finding::new(
                code::IO,
                "index/",
                format!("cannot make a scratch directory: {e}"),
            ));
            return None;
        }
    };
    let src = s.dir.join("index");
    let dst = scratch.path().join("index");
    if let Err(e) = fs::create_dir_all(&dst) {
        sink.push(Finding::new(code::IO, "index/", format!("{e}")));
        return None;
    }
    if let Ok(rd) = fs::read_dir(&src) {
        for ent in rd.flatten() {
            let Ok(name) = ent.file_name().into_string() else {
                continue;
            };
            if name.ends_with(".tmp") {
                continue;
            }
            let ok = ent.metadata().is_ok_and(|m| m.is_file());
            if !ok {
                continue;
            }
            let known = name == "manifest"
                || name == "manifest.prev"
                || crate::containers::parse_hex16(&name).is_some();
            if !known {
                sink.push(Finding::new(
                    code::INDEX_FILE,
                    format!("index/{name}"),
                    "not a run and not the manifest",
                ));
                continue;
            }
            if let Err(e) = fs::copy(ent.path(), dst.join(&name)) {
                sink.push(Finding::new(
                    code::IO,
                    format!("index/{name}"),
                    format!("cannot read: {e}"),
                ));
            }
        }
    }
    let idx = match LsmIndex::open(scratch.path(), s.vol.ids(), s.desc.id_len) {
        Ok(i) => i,
        Err(e) => {
            sink.push(Finding::new(
                code::INDEX_RUN,
                "index/",
                format!("the index does not open: {e} (repair: --rebuild-index)"),
            ));
            return None;
        }
    };
    let report = idx.open_report().clone();
    if report.fell_back || !report.discarded_runs.is_empty() {
        let runs: Vec<String> = report
            .discarded_runs
            .iter()
            .map(|r| format!("{:016x}", r.raw()))
            .collect();
        sink.push(Finding::new(
            code::INDEX_RUN,
            "index/",
            format!(
                "the newest manifest or a run it names is damaged{}; mount falls back and catches up (repair: --rebuild-index)",
                if runs.is_empty() {
                    String::new()
                } else {
                    format!(" (runs {})", runs.join(", "))
                }
            ),
        ));
    }
    Some(IndexView {
        idx,
        _scratch: scratch,
    })
}

/// A second index, built from the footers of the good segments, for the
/// delete-time invariant (M17 rule 5).
pub(crate) fn rebuilt(s: &Session, facts: &Facts) -> Option<IndexView> {
    let scratch = Scratch::new(&s.scratch_base, "rebuilt").ok()?;
    let idx = LsmIndex::open(scratch.path(), s.vol.ids(), s.desc.id_len).ok()?;
    let segs: Vec<ObjId> = facts
        .segments
        .iter()
        .filter(|(_, c)| c.good)
        .map(|(id, _)| *id)
        .collect();
    idx.rebuild(&segs, &s.vol).ok()?;
    Some(IndexView {
        idx,
        _scratch: scratch,
    })
}

/// Compares the index with the footers of the segments in use.
pub(crate) fn compare(view: &IndexView, facts: &Facts, sink: &mut Sink) {
    let in_use = |seg: &ObjId| {
        facts.named_segments.contains(seg)
            || facts.referenced_segments.contains(seg)
            || facts.condemned_ids.contains(seg)
    };
    let mut footer_ids: HashSet<ChunkId> = HashSet::new();
    for (id, c) in &facts.segments {
        if c.good && in_use(id) {
            footer_ids.extend(c.exts.iter().map(|e| e.id));
        }
    }
    let mut all_footer_ids: Option<HashSet<ChunkId>> = None;
    let mut seen: HashSet<ChunkId> = HashSet::new();
    let (mut stale, mut extra) = (0u64, 0u64);
    let mut first_stale: Option<String> = None;
    let r = view.idx.iter_entries(&mut |e| {
        let at_hint = facts.segments.get(&e.hint.seg).is_some_and(|c| {
            c.good
                && c.find(e.hint.off)
                    .is_some_and(|x| x.id == e.id && x.clen == e.hint.clen)
        });
        if at_hint {
            seen.insert(e.id);
            return;
        }
        // Not at its hint: stale if the chunk is stored somewhere, else extra.
        let all = all_footer_ids.get_or_insert_with(|| {
            facts
                .segments
                .values()
                .filter(|c| c.good)
                .flat_map(|c| c.exts.iter().map(|x| x.id))
                .collect()
        });
        if all.contains(&e.id) {
            stale += 1;
            first_stale.get_or_insert_with(|| format!("{}", e.id));
            seen.insert(e.id);
        } else {
            extra += 1;
        }
    });
    if let Err(e) = r {
        sink.push(Finding::new(
            code::INDEX_RUN,
            "index/",
            format!("the index cannot be iterated: {e} (repair: --rebuild-index)"),
        ));
        return;
    }
    let missing = footer_ids.iter().filter(|id| !seen.contains(id)).count() as u64;
    if missing > 0 {
        sink.push(Finding::new(
            code::INDEX_MISSING,
            "index/",
            format!("{missing} chunks in sealed segments have no index entry"),
        ));
    }
    if stale > 0 {
        sink.push(Finding::new(
            code::INDEX_STALE,
            "index/",
            format!(
                "{stale} entries name a copy that is not at that location (first id {})",
                first_stale.unwrap_or_default()
            ),
        ));
    }
    if extra > 0 {
        sink.push(Finding::new(
            code::INDEX_EXTRA,
            "index/",
            format!("{extra} entries are for chunks no segment holds"),
        ));
    }
}

/// `--rebuild-index`: rewrites the index from the footers of every segment in
/// use and persists it. Returns the number of entries in the new index.
pub(crate) fn rebuild(s: &Session, facts: &Facts) -> lunix_iface::Result<u64> {
    let idx = LsmIndex::open(&s.dir, s.vol.ids(), s.desc.id_len)?;
    // Every good sealed segment at or below the high-water mark (DESIGN 4.4
    // step 4, M17 rule 7) and the ones the retained roots reach: a stale hint
    // is resolved only through the index, so a segment left out here would
    // leave its entries unresolved and look like an orphan.
    let segs: Vec<ObjId> = facts
        .segments
        .iter()
        .filter(|(id, c)| {
            c.good
                && (id.raw() <= facts.high_water
                    || facts.named_segments.contains(id)
                    || facts.referenced_segments.contains(id)
                    || facts.condemned_ids.contains(id))
        })
        .map(|(id, _)| *id)
        .collect();
    let footers: &dyn SegmentFooters = &s.vol;
    idx.rebuild(&segs, footers)?;
    // Persist what the durable commits cover: every segment up to the highest
    // one the retained chain names (the engine's own rule, DESIGN 4.4).
    let upto = facts
        .segments
        .keys()
        .filter(|id| id.raw() <= facts.high_water)
        .copied()
        .max();
    if let Some(upto) = upto {
        idx.persist(upto)?;
    }
    let mut n = 0u64;
    idx.iter_entries(&mut |_| n += 1)?;
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn a_scratch_directory_is_private_and_never_reused() {
        let base = std::env::temp_dir();
        let a = Scratch::new(&base, "t").expect("a");
        let b = Scratch::new(&base, "t").expect("b");
        assert_ne!(a.path(), b.path());
        let mode = fs::metadata(a.path()).expect("meta").permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
        let p = a.path().to_path_buf();
        drop(a);
        assert!(!p.exists(), "removed on drop");
    }
}
