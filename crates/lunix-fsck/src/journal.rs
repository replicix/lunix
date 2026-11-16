//! Step 6: the journal, scanned like recovery with the tip's `cut_seq` as the
//! floor (DESIGN section 4.1, D17).
//!
//! This is the normative reader (`lunix_format::journal::scan_file`) applied
//! to every ring file of every lane and of the mark ring (on-disk-formats
//! §5.1-5.3), plus the merge by `write_seq` and the flushed watermark
//! (ADR-4); it never writes.

use std::fs;
use std::ops::Range;

use lunix_format::journal::{scan_file, RecordKind, BLOCK};
use lunix_format::WriteSeq;
use lunix_iface::journal::MARK_RING_LANE;

use crate::code;
use crate::facts::Facts;
use crate::report::{Finding, Sink};
use crate::session::Session;

/// A damaged byte range of one ring file, with its neighbours in the lane.
#[derive(Clone, Debug)]
pub(crate) struct Damage {
    pub(crate) file: String,
    pub(crate) range: Range<u64>,
    /// Last record accepted before the damage in its lane (0 if none).
    pub(crate) prev_seq: u64,
    /// First record accepted after it in its lane.
    pub(crate) next_seq: Option<u64>,
}

/// Everything one pass over `journal/` finds.
#[derive(Default, Debug)]
pub(crate) struct Scan {
    /// Every valid record's and mark's `write_seq`, sorted: a mark fills its
    /// number for the contiguity of the merge (on-disk-formats §5.3).
    pub(crate) seqs: Vec<u64>,
    /// The marks' `write_seq`s, sorted (a subset of `seqs`).
    pub(crate) marks: Vec<u64>,
    /// The flushed watermark: the highest `flushed` of any valid record or
    /// mark (ADR-4).
    pub(crate) flushed: u64,
    pub(crate) damaged: Vec<Damage>,
    pub(crate) bytes: u64,
    /// Queue lanes (highest lane + 1); the mark ring is not one.
    pub(crate) lanes: usize,
}

/// `q<lane:02x>-<ordinal:08x>.j` to `(lane, ordinal)`, and the mark ring's
/// `m00-<ordinal:08x>.j` to `(MARK_RING_LANE, ordinal)`, so the mark ring
/// sorts after every lane.
fn parse_name(name: &str) -> Option<(u16, u32)> {
    let rest = name.strip_suffix(".j")?;
    let (lane, o) = if let Some(o) = rest.strip_prefix("m00-") {
        (MARK_RING_LANE.0, o)
    } else {
        let (l, o) = rest.strip_prefix('q')?.split_once('-')?;
        if l.len() != 2 {
            return None;
        }
        (u16::from_str_radix(l, 16).ok()?, o)
    };
    if o.len() != 8 {
        return None;
    }
    let ordinal = u32::from_str_radix(o, 16).ok()?;
    (ordinal >= 1).then_some((lane, ordinal))
}

/// The ring a file belongs to, for findings.
fn is_mark_ring(lane: u16) -> bool {
    lane == MARK_RING_LANE.0
}

