//! [`FileJournal`]: the `Journal` implementation over ring files.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use lunix_format::local::CheckpointSlot;
use lunix_format::WriteSeq;
use lunix_iface::journal::{
    FlushDone, Journal, JournalFill, JournalLane, JournalLoc, JournalTail, LaneId, ReclaimReport,
    Recovered,
};
use lunix_iface::wake::WakeFd;
use lunix_iface::{Error, Result};

use crate::aligned::AlignedBuf;
use crate::config::JournalConfig;
use crate::coord;
use crate::lane::FileLane;
use crate::ring::{io_err, lock, open_rings, Reinit, Role};
use crate::scan::{self, Plan};
use crate::shared::{Coord, LaneFlight, Shared, State, NOT_POISONED};
use crate::sys::{self, round4k, B, ZERO_LEN};

/// The journal of one volume: lanes over preallocated `O_DIRECT` ring files,
/// a flush coordinator thread, recovery and reclaim.
pub struct FileJournal {
    sh: Arc<Shared>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl FileJournal {
    /// Opens (creating the missing ring files of) the journal directory and
    /// starts the flush coordinator. Existing files are only read; call
    /// [`Journal::recover`] next.
    ///
    /// # Errors
    /// `Invalid` for a bad configuration, `Unsupported` when the directory has
    /// files of a lane beyond `lanes`, `NoSpace` or `Io` while creating files.
    pub fn open(cfg: JournalConfig) -> Result<Self> {
        cfg.validate()?;
        let zero = AlignedBuf::zeroed(ZERO_LEN).map_err(Error::Io)?;
        let rings = open_rings(&cfg, &zero)?;
        let lanes = usize::from(cfg.lanes);
        let sh = Arc::new(Shared {
            cfg,
            rings,
            counter: AtomicU64::new(0),
            poison_at: AtomicU64::new(NOT_POISONED),
            flights: (0..lanes).map(|_| Mutex::new(LaneFlight::new())).collect(),
            coord: Coord::new(),
            st: Mutex::new(State {
                plan: None,
                prepared: false,
                lane_open: vec![false; lanes],
                floor: 0,
            }),
            closed: AtomicBool::new(false),
            max_cut_asked: AtomicU64::new(0),
            zero,
            fail_next: Mutex::new(BTreeSet::new()),
            fail_any: AtomicBool::new(false),
            fail_sync: AtomicBool::new(false),
        });
        let worker = Arc::clone(&sh);
        let handle = std::thread::Builder::new()
            .name("lunix-journal-flush".into())
            .spawn(move || coord::run(worker))
            .map_err(Error::Io)?;
        Ok(Self {
            sh,
            thread: Mutex::new(Some(handle)),
        })
    }

    /// Test hook: the next `append` on `lane` fails with an I/O error after
    /// its slot was consumed, as a failed write would.
    #[doc(hidden)]
    pub fn fail_next_append(&self, lane: LaneId) {
        lock(&self.sh.fail_next).insert(lane.0);
        self.sh.fail_any.store(true, Ordering::Relaxed);
    }

    /// Test hook: the next group flush that has files to sync reports a failed
    /// `fdatasync`.
    #[doc(hidden)]
    pub fn fail_next_sync(&self) {
        self.sh.fail_sync.store(true, Ordering::SeqCst);
    }

    fn apply_plan(&self, plan: &Plan) {
        for (ring, lp) in self.sh.rings.lanes.iter().zip(&plan.lanes) {
            let mut inner = lock(&ring.inner);
            inner.free.clear();
            for fp in &lp.files {
                inner.slots[fp.slot].role = fp.role;
                let file = &ring.files[fp.slot];
                file.max_seq.store(fp.max_seq, Ordering::Relaxed);
                file.rec_bytes.store(fp.rec_bytes, Ordering::Relaxed);
                file.hi_off.store(fp.hi_off, Ordering::Relaxed);
                if fp.role == Role::Free {
                    inner.free.push_back(fp.slot);
                }
            }
            match lp.active {
                Some((slot, off)) => {
                    inner.active = Some(slot);
                    inner.resume_off = off;
                }
                None => {
                    inner.active = None;
                    inner.resume_off = B;
                }
            }
        }
    }

    /// Executes the wipe plan: zero what lies beyond the stop point, re-initialise
    /// files with a bad header, make everything that holds records durable.
    /// Runs once per `recover`, before the first lane opens or reclaim runs.
    fn prepare(&self) -> Result<()> {
        let mut st = lock(&self.sh.st);
        if st.prepared {
            return Ok(());
        }
        let Some(plan) = st.plan.clone() else {
            return Err(Error::invalid("recover must run before lanes are opened"));
        };
        let rings = &self.sh.rings;
        let mut renamed = false;
        for (ring, lp) in rings.lanes.iter().zip(&plan.lanes) {
            let mut touched: Vec<usize> = Vec::new();
            for fp in &lp.files {
                let file = &ring.files[fp.slot];
                if fp.bad {
                    let new_ordinal = lock(&ring.inner).take_ordinal();
                    let gen = rings
                        .reinit_slot(
                            &self.sh.cfg,
                            file,
                            Reinit {
                                lane: ring.lane,
                                old_ordinal: fp.ordinal,
                                new_ordinal,
                                zero_to: rings.file_len,
                            },
                            &self.sh.zero,
                        )
                        .map_err(io_err)?;
                    renamed = true;
                    let mut inner = lock(&ring.inner);
                    let s = &mut inner.slots[fp.slot];
                    s.ordinal = new_ordinal;
                    s.gen = gen;
                    s.bad = false;
                    drop(inner);
                    file.reset_counters();
                } else if let Some((from, to)) = fp.wipe {
                    sys::zero_range(&file.file, from, to, &self.sh.zero).map_err(io_err)?;
                    touched.push(fp.slot);
                } else if matches!(fp.role, Role::Used | Role::Active) {
                    touched.push(fp.slot);
                }
            }
            // Records that survived are made durable here: a read after a
            // crash may have been served from a volatile cache, and no later
            // group flush would cover them.
            for slot in touched {
                ring.files[slot].file.sync_data().map_err(io_err)?;
            }
        }
        if renamed {
            sys::fsync_dir(&rings.dir).map_err(io_err)?;
        }
        st.prepared = true;
        Ok(())
    }

    fn require_recovered(&self) -> Result<()> {
        if lock(&self.sh.st).plan.is_some() {
            Ok(())
        } else {
            Err(Error::invalid("recover must run first"))
        }
    }
}

impl Drop for FileJournal {
    fn drop(&mut self) {
        self.sh.closed.store(true, Ordering::SeqCst);
        {
            let mut q = lock(&self.sh.coord.q);
            q.shutdown = true;
        }
        self.sh.coord.cv.notify_all();
        self.sh.coord.notify();
        if let Some(h) = lock(&self.thread).take() {
            let _ = h.join();
        }
    }
}

impl Journal for FileJournal {
    fn lanes(&self) -> u16 {
        self.sh.cfg.lanes
    }

