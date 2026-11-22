//! The ledger as a plain state machine: no locks, no threads, no clock of its
//! own. [`crate::LunixSpace`] wraps it in one short mutex, which is what makes
//! every operation (and `transfer` in particular) atomic for observers.

use std::collections::VecDeque;
use std::time::Duration;

use lunix_format::local::OnFull;
use lunix_iface::space::{
    ReserveClass, ReserveOutcome, SpaceKind, SpaceSnapshot, ThrottleState, Waiter, WaiterState,
};
use lunix_iface::wake::WakeFd;
use lunix_iface::{Error, Result};

use crate::policy::Policy;
use crate::probe::FsStat;
use crate::throttle::{Bucket, Counters, Meter, StateClock, ThrottleConfig, FACTOR_ONE};

const HEADROOM: usize = SpaceKind::Headroom as usize;

/// A parked reservation. Every one has a deadline (ADR-32): its arrival plus
/// the policy's `full_timeout`, recomputed when the policy changes.
#[derive(Debug)]
pub(crate) struct Parked {
    pub waiter: Waiter,
    pub kind: SpaceKind,
    pub bytes: u64,
    pub arrived: Duration,
    pub deadline: Duration,
    /// Only the ring pace delays it (the space curve is not pacing): its
    /// wait is counted in the ring pace's figures.
    pub ring_paced: bool,
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
    /// Latched full (ADR-32): a wait for space expired, so writes fail at
    /// once until the headroom reaches [`Ledger::resume_mark`].
    pub full: bool,
    /// A quiesce began (`LunixSpace::begin_close`): nothing parks any more,
    /// a reservation that does not fit fails at once (decision M5).
    pub closing: bool,
    /// Times the volume latched full since the gate was built.
    pub fulls: u64,
    /// The paced band's settings (ADR-32).
    pub throttle: ThrottleConfig,
    /// The rate the engine paces admissions to while the journal ring is
    /// nearly full (`SpaceGate::set_ring_pace`), bytes per second. A second
    /// input of the band: the space state does not follow it.
    pub ring_pace: Option<u64>,
    /// The engine's store factor `A_hat` (`SpaceGate::set_store_pace`),
    /// thousandths; 1000 before its first cut.
    pub store_factor: u64,
    /// The journal bytes of the cut in flight (0 between cuts).
    pub store_in_flight: u64,
    pub bucket: Bucket,
    pub counters: Counters,
    pub state: StateClock,
    meter: Meter,
}

