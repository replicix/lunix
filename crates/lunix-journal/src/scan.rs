//! Recovery: scan every ring file with the normative reader, merge the lanes
//! by `write_seq`, stop at the first gap, and plan how each lane resumes.
//!
//! # Why recovery also plans a wipe
//!
//! Valid records beyond the stop point (valid records after a gap, or records
//! of an unflushed tail that happen to be intact) stay on disk. The next
//! incarnation reissues those sequence numbers, so a later scan would find a
//! stale record and a new record with the same `write_seq`, or accept a stale
//! record after the new ones and apply an old write over a newer, flushed one.
//! The plan therefore zeroes everything a lane holds after the end of its last
//! record below `next_seq`. The wipe runs before the first lane is opened
//! (`FileJournal::open_lane`), never in `recover` itself, so a read-only caller
//! (fsck) can still use `recover`.

use std::ops::Range;

use lunix_format::journal::{scan_file, RecordKind, BLOCK};
use lunix_format::local::CheckpointSlot;
use lunix_format::WriteSeq;
use lunix_iface::journal::{
    DamagedRange, RecordInfo, Recovered, RecoveredRecord, WriteSeqRange, LaneId, JournalLoc,
};
use lunix_iface::{Error, Result};

use crate::ring::{io_err, lock, Role, Rings};
use crate::sys::{self, B};

/// One accepted record of a scanned file.
#[derive(Clone, Copy, Debug)]
struct ScRec {
    seq: u64,
    kind: RecordKind,
    lba: u64,
    blocks: u32,
    off: u64,
    enc: u64,
}

#[derive(Debug, Default)]
struct ScannedFile {
    slot: usize,
    ordinal: u32,
    len: u64,
    bad: bool,
    skipped: bool,
    recs: Vec<ScRec>,
    damaged: Vec<Range<u64>>,
    /// End of the last non-zero block (at least one block).
    nz_end: u64,
}

