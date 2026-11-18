//! Step 3 (second half) and step 7: every map entry of every retained root
//! must resolve to an existing extent, through its hint or, for a stale hint,
//! through the index confirmed against the full id; then the ratios.

use std::collections::HashSet;

use lunix_format::{ChunkId, IdLen};
use lunix_iface::index::ChunkIndex;

use crate::code;
use crate::facts::Facts;
use crate::indexcheck::{self, IndexView};
use crate::mapwalk::EntryRec;
use crate::report::{CategoryBytes, Finding, Ratios, Sink};
use crate::session::Session;

/// What an entry resolved to.
#[derive(Clone, Copy, Debug)]
struct Found {
    id: ChunkId,
    clen: u32,
    ulen: u32,
    seg: lunix_format::ObjId,
    off: u32,
}

fn by_hint(facts: &Facts, id_len: IdLen, e: &EntryRec) -> Option<Found> {
    let c = facts.segments.get(&e.seg)?;
    if !c.good {
        return None;
    }
    let x = c.find(e.off)?;
    if x.clen != e.clen || (id_len != IdLen::Zero && !e.prefix.matches(&x.id)) {
        return None;
    }
    Some(Found {
        id: x.id,
        clen: x.clen,
        ulen: x.ulen,
        seg: e.seg,
        off: e.off,
    })
}

fn by_index(facts: &Facts, view: &IndexView, e: &EntryRec) -> Option<Found> {
    let cands = view.idx.get_by_prefix(&e.prefix).ok()?;
    cands.into_iter().find_map(|c| {
        if !e.prefix.matches(&c.id) {
            return None;
        }
        let seg = facts.segments.get(&c.hint.seg)?;
        if !seg.good {
            return None;
        }
        let x = seg.find(c.hint.off)?;
        (x.id == c.id && x.clen == c.hint.clen).then_some(Found {
            id: x.id,
            clen: x.clen,
            ulen: x.ulen,
            seg: c.hint.seg,
            off: c.hint.off,
        })
    })
}

fn same(a: &EntryRec, b: &EntryRec) -> bool {
    a.seg == b.seg && a.off == b.off && a.clen == b.clen && a.prefix == b.prefix
}

struct Ctx<'a> {
    facts: &'a Facts,
    id_len: IdLen,
    view: Option<&'a IndexView>,
    rebuilt: Option<Option<IndexView>>,
    s: &'a Session,
    referenced: HashSet<lunix_format::ObjId>,
    /// The extents entries resolved to, `(segment, offset)`.
    live: Vec<(lunix_format::ObjId, u32)>,
    unresolved: u64,
}

impl Ctx<'_> {
    /// Resolves one entry; findings go to `sink`.
    fn resolve(&mut self, e: &EntryRec, sink: &mut Sink) -> Option<Found> {
        if let Some(f) = by_hint(self.facts, self.id_len, e) {
            self.referenced.insert(f.seg);
            self.live.push((f.seg, f.off));
            return Some(f);
        }
        // A stale hint: the index, confirmed against the full id.
        let found = if self.id_len == IdLen::Zero {
            None
        } else {
            self.view.and_then(|v| by_index(self.facts, v, e))
        };
        let where_ = format!(
            "chunk {} names segment {:016x} offset {}",
            e.chunk,
            e.seg.raw(),
            e.off
        );
        match found {
            Some(f) => {
                self.referenced.insert(f.seg);
                self.live.push((f.seg, f.off));
                // The delete-time invariant: after the index is rebuilt from
                // the footers the entry still resolves.
                let facts = self.facts;
                let s = self.s;
                let rb = self
                    .rebuilt
                    .get_or_insert_with(|| indexcheck::rebuilt(s, facts));
                let ok = rb.as_ref().is_some_and(|v| by_index(facts, v, e).is_some());
                if !ok {
                    sink.push(Finding::new(
                        code::INDEX_INVARIANT,
                        "index/",
                        format!("{where_}: stale hint, resolves through the index but not after a rebuild"),
                    ));
                }
                Some(f)
            }
            None => {
                self.unresolved += 1;
                let why = match self.facts.segments.get(&e.seg) {
                    None => "the segment does not exist",
                    Some(c) if !c.good => "the segment failed its own checks",
                    Some(_) => "no extent of that length and id prefix is at that offset",
                };
                sink.push(Finding::new(
                    code::MAP_ENTRY_UNRESOLVED,
                    format!("segments/{:016x}.seg", e.seg.raw()),
                    format!("{where_}: {why}, and the index has no copy"),
                ));
                None
            }
        }
    }
}