    fn open_lane(&self, lane: LaneId, wake: WakeFd) -> Result<Box<dyn JournalLane>> {
        if lane.0 >= self.sh.cfg.lanes {
            return Err(Error::invalid("no such lane"));
        }
        if self.sh.closed.load(Ordering::SeqCst) {
            return Err(Error::Closed);
        }
        self.require_recovered()?;
        if lock(&self.sh.st).lane_open[usize::from(lane.0)] {
            return Err(Error::invalid("lane already open"));
        }
        self.prepare()?;
        let mut st = lock(&self.sh.st);
        if st.lane_open[usize::from(lane.0)] {
            return Err(Error::invalid("lane already open"));
        }
        let l = FileLane::new(Arc::clone(&self.sh), lane.0, wake)?;
        st.lane_open[usize::from(lane.0)] = true;
        Ok(Box::new(l))
    }

    fn last_issued(&self) -> WriteSeq {
        WriteSeq(self.sh.counter.load(Ordering::SeqCst))
    }

    fn submit_group_flush(&self, upto: WriteSeq, done: FlushDone) -> Result<()> {
        if self.sh.closed.load(Ordering::SeqCst) {
            return Err(Error::Closed);
        }
        if self.sh.poisoned_at().is_some_and(|p| upto.0 >= p) {
            done(Err(Shared::poisoned_error()));
            return Ok(());
        }
        {
            let mut q = lock(&self.sh.coord.q);
            if q.shutdown {
                return Err(Error::Closed);
            }
            q.queue.push((upto.0, done));
        }
        self.sh.coord.cv.notify_all();
        Ok(())
    }

    fn read_at(&self, loc: JournalLoc, buf: &mut [u8]) -> Result<()> {
        let rings = &self.sh.rings;
        if rings.lanes.get(usize::from(loc.lane.0)).is_none() {
            return Err(Error::invalid("no such lane"));
        }
        let end = loc
            .offset
            .checked_add(buf.len() as u64)
            .ok_or(Error::invalid("journal location overflows"))?;
        if loc.offset < B || end > rings.file_len {
            return Err(Error::invalid("journal location outside the ring file"));
        }
        let file = rings
            .find_in_use(loc.lane.0, loc.ordinal)
            .ok_or(Error::not_found("journal data"))?;
        if buf.is_empty() {
            return Ok(());
        }
        let a0 = sys::floor4k(loc.offset);
        let a1 = round4k(end);
        let abuf = sys::read_aligned(&file.file, a0, (a1 - a0) as usize).map_err(io_err)?;
        let delta = (loc.offset - a0) as usize;
        buf.copy_from_slice(&abuf.as_slice()[delta..delta + buf.len()]);
        Ok(())
    }

