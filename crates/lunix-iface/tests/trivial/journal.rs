use std::collections::BTreeSet;
use std::sync::atomic::{AtomicU64, Ordering};
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
    record: RecordInfo,
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
    /// Sequence numbers issued and not yet completed (slot or append).
    inflight: BTreeSet<u64>,
    pending_flushes: Vec<(WriteSeq, FlushDone)>,
    /// The lowest failed sequence number once poisoned.
    poisoned: Option<u64>,
    /// Lane whose next append fails (test hook).
    fail_next: Option<u16>,
}

/// Poisons the journal at `seq` and fails the flushes that cover it.
fn poison(st: &mut State, seq: u64) {
    let at = st.poisoned.map_or(seq, |p| p.min(seq));
    st.poisoned = Some(at);
    let mut i = 0;
    while i < st.pending_flushes.len() {
        if st.pending_flushes[i].0 .0 >= at {
            let (_, done) = st.pending_flushes.remove(i);
            done(Err(io_err()));
        } else {
            i += 1;
        }
    }
}

fn io_err() -> Error {
    Error::Io(std::io::Error::other("journal poisoned"))
}

pub struct TJournal {
    lanes: u16,
    counter: Arc<AtomicU64>,
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
            counter: Arc::new(AtomicU64::new(0)),
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

/// Runs the flushes whose waits are over.
fn settle(st: &mut State) {
    let mut ready = Vec::new();
    let mut i = 0;
    while i < st.pending_flushes.len() {
        let upto = st.pending_flushes[i].0;
        if st.inflight.range(..=upto.0).next().is_none() {
            ready.push(st.pending_flushes.remove(i));
        } else {
            i += 1;
        }
    }
    for (upto, done) in ready {
        for r in &mut st.records {
            if r.record.write_seq <= upto {
                r.flushed = true;
            }
        }
        done(Ok(()));
    }
}

struct Lane {
    id: u16,
    counter: Arc<AtomicU64>,
    state: Arc<Mutex<State>>,
    wake: WakeFd,
    done: Vec<JournalCompletion>,
    last: WriteSeq,
    outstanding: Option<u64>,
}

impl JournalLane for Lane {
    fn lane(&self) -> LaneId {
        LaneId(self.id)
    }

    fn try_reserve(&mut self, kind: RecordKind, blocks: u32) -> Result<SlotOutcome> {
        if blocks == 0 || (kind == RecordKind::Write && blocks > 256) {
            return Err(Error::invalid("bad record length"));
        }
        let mut st = lock(&self.state);
        if st.poisoned.is_some() {
            return Err(io_err());
        }
        if let Some(seq) = self.outstanding {
            poison(&mut st, seq);
            return Err(Error::invalid(
                "an earlier slot of this lane is outstanding",
            ));
        }
        let seq = self.counter.fetch_add(1, Ordering::SeqCst) + 1;
        st.inflight.insert(seq);
        self.outstanding = Some(seq);
        Ok(SlotOutcome::Reserved(Slot::new(
            WriteSeq(seq),
            kind,
            blocks,
            u64::from(self.id),
        )))
    }

    fn append(&mut self, slot: Slot, lba: u64, data: &[u8]) -> Result<Appended> {
        let slot = slot.take();
        let mut st = lock(&self.state);
        if self.outstanding != Some(slot.write_seq.0) || slot.token != u64::from(self.id) {
            poison(&mut st, slot.write_seq.0);
            return Err(Error::invalid("slot was not issued by this lane"));
        }
        self.outstanding = None;
        if st.poisoned.is_some() {
            return Err(io_err());
        }
        if st.fail_next == Some(self.id) {
            st.fail_next = None;
            poison(&mut st, slot.write_seq.0);
            return Err(io_err());
        }
        let want = match slot.kind {
            RecordKind::Write => slot.blocks as usize * 4096,
            RecordKind::Hole => 0,
        };
        if data.len() != want {
            poison(&mut st, slot.write_seq.0);
            return Err(Error::invalid("data does not match the slot"));
        }
        let off = st.next_offset[usize::from(self.id)];
        st.next_offset[usize::from(self.id)] = off + record_bytes(slot.kind, slot.blocks);
        let record = RecordInfo {
            kind: slot.kind,
            write_seq: slot.write_seq,
            lba,
            blocks: slot.blocks,
        };
        st.records.push(Stored {
            record,
            lane: self.id,
            offset: off,
            data: data.to_vec(),
            flushed: false,
        });
        st.inflight.remove(&slot.write_seq.0);
        settle(&mut st);
        drop(st);
        self.last = slot.write_seq;
        self.done.push(JournalCompletion::Append {
            write_seq: slot.write_seq,
            status: IoStatus::ok(0),
        });
        self.wake.signal();
        Ok(Appended {
            write_seq: slot.write_seq,
            loc: JournalLoc {
                lane: LaneId(self.id),
                ordinal: 0,
                offset: off,
            },
        })
    }

    fn submit_read(&mut self, loc: JournalLoc, mut buf: Vec<u8>, token: Token) -> Result<()> {
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
            counter: self.counter.clone(),
            state: self.state.clone(),
            wake,
            done: Vec::new(),
            last: WriteSeq(0),
            outstanding: None,
        }))
    }

    fn last_issued(&self) -> WriteSeq {
        WriteSeq(self.counter.load(Ordering::SeqCst))
    }

    fn submit_group_flush(&self, upto: WriteSeq, done: FlushDone) -> Result<()> {
        let mut st = lock(&self.state);
        if st.poisoned.is_some_and(|p| upto.0 >= p) {
            drop(st);
            done(Err(io_err()));
            return Ok(());
        }
        st.pending_flushes.push((upto, done));
        settle(&mut st);
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
        let mut gap_at = None;
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
                gap_at.get_or_insert(WriteSeq(next));
                ignored = Some(match ignored {
                    None => WriteSeqRange { first: s, last: s },
                    Some(g) => WriteSeqRange {
                        first: g.first,
                        last: s,
                    },
                });
            }
        }
        self.counter.store(next - 1, Ordering::SeqCst);
        Ok(Recovered {
            records,
            next_seq: WriteSeq(next),
            ignored_after_gap: ignored,
            gap_at,
            damaged: Vec::new(),
            flushed: WriteSeq(0),
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
    fn fail_next_append(&mut self, lane: LaneId) -> bool {
        lock(&self.j.state).fail_next = Some(lane.0);
        true
    }

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