/// Scans every ring file. Findings about names and owners go to `sink`.
pub(crate) fn scan(s: &Session, sink: &mut Sink) -> Scan {
    let mut out = Scan::default();
    let dir = s.dir.join("journal");
    let mut files: Vec<(u16, u32, String)> = Vec::new();
    for e in fs::read_dir(&dir).into_iter().flatten().flatten() {
        let Ok(name) = e.file_name().into_string() else {
            continue;
        };
        if name.ends_with(".tmp") {
            continue;
        }
        match parse_name(&name) {
            Some((lane, ord)) => files.push((lane, ord, name)),
            None => sink.push(Finding::new(
                code::JOURNAL_FILE,
                format!("journal/{name}"),
                "not a ring file name",
            )),
        }
    }
    files.sort();
    if let Some(max) = files
        .iter()
        .map(|f| f.0)
        .filter(|l| !is_mark_ring(*l))
        .max()
    {
        out.lanes = usize::from(max) + 1;
    }
    let mut lane = None;
    let mut prev = 0u64;
    let mut last_seq = 0u64;
    // Damage not yet followed by a record of its lane.
    let mut open: Vec<usize> = Vec::new();
    for (l, ord, name) in &files {
        if lane != Some(*l) {
            lane = Some(*l);
            prev = 0;
            last_seq = 0;
            open.clear();
        }
        let rel = format!("journal/{name}");
        let buf = match fs::read(dir.join(name)) {
            Ok(b) => b,
            Err(e) => {
                sink.push(Finding::new(code::IO, &rel, format!("cannot read: {e}")));
                continue;
            }
        };
        out.bytes += buf.len() as u64;
        let len = buf.len() / BLOCK * BLOCK;
        let whole = &buf[..len];
        let mut bad = len < BLOCK;
        let mut header_bad = bad;
        let mark_ring = is_mark_ring(*l);
        // A mark ring file's header says lane 0 (on-disk-formats §5.1).
        let header_lane = if mark_ring { 0 } else { *l };
        let mut recs: Vec<(u64, usize)> = Vec::new();
        let mut damaged: Vec<Range<usize>> = Vec::new();
        if !bad {
            match scan_file(whole, WriteSeq(prev)) {
                Err(_) => {
                    bad = true;
                    header_bad = true;
                }
                Ok(res) => {
                    if res.header.volume_uuid != s.desc.volume_uuid {
                        sink.push(Finding::new(
                            code::JOURNAL_UUID,
                            &rel,
                            "the file belongs to another volume",
                        ));
                        bad = true;
                    } else if res.header.lane == header_lane
                        && res.header.ordinal > *ord
                        && whole[BLOCK..].iter().all(|b| *b == 0)
                    {
                        // A reinitialisation interrupted between its header
                        // write and its rename: it held no record since its
                        // zeroing was synced, not damage (on-disk-formats
                        // §5.1).
                        continue;
                    } else if res.header.lane != header_lane || res.header.ordinal != *ord {
                        sink.push(Finding::new(
                            code::JOURNAL_FILE,
                            &rel,
                            "the header's lane or ordinal differs from the file name",
                        ));
                        bad = true;
                    } else {
                        damaged = res.damaged;
                        for r in &res.records {
                            let seq = r.header.write_seq.0;
                            let off = r.header.off as usize;
                            // The reader's rising rule counts it either way.
                            prev = seq;
                            // Marks live only in the mark ring, writes and
                            // holes only in lane files (on-disk-formats
                            // §5.3): a record of the other kind is damage,
                            // never a record.
                            if (r.header.kind == RecordKind::Mark) != mark_ring {
                                damaged.push(off..off + r.encoded_len());
                                continue;
                            }
                            out.flushed = out.flushed.max(r.header.flushed.0);
                            if mark_ring {
                                out.marks.push(seq);
                            }
                            recs.push((seq, off));
                        }
                        damaged.sort_by_key(|d| d.start);
                    }
                }
            }
        }
        if bad {
            // A file that was never initialised is all zeros and holds nothing.
            if buf.iter().all(|b| *b == 0) {
                continue;
            }
            // Every record in a file whose header is gone is lost: at least a
            // warning, whatever follows in its lane.
            if header_bad {
                sink.push(Finding::new(
                    code::JOURNAL_FILE,
                    &rel,
                    "the file header is damaged or wrong: every record in the file is unreadable",
                ));
            }
            open.push(out.damaged.len());
            out.damaged.push(Damage {
                file: rel,
                range: 0..len.max(BLOCK) as u64,
                prev_seq: last_seq,
                next_seq: None,
            });
            continue;
        }
        let mut di = 0;
        for (seq, off) in &recs {
            while di < damaged.len() && damaged[di].start < *off {
                push_damage(&mut out, &mut open, &rel, &damaged[di], last_seq);
                di += 1;
            }
            for i in open.drain(..) {
                out.damaged[i].next_seq = Some(*seq);
            }
            last_seq = *seq;
            out.seqs.push(*seq);
        }
        while di < damaged.len() {
            push_damage(&mut out, &mut open, &rel, &damaged[di], last_seq);
            di += 1;
        }
    }
    out.seqs.sort_unstable();
    out.marks.sort_unstable();
    out
}

fn push_damage(out: &mut Scan, open: &mut Vec<usize>, rel: &str, r: &Range<usize>, prev_seq: u64) {
    open.push(out.damaged.len());
    out.damaged.push(Damage {
        file: rel.to_owned(),
        range: r.start as u64..r.end as u64,
        prev_seq,
        next_seq: None,
    });
}

/// Where the merge by `write_seq` above the floor stops (on-disk-formats §5.3).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Stop {
    /// The first missing `write_seq`, or the number found twice.
    pub(crate) at: u64,
    /// A `write_seq` held by two valid records (never crash residue).
    pub(crate) duplicate: bool,
    /// The valid records and marks the merge ignores: first, last, count.
    pub(crate) first: u64,
    pub(crate) last: u64,
    pub(crate) ignored: u64,
}

/// The first `write_seq` above `floor` that is missing from the merged order
/// (or found twice), and the valid records beyond it.
pub(crate) fn first_gap(seqs: &[u64], floor: u64) -> Option<Stop> {
    let mut expected = floor + 1;
    let mut stop: Option<Stop> = None;
    for &q in seqs.iter().filter(|q| **q > floor) {
        match &mut stop {
            None if q == expected => expected += 1,
            None => {
                let duplicate = q < expected;
                stop = Some(Stop {
                    at: if duplicate { q } else { expected },
                    duplicate,
                    first: q,
                    last: q,
                    ignored: 1,
                });
            }
            Some(s) => {
                s.last = q;
                s.ignored += 1;
            }
        }
    }
    stop
}