    fn recover(&self, hint: Option<&CheckpointSlot>, floor: WriteSeq) -> Result<Recovered> {
        let mut st = lock(&self.sh.st);
        if st.lane_open.iter().any(|&o| o) {
            return Err(Error::invalid("recover after a lane was opened"));
        }
        let (rec, plan) = scan::recover(&self.sh.rings, hint, floor)?;
        self.apply_plan(&plan);
        self.sh.counter.store(rec.next_seq.0 - 1, Ordering::SeqCst);
        self.sh.poison_at.store(NOT_POISONED, Ordering::SeqCst);
        for f in &self.sh.flights {
            *lock(f) = LaneFlight::new();
        }
        self.sh.max_cut_asked.store(floor.0, Ordering::SeqCst);
        st.plan = Some(plan);
        st.prepared = false;
        st.floor = floor.0;
        Ok(rec)
    }

    fn tail_after(&self, cut: WriteSeq) -> Result<JournalTail> {
        self.require_recovered()?;
        self.sh.max_cut_asked.fetch_max(cut.0, Ordering::SeqCst);
        let mut lane_ordinals = Vec::with_capacity(self.sh.rings.lanes.len());
        for ring in &self.sh.rings.lanes {
            let inner = lock(&ring.inner);
            let lowest = inner
                .slots
                .iter()
                .enumerate()
                .filter(|(i, s)| {
                    matches!(s.role, Role::Used | Role::Active)
                        && ring.files[*i].max_seq.load(Ordering::Acquire) > cut.0
                })
                .map(|(_, s)| s.ordinal)
                .min();
            let ordinal = lowest
                .or_else(|| inner.active.map(|a| inner.slots[a].ordinal))
                .or_else(|| inner.free.front().map(|&f| inner.slots[f].ordinal))
                .unwrap_or(1);
            lane_ordinals.push(ordinal);
        }
        Ok(JournalTail { lane_ordinals })
    }

    fn reclaim(&self, upto: &JournalTail) -> Result<ReclaimReport> {
        if upto.lane_ordinals.len() != self.sh.rings.lanes.len() {
            return Err(Error::invalid("tail has the wrong lane count"));
        }
        self.require_recovered()?;
        self.prepare()?;
        let rings = &self.sh.rings;
        let cut_limit = self.sh.max_cut_asked.load(Ordering::SeqCst);
        let mut report = ReclaimReport::default();
        for (ring, &tail) in rings.lanes.iter().zip(&upto.lane_ordinals) {
            struct Job {
                slot: usize,
                old: u32,
                new: u32,
                hi: u64,
                bytes: u64,
            }
            let mut jobs = Vec::new();
            {
                let mut inner = lock(&ring.inner);
                let mut cands: Vec<usize> = (0..inner.slots.len())
                    .filter(|&i| {
                        let s = &inner.slots[i];
                        let f = &ring.files[i];
                        s.role == Role::Used
                            && s.ordinal < tail
                            && f.inflight.load(Ordering::Acquire) == 0
                            && f.max_seq.load(Ordering::Acquire) <= cut_limit
                    })
                    .collect();
                cands.sort_by_key(|&i| inner.slots[i].ordinal);
                for slot in cands {
                    inner.slots[slot].role = Role::Recycling;
                    let new = inner.take_ordinal();
                    let f = &ring.files[slot];
                    jobs.push(Job {
                        slot,
                        old: inner.slots[slot].ordinal,
                        new,
                        hi: f.hi_off.load(Ordering::Acquire),
                        bytes: f.rec_bytes.load(Ordering::Acquire),
                    });
                }
            }
            for job in jobs {
                let file = &ring.files[job.slot];
                lunix_iface::crashpoint!("journal.before_reclaim_delete");
                let done = rings
                    .reinit_slot(
                        &self.sh.cfg,
                        file,
                        Reinit {
                            lane: ring.lane,
                            old_ordinal: job.old,
                            new_ordinal: job.new,
                            zero_to: job.hi,
                        },
                        &self.sh.zero,
                    )
                    .and_then(|g| sys::fsync_dir(&rings.dir).map(|()| g));
                let mut inner = lock(&ring.inner);
                match done {
                    Ok(gen) => {
                        let s = &mut inner.slots[job.slot];
                        s.ordinal = job.new;
                        s.gen = gen;
                        s.role = Role::Free;
                        inner.free.push_back(job.slot);
                        drop(inner);
                        file.reset_counters();
                        report.files += 1;
                        report.bytes += job.bytes;
                        lunix_iface::crashpoint!("journal.after_reclaim_delete");
                    }
                    Err(e) => {
                        inner.slots[job.slot].role = Role::Used;
                        return Err(io_err(e));
                    }
                }
            }
        }
        Ok(report)
    }

    fn fill(&self) -> JournalFill {
        let len = self.sh.rings.file_len;
        let mut used = 0u64;
        let mut total = 0u64;
        for ring in &self.sh.rings.lanes {
            let inner = lock(&ring.inner);
            for (i, s) in inner.slots.iter().enumerate() {
                total += len;
                let holds = match s.role {
                    Role::Used | Role::Recycling => true,
                    Role::Active => ring.files[i].hi_off.load(Ordering::Acquire) > B,
                    Role::Free => false,
                };
                if holds {
                    used += len;
                }
            }
        }
        JournalFill {
            used_bytes: used,
            capacity_bytes: total,
        }
    }
}
