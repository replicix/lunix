//! The ledger as a plain state machine: no locks, no threads, no clock of its
//! own. [`crate::LunixSpace`] wraps it in one short mutex, which is what makes
//! every operation (and `transfer` in particular) atomic for observers.

use std::collections::VecDeque;
use std::time::Duration;

use lunix_iface::space::{SpaceKind, SpaceSnapshot, Waiter, WaiterState};
use lunix_iface::{Error, Result};

use crate::policy::Policy;
use crate::probe::FsStat;

const HEADROOM: usize = SpaceKind::Headroom as usize;

/// A parked reservation.
#[derive(Debug)]
pub(crate) struct Parked {
    pub waiter: Waiter,
    pub kind: SpaceKind,
    pub bytes: u64,
    pub deadline: Option<Duration>,
}

/// The last file-system reading and the ledger level it was taken at.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct FsView {
    pub stat: FsStat,
    /// `used` when the reading was started. The room the file system still
    /// has is counted from there: `fs_ceiling = used_mark + free - floor`.
    pub used_mark: u64,
}

#[derive(Debug)]
pub(crate) struct Ledger {
    pub held: [u64; 7],
    pub waiters: VecDeque<Parked>,
    pub policy: Policy,
    pub fs: FsView,
    /// 2 x segment size, added to `policy.headroom`.
    pub segment_reserve: u64,
    pub read_only: bool,
}

impl Ledger {
    pub fn new(policy: Policy, segment_size: u64) -> Self {
        Self {
            held: [0; 7],
            waiters: VecDeque::new(),
            policy,
            fs: FsView::default(),
            segment_reserve: segment_size.saturating_mul(2),
            read_only: false,
        }
    }

    pub fn used(&self) -> u64 {
        SpaceKind::ALL
            .iter()
            .filter(|k| **k != SpaceKind::Headroom)
            .fold(0u64, |a, k| a.saturating_add(self.held[k.index()]))
    }

    /// The region writes can never take: 2 x segment size plus the headroom.
    pub fn reserve_region(&self) -> u64 {
        self.segment_reserve.saturating_add(self.policy.headroom)
    }

    /// What the host file system allows the whole local store, reserve
    /// included: `used_mark + free - floor`.
    fn fs_ceiling(&self) -> u64 {
        self.fs
            .used_mark
            .saturating_add(self.fs.stat.free)
            .saturating_sub(self.policy.floor_bytes(self.fs.stat.total))
    }

    /// The limit for writes: `min(cache_max, fs_ceiling - reserve)`.
    pub fn limit(&self) -> u64 {
        let fs = self.fs_ceiling().saturating_sub(self.reserve_region());
        self.policy.cache_max.map_or(fs, |c| c.min(fs))
    }

    pub fn fits(&self, bytes: u64) -> bool {
        self.used().saturating_add(bytes) <= self.limit()
    }

    /// True when charging `bytes` would reach 95 % of the limit.
    pub fn near_limit(&self, bytes: u64) -> bool {
        let limit = self.limit();
        self.used().saturating_add(bytes) >= limit - limit / 20
    }

    pub fn charge_raw(&mut self, kind: SpaceKind, bytes: u64) {
        let h = &mut self.held[kind.index()];
        *h = h.saturating_add(bytes);
    }

    /// Drops waiters at the front that are no longer pending.
    pub fn prune_front(&mut self) {
        while self
            .waiters
            .front()
            .is_some_and(|p| p.waiter.state() != WaiterState::Pending)
        {
            self.waiters.pop_front();
        }
    }

    pub fn pending_waiters(&self) -> u64 {
        self.waiters
            .iter()
            .filter(|p| p.waiter.state() == WaiterState::Pending)
            .count() as u64
    }

    /// How many bytes the head waiter is short of fitting (0 if none).
    pub fn head_shortfall(&self) -> u64 {
        self.waiters
            .iter()
            .find(|p| p.waiter.state() == WaiterState::Pending)
            .map_or(0, |p| {
                self.used()
                    .saturating_add(p.bytes)
                    .saturating_sub(self.limit())
            })
    }

    /// Grants parked requests that now fit, strictly first in, first out.
    /// A granted request is charged before it is told.
    pub fn grant_waiters(&mut self) {
        loop {
            self.prune_front();
            let Some(front) = self.waiters.front() else {
                return;
            };
            if !self.fits(front.bytes) {
                return;
            }
            let Some(p) = self.waiters.pop_front() else {
                return;
            };
            self.charge_raw(p.kind, p.bytes);
            if !p.waiter.grant() {
                // Cancelled between the check and the grant: undo.
                let h = &mut self.held[p.kind.index()];
                *h = h.saturating_sub(p.bytes);
            }
        }
    }

    /// Expires waiters whose deadline is at or before `now`; returns how many
    /// this call expired, then lets the ones behind them be granted.
    pub fn expire(&mut self, now: Duration) -> usize {
        let n = self
            .waiters
            .iter()
            .filter(|p| p.deadline.is_some_and(|d| d <= now) && p.waiter.expire())
            .count();
        self.waiters
            .retain(|p| p.waiter.state() == WaiterState::Pending);
        self.grant_waiters();
        n
    }

    /// Expires every parked request (the volume went read-only).
    pub fn expire_all(&mut self) {
        for p in self.waiters.drain(..) {
            p.waiter.expire();
        }
    }

    /// `reserve_headroom`: bounded by the whole store (limit plus reserve),
    /// counting the bounds already held; the `Headroom` pseudo-kind is also
    /// capped at the reserved region itself.
    pub fn reserve_headroom(&mut self, kind: SpaceKind, bytes: u64) -> Result<()> {
        let region = self.reserve_region();
        // The pseudo-kind lives entirely inside the reserved region, so the
        // objects it pays for can never push `used` more than `region` past
        // the limit.
        if kind == SpaceKind::Headroom && self.held[HEADROOM].saturating_add(bytes) > region {
            return Err(Error::NoSpace);
        }
        let ceiling = self.limit().saturating_add(region);
        let committed = self.used().saturating_add(self.held[HEADROOM]);
        if committed.saturating_add(bytes) > ceiling {
            return Err(Error::NoSpace);
        }
        self.charge_raw(kind, bytes);
        Ok(())
    }

    pub fn release(&mut self, kind: SpaceKind, bytes: u64) {
        debug_assert!(
            self.held[kind.index()] >= bytes,
            "released more than {kind:?} holds"
        );
        let h = &mut self.held[kind.index()];
        *h = h.saturating_sub(bytes);
        self.grant_waiters();
    }

    pub fn transfer(
        &mut self,
        from_kind: SpaceKind,
        to_kind: SpaceKind,
        from_bytes: u64,
        to_bytes: u64,
    ) -> Result<()> {
        if self.held[from_kind.index()] < from_bytes {
            return Err(Error::invalid("transfer exceeds what the kind holds"));
        }
        self.held[from_kind.index()] -= from_bytes;
        self.charge_raw(to_kind, to_bytes);
        self.grant_waiters();
        Ok(())
    }

    pub fn snapshot(&self) -> SpaceSnapshot {
        SpaceSnapshot {
            by_kind: self.held,
            limit: self.limit(),
            fs_free: self.fs.stat.free,
            headroom_total: self.reserve_region(),
            waiters: self.pending_waiters(),
            read_only: self.read_only,
        }
    }
}
