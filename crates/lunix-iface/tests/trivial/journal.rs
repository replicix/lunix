use std::sync::{Arc, Mutex};

use lunix_format::journal::RecordKind;
use lunix_format::local::CheckpointSlot;
use lunix_format::WriteSeq;
use lunix_iface::block::IoStatus;
use lunix_iface::contract::journal::JournalFixture;
use lunix_iface::journal::*;
use lunix_iface::wake::WakeFd;
use lunix_iface::{Error, Result};

use super::lock;

#[derive(Clone)]
struct Stored {
    record: AppendRecord,
    lane: u16,
    offset: u64,
    data: Vec<u8>,
    flushed: bool,
}

#[derive(Default)]
struct State {
    records: Vec<Stored>,
    next_offset: Vec<u64>,
    open: Vec<bool>,
}

pub struct TJournal {
    lanes: u16,
    state: Arc<Mutex<State>>,
}

impl TJournal {
    pub fn new(lanes: u16) -> Self {
        let state = State {
            next_offset: vec![4096; usize::from(lanes)],
            open: vec![false; usize::from(lanes)],
            ..State::default()
        };
        Self {
            lanes,
            state: Arc::new(Mutex::new(state)),
        }
    }
}

fn read(st: &State, loc: JournalLoc, buf: &mut [u8]) -> bool {
    for r in &st.records {
        let len = r.data.len() as u64;
        if r.lane == loc.lane.0
            && loc.ordinal == 0
            && loc.offset >= r.offset
            && loc.offset < r.offset + len.max(1)
        {
            let start = (loc.offset - r.offset) as usize;
            if let Some(src) = r.data.get(start..start + buf.len()) {
                buf.copy_from_slice(src);
                return true;
            }
        }
    }
    false
}

struct Lane {
    id: u16,
    state: Arc<Mutex<State>>,
    wake: WakeFd,
    done: Vec<JournalCompletion>,
    last: WriteSeq,
}

impl JournalLane for Lane {
    fn lane(&self) -> LaneId {
        LaneId(self.id)
    }

    fn try_append(&mut self, rec: &AppendRecord, data: &[u8]) -> Result<AppendOutcome> {
        let want = match rec.kind {
            RecordKind::Write => rec.blocks as usize * 4096,
            RecordKind::Hole => 0,
        };
        if data.len() != want || rec.write_seq <= self.last {
            return Err(Error::invalid("malformed record"));
        }
        let mut st = lock(&self.state);
        let off = st.next_offset[usize::from(self.id)];
        st.next_offset[usize::from(self.id)] = off + want as u64 + 64;
        st.records.push(Stored {
            record: *rec,
            lane: self.id,
            offset: off,
            data: data.to_vec(),
            flushed: false,
        });
        drop(st);
        self.last = rec.write_seq;
        self.done.push(JournalCompletion::Append {
            write_seq: rec.write_seq,
            status: IoStatus::ok(0),
        });
        self.wake.signal();
        Ok(AppendOutcome::Appended {
            loc: JournalLoc {
                lane: LaneId(self.id),
                ordinal: 0,
                offset: off,
            },
        })
    }

    fn submit_read(&mut self, loc: JournalLoc, mut buf: Vec<u8>, token: Token) -> Result<()> {
        if loc.lane.0 != self.id {
            return Err(Error::invalid("location on another lane"));
        }
        let ok = read(&lock(&self.state), loc, &mut buf);
        let status = if ok {
            IoStatus::ok(buf.len() as u32)
        } else {
            IoStatus::err(5)
        };
        self.done
            .push(JournalCompletion::Read { token, status, buf });
        self.wake.signal();
        Ok(())
    }

    fn poll(&mut self, out: &mut Vec<JournalCompletion>) {
        self.wake.drain();
        out.append(&mut self.done);
    }

    fn completed_watermark(&self) -> WriteSeq {
        self.last
    }
}

impl Journal for TJournal {
    fn lanes(&self) -> u16 {
        self.lanes
    }