impl Ledger {
    pub fn new(policy: Policy, segment_size: u64, throttle: ThrottleConfig, now: Duration) -> Self {
        Self {
            held: [0; 7],
            waiters: VecDeque::new(),
            policy,
            fs: FsView::default(),
            segment_reserve: segment_size.saturating_mul(2),
            read_only: false,
            full: false,
            closing: false,
            fulls: 0,
            throttle,
            ring_pace: None,
            store_factor: FACTOR_ONE,
            store_in_flight: 0,
            bucket: Bucket::new(),
            counters: Counters::default(),
            state: StateClock::new(now),
            meter: Meter::default(),
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

    /// `limit - used`, the headroom below the limit (ADR-32's signal).
    pub fn headroom(&self) -> u64 {
        self.limit().saturating_sub(self.used())
    }

    /// The resume mark of the full latch: the larger of 4 segments and 1 % of
    /// the limit, and never more than half the limit (so a small test volume
    /// can come back at all).
    pub fn resume_mark(&self) -> u64 {
        let limit = self.limit();
        self.segment_reserve
            .saturating_mul(2)
            .max(limit / 100)
            .min(limit / 2)
    }

    /// Opens the latch once the headroom reached the resume mark.
    pub fn update_latch(&mut self) {
        // At least one byte of headroom: at a zero limit the mark is 0, and
        // the latch must not open in the call that closed it.
        if self.full && self.headroom() >= self.resume_mark().max(1) {
            self.full = false;
        }
    }

    /// What the ladder should try to free: every waiting reservation plus
    /// the resume mark, less the headroom there is (ADR-32 §5.6).
    pub fn ladder_want(&self) -> u64 {
        let waiting = self
            .waiters
            .iter()
            .filter(|p| p.waiter.state() == WaiterState::Pending)
            .fold(0u64, |a, p| a.saturating_add(p.bytes));
        waiting
            .saturating_add(self.resume_mark())
            .saturating_sub(self.headroom())
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

    /// The soft mark now (0 with the band off).
    pub fn soft(&self) -> u64 {
        self.throttle.soft_mark(self.limit())
    }

    /// The band's rate now, bytes per second: `None` above the soft mark,
    /// with the band off, or once a quiesce began (it admits everything that
    /// fits).
    pub fn band_rate(&self) -> Option<u64> {
        if self.closing {
            return None;
        }
        self.throttle.paced_rate(
            self.headroom(),
            self.soft(),
            self.biased_factor(),
            self.pending_journal(),
        )
    }

    /// The factor the curve reads: the engine's `A_hat` with the margin.
    pub fn biased_factor(&self) -> u64 {
        self.throttle.biased_factor(self.store_factor)
    }

    /// `J`: the journal bytes reserved and not yet in a cut. The cut in
    /// flight is left out: its segment bound is held already.
    pub fn pending_journal(&self) -> u64 {
        self.held[SpaceKind::JournalReserved.index()].saturating_sub(self.store_in_flight)
    }

    /// The projected headroom `headroom - (A_hat - 1) * J`.
    pub fn projected_headroom(&self) -> u64 {
        ThrottleConfig::projected_headroom(
            self.headroom(),
            self.biased_factor(),
            self.pending_journal(),
        )
    }

    /// Records the store factor and the cut in flight; true when either
    /// changed and the pace may be different now.
    pub fn set_store_pace(&mut self, factor_milli: u64, in_flight: u64) -> bool {
        let factor = factor_milli.max(FACTOR_ONE);
        if self.store_factor == factor && self.store_in_flight == in_flight {
            return false;
        }
        self.store_factor = factor;
        self.store_in_flight = in_flight;
        self.throttle.pace_store_bytes
    }

    /// The rate admissions are paced at now: the space curve's and the ring
    /// pace's, the slower of the two. `None` when neither paces, with the band
    /// off, or once a quiesce began. The throttle state follows
    /// [`band_rate`](Self::band_rate) only.
    pub fn pace_rate(&self) -> Option<u64> {
        if self.closing || !self.throttle.enabled {
            return None;
        }
        match (self.band_rate(), self.ring_pace) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// Sets the ring pace (raised to the band's minimum rate).
    pub fn set_ring_pace(&mut self, rate: Option<u64>) {
        self.ring_pace = rate.map(|r| r.max(self.throttle.min_rate));
    }

    /// The first request still waiting.
    fn head(&self) -> Option<&Parked> {
        self.waiters
            .iter()
            .find(|p| p.waiter.state() == WaiterState::Pending)
    }

    /// The gate's state from the ledger.
    pub fn throttle_state(&self) -> ThrottleState {
        if self.full {
            ThrottleState::Full
        } else if self.head().is_some_and(|p| !self.fits(p.bytes)) {
            ThrottleState::Stop
        } else if self.band_rate().is_some() {
            ThrottleState::Slowdown
        } else {
            ThrottleState::Open
        }
    }

    /// Records the state at `now` (and the rate meter); drops the bucket when
    /// the band is left. Returns true when the state changed.
    pub fn note_state(&mut self, now: Duration) -> bool {
        let to = self.throttle_state();
        if self.pace_rate().is_none() {
            self.bucket.deactivate();
        }
        self.meter.sample(now, &self.counters);
        self.state.enter(to, now, &mut self.counters)
    }

    /// A request that waited in the gate was admitted. Its wait is counted
    /// from its arrival (at its queue), not from the moment it parked: a
    /// request that stood behind others before it reached the gate waited
    /// all that time too (the delay cap and `delay_max_ms` share this
    /// origin).
    fn note_delay(&mut self, p: &Parked, now: Duration, granted: bool) {
        let ms = u64::try_from(now.saturating_sub(p.arrived).as_millis()).unwrap_or(u64::MAX);
        if p.ring_paced {
            self.counters.ring_delay_max_ms = self.counters.ring_delay_max_ms.max(ms);
        } else {
            self.counters.delay_max_ms = self.counters.delay_max_ms.max(ms);
        }
        if granted {
            if p.ring_paced {
                self.counters.ring_delayed_writes += 1;
                self.counters.ring_delayed_ms = self.counters.ring_delayed_ms.saturating_add(ms);
            } else {
                self.counters.delayed_writes += 1;
                self.counters.delayed_ms = self.counters.delayed_ms.saturating_add(ms);
            }
            self.counters.admitted = self.counters.admitted.saturating_add(p.bytes);
        }
    }

    /// A request admitted at its first contact with the gate while the pace
    /// is active, after it stood in its queue since `arrived` (behind requests that were pacing or
    /// waiting): that wait is a delay like any other, counted from the
    /// arrival. Under a millisecond it is not a delay (a request reaches the
    /// gate within microseconds of its arrival).
    fn note_direct_delay(&mut self, arrived: Duration, now: Duration) {
        let waited = now.saturating_sub(arrived);
        if waited < Duration::from_millis(1) {
            return;
        }
        let ms = u64::try_from(waited.as_millis()).unwrap_or(u64::MAX);
        if self.ring_only() {
            self.counters.ring_delay_max_ms = self.counters.ring_delay_max_ms.max(ms);
            self.counters.ring_delayed_writes += 1;
            self.counters.ring_delayed_ms = self.counters.ring_delayed_ms.saturating_add(ms);
        } else {
            self.counters.delay_max_ms = self.counters.delay_max_ms.max(ms);
            self.counters.delayed_writes += 1;
            self.counters.delayed_ms = self.counters.delayed_ms.saturating_add(ms);
        }
    }

    /// True when the ring pace alone paces now (the space curve does not).
    fn ring_only(&self) -> bool {
        self.band_rate().is_none() && self.pace_rate().is_some()
    }

    /// The time at which the delay cap admits a request that arrived at
    /// `arrived`: the cap, or the volume's `full_timeout` if that is shorter
    /// (a paced request is never failed for the pace).
    fn cap_at(&self, arrived: Duration) -> Duration {
        arrived.saturating_add(self.throttle.max_delay.min(self.policy.full_timeout))
    }

    /// Grants parked requests that now fit, strictly first in, first out. A
    /// granted request is charged before it is told. Inside the band the head
    /// also needs its bytes from the bucket, or to have waited the delay cap
    /// (then the bucket carries the debt).
    pub fn grant_waiters(&mut self, now: Duration) {
        self.grant_inner(now, self.policy.on_full == OnFull::Error);
    }

    /// [`grant_waiters`](Self::grant_waiters); `refuse_unfit` refuses a head
    /// that does not fit at once (`on_full = error`) instead of stopping.
    fn grant_inner(&mut self, now: Duration, refuse_unfit: bool) {
        self.update_latch();
        loop {
            self.prune_front();
            let Some(front) = self.waiters.front() else {
                break;
            };
            let bytes = front.bytes;
            let arrived = front.arrived;
            if !self.fits(bytes) {
                if refuse_unfit {
                    // `error` refuses a request that does not fit at once:
                    // one that was paced while it fitted does not wait for
                    // space, and nothing latches (review M3).
                    if let Some(p) = self.waiters.pop_front() {
                        if p.waiter.expire() {
                            self.note_delay(&p, now, false);
                            self.counters.enospc += 1;
                        }
                    }
                    continue;
                }
                break;
            }
            if let Some(rate) = self.pace_rate() {
                self.bucket.refill(now, rate);
                if self.bucket.has(bytes) {
                    self.bucket.take(bytes);
                } else if now >= self.cap_at(arrived) || self.due_behind(now) {
                    self.bucket.overdraw(bytes, rate);
                    if self.ring_only() {
                        self.counters.ring_capped += 1;
                    } else {
                        self.counters.delay_capped += 1;
                    }
                } else {
                    break;
                }
            }
            let Some(p) = self.waiters.pop_front() else {
                break;
            };
            self.charge_raw(p.kind, p.bytes);
            if p.waiter.grant() {
                self.note_delay(&p, now, true);
            } else {
                // Cancelled between the check and the grant: undo.
                let h = &mut self.held[p.kind.index()];
                *h = h.saturating_sub(p.bytes);
            }
        }
        self.note_state(now);
    }

    /// True when a request behind the head has waited its delay cap: the
    /// gate's queue is in order of `reserve` calls, not of arrival, so an old
    /// request can stand behind a young head. The head, and what stands
    /// before the due request, is admitted with it (the bucket takes the
    /// debt): a request that fits is never failed for the pace (review M1).
    fn due_behind(&self, now: Duration) -> bool {
        // The scan stops at a request that does not fit: nothing behind it
        // can follow the head.
        self.waiters
            .iter()
            .skip(1)
            .take_while(|p| self.fits(p.bytes))
            .any(|p| p.waiter.state() == WaiterState::Pending && now >= self.cap_at(p.arrived))
    }

    /// Expires waiters whose deadline is at or before `now`. The first expiry
    /// latches the volume full and fails every other request waiting for
    /// space with it (ADR-32). Returns how many this call failed, then lets
    /// what fits be granted (none is left unless the latch opened).
    pub fn expire(&mut self, now: Duration) -> usize {
        // A paced request that fits is admitted by its cap before its
        // deadline can latch the volume.
        self.grant_waiters(now);
        // Under `error` nothing waits for space and nothing latches.
        if self.policy.on_full == OnFull::Error {
            return 0;
        }
        // What is left after the grants is a request that does not fit, or
        // stands behind one (the cap is never later than the deadline): the
        // wait is for space.
        let due = self
            .waiters
            .iter()
            .any(|p| p.deadline <= now && p.waiter.state() == WaiterState::Pending);
        if !due {
            return 0;
        }
        let n = self.fail_waiters(now, true);
        self.full = true;
        self.fulls += 1;
        self.grant_waiters(now);
        n
    }

    /// Fails every pending request with `TimedOut` (the queue answers
    /// `ENOSPC`, or `EIO` on a read-only volume); returns how many.
    /// `enospc` counts them as failed for space.
    pub fn fail_waiters(&mut self, now: Duration, enospc: bool) -> usize {
        let all: Vec<Parked> = self.waiters.drain(..).collect();
        let mut n = 0;
        for p in all {
            if p.waiter.expire() {
                self.note_delay(&p, now, false);
                n += 1;
            }
        }
        if enospc {
            self.counters.enospc += n as u64;
        }
        self.note_state(now);
        n
    }

    /// The earliest deadline of a pending request.
    pub fn next_deadline(&self) -> Option<Duration> {
        self.waiters
            .iter()
            .filter(|p| p.waiter.state() == WaiterState::Pending)
            .map(|p| p.deadline)
            .min()
    }

    /// When the timer thread must look again: the earliest deadline, and for
    /// a paced head the moment its tokens are there or the delay cap passes.
    pub fn next_wake(&self, now: Duration) -> Option<Duration> {
        let deadline = self.next_deadline();
        let paced = self.head().and_then(|p| {
            let rate = self.pace_rate()?;
            if !self.fits(p.bytes) {
                return None;
            }
            let mut b = self.bucket;
            b.refill(now, rate);
            let tokens = now.saturating_add(b.wait_for(p.bytes, rate));
            Some(tokens.min(self.cap_at(p.arrived)))
        });
        // A request behind the head is due at its own cap, which may be
        // before the head's tokens (it arrived earlier, in another queue):
        // the head and what stands before it are admitted with it
        // (`due_behind`). The scan stops where a request does not fit.
        let due = if self.pace_rate().is_some() {
            // The same scan as `due_behind`: up to the first request that
            // does not fit, whatever its state.
            self.waiters
                .iter()
                .take_while(|p| self.fits(p.bytes))
                .filter(|p| p.waiter.state() == WaiterState::Pending)
                .map(|p| self.cap_at(p.arrived))
                .min()
        } else {
            None
        };
        [deadline, paced, due].into_iter().flatten().min()
    }

    /// Re-judges every parked request under the current policy (after
    /// `reconfigure`): deadlines count from each arrival with the new
    /// `full_timeout`; what fits now is granted; under `on_full = error` (or
    /// once closing) the rest fail now, else the overdue ones expire (and
    /// latch). Returns how many failed.
    pub fn rejudge(&mut self, now: Duration) -> usize {
        if self.policy.on_full == OnFull::Error {
            // `error` refuses per request; a latch from `wait` ends here.
            self.full = false;
        }
        let t = self.policy.full_timeout;
        for p in &mut self.waiters {
            p.deadline = p.arrived.saturating_add(t);
        }
        // The model of `on_full = error`: grant what fits, then refuse
        // (the grant loop itself would refuse a head that does not fit and
        // go on to grant what stands behind it).
        self.grant_inner(now, false);
        if self.closing {
            self.fail_waiters(now, true)
        } else if self.policy.on_full == OnFull::Error {
            self.fail_unfit(now)
        } else {
            self.expire(now)
        }
    }

    /// Fails the pending requests (`on_full = error`), except those inside
    /// the band that fit: they wait for their pace, not for space, and pacing
    /// is not a refusal (ADR-32). Returns how many failed.
    pub fn fail_unfit(&mut self, now: Duration) -> usize {
        let all: Vec<Parked> = self.waiters.drain(..).collect();
        let paced = self.pace_rate().is_some();
        let mut n = 0;
        for p in all {
            if p.waiter.state() != WaiterState::Pending {
                continue;
            }
            if paced && self.fits(p.bytes) {
                self.waiters.push_back(p);
            } else if p.waiter.expire() {
                self.note_delay(&p, now, false);
                n += 1;
            }
        }
        self.counters.enospc += n as u64;
        self.note_state(now);
        n
    }

    /// Expires every parked request (the volume went read-only).
    pub fn expire_all(&mut self, now: Duration) {
        self.fail_waiters(now, false);
    }

    /// Decides a reservation (ADR-32): charges it, parks it behind the head
    /// (for space, or for the band's pace) or refuses it. `arrived` is when
    /// the request reached its queue.
    pub fn reserve(
        &mut self,
        kind: SpaceKind,
        bytes: u64,
        wake: Option<&WakeFd>,
        arrived: Duration,
        class: ReserveClass,
        now: Duration,
    ) -> ReserveOutcome {
        self.update_latch();
        if self.full {
            // Latched full: fail at once until the headroom recovers.
            self.counters.enospc += 1;
            self.note_state(now);
            return ReserveOutcome::NoSpace;
        }
        self.prune_front();
        let no_wait = self.policy.on_full == OnFull::Error || self.closing;
        if !self.fits(bytes) {
            if no_wait {
                self.counters.enospc += 1;
                return ReserveOutcome::NoSpace;
            }
            return self.park(kind, bytes, wake, arrived, now, false);
        }
        let head_fits = self.head().is_none_or(|p| self.fits(p.bytes));
        let ahead = self.head().is_some();
        if class == ReserveClass::Retry && head_fits {
            // Paid at the first pass: no tokens, no overdraw, no cap.
            self.admit(kind, bytes);
            self.note_state(now);
            return ReserveOutcome::Reserved;
        }
        let Some(rate) = self.pace_rate() else {
            if !ahead || no_wait {
                // Nothing paces: whatever the request waited in its queue
                // (a parked head, the ring, a slow append) is not the gate's
                // delay and is not counted here.
                self.admit(kind, bytes);
                return ReserveOutcome::Reserved;
            }
            return self.park(kind, bytes, wake, arrived, now, false);
        };
        // Inside the band.
        self.bucket.refill(now, rate);
        if class == ReserveClass::Sync && head_fits {
            // The write an fsync waits on: at once, charged to the bucket.
            self.bucket.overdraw(bytes, rate);
            if self.ring_only() {
                self.counters.ring_bypassed_fua += 1;
            } else {
                self.counters.bypassed_fua += 1;
            }
            self.admit(kind, bytes);
            self.note_state(now);
            return ReserveOutcome::Reserved;
        }
        if !ahead {
            if self.bucket.has(bytes) {
                self.bucket.take(bytes);
                self.note_direct_delay(arrived, now);
                self.admit(kind, bytes);
                self.note_state(now);
                return ReserveOutcome::Reserved;
            }
            if now >= self.cap_at(arrived) {
                // It waited its cap already (in its queue): admitted, the
                // bucket carries the debt.
                self.bucket.overdraw(bytes, rate);
                if self.ring_only() {
                    self.counters.ring_capped += 1;
                } else {
                    self.counters.delay_capped += 1;
                }
                self.note_direct_delay(arrived, now);
                self.admit(kind, bytes);
                self.note_state(now);
                return ReserveOutcome::Reserved;
            }
        }
        self.park(kind, bytes, wake, arrived, now, head_fits)
    }

    /// Charges an admitted reservation.
    fn admit(&mut self, kind: SpaceKind, bytes: u64) {
        self.charge_raw(kind, bytes);
        self.counters.admitted = self.counters.admitted.saturating_add(bytes);
    }

    /// Parks a request behind the queue. `fits` says the request itself fits
    /// (it waits for its turn or for tokens): `Paced`; else it waits for
    /// space: `Parked`.
    fn park(
        &mut self,
        kind: SpaceKind,
        bytes: u64,
        wake: Option<&WakeFd>,
        arrived: Duration,
        now: Duration,
        fits: bool,
    ) -> ReserveOutcome {
        let waiter = Waiter::new(wake.cloned());
        let arrived = arrived.min(now);
        let deadline = arrived.saturating_add(self.policy.full_timeout);
        let ring_paced = fits && self.fits(bytes) && self.ring_only();
        self.waiters.push_back(Parked {
            waiter: waiter.clone(),
            kind,
            bytes,
            arrived,
            deadline,
            // A request that waits for space is the space throttle's, one
            // that waits for tokens is the ring pace's when only it paces.
            ring_paced,
        });
        let fits = fits && self.fits(bytes);
        if deadline <= now {
            if !fits {
                // It waited its full_timeout already, in its queue or for
                // the journal ring (the wait counts from the arrival):
                // refused now, without parking and without the latch, since
                // it never waited in the gate (review m1).
                self.waiters.pop_back();
                self.counters.enospc += 1;
                return ReserveOutcome::NoSpace;
            }
            // A request that fits is only waiting for its pace: it is due,
            // so it is admitted with what stands before it (review M1). If
            // that is not so (a request that waits for space stands before
            // it), it never waited in the gate: refused now, without the
            // latch, and what stands before it keeps its own deadline
            // (re-review N1).
            self.grant_waiters(now);
            if waiter.state() == WaiterState::Pending {
                // Still ours, still at the back.
                self.waiters.pop_back();
                self.counters.enospc += 1;
                self.note_state(now);
                return ReserveOutcome::NoSpace;
            }
        }
        if fits && deadline > now && now >= self.cap_at(arrived) {
            // Past its delay cap on first contact (it waited in its queue):
            // it is admitted now, with what stands before it, not at the
            // next tick. A request that does not fit, or stands behind one
            // that does not, stays: the grant loop stops there.
            self.grant_waiters(now);
        }
        self.note_state(now);
        if fits {
            ReserveOutcome::Paced(waiter)
        } else {
            ReserveOutcome::Parked(waiter)
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

    pub fn release(&mut self, kind: SpaceKind, bytes: u64, now: Duration) {
        debug_assert!(
            self.held[kind.index()] >= bytes,
            "released more than {kind:?} holds"
        );
        let h = &mut self.held[kind.index()];
        *h = h.saturating_sub(bytes);
        if !matches!(kind, SpaceKind::JournalReserved | SpaceKind::Headroom) {
            self.counters.freed = self.counters.freed.saturating_add(bytes);
        }
        self.grant_waiters(now);
    }

    pub fn transfer(
        &mut self,
        from_kind: SpaceKind,
        to_kind: SpaceKind,
        from_bytes: u64,
        to_bytes: u64,
        now: Duration,
    ) -> Result<()> {
        if self.held[from_kind.index()] < from_bytes {
            return Err(Error::invalid("transfer exceeds what the kind holds"));
        }
        self.held[from_kind.index()] -= from_bytes;
        self.charge_raw(to_kind, to_bytes);
        self.grant_waiters(now);
        Ok(())
    }

    /// The ledger as a snapshot at `now`.
    pub fn snapshot(&mut self, now: Duration) -> SpaceSnapshot {
        self.note_state(now);
        let pending = self.pending_waiters();
        let head_fits = self.head().is_some_and(|p| self.fits(p.bytes));
        let (paced, space) = if head_fits {
            (pending, 0)
        } else {
            (0, pending)
        };
        let mut t =
            crate::throttle::figures(&self.counters, &self.state, &self.meter, now, self.fulls);
        t.headroom = self.headroom();
        t.soft = self.soft();
        t.resume = self.resume_mark();
        t.rate = self.band_rate();
        t.ring_pace_rate = self.pace_rate().and(self.ring_pace).unwrap_or(0);
        t.store_factor_milli = self.store_factor;
        t.store_factor_biased_milli = self.biased_factor();
        t.store_pending = self.pending_journal();
        t.h_proj = self.projected_headroom();
        t.paced_waiters = paced;
        t.space_waiters = space;
        SpaceSnapshot {
            by_kind: self.held,
            limit: self.limit(),
            fs_free: self.fs.stat.free,
            headroom_total: self.reserve_region(),
            waiters: pending,
            read_only: self.read_only,
            full: self.full,
            throttle: t,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lunix_format::local::{FreeFloor, SpaceSettings};

    const GIB: u64 = 1 << 30;
    const REC: u64 = 64 << 10;

    /// A 10 GiB limit, 8 MiB of headroom (inside the band at its minimum
    /// rate, 64 KiB/s), a 10 s cap, the bucket empty.
    fn band_ledger() -> Ledger {
        let s = SpaceSettings {
            free_floor: FreeFloor::Bytes(0),
            on_full: OnFull::Wait,
            full_timeout_ms: Some(60_000),
            cache_max: None,
            cache_max_percent: None,
            headroom: 0,
        };
        let policy = Policy::from_settings(&s, Duration::from_secs(60)).expect("policy");
        let mut throttle = ThrottleConfig::default().with_gc_rate(0);
        throttle.min_rate = 64 << 10;
        let mut l = Ledger::new(policy, 0, throttle, Duration::ZERO);
        l.fs = FsView {
            stat: FsStat {
                total: 10 * GIB,
                free: 10 * GIB,
            },
            used_mark: 0,
        };
        l.charge_raw(SpaceKind::Segments, 10 * GIB - (8 << 20));
        for _ in 0..16 {
            let o = l.reserve(
                SpaceKind::JournalReserved,
                REC,
                None,
                Duration::ZERO,
                ReserveClass::Bulk,
                Duration::ZERO,
            );
            assert!(matches!(o, ReserveOutcome::Reserved));
        }
        l
    }

    #[test]
    fn next_wake_is_the_earliest_cap_among_the_requests_that_fit() {
        // Head (young) has a long way to its tokens (a 1 MiB record at
        // 64 KiB/s: 16 s) and its cap is at 11 s; a request behind it,
        // arrived at 0, is due at 10 s: the timer must wake then.
        let mut l = band_ledger();
        let now = Duration::from_secs(1);
        l.bucket.refill(now, 64 << 10);
        while l.bucket.has(REC) {
            l.bucket.take(REC);
        }
        let big = l.reserve(
            SpaceKind::JournalReserved,
            1 << 20,
            None,
            now,
            ReserveClass::Bulk,
            now,
        );
        assert!(matches!(big, ReserveOutcome::Paced(_)), "{big:?}");
        let old = l.reserve(
            SpaceKind::JournalReserved,
            REC,
            None,
            Duration::ZERO,
            ReserveClass::Bulk,
            now,
        );
        assert!(matches!(old, ReserveOutcome::Paced(_)), "{old:?}");
        assert_eq!(l.next_wake(now), Some(Duration::from_secs(10)));
    }
}
