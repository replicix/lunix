//! Segment seals off the packer thread (ADR-33 T8).
//!
//! A full segment is handed to a short-lived sealer thread, so the packer
//! goes on appending to the next segment while the footer, `fdatasync`,
//! rename and directory `fsync` of the previous one run. At most
//! `max_in_flight` seals run at once (the packer waits for one to end when the
//! bound is reached); `0` seals inline on the packer thread, the behaviour
//! before T8. The map transaction of the cut is committed only after
//! [`Sealer::wait_all`] returned and the seals were checked, so no map entry
//! names a segment that is not sealed.

use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::Instant;

use lunix_iface::journal::WriteSeqRange;
use lunix_iface::store::{SealedSegment, SegmentWriter};
use lunix_iface::{Error, Result};

use crate::stats::ns;
use crate::worker::lock;

/// The outcome of one seal, by ticket.
pub(crate) enum Slot {
    Pending,
    Done(SealedSegment),
    Failed(Error),
    /// A failure already handed to the packer.
    Taken,
}

#[derive(Default)]
struct State {
    in_flight: usize,
    in_flight_max: u64,
    slots: Vec<Slot>,
    seals: u64,
    busy_ns: u64,
}

impl State {
    fn take_failure(&mut self) -> Option<Error> {
        let slot = self
            .slots
            .iter_mut()
            .find(|s| matches!(s, Slot::Failed(_)))?;
        match std::mem::replace(slot, Slot::Taken) {
            Slot::Failed(e) => Some(e),
            _ => None,
        }
    }
}

struct Shared {
    st: Mutex<State>,
    cv: Condvar,
}

/// What the seals of a cut cost.
#[derive(Clone, Copy, Default, Debug)]
pub(crate) struct SealFigures {
    pub seals: u64,
    pub in_flight_max: u64,
    /// Summed duration of the seals, wherever they ran.
    pub busy_ns: u64,
    /// Time the packer waited for a free seal slot.
    pub stall_ns: u64,
}

pub(crate) struct Sealer {
    max_in_flight: usize,
    shared: Arc<Shared>,
    stall_ns: u64,
}

/// Records the outcome of a seal even if the seal panics.
struct Completion {
    shared: Arc<Shared>,
    ticket: usize,
    started: Instant,
    outcome: Option<Result<SealedSegment>>,
}

impl Drop for Completion {
    fn drop(&mut self) {
        let outcome = self
            .outcome
            .take()
            .unwrap_or_else(|| Err(Error::invalid("segment seal panicked")));
        let mut g = lock(&self.shared.st);
        g.busy_ns = g.busy_ns.saturating_add(ns(self.started.elapsed()));
        g.seals += 1;
        g.slots[self.ticket] = match outcome {
            Ok(s) => Slot::Done(s),
            Err(e) => Slot::Failed(e),
        };
        g.in_flight -= 1;
        self.shared.cv.notify_all();
    }
}

fn run_seal(
    shared: Arc<Shared>,
    ticket: usize,
    writer: Box<dyn SegmentWriter>,
    writes: WriteSeqRange,
) {
    let mut done = Completion {
        shared,
        ticket,
        started: Instant::now(),
        outcome: None,
    };
    let r = writer.seal(Some(writes));
    if r.is_ok() {
        lunix_iface::crashpoint!("destage.after_segment_seal");
    }
    done.outcome = Some(r);
}

impl Sealer {
    pub(crate) fn new(max_in_flight: usize) -> Self {
        Self {
            max_in_flight,
            shared: Arc::new(Shared {
                st: Mutex::new(State::default()),
                cv: Condvar::new(),
            }),
            stall_ns: 0,
        }
    }

    /// Starts sealing `writer`. Waits first while the bound is reached.
    ///
    /// # Errors
    /// The error of an earlier seal that failed (the writer is then dropped
    /// and abandons its file); `Io` if no thread could be started.
    pub(crate) fn submit(
        &mut self,
        writer: Box<dyn SegmentWriter>,
        writes: WriteSeqRange,
    ) -> Result<()> {
        let ticket;
        {
            let t = Instant::now();
            let mut g = lock(&self.shared.st);
            let mut waited = false;
            loop {
                if let Some(e) = g.take_failure() {
                    return Err(e);
                }
                if self.max_in_flight == 0 || g.in_flight < self.max_in_flight {
                    break;
                }
                waited = true;
                g = self.shared.cv.wait(g).unwrap_or_else(|p| p.into_inner());
            }
            if waited {
                self.stall_ns = self.stall_ns.saturating_add(ns(t.elapsed()));
            }
            ticket = g.slots.len();
            g.slots.push(Slot::Pending);
            g.in_flight += 1;
            g.in_flight_max = g.in_flight_max.max(g.in_flight as u64);
        }
        if self.max_in_flight == 0 {
            run_seal(Arc::clone(&self.shared), ticket, writer, writes);
            return Ok(());
        }
        let shared = Arc::clone(&self.shared);
        let spawned = thread::Builder::new()
            .name("lunix-seal".into())
            .spawn(move || run_seal(shared, ticket, writer, writes));
        if let Err(e) = spawned {
            // The closure (and the writer in it) is gone: the file is
            // abandoned, the cut fails as for a failed seal.
            let mut g = lock(&self.shared.st);
            g.slots[ticket] = Slot::Failed(Error::Io(e));
            g.in_flight -= 1;
            self.shared.cv.notify_all();
        }
        Ok(())
    }

    /// Waits until no seal runs and returns the outcomes in submit order.
    pub(crate) fn wait_all(&mut self) -> Vec<Slot> {
        let mut g = lock(&self.shared.st);
        while g.in_flight > 0 {
            g = self.shared.cv.wait(g).unwrap_or_else(|p| p.into_inner());
        }
        std::mem::take(&mut g.slots)
    }

    pub(crate) fn figures(&self) -> SealFigures {
        let g = lock(&self.shared.st);
        SealFigures {
            seals: g.seals,
            in_flight_max: g.in_flight_max,
            busy_ns: g.busy_ns,
            stall_ns: self.stall_ns,
        }
    }
}