    fn open_lane(&self, lane: LaneId, wake: WakeFd) -> Result<Box<dyn JournalLane>> {
        let mut st = lock(&self.state);
        match st.open.get_mut(usize::from(lane.0)) {
            Some(o) if !*o => *o = true,
            _ => return Err(Error::invalid("bad or open lane")),
        }
        Ok(Box::new(Lane {
            id: lane.0,
            state: self.state.clone(),
            wake,
            done: Vec::new(),
            last: WriteSeq(0),
        }))
    }

    fn submit_group_flush(&self, upto: WriteSeq, done: FlushDone) -> Result<()> {
        for r in &mut lock(&self.state).records {
            if r.record.write_seq <= upto {
                r.flushed = true;
            }
        }
        done(Ok(()));
        Ok(())
    }

    fn read_at(&self, loc: JournalLoc, buf: &mut [u8]) -> Result<()> {
        if read(&lock(&self.state), loc, buf) {
            Ok(())
        } else {
            Err(Error::not_found("journal data"))
        }
    }

    fn recover(&self, _hint: Option<&CheckpointSlot>, floor: WriteSeq) -> Result<Recovered> {
        let st = lock(&self.state);
        let mut all: Vec<&Stored> = st
            .records
            .iter()
            .filter(|r| r.record.write_seq > floor)
            .collect();
        all.sort_by_key(|r| r.record.write_seq);
        let mut records = Vec::new();
        let mut next = floor.0 + 1;
        let mut ignored: Option<WriteSeqRange> = None;
        for r in all {
            if ignored.is_none() && r.record.write_seq.0 == next {
                records.push(RecoveredRecord {
                    record: r.record,
                    loc: JournalLoc {
                        lane: LaneId(r.lane),
                        ordinal: 0,
                        offset: r.offset,
                    },
                });
                next += 1;
            } else {
                let s = r.record.write_seq;
                ignored = Some(match ignored {
                    None => WriteSeqRange { first: s, last: s },
                    Some(g) => WriteSeqRange {
                        first: g.first,
                        last: s,
                    },
                });
            }
        }
        Ok(Recovered {
            records,
            next_seq: WriteSeq(next),
            ignored_after_gap: ignored,
            damaged: Vec::new(),
        })
    }

    fn tail_after(&self, _cut: WriteSeq) -> Result<JournalTail> {
        Ok(JournalTail {
            lane_ordinals: vec![0; usize::from(self.lanes)],
        })
    }

    fn reclaim(&self, upto: &JournalTail) -> Result<ReclaimReport> {
        if upto.lane_ordinals.len() != usize::from(self.lanes) {
            return Err(Error::invalid("wrong lane count"));
        }
        Ok(ReclaimReport::default())
    }

    fn fill(&self) -> JournalFill {
        JournalFill {
            used_bytes: lock(&self.state).records.len() as u64 * 4096,
            capacity_bytes: 1 << 30,
        }
    }
}

pub struct JFix {
    j: TJournal,
}

impl JFix {
    pub fn new() -> Self {
        Self {
            j: TJournal::new(2),
        }
    }
}

impl JournalFixture for JFix {
    fn journal(&self) -> &dyn Journal {
        &self.j
    }

    /// Keeps flushed records and, to be adversarial, the unflushed ones with an
    /// odd `write_seq` (so recovery meets a gap and must ignore what follows).
    fn crash_and_reopen(&mut self) {
        let old = lock(&self.j.state).records.clone();
        let fresh = TJournal::new(self.j.lanes);
        {
            let mut st = lock(&fresh.state);
            for mut r in old {
                if r.flushed || r.record.write_seq.0 % 2 == 1 {
                    r.flushed = true;
                    let o = &mut st.next_offset[usize::from(r.lane)];
                    *o = (*o).max(r.offset + r.data.len() as u64 + 64);
                    st.records.push(r);
                }
            }
        }
        self.j = fresh;
    }
}