/// What `prepare` must do to one file.
#[derive(Clone, Debug)]
pub(crate) struct FilePlan {
    pub slot: usize,
    pub ordinal: u32,
    pub role: Role,
    pub bad: bool,
    /// Zero `[from, to)` before the file is used.
    pub wipe: Option<(u64, u64)>,
    pub max_seq: u64,
    pub rec_bytes: u64,
    pub hi_off: u64,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct LanePlan {
    /// Files in ordinal order.
    pub files: Vec<FilePlan>,
    /// The active slot and where its next batch starts; `None`: every file is
    /// free and the lane starts at the first.
    pub active: Option<(usize, u64)>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Plan {
    pub lanes: Vec<LanePlan>,
}

/// The scan could not use the hint (it skipped the file the lane must resume
/// in); run again without it.
struct HintUnusable;

pub(crate) fn recover(
    rings: &Rings,
    hint: Option<&CheckpointSlot>,
    floor: WriteSeq,
) -> Result<(Recovered, Plan)> {
    let hint = hint.filter(|h| h.lane_tail_ordinals.len() == rings.lanes.len());
    match run(rings, hint, floor) {
        Ok(r) => Ok(r),
        Err(RunErr::Hint(HintUnusable)) => match run(rings, None, floor) {
            Ok(r) => Ok(r),
            Err(RunErr::Hint(_)) => Err(Error::invalid("recovery scan inconsistent")),
            Err(RunErr::Fail(e)) => Err(e),
        },
        Err(RunErr::Fail(e)) => Err(e),
    }
}

enum RunErr {
    Hint(HintUnusable),
    Fail(Error),
}

impl From<Error> for RunErr {
    fn from(e: Error) -> Self {
        Self::Fail(e)
    }
}

fn scan_lane(
    rings: &Rings,
    lane: usize,
    tail: Option<u32>,
) -> std::result::Result<Vec<ScannedFile>, Error> {
    let ring = &rings.lanes[lane];
    let mut order: Vec<(usize, u32, bool)> = {
        let inner = lock(&ring.inner);
        inner
            .slots
            .iter()
            .enumerate()
            .map(|(i, s)| (i, s.ordinal, s.bad))
            .collect()
    };
    order.sort_by_key(|&(_, o, _)| o);
    let highest_valid = order.iter().rev().find(|e| !e.2).map(|e| e.0);
    let mut out = Vec::with_capacity(order.len());
    let mut prev = 0u64;
    for (slot, ordinal, bad) in order {
        let file = &ring.files[slot].file;
        let len = floor4k_len(file.metadata().map_err(io_err)?.len());
        let mut sf = ScannedFile {
            slot,
            ordinal,
            len,
            nz_end: B,
            ..ScannedFile::default()
        };
        if bad || len < B {
            sf.bad = true;
            out.push(sf);
            continue;
        }
        if tail.is_some_and(|t| ordinal < t) && Some(slot) != highest_valid {
            sf.skipped = true;
            sf.nz_end = len;
            out.push(sf);
            continue;
        }
        let buf = sys::read_aligned(file, 0, usize::try_from(len).map_err(|_| Error::invalid("file length"))?)
            .map_err(io_err)?;
        match scan_file(buf.as_slice(), WriteSeq(prev)) {
            Err(_) => sf.bad = true,
            Ok(res) => {
                for r in &res.records {
                    prev = r.header.write_seq.0;
                    sf.recs.push(ScRec {
                        seq: prev,
                        kind: r.header.kind,
                        lba: r.header.lba,
                        blocks: r.header.len,
                        off: r.header.off,
                        enc: r.encoded_len() as u64,
                    });
                }
                sf.damaged = res
                    .damaged
                    .iter()
                    .map(|d| d.start as u64..d.end as u64)
                    .collect();
                sf.nz_end = sys::nonzero_end(buf.as_slice());
            }
        }
        out.push(sf);
    }
    Ok(out)
}

fn floor4k_len(len: u64) -> u64 {
    sys::floor4k(len)
}

fn run(
    rings: &Rings,
    hint: Option<&CheckpointSlot>,
    floor: WriteSeq,
) -> std::result::Result<(Recovered, Plan), RunErr> {
    let floor = floor.0;
    let nl = rings.lanes.len();
    let mut scans: Vec<Vec<ScannedFile>> = Vec::with_capacity(nl);
    for lane in 0..nl {
        let tail = hint.and_then(|h| h.lane_tail_ordinals.get(lane)).copied();
        scans.push(scan_lane(rings, lane, tail)?);
    }

    // Damage, with the lane's neighbours for fsck.
    let mut damaged = Vec::new();
    for (lane, files) in scans.iter().enumerate() {
        let lane_id = LaneId(lane as u16);
        let mut last_seq = 0u64;
        let mut open: Vec<usize> = Vec::new();
        for f in files {
            if f.skipped {
                continue;
            }
            if f.bad {
                open.push(damaged.len());
                damaged.push(DamagedRange {
                    lane: lane_id,
                    ordinal: f.ordinal,
                    start: 0,
                    end: f.len.max(B),
                    prev_seq: WriteSeq(last_seq),
                    next_seq: None,
                });
                continue;
            }
            let mut di = 0;
            for r in &f.recs {
                while di < f.damaged.len() && f.damaged[di].start < r.off {
                    open.push(damaged.len());
                    damaged.push(DamagedRange {
                        lane: lane_id,
                        ordinal: f.ordinal,
                        start: f.damaged[di].start,
                        end: f.damaged[di].end,
                        prev_seq: WriteSeq(last_seq),
                        next_seq: None,
                    });
                    di += 1;
                }
                for i in open.drain(..) {
                    damaged[i].next_seq = Some(WriteSeq(r.seq));
                }
                last_seq = r.seq;
            }
            while di < f.damaged.len() {
                open.push(damaged.len());
                damaged.push(DamagedRange {
                    lane: lane_id,
                    ordinal: f.ordinal,
                    start: f.damaged[di].start,
                    end: f.damaged[di].end,
                    prev_seq: WriteSeq(last_seq),
                    next_seq: None,
                });
                di += 1;
            }
        }
    }

    // Merge by write_seq, stop at the first gap (or duplicate).
    struct Cand {
        seq: u64,
        lane: u16,
        ordinal: u32,
        rec: ScRec,
    }
    let mut all: Vec<Cand> = Vec::new();
    for (lane, files) in scans.iter().enumerate() {
        for f in files {
            for r in &f.recs {
                if r.seq > floor {
                    all.push(Cand {
                        seq: r.seq,
                        lane: lane as u16,
                        ordinal: f.ordinal,
                        rec: *r,
                    });
                }
            }
        }
    }
    all.sort_by_key(|c| c.seq);
    let mut expected = floor + 1;
    let mut records = Vec::new();
    let mut ignored: Option<WriteSeqRange> = None;
    let mut gap_at = None;
    for c in &all {
        match &mut ignored {
            Some(range) => range.last = WriteSeq(c.seq),
            None if c.seq == expected => {
                records.push(RecoveredRecord {
                    record: RecordInfo {
                        kind: c.rec.kind,
                        write_seq: WriteSeq(c.seq),
                        lba: c.rec.lba,
                        blocks: c.rec.blocks,
                    },
                    loc: JournalLoc {
                        lane: LaneId(c.lane),
                        ordinal: c.ordinal,
                        offset: c.rec.off + lunix_format::journal::RECORD_HEADER_LEN as u64,
                    },
                });
                expected += 1;
            }
            None => {
                gap_at = Some(WriteSeq(expected));
                ignored = Some(WriteSeqRange {
                    first: WriteSeq(c.seq),
                    last: WriteSeq(c.seq),
                });
            }
        }
    }
    let next_seq = expected;

    // Plan each lane.
    let mut plan = Plan::default();
    for files in &scans {
        plan.lanes
            .push(plan_lane(files, floor, next_seq, rings.file_len)?);
    }
    Ok((
        Recovered {
            records,
            next_seq: WriteSeq(next_seq),
            ignored_after_gap: ignored,
            gap_at,
            damaged,
        },
        plan,
    ))
}

fn plan_lane(
    files: &[ScannedFile],
    floor: u64,
    next_seq: u64,
    file_len: u64,
) -> std::result::Result<LanePlan, RunErr> {
    // The resume file F: the highest file holding a record below `next_seq`.
    // A skipped file counts as holding one (it holds only records at or below
    // the floor, if the hint is right).
    let mut f_idx: Option<usize> = None;
    let mut f_end = B;
    for (i, f) in files.iter().enumerate() {
        if f.bad {
            continue;
        }
        if f.skipped {
            f_idx = Some(i);
            continue;
        }
        if let Some(last) = f.recs.iter().rev().find(|r| r.seq < next_seq) {
            f_idx = Some(i);
            f_end = last.off + last.enc;
        }
    }
    if f_idx.is_some_and(|i| files[i].skipped) {
        return Err(RunErr::Hint(HintUnusable));
    }

    let mut lane = LanePlan::default();
    let mut after: Vec<FilePlan> = Vec::new();
    let mut bad: Vec<FilePlan> = Vec::new();
    for (i, f) in files.iter().enumerate() {
        let mut fp = FilePlan {
            slot: f.slot,
            ordinal: f.ordinal,
            role: Role::Free,
            bad: f.bad,
            wipe: None,
            max_seq: 0,
            rec_bytes: 0,
            hi_off: B,
        };
        if f.bad {
            bad.push(fp);
            continue;
        }
        match f_idx {
            Some(fi) if i < fi => {
                fp.role = Role::Used;
                fp.max_seq = f
                    .recs
                    .iter()
                    .filter(|r| r.seq < next_seq)
                    .map(|r| r.seq)
                    .max()
                    .unwrap_or(0);
                fp.rec_bytes = kept_bytes(f, floor, next_seq);
                fp.hi_off = if f.skipped { file_len } else { f.nz_end };
                lane.files.push(fp);
            }
            Some(fi) if i == fi => {
                fp.role = Role::Active;
                fp.max_seq = f
                    .recs
                    .iter()
                    .filter(|r| r.seq < next_seq)
                    .map(|r| r.seq)
                    .max()
                    .unwrap_or(0);
                fp.rec_bytes = kept_bytes(f, floor, next_seq);
                let aligned = sys::round4k(f_end);
                if !f_end.is_multiple_of(B) || f.nz_end > f_end {
                    fp.wipe = Some((f_end, f.nz_end.max(aligned)));
                }
                fp.hi_off = aligned;
                lane.active = Some((f.slot, aligned));
                lane.files.push(fp);
            }
            _ => {
                if f.nz_end > B {
                    fp.wipe = Some((B, f.nz_end));
                }
                after.push(fp);
            }
        }
    }
    lane.files.extend(after);
    lane.files.extend(bad);
    Ok(lane)
}

fn kept_bytes(f: &ScannedFile, floor: u64, next_seq: u64) -> u64 {
    f.recs
        .iter()
        .filter(|r| r.seq > floor && r.seq < next_seq)
        .map(|r| r.enc)
        .sum()
}

/// Blocks per file header, re-exported for callers sizing wipes.
pub(crate) const HEADER_BYTES: u64 = BLOCK as u64;
