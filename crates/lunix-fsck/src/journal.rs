//! Step 6: the journal, scanned like recovery with the tip's `cut_seq` as the
//! floor (DESIGN section 4.1, D17).
//!
//! This is the normative reader (`lunix_format::journal::scan_file`) applied
//! to every ring file, plus the merge by `write_seq`; it never writes.

use std::fs;
use std::ops::Range;

use lunix_format::journal::{scan_file, BLOCK};
use lunix_format::WriteSeq;

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
    /// Every valid record's `write_seq`, sorted.
    pub(crate) seqs: Vec<u64>,
    pub(crate) damaged: Vec<Damage>,
    pub(crate) bytes: u64,
    pub(crate) lanes: usize,
}

/// `q<lane:02x>-<ordinal:08x>.j`.
fn parse_name(name: &str) -> Option<(u16, u32)> {
    let rest = name.strip_prefix('q')?.strip_suffix(".j")?;
    let (l, o) = rest.split_once('-')?;
    if l.len() != 2 || o.len() != 8 {
        return None;
    }
    let lane = u16::from_str_radix(l, 16).ok()?;
    let ordinal = u32::from_str_radix(o, 16).ok()?;
    (ordinal >= 1).then_some((lane, ordinal))
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
    if let Some(max) = files.iter().map(|f| f.0).max() {
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
        let mut recs: Vec<(u64, usize)> = Vec::new();
        let mut damaged: Vec<Range<usize>> = Vec::new();
        if !bad {
            match scan_file(whole, WriteSeq(prev)) {
                Err(_) => bad = true,
                Ok(res) => {
                    if res.header.volume_uuid != s.desc.volume_uuid {
                        sink.push(Finding::new(
                            code::JOURNAL_UUID,
                            &rel,
                            "the file belongs to another volume",
                        ));
                        bad = true;
                    } else if res.header.lane != *l || res.header.ordinal != *ord {
                        sink.push(Finding::new(
                            code::JOURNAL_FILE,
                            &rel,
                            "the header's lane or ordinal differs from the file name",
                        ));
                        bad = true;
                    } else {
                        recs = res
                            .records
                            .iter()
                            .map(|r| (r.header.write_seq.0, r.header.off as usize))
                            .collect();
                        damaged = res.damaged;
                    }
                }
            }
        }
        if bad {
            // A file that was never initialised is all zeros and holds nothing.
            if buf.iter().all(|b| *b == 0) {
                continue;
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
            prev = *seq;
            out.seqs.push(*seq);
        }
        while di < damaged.len() {
            push_damage(&mut out, &mut open, &rel, &damaged[di], last_seq);
            di += 1;
        }
    }
    out.seqs.sort_unstable();
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

/// The first sequence number at or above `floor + 1` that is missing from the
/// merged order, and the valid records beyond it.
pub(crate) fn first_gap(seqs: &[u64], floor: u64) -> Option<(u64, u64, u64, u64)> {
    let mut expected = floor + 1;
    let mut gap: Option<u64> = None;
    let (mut first, mut last, mut n) = (0, 0, 0u64);
    for &q in seqs.iter().filter(|q| **q > floor) {
        match gap {
            None if q == expected => expected += 1,
            None => {
                gap = Some(expected);
                first = q;
                last = q;
                n = 1;
            }
            Some(_) => {
                last = q;
                n += 1;
            }
        }
    }
    gap.map(|g| (g, first, last, n))
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
    for d in &sc.damaged {
        let f = match d.next_seq {
            Some(n) if n > floor => Finding::new(
                code::JOURNAL_DAMAGE,
                &d.file,
                format!(
                    "damage after record {} with valid records after it (first {n}), above the cut {floor}: recovery stops here",
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
    let gap = first_gap(&sc.seqs, floor);
    let above = sc.seqs.iter().filter(|q| **q > floor).count() as u64;
    facts.stats.journal_records = above - gap.map_or(0, |g| g.3);
    if let Some((at, first, last, n)) = gap {
        sink.push(Finding::new(
            code::IGNORED_AFTER_GAP,
            "journal/",
            format!(
                "write_seq {at} is missing; {n} valid records ({first}..={last}) beyond it are ignored by recovery"
            ),
        ));
    }
}