pub(crate) fn check(s: &Session, sink: &mut Sink, facts: &mut Facts) {
    let sc = scan(s, sink);
    facts.journal_lanes = sc.lanes;
    facts.journal_bytes = sc.bytes;
    let Some(tip) = &facts.tip else {
        facts
            .uncovered
            .push("journal: not classified, the tip did not authenticate".to_owned());
        return;
    };
    let floor = tip.full.cut_seq.0;
    // ADR-4: everything at or below the watermark was durable on every lane
    // before a surviving record or mark witnessed it, so its loss is real;
    // anything above it is crash residue that recovery wipes.
    let wm = sc.flushed;
    for d in &sc.damaged {
        let f = match d.next_seq {
            Some(n) if n > floor && n <= wm => Finding::new(
                code::JOURNAL_DAMAGE,
                &d.file,
                format!(
                    "damage after record {} with valid records after it (first {n}), above the cut {floor} and at or below the flushed watermark {wm}: durable records are lost, recovery stops here",
                    d.prev_seq
                ),
            ),
            Some(n) if n > floor => Finding::new(
                code::JOURNAL_RESIDUE,
                &d.file,
                format!(
                    "damage after record {} followed by record {n}, above the cut {floor} and the flushed watermark {wm}: crash residue, recovery wipes from here",
                    d.prev_seq
                ),
            ),
            Some(n) => Finding::new(
                code::JOURNAL_DAMAGE_BELOW_FLOOR,
                &d.file,
                format!(
                    "damage after record {} followed by record {n}, at or below the cut {floor}: the commit covers it",
                    d.prev_seq
                ),
            ),
            None => Finding::new(
                code::JOURNAL_TAIL,
                &d.file,
                "bytes that are not a valid record at the end of the lane: crash residue, legal",
            ),
        };
        sink.push(f.with_range(d.range.start, d.range.end));
    }
    let stop = first_gap(&sc.seqs, floor);
    let end = stop.map_or(u64::MAX, |g| g.at);
    let kept = |q: &&u64| **q > floor && **q < end;
    let marks = sc.marks.iter().filter(kept).count() as u64;
    facts.stats.journal_records = sc.seqs.iter().filter(kept).count() as u64 - marks;
    facts.stats.journal_marks = marks;
    facts.stats.journal_flushed = wm;
    match stop {
        None => {}
        Some(g) if g.duplicate => sink.push(Finding::new(
            code::JOURNAL_LOSS,
            "journal/",
            format!(
                "write_seq {} is held by two valid records: recovery stops there and ignores {} records ({}..={})",
                g.at, g.ignored, g.first, g.last
            ),
        )),
        Some(g) if g.at <= wm => sink.push(Finding::new(
            code::JOURNAL_LOSS,
            "journal/",
            format!(
                "write_seq {} is missing at or below the flushed watermark {wm}: durable records are lost; {} valid records ({}..={}) beyond it are ignored by recovery",
                g.at, g.ignored, g.first, g.last
            ),
        )),
        Some(g) => sink.push(Finding::new(
            code::IGNORED_AFTER_GAP,
            "journal/",
            format!(
                "write_seq {} is missing above the flushed watermark {wm}: crash residue; {} valid records ({}..={}) beyond it are ignored and wiped by recovery",
                g.at, g.ignored, g.first, g.last
            ),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lane_and_mark_ring_names_parse_and_the_mark_ring_sorts_last() {
        assert_eq!(parse_name("q00-00000001.j"), Some((0, 1)));
        assert_eq!(parse_name("q1f-0000000a.j"), Some((0x1f, 10)));
        assert_eq!(parse_name("m00-00000004.j"), Some((MARK_RING_LANE.0, 4)));
        for bad in [
            "q00-00000000.j",
            "m00-00000000.j",
            "m01-00000001.j",
            "q0-00000001.j",
            "q00-0000001.j",
            "m00-00000001",
            "x00-00000001.j",
        ] {
            assert_eq!(parse_name(bad), None, "{bad}");
        }
    }

    #[test]
    fn the_merge_stops_at_a_gap_or_a_duplicate() {
        assert_eq!(first_gap(&[1, 2, 3, 4], 1), None);
        let g = first_gap(&[1, 2, 4, 5, 6], 1).expect("gap");
        assert_eq!(
            (g.at, g.duplicate, g.first, g.last, g.ignored),
            (3, false, 4, 6, 3)
        );
        let d = first_gap(&[2, 3, 3, 4], 1).expect("duplicate");
        assert_eq!(
            (d.at, d.duplicate, d.first, d.last, d.ignored),
            (3, true, 3, 4, 2)
        );
        assert_eq!(first_gap(&[5, 6], 6), None);
    }
}
