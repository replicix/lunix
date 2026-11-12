//! Flush groups (DESIGN §4.1, ADR-4): FLUSH requests and FUA writes from
//! every queue gather into the next group; one `submit_group_flush` per
//! group, at most one group in flight, the next accumulating meanwhile.
//!
//! A group's `upto` is `Journal::last_issued()` read **after** its waiters
//! were taken, so it covers every write acknowledged before any of them
//! arrived (and, for a FUA write, the write itself: ADR-28's cut at its own
//! `write_seq`).

use std::sync::{Arc, Mutex, MutexGuard};

use lunix_iface::block::Tag;
use lunix_iface::journal::Journal;

use crate::queue::{Deferred, QueueShared};
use crate::stats::{add, max, Counters};

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

type Waiter = (Arc<QueueShared>, Tag, u64);

#[derive(Default)]
struct State {
    in_flight: bool,
    next: Vec<Waiter>,
}

/// The engine's flush groups.
#[derive(Default)]
pub(crate) struct FlushGroups {
    st: Mutex<State>,
}

fn finish(waiters: Vec<Waiter>, ok: bool) {
    for (q, tag, gen) in waiters {
        q.post(Deferred::Flush { tag, gen, ok });
    }
}

impl FlushGroups {
    /// Adds a waiter to the next group. Queue thread.
    pub(crate) fn join(&self, queue: Arc<QueueShared>, tag: Tag, gen: u64) {
        lock(&self.st).next.push((queue, tag, gen));
    }

    /// Forms and submits the next group if none is in flight. Non-blocking;
    /// called from queue threads.
    pub(crate) fn kick(self: &Arc<Self>, journal: &Arc<dyn Journal>, stats: &Counters) {
        let waiters = {
            let mut st = lock(&self.st);
            if st.in_flight || st.next.is_empty() {
                return;
            }
            st.in_flight = true;
            std::mem::take(&mut st.next)
        };
        let n = waiters.len() as u64;
        add(&stats.flush_groups, 1);
        add(&stats.flush_group_waiters, n);
        max(&stats.flush_group_max, n);
        let upto = journal.last_issued();
        // The waiters are shared with the callback so that a submission the
        // journal refuses (without ever calling `done`) still completes them.
        let slot = Arc::new(Mutex::new(Some(waiters)));
        let cb_slot = Arc::clone(&slot);
        let me = Arc::clone(self);
        let done = Box::new(move |res: lunix_iface::Result<()>| {
            if let Some(w) = lock(&cb_slot).take() {
                finish(w, res.is_ok());
            }
            me.group_done();
        });
        if journal.submit_group_flush(upto, done).is_err() {
            if let Some(w) = lock(&slot).take() {
                finish(w, false);
                self.group_done();
            }
        }
    }

    /// The group in flight ended: wake the queues of the next group's
    /// waiters, so one of them forms it from its `poll`.
    fn group_done(&self) {
        let queues: Vec<Arc<QueueShared>> = {
            let mut st = lock(&self.st);
            st.in_flight = false;
            st.next.iter().map(|w| Arc::clone(&w.0)).collect()
        };
        for q in queues {
            q.signal();
        }
    }
}