pub(crate) fn run(s: &Session, view: Option<&IndexView>, sink: &mut Sink, facts: &mut Facts) {
    let Some(mut walk) = facts.walk.take() else {
        return;
    };
    let id_len = s.desc.id_len;
    let mut referenced = HashSet::new();
    let mut live: Vec<(lunix_format::ObjId, u32)>;
    let mut ratios = Ratios {
        chunk_size: s.desc.chunk_size,
        chunk_count: walk.tip_chunks,
        ..Ratios::default()
    };
    {
        let mut cx = Ctx {
            facts,
            id_len,
            view,
            rebuilt: None,
            s,
            referenced: HashSet::new(),
            live: Vec::new(),
            unresolved: 0,
        };
        let key = |e: &EntryRec| (e.seg, e.off, e.clen);
        // One record per distinct extent the tip resolves to (the entries are
        // grouped by extent), deduplicated by id at the end: a sorted vector
        // of 40 bytes per extent instead of a hash set.
        let mut unique: Vec<(ChunkId, u32, u32)> = Vec::new();
        walk.tip_entries.sort_unstable_by_key(key);
        let mut i = 0;
        while i < walk.tip_entries.len() {
            let mut j = i + 1;
            while j < walk.tip_entries.len() && same(&walk.tip_entries[i], &walk.tip_entries[j]) {
                j += 1;
            }
            let run = (j - i) as u64;
            ratios.mapped_chunks += run;
            match cx.resolve(&walk.tip_entries[i], sink) {
                Some(f) => unique.push((f.id, f.ulen, f.clen)),
                None => ratios.unresolved_entries += run,
            }
            i = j;
        }
        // The tip's entries are done with: free them before the rest.
        walk.tip_entries = Vec::new();
        // By (id, clen): one id stored twice with different lengths (a
        // re-sealed compaction output, a duplicate) counts the same copy on
        // every run.
        unique.sort_unstable_by_key(|u| (u.0, u.2));
        unique.dedup_by_key(|u| u.0);
        for &(_, ulen, clen) in &unique {
            ratios.unique_ulen += u64::from(ulen);
            ratios.unique_clen += u64::from(clen);
        }
        ratios.unique_chunks = unique.len() as u64;
        drop(unique);
        walk.other_entries.sort_unstable_by_key(key);
        let mut last: Option<EntryRec> = None;
        for e in &walk.other_entries {
            if last.as_ref().is_some_and(|l| same(l, e)) {
                continue;
            }
            last = Some(*e);
            cx.resolve(e, sink);
        }
        walk.other_entries = Vec::new();
        referenced.extend(cx.referenced);
        live = cx.live;
    }
    // Mark the live extents in their containers (the index comparison
    // counts a missing entry as lost only for those, M17.B review B-4).
    live.sort_unstable();
    live.dedup();
    for (seg, off) in live {
        if let Some(c) = facts.segments.get_mut(&seg) {
            if let Ok(i) = c.exts.binary_search_by_key(&off, |e| e.off) {
                c.exts[i].live = true;
            }
        }
    }
    ratios.hole_chunks = ratios.chunk_count.saturating_sub(ratios.mapped_chunks);
    if ratios.unique_chunks > 0 {
        ratios.dedup_ratio = ratios.mapped_chunks as f64 / ratios.unique_chunks as f64;
    }
    if ratios.unique_clen > 0 {
        ratios.compression_ratio = ratios.unique_ulen as f64 / ratios.unique_clen as f64;
    }
    if ratios.chunk_count > 0 {
        ratios.hole_share = ratios.hole_chunks as f64 / ratios.chunk_count as f64;
    }
    facts.referenced_segments = referenced;
    if walk.tip_complete {
        facts.ratios = Some(ratios);
    }
    facts.walk = Some(walk);
}

/// Segments and packs nothing references and nothing condemns.
pub(crate) fn orphans(sink: &mut Sink, facts: &Facts) {
    let code = if facts.closed_cleanly {
        code::ORPHAN
    } else {
        code::CRASH_LEFTOVER
    };
    let used_packs = facts
        .walk
        .as_ref()
        .map(|w| &w.used_packs)
        .cloned()
        .unwrap_or_default();
    if facts.walk.is_none() {
        // Without a walk nothing can be said about references.
        return;
    }
    // At or below the high-water mark an unreferenced container is GC's
    // garbage (its naming commit may already be collected): the GC hook's dry
    // run counts it. Above the mark it is crash residue or a leftover.
    let gc_garbage = |id: &lunix_format::ObjId| id.raw() <= facts.high_water;
    for id in facts.segments.keys() {
        if gc_garbage(id)
            || facts.named_segments.contains(id)
            || facts.referenced_segments.contains(id)
            || facts.condemned_ids.contains(id)
        {
            continue;
        }
        sink.push(Finding::new(
            code,
            format!("segments/{:016x}.seg", id.raw()),
            "no retained commit names it, no retained root references it and the condemned list does not name it",
        ));
    }
    for id in facts.packs.keys() {
        if gc_garbage(id)
            || facts.named_packs.contains(id)
            || used_packs.contains(id)
            || facts.condemned_ids.contains(id)
        {
            continue;
        }
        sink.push(Finding::new(
            code,
            format!("map/{:016x}", id.raw()),
            "no retained commit names it, no retained root reaches it and the condemned list does not name it",
        ));
    }
}

/// Bytes per category, from the files.
pub(crate) fn category_bytes(s: &Session, facts: &Facts) -> CategoryBytes {
    fn dir_bytes(p: &std::path::Path) -> u64 {
        let mut total = 0;
        let mut stack = vec![p.to_path_buf()];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
                match e.metadata() {
                    Ok(m) if m.is_dir() => stack.push(e.path()),
                    Ok(m) => total += m.len(),
                    Err(_) => {}
                }
            }
        }
        total
    }
    let seg = dir_bytes(&s.dir.join("segments"));
    let map = dir_bytes(&s.dir.join("map"));
    let commits = dir_bytes(&s.dir.join("state/commits"));
    let index = dir_bytes(&s.dir.join("index"));
    let journal = facts.journal_bytes;
    let all = dir_bytes(&s.dir);
    let cache = dir_bytes(&s.dir.join("cache"));
    let other = all.saturating_sub(seg + map + commits + index + journal + cache);
    CategoryBytes {
        segments: seg,
        map_packs: map,
        commits,
        index,
        journal,
        other,
    }
}
