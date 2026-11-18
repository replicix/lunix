# Write back-pressure and the full volume

Date: 2026-11-18. Status: design note for the **proposed**
[ADR-32](../DECISIONS.md#adr-32-write-back-pressure-a-paced-admission-band-below-the-limit-and-a-bounded-full-volume).
It carries the evidence, the analysis, the prior art, the alternatives and
the implementation sketch. Nothing here is decided until the maintainer
accepts the ADR.

Method and limits. The code claims come from reading `main` at `6d16fbf` and
`p1/gcrate` at `5773f3a` (crates `lunix-space`, `lunix-engine`, `lunix-gc`,
`lunix-ublk`, `lunixd`, `lunix-iface`). Nothing was compiled, run or measured
for this note: the three observations in §1 are the maintainer's runs of
2026-11-18, and every "why" attached to them is derived from the code and
marked as such. The prior-art numbers were checked against the OpenZFS, RocksDB
and kernel device-mapper documentation on 2026-11-18 where a page states them;
the rest are from the upstream sources as I know them and are marked
UNVERIFIED. File-system reactions to `ENOSPC` from a block device are
UNVERIFIED on the test kernel; §9 lists the cell that records them.

## Summary

1. **Two different problems.** "Garbage outruns GC" is a rate problem: the
   store holds about `R × τ` of garbage, where `R` is the write rate and `τ`
   the time a dead byte waits to be deleted (about 95 s with the defaults on
   `p1/gcrate`, of which the grace is 60 s). "The volume is full" is a
   capacity problem: live data plus the unavoidable overhead reach the limit.
   The first wants writes **slowed**; the second wants them **refused, in
   bounded time**. Today both end in the same place: a write parked in the
   space gate with no deadline.
2. **A parked write can stay parked for ever, by six separate paths** (§2).
   The default policy is `on_full = wait` with no `full_timeout`; a policy
   change does not re-judge parked requests; a quiesce cannot drain them; a
   discard queues behind them; the timer that would expire them shares a
   thread with the urgent GC pass; and a write parked for a full journal ring
   has no deadline at all.
3. **Recommended design** (§5): one signal, the **headroom** below the
   store's limit, read from the ledger. Above a soft mark writes are not
   touched. Between the soft mark and the limit, the **admission of a write's
   journal reservation** is paced by a token bucket whose rate falls with the
   headroom on a fixed curve (the OpenZFS shape), never below a minimum rate
   and never delaying one request longer than a cap. At the limit a write
   waits for space, FIFO, for at most `full_timeout` (default 60 s, the
   dm-thin default), then gets `ENOSPC`, and the volume **latches full**:
   writes fail at once until the headroom recovers. Reads, FLUSH and discards
   are never paced; a discard is admitted even on a full volume and makes the
   coordinator cut and turn the retained window so its space comes back.
4. **GC works harder while writes are slowed.** The engine reads the gate's
   state at every tick and hands the collector a larger step budget, a higher
   compaction rate and the floor grace (30 s) instead of 60 s, the same
   concession the urgent pass already makes. No collector interface changes.
5. **No on-disk format changes in phase 1.** Every new knob is a
   `lunixd.toml` key; `volume.json` keeps its keys and bytes. One frozen
   *meaning* changes (a null `full_timeout_ms` stops meaning "for ever"); it
   is flagged for the FREEZE owner in §8.
6. **A consequence the maintainer must see:** with a grace, the sustained
   write rate of any volume is `garbage room / τ`. The DESIGN §20 write
   targets are burst figures unless the volume has `R × τ` of room: 300 K
   4 KiB IOPS sustained needs about 110 GiB of garbage room at the default
   grace (§9.5).

## 1. Evidence (2026-11-18)

| # | build | observation |
|---|---|---|
| E1 | old build, no collector | `fio-verify`, 4 GiB working set, 64 KiB chunks, integrity. The store reached its limit (245.7 GB used, limit 245.3 GB). One write parked in the space gate and stayed there: 128 requests in flight, hung-task reports from the kernel, no progress for more than 25 min. Restarting the daemon with `on_full = error` did not release it. Killing the daemon and force-deleting the ublk device ended it. |
| E2 | `main` (GC on) | The store reached 107 GiB on a 4 GiB working set within minutes. The urgent pass never ran: with no `cache_max`, `NoSpace` fires only at the host file system's 10 % free floor. |
| E3 | `p1/gcrate` (pipelined passes) | Store about 16 × the working set at 256 MiB, about 49 GiB after 10 min at 4 GiB. Compaction is the bottleneck (about 12 MiB/s against an intended 64 MiB/s), because most segments are partly live. |

The `p1/gcrate` report names the gap this note fills (its Open item 2): "No
back-pressure on garbage. Nothing slows writes when garbage outruns GC; the
space ladder acts only at `NoSpace`."

## 2. What the code does today

Admission (`lunix-engine/src/queue.rs`, `advance`): a WRITE, DISCARD or
WRITE_ZEROES reserves its journal record with
`SpaceGate::reserve(JournalReserved, record_bytes)`, then takes a `write_seq`
from the journal lane. The gate (`lunix-space/src/gate.rs`, `ledger.rs`)
charges when `used + bytes <= limit`, where `limit = min(cache_max, fs_ceiling
- reserve)`. Otherwise `on_full = error` answers `NoSpace` (the queue fails
the request with `ENOSPC`) and `on_full = wait` parks the request FIFO behind
a `Waiter`. The commit path and GC never call `reserve`: they draw on the
reserved region with `reserve_headroom` and `transfer`, which never park. A
write parks before it holds a `write_seq`, so a cut never waits for a parked
write. That structure is sound and the design keeps it.

The paths by which a request stays parked without bound, all read from the
code:

| # | path | where |
|---|---|---|
| P1 | The defaults are `on_full = wait` and `full_timeout_ms = null`: a waiter has no deadline. | `lunix-format` `SpaceSettings::default`; `gate.rs` `reserve` |
| P2 | A policy change does not re-judge parked requests. `reconfigure` stores the new policy and calls `grant_waiters`; a waiter parked under `wait` stays parked after a switch to `error`, and keeps its old (absent) deadline after a new `full_timeout`. | `gate.rs` `reconfigure` |
| P3 | A quiesce (`SIGTERM`, upgrade) cannot finish: the queue drains only when no started request is held, and nothing completes a parked one. `QUIESCE_DEV` times out and the device then refuses new I/O. A "restart" in this state is not a clean restart. | `lunix-ublk/src/queue.rs` drain; `lunixd/src/daemon.rs` `quiesce_all` |
| P4 | A discard cannot help. A hole record reserves 64 bytes through the same gate, and a queue with any parked tag parks every later mutation behind it (`self.parked` is FIFO). | `queue.rs` `handle`, `advance` |
| P5 | Waiter expiry, the file-system refresh and the ladder share one thread. While the compact step (an urgent pass: a whole mark, then compaction) runs, no deadline fires. While requests stay parked the thread starts another ladder run each second, each a whole mark under the coordinator mutex, with no back-off on this path (the 2 s to 60 s back-off exists only for commits that fail for space). | `gate.rs` `thread_main`; `lunix-engine/src/gc.rs` `run_urgent`, `space_short` |
| P6 | A write refused by a full journal ring (`SlotOutcome::Full` or `NoSpace`) gives its space back, sets `journal_full` and parks with no deadline and no `on_full`. If commits cannot run (the host file system is out of space, or the engine has stopped after a failed commit), it parks for ever; `read_only()` is checked only when a request arrives. | `queue.rs` `advance`, `retry_parked` |

**E1, read against this (derived, not reproduced):** the store was at its
limit with nothing to reclaim it, so the first reservation that did not fit
parked under P1. The restart could not be clean (P3). After a kill, recovery
reissues the in-flight requests and they are admitted from scratch under the
policy in `volume.json` at that moment; if the new `on_full` was applied to
the running daemon rather than to the file read at open, P2 kept the old
waiters. The design removes each path and adds a test that replays E1 (§9.3,
§9.4); the test will say which of the paths E1 took.

**E2 and E3:** the gate has one threshold, the limit, and the default limit
is the host file system. Between "plenty of room" and "no room" nothing
reacts.

## 3. The rate law

On `p1/gcrate` the report derives and tests

```text
store <= 2 W + R (G + 3 P + 2 C) + slack
```

with `W` the working set, `R` the write rate, `G` the grace, `P` a pass
period and `C` the commit spacing. Call `τ = G + 3 P + 2 C`: about 95 s with
the defaults (`G` = 60 s), about 65 s with the floor grace (30 s). Three
facts follow, and they shape the design.

1. **The garbage is proportional to the write rate.** Slowing writes by half
   halves the steady garbage. Back-pressure therefore works even though it
   does not make GC any faster.
2. **Most of the garbage cannot be reclaimed by effort.** `R × G` waits out a
   timer. Only a shorter grace, or a lower `R`, reduces it.
3. **A cap `L` on the store fixes the sustained rate:**
   `R_sus = (L - live store) / τ`. Any controller ends at this rate; the
   choice is how smoothly it gets there and what happens when even
   `R_sus` is not met.

| garbage room `L - live store` | `R_sus` at τ = 95 s | at τ = 65 s (floor grace) |
|---|---|---|
| 16 GiB | 172 MiB/s | 252 MiB/s |
| 40 GiB | 431 MiB/s | 630 MiB/s |
| 111 GiB | 1.17 GiB/s (300 K 4 KiB IOPS) | |
| 76 GiB | | 1.17 GiB/s |

The compaction term is separate. A segment that is partly live is not
garbage the grace can free; it needs `gc_rate` of copying. E3 shows that
term is real (12 MiB/s achieved). `p1/gcrate` is working on it; the throttle
must not depend on it being solved, and §5.6 says what the throttle asks of
GC.

One more consequence: **the measured reclaim rate is the wrong thing to pace
writes by.** In steady state GC reclaims what was written `τ` earlier. If
writes are paced to the measured reclaim rate, a slowdown lowers the garbage
produced, which lowers the reclaim rate `τ` later, which lowers the pace
again: the loop has a dead time of a minute and a gain of one, and it winds
down. What matters is reclaim *capacity*, which is not observable. This is
why the recommended controller reads the headroom and not a rate (§6,
alternative C).

## 4. Prior art

| system | signal | slowdown | stop | bound |
|---|---|---|---|---|
| **OpenZFS write throttle** | dirty data against `zfs_dirty_data_max` | above `zfs_delay_min_dirty_percent` (60 % UNVERIFIED default) each transaction is delayed by `zfs_delay_scale × (dirty - min) / (max - dirty)`: zero at the soft mark, unbounded at the hard one | at `zfs_dirty_data_max` writers wait for the sync | the delay is capped at 100 ms per transaction |
| **RocksDB write stalls** | immutable memtables, L0 file count, estimated pending compaction bytes | `level0_slowdown_writes_trigger`, `soft_pending_compaction_bytes_limit` (defaults 20 files and 64 GiB, UNVERIFIED): writes limited to `delayed_write_rate`, lowered further while the backlog keeps growing and raised when it shrinks (factors 0.8 and 1.25, UNVERIFIED) | `level0_stop_writes_trigger`, `hard_pending_compaction_bytes_limit` (36 files, 256 GiB, UNVERIFIED): writes stop until compaction catches up | none on the stop; `no_slowdown` writes fail instead of waiting. Compaction gets more threads under pressure |
| **Linux dirty throttling** (`balance_dirty_pages`) | dirty pages against the dirty limits, and the device's measured write-out bandwidth | each dirtier sleeps so that its rate is the write-out bandwidth scaled by its position between the limits | above the hard limit dirtiers wait | pauses capped at 200 ms (UNVERIFIED) |
| **LFS** (Rosenblum and Ousterhout, TOCS 1992) | clean segments | the cleaner starts below a low-water mark and stops above a high-water mark; cost-benefit selection | no clean segment: writers wait for the cleaner | none; the paper's write cost `2 / (1 - u)` is the price of cleaning at utilisation `u` |
| **F2FS, NILFS2** | free sections; clean segments | a background cleaner on a timer (NILFS2 cleans faster below `min_clean_segments`; its `protection_period` is a grace like ours) | F2FS runs foreground GC in the writer's context when free sections run short | none: foreground GC is the known source of multi-second stalls |
| **SSD FTLs** | free blocks against over-provisioning | background GC below a soft mark | foreground GC below a hard mark: host writes proceed at the reclaim rate (the "write cliff" of steady-state benchmarks) | vendor-specific; steady-state performance is specified separately from fresh-out-of-box |
| **dm-thin** (the closest analogue for a full block device) | free data blocks in the pool | none | out of data space: I/O is queued, or failed at once with `error_if_no_space` | queued I/O fails after `no_space_timeout`, **default 60 s**; the pool then stays in the error mode until space is added |

Sources: OpenZFS "ZFS Transaction Delay" (formula and 100 ms cap confirmed);
RocksDB wiki "Write Stalls" (triggers and option names confirmed);
kernel `Documentation/admin-guide/device-mapper/thin-provisioning.rst`
(60 s default and `error_if_no_space` confirmed); the rest from the upstream
sources and papers.

Lessons taken:

- **Two marks, not one.** Every system that behaves well has a soft mark
  where it starts to slow and a hard one where it stops. Lunix has only the
  hard one.
- **Slow by position, not by measured rate** (OpenZFS). A fixed curve from
  backlog to delay needs no estimate of the cleaner's capacity, finds the
  equilibrium by itself, and is deterministic to test.
- **Cap the delay per request** (OpenZFS, Linux). A throttle that can hold
  one request for an unbounded time is a stall with a different name.
- **Bound the stop and then latch** (dm-thin). A block device that is out of
  space should say so within a known time and keep saying so until space is
  back, instead of failing a random subset of requests.
- **Give the cleaner more when writers are held** (RocksDB, NILFS2).
- **Do not clean in the writer's context** (F2FS's stalls). Lunix's queue
  threads must never block, and GC must run between cuts under the
  coordinator; the urgent pass on the gate's thread is the nearest thing and
  stays a last resort.

## 5. The recommended design

### 5.1 Signal

`H = limit - used`, the headroom, from the ledger under the gate's own lock.
`limit = min(cache_max, fs_ceiling - reserve)` as today. It is exact, already
maintained, costs nothing to read on the write path, and it is the quantity
that actually runs out.

Not used in phase 1, and why:

- *Garbage backlog* (`segments - live bytes of the last mark`, or condemned
  bytes). The mark is stale by a pass, so a fast first fill, which makes no
  garbage, would look like garbage until the next mark and be throttled; the
  §20 ingest target would be the victim. An incremental estimate (count the
  `clen` of every replaced map entry at destage, subtract what GC frees,
  resync at each mark) fixes that and is the right second signal later.
- *Store over live data.* Same staleness; and garbage that fits under the
  limit is not a problem to solve. The store is allowed to use its room, as
  an SSD uses its spare area.
- *Reclaim rate against ingest rate.* §3: unstable as a control input.
  Both rates are reported (§7), not controlled on.

The controller is written as "the largest pressure of several sources" so
that phase 2 can add the unsynced bytes against `B_max` (DESIGN §19, "then
writes throttle") and the cache tier without a second mechanism.

### 5.2 Thresholds

Both marks are relative to the limit, with absolute clamps so they make sense
from a 1 GiB test volume to a multi-TiB store.

| mark | default | meaning |
|---|---|---|
| soft, `H_soft` | 15 % of the limit, at least 1 GiB, at most 64 GiB, and never more than half the limit | pacing starts below it |
| hard | the reservation does not fit (`H < bytes`), as today | a write waits for space |
| resume, `H_resume` | the larger of 4 segments (64 MiB) and 1 % of the limit | a latched-full volume takes writes again above it |

**The limit itself needs a default.** With no `cache_max` the limit is the
host file system, which is what E2 shows: a 4 GiB working set took 107 GiB
before anything reacted, and on a shared file system one volume's garbage
shrinks every other volume's limit. Recommended: `lunix volume create`
writes an explicit `cache_max` of `store_max_percent` (default 200) of the
volume size into `volume.json`, using the existing key; `lunix space` changes
it. Twice the size is the worst case of live data at the default compaction
threshold (every segment just over half live), so a full volume still fits.
This is decision M2 in the ADR.

### 5.3 Mechanism: pace the admission of the journal reservation

The only place a guest write waits today is its
`reserve(JournalReserved, ..)`, before it has a `write_seq`, a lock or a
charge. Pacing goes there and nowhere else.

```text
x     = H / H_soft                      (0 < x < 1 inside the band)
rate  = max(r_min, r_mid * x / (1 - x)) bytes per second
r_mid = max(gc_rate, 16 MiB/s)          (64 MiB/s with the defaults)
r_min = throttle_min_rate               (4 MiB/s)
```

- `x >= 1`: no pacing, no bucket, one comparison under the lock the write
  already takes.
- The curve is the OpenZFS hyperbola in rate form. It is continuous at the
  soft mark (the rate is unbounded there), reaches `r_mid` at the middle of
  the band, and meets `r_min` at about 6 % of the band. With the defaults:
  576 MiB/s at `x` = 0.9, 192 MiB/s at 0.75, 64 MiB/s at 0.5, 21 MiB/s at
  0.25.
- A **token bucket** in bytes of journal record enforces the rate. Its
  capacity is one largest record (1 MiB + 64 bytes) or 50 ms of the rate,
  whichever is larger. Requests wait in the gate's existing FIFO; the head
  is admitted when it fits the limit and the bucket holds its bytes.
- **Delay cap.** A request that has waited `throttle_max_delay_ms` (10 s) is
  admitted if it fits, and the bucket goes into debt (at most one second of
  the rate). Latency is bounded first; the pace is best effort above that.
  With many large requests in flight the band can therefore be overrun, and
  the hard mark, where each admitted byte needs a freed byte, takes over.
  OpenZFS makes the same choice.

Who is not paced:

| request | treatment | why |
|---|---|---|
| READ | never touches the gate | as today |
| FLUSH | never touches the gate | it reserves nothing; it covers the writes already journaled, and a parked write has not been acknowledged, so a flush never waits for one |
| FUA write of at most 256 KiB | admitted at once inside the band, its bytes charged to the bucket | this is the journal commit block of ext4 and the log write of xfs: the write an fsync waits on. Bulk writes pay the debt. At the hard mark it waits like any write |
| DISCARD, WRITE_ZEROES (hole records, 64 bytes) | admitted from the reserved region (`reserve_headroom`), never parked, never paced, and not queued behind parked writes | a request that frees space must not wait for space. Two requests in flight together have no order, so overtaking is legal |
| commit, destage, GC, compaction output | `reserve_headroom` and `transfer`, as today | they are what frees space |

Why not elsewhere (the deadlock argument):

- **Not at destage or the cut.** The commit is what releases journal bytes
  (C9), names compaction output and turns the retained window so garbage
  becomes condemnable. Delaying it delays the cure, and the journal ring
  then fills and parks writes anyway, abruptly.
- **Not in a queue thread.** Queue threads never block. The gate already
  parks a request behind a `Waiter` and signals the queue's wake fd; pacing
  reuses exactly that.
- **Not on the ladder's thread.** Grants and deadlines move to a timer
  thread that takes only the gate's lock (P5). The ladder keeps its own
  thread and may block on the coordinator as it does now.

Invariants the implementation must keep, each with a test:

1. Nothing that holds a `write_seq`, a ledger charge or a lock waits in the
   throttle.
2. The commit path, GC and the checkpoint never call `reserve`.
3. The timer thread never waits for the coordinator, the ladder or I/O.
4. Every parked request has a deadline, whatever parked it.
5. `used` exceeds the limit only by the reserved region and by the
   double-count of a cut in flight (journal and segment bytes are both
   counted between C2 and C9), as today.

### 5.4 Bounds

| bound | value | holds when |
|---|---|---|
| delay added to a write inside the band | at most `throttle_max_delay_ms` (10 s) | always |
| total wait of a write | at most `full_timeout` (60 s), measured from its arrival, pacing included | always; then `ENOSPC` |
| wait of a write on a latched-full volume | none: `ENOSPC` at once | until `H >= H_resume` |
| minimum write rate | `throttle_min_rate` (4 MiB/s, about 1 000 4 KiB writes per second) per volume | while the reservation fits. At the hard mark no rate can be promised: there is no space |
| store size | `cache_max` + the reserved region (2 segments + `space.headroom`, 288 MiB by default) + one cut in flight | always |
| added cost with headroom above the soft mark | one comparison under a lock already held | always |

The default `full_timeout` of 60 s stays under the kernel's hung-task
threshold (120 s by default, UNVERIFIED on the test kernel); a value above
100 s is accepted with a warning that says so.

**Fairness across volumes.** Each volume has its own gate, bucket and
`r_min`. A volume with a `cache_max` is isolated from the others, which is
the main reason to give every volume one (M2). When the shared host file
system is the binding limit, every volume on it sees the same shrinking
headroom and gets the same rate cap from the curve: a light writer under the
cap is not slowed, the heavy ones are clipped to the same figure (max-min
fairness in bytes per second), and each keeps its `r_min`. Within a volume
the queue is one FIFO across all ublk queues, as today. A daemon-wide budget
for boosted GC I/O is left for later (§10).

### 5.5 The full volume

States of the gate: `open` (above the soft mark), `slowdown` (inside the
band), `stop` (a write waits for space), `full` (latched).

- **`on_full = wait`** (default): a write that does not fit waits FIFO. The
  ladder runs (the urgent pass, with back-off). If space comes, it is
  granted. If its `full_timeout` passes first, it fails with `ENOSPC`, **the
  volume latches `full`**, and every other request waiting for space fails
  with it. While latched, writes fail at once; reads, FLUSH and discards
  work; GC keeps running. The latch opens when `H >= H_resume`.
- **`on_full = error`**: a write that does not fit fails at once, as today.
  It is the latch without the wait.
- **No `readonly` choice.** A ublk device's read-only attribute is set at
  `SET_PARAMS` and cannot be flipped live, so a read-only volume would need a
  re-attach to come back. The latch gives the file system the same view
  (every write fails) and heals by itself. `on_full` keeps its two values and
  `volume.json` its bytes.
- **The error is `ENOSPC`** (`BLK_STS_NOSPC`), not `EIO`. `EIO` stays the
  answer of a failed or read-only engine. What the layers above do with it
  (UNVERIFIED on the test kernel; the full-volume cell in §9.4 records it):
  the block layer does not retry it; a buffered write fails at writeback and
  the next `fsync` returns the error once; ext4 aborts its journal and
  follows `errors=` if a journal block fails; xfs shuts down on a log write
  error and retries failed metadata writes according to its per-error
  settings, which have an `ENOSPC` class of their own. A full Lunix volume
  under a mounted file system is as unpleasant as a full thin pool; the
  design's job is to make it prompt, visible and reversible.
- **A write never parks for ever.** Every wait has a deadline (invariant 4),
  including the journal-ring wait of P6: a write refused by a full ring
  fails after `full_timeout` with `ENOSPC` if the last commit failed for
  space and `EIO` otherwise, and at once when the engine has stopped.
- **Policy change** (`lunix space NAME ...`): `reconfigure` re-judges every
  parked request under the new policy. `on_full = error` fails all of them
  now. A new `full_timeout` is counted from each request's arrival, so the
  overdue ones fail now. A larger `cache_max` grants what fits and opens the
  latch.
- **Daemon restart.**
  - *Crash or `kill -9`, then recovery:* nothing about a parked request is
    persisted. `REISSUE` delivers the in-flight requests again and each is
    admitted from scratch: a new arrival time, the policy of `volume.json` as
    read at open, the gate `open` or `stop` as the ledger says (the latch is
    not persisted; the first waiter decides again within `full_timeout`, or
    at once under `error`). Recovery itself needs no free space from the
    gate: replay reads, startup accounting uses `charge`, the first commit
    uses the reserved region.
  - *Quiesce (`SIGTERM`, upgrade):* from the moment a quiesce begins the
    volume admits every paced request that fits and fails every request
    waiting for space with `ENOSPC`, so the drain finishes. The alternative
    is to abort for `REISSUE`; decision M5.
- **Discard.** A discard is admitted on a full volume (§5.3). Its space
  comes back only after the hole is cut, the retained FULLs that still
  reference the old extents have been replaced, a mark has condemned them
  and the floor grace has run. So in `stop` or `full` the coordinator (a)
  cuts as soon as hole records are journaled, at most once a second, and
  (b) follows with as many FULLs as GC's retained set holds (three with
  `retain_last` = 2), empty if the volume is idle, one per second: the
  **window turn**. The urgent pass then finds the extents dead. From discard
  to granted space is about three commits, two pass periods and the floor
  grace: about 45 s with the defaults. The latch opens when the headroom
  passes `H_resume`; a writer still waiting inside its `full_timeout` is
  granted directly. The window turn spends the retained window on empty
  FULLs, as GC's own `cut_wanted` cuts already do (M17.B stack review M-5);
  it happens only on a volume that is refusing writes. Decision M6.

### 5.6 GC's duty

The engine reads the gate's state at each coordinator tick and sets what the
collector gets. `GcBudget` and `GcParams` are built per step, so this needs
no change to the `Collector` trait.

| gate state | step budget | compaction rate | grace | passes |
|---|---|---|---|---|
| `open` | `gc_step_budget_ms` (50 ms) | `gc_rate` | `gc_grace_ms` (60 s) | as `p1/gcrate`: pipelined, busy ticking |
| `slowdown` | 4 × (200 ms) | `gc_rate × gc_pressure_rate_factor` (4 ×) | the floor (10 s + 2 × `read_timeout`, 30 s) | the same, always busy |
| `stop`, `full` | as `slowdown` | uncapped | the floor | the ladder's urgent pass as well, with back-off |

- **The floor grace is the urgent pass's rule**, already argued safe in
  `docs/reference/gc.md`: the recheck mark's roots are pinned after the flag
  and the grace, and the floor covers in-flight cuts, destage and reads. The
  pipelined pass on `p1/gcrate` takes the grace from the step's params at
  verdict time; the implementer must confirm that a shorter grace handed in
  mid-flight is compared against the same monotonic clock (the review's
  safety table says it is) and add the directed test.
- **A cut may wait one unit past a 200 ms step** instead of a 50 ms one.
  Writes are being delayed already; this is the trade the urgent pass makes
  for a whole mark.
- **Pressure ends with hysteresis:** the engine returns to the normal budget
  after the gate has been `open` for 5 s.
- **The urgent pass gets a back-off on the ladder path too** (P5): not
  sooner than 2 s after the last one, doubling to 60 s while passes free
  nothing, back to 2 s after one that frees bytes: the rule `space_short`
  already applies to commits that fail for space. Its `want` is the sum of
  the waiting reservations plus `H_resume`, not the head's shortfall alone.
- **What this asks of `p1/gcrate`:** that compaction can actually reach the
  rate it is given (E3: 12 of 64 MiB/s; the report traces it to the per-step
  byte cap `gc_rate × step budget`). A rate credit that carries across steps
  fixes that inside the collector. The throttle works without it, at a lower
  `R_sus`; with it, the boosted rate means something.
- **What the throttle gives `p1/gcrate`:** the bound its report could not
  give. With a cap, `store <= cap` holds by admission, whatever the rate.

## 6. Alternatives

| | alternative | for | against | verdict |
|---|---|---|---|---|
| A | **Fix the hard stop only**: bounded wait, latch, re-judge on policy change, discard bypass; no slowdown band | smallest change; removes every "parks for ever" path | full speed, then a stall: the SSD write cliff. Under a GC that is merely slow, requests reach the 60 s bound and fail on a volume that is not full | **necessary, not sufficient.** It is step 1 of the recommended design and the fallback if the band is postponed |
| B | **Headroom band with a fixed curve and a token bucket at journal admission** (OpenZFS shape) | one exact signal; no rate estimates; deterministic; finds `R_sus` by itself; zero cost when open; cannot deadlock (it delays only unadmitted writes) | the curve's shape is a guess until measured; with many large requests in flight the delay cap lets the band be overrun | **recommended** |
| C | **Token bucket at the measured reclaim rate** (Linux `balance_dirty_pages` shape) | "ingest equals reclaim" is the true steady state | the measured reclaim rate follows the write rate with a dead time of `τ`, so the loop winds itself down (§3); deletes arrive in pass-sized bursts; needs a floor and a ceiling that end up doing the work | rejected as the controller. At the hard mark the gate already is this: a byte is admitted when a byte is freed |
| D | **Backlog triggers with an adaptive rate** (RocksDB: soft and hard pending-compaction bytes, rate × 0.8 while the backlog grows, × 1.25 while it shrinks) | reacts to garbage directly; adapts to an unknown capacity | needs a garbage estimate that is not stale (§5.1); absolute byte triggers do not scale from 1 GiB to TiB volumes; RocksDB's stop is unbounded | deferred. The adaptive step is a good refinement of B if the fixed curve oscillates in the M15 cells |
| E | **Throttle at destage or at the cut** (delay commits, cap dirty bytes) | uses the ladder's existing "throttle destage" rung | delays the step that frees journal bytes and turns the window; the journal ring fills and parks writes with no pacing at all; an fsync waits behind it | rejected. The rung stays unused in phase 1; the ADR moves "throttle" in front of the limit |
| F | **Shrink the queue depth** | bounds latency by bounding what is in flight | ublk's depth is fixed at `ADD_DEV`; the device's `nr_requests` can be lowered through sysfs but counts requests, not bytes (4 KiB and 1 MiB weigh the same) and is per queue | deferred as a complement to B for large-request workloads (§10) |
| G | **Foreground GC in the writer's context** (F2FS, SSD foreground GC) | the writer pays exactly for the space it needs | queue threads must not block; GC needs the coordinator between cuts; the stalls are the known failure of that design | rejected. The urgent pass on the gate's thread is the bounded version and stays the last resort |

## 7. Observability

**Status** (`lunix status NAME --json`, a new `throttle` object next to
`space`; counters are since the attach and live in the gate, not in
`EngineStats`, so the checkpoint is untouched):

| field | meaning |
|---|---|
| `state` | `open`, `slowdown`, `stop`, `full` |
| `state_since_ms` | when the state was entered |
| `headroom`, `soft`, `resume`, `limit` | bytes |
| `rate` | the bucket's rate now, bytes per second; null when `open` |
| `paced_waiters`, `space_waiters` | requests waiting for tokens, for space |
| `delayed_writes`, `delayed_ms` | writes that waited at all, and their summed wait |
| `delay_max_ms` | the longest wait of one request since the attach |
| `delay_capped` | requests admitted by the delay cap |
| `bypassed_fua`, `holes_admitted` | requests that skipped the queue |
| `enospc` | requests failed for space |
| `stops`, `fulls` | times the gate entered `stop`, latched `full` |
| `throttled_ms` | time spent outside `open` |
| `ingest_rate`, `reclaim_rate` | 10 s averages of bytes admitted and bytes freed, for the operator; not control inputs |
| `gc_pressure` | `normal`, `raised`, `urgent`: what the collector is getting |
| `amplification` | store bytes over the live bytes of the last mark |

`lunix status` prints one line when the state is not `open`, for example
`throttle: slowdown for 42 s, 38 MiB/s, headroom 3.1 GiB of 48 GiB, 17 writes
waiting, longest wait 2.4 s`.

**Log lines** (structured fields; one line per transition, transitions
closer than 5 s coalesced):

| level | when | message and fields |
|---|---|---|
| INFO | `open` to `slowdown` | `write throttle on` `headroom`, `soft`, `limit`, `rate`, `ingest_rate`, `reclaim_rate` |
| INFO | every 30 s outside `open` | `write throttle` `state`, `rate`, `headroom`, `delayed_writes`, `delay_max_ms`, `gc_pressure` |
| INFO | back to `open` | `write throttle off` `throttled_ms`, `delayed_writes`, `delayed_ms`, `delay_max_ms` |
| WARN | entering `stop` | `writes wait for space` `waiters`, `want`, `limit`, `used`, `full_timeout_ms` |
| ERROR | latching `full` | `volume full: writes fail with ENOSPC until space is freed` `used`, `limit`, `live_bytes`, `condemned_pending`, `failed` |
| INFO | latch opens | `volume no longer full` `headroom`, `full_ms`, `enospc` |
| WARN | a ring-full wait expires | `write failed: journal ring full for full_timeout` `last_commit_error` |

**Metrics** (when `/metrics` lands): `lunix_throttle_state`,
`lunix_throttle_rate_bytes`, `lunix_throttle_delay_seconds` (histogram),
`lunix_throttle_enospc_total`, `lunix_space_headroom_bytes`.

## 8. Configuration, and what touches a frozen format

**`volume.json` (freeze item 14): no key added, no byte changed.** The
existing keys carry the per-volume policy: `space.cache_max` (the store's
cap), `space.on_full` (`wait`, `error`), `space.full_timeout_ms`,
`space.free_floor`, `space.headroom`, `gc_rate`.

**`lunixd.toml`** (not a frozen format; unknown keys are already errors):

| key | default | validation | meaning |
|---|---|---|---|
| `throttle` | `true` | boolean | `false` turns the band off; the bounded stop and the latch stay |
| `throttle_soft_percent` | 15 | 1 ..= 50 | the soft mark, percent of the limit |
| `throttle_soft_min_bytes` | 1 GiB | at least 64 MiB | lower clamp of the soft mark (the mark is still at most half the limit) |
| `throttle_soft_max_bytes` | 64 GiB | at least the minimum | upper clamp |
| `throttle_min_rate` | 4 194 304 | at least 65 536 | `r_min`, bytes per second per volume |
| `throttle_max_delay_ms` | 10 000 | 10 ..= `full_timeout_ms` | the longest pacing delay of one request |
| `full_timeout_ms` | 60 000 | 1 000 ..= 3 600 000; a warning above 100 000 | the wait at the limit when `volume.json` has none |
| `gc_pressure_rate_factor` | 4 | 1 ..= 64 | compaction rate multiplier in `slowdown` |
| `store_max_percent` | 200 | 0, or 100 ..= 10 000 | `cache_max` of new volumes, percent of the size; 0 writes none |

The daemon keys follow the existing pattern of `journal_max` and
`commit_interval_ms` ("of new volumes" or daemon-wide defaults).

**Flagged for the FREEZE owner:**

| # | what | kind | needed for phase 1 |
|---|---|---|---|
| F1 | `space.full_timeout_ms = null` no longer means "wait for ever"; it means "the daemon's `full_timeout_ms`". The golden `14-volume.json` keeps its bytes; the row of on-disk-formats.md §21 changes its text. | meaning of a frozen key | **yes** (it is path P1) |
| F2 | Per-volume overrides of the throttle keys (`space.throttle_*` in `volume.json`). | new keys, golden regenerated | no; deferred |
| F3 | A third `on_full` value. | new enum value | no; rejected (§5.5) |
| F4 | Throttle counters in the checkpoint's stats slots. | `state/checkpoint` | no; avoided: the counters live in the gate and restart at attach |

**Interface changes in `lunix-iface`** (not on disk; they need the lead's
sign-off because other crates implement or read these types):

| # | change | why |
|---|---|---|
| I1 | `SpaceGate::reserve` learns the request's class (`Bulk`, `Sync`), or a second method does | the FUA bypass |
| I2 | `SpaceSnapshot` gains the throttle state and counters | status, and the engine's reading of the state |
| I3 | `SpaceFigures` or a new `ThrottleFigures` in the control messages | `lunix status` |
| I4 | none for GC: `GcBudget` and `GcParams` are per step already | |

`IoRequest` carries `fua` today and nothing else. Passing ublk's
`UBLK_IO_F_META` through would let metadata writes bypass the band too; that
is one more interface change and is left for later (§10).

## 9. Testing

### 9.1 Gate tests (`lunix-space`, manual clock, no background timers)

- The curve: monotonic in `H`; unbounded at the soft mark; `r_min` at the
  hard mark; the documented values at `x` = 0.25, 0.5, 0.75, 0.9.
- The bucket: at a fixed `H`, bytes admitted over a virtual interval equal
  `rate × interval` within one bucket; FIFO order; the delay cap admits at
  exactly the cap and records the debt; debt is bounded.
- Classes: a `Sync` reservation inside the band is admitted at once and
  charged; a hole reservation is admitted with the gate `full`.
- Deadlines: expiry within 50 ms of `full_timeout` on the fake clock; the
  latch closes on the first expiry, fails the rest, refuses at once, opens at
  `H_resume` and not below.
- `reconfigure`: to `error` fails every waiter now; a shorter
  `full_timeout` fails the overdue ones; a larger `cache_max` grants and
  opens the latch.
- Threads: a ladder step that blocks does not delay a grant or an expiry.
- Property test (5 000 sequences, simulated `fs_free`, random reserves,
  releases, reconfigures and clock steps): `used` never exceeds the limit by
  more than the reserved region; **no waiter is pending longer than
  `full_timeout`**; the ledger is exact at the end.

### 9.2 Deterministic engine test (`lunix-engine`, manual clock, background off)

A scripted collector frees `Y` bytes per virtual second after a lag; a
writer offers `X > Y`. One test, fixed seed, no wall-clock time:

- the gate passes `open`, `slowdown`, and settles; the bytes admitted per
  virtual second follow the curve at the observed headroom;
- `used <= limit + reserve + one cut` at every step;
- the budgets and params the collector received are the boosted ones exactly
  while the gate is not `open`, and the normal ones 5 s after;
- a FLUSH issued while writes are parked completes without them; a small FUA
  write completes inside the band without waiting;
- with `Y` = 0: `stop`, then `ENOSPC` at `full_timeout`, then the latch;
  commits still run; a discard is admitted, the coordinator cuts and turns
  the window, the scripted collector frees, the latch opens, a write
  succeeds;
- a ring-full wait fails at `full_timeout`; a stopped engine fails its
  parked writes with `EIO` at once;
- over 10 000 virtual seconds of random load: no request older than
  `full_timeout` (the liveness check).

### 9.3 Stack tests (`lunixd/tests/stack_throttle.rs`, no root)

Shaped like `stack_gc_rate.rs`: a 256 MiB working set, 64 KiB chunks,
incompressible random overwrite.

- **Slow GC.** `cache_max` 1 GiB, `gc_rate` 1 MiB/s, a short grace: for
  120 s the store (sampled every second) never exceeds `cache_max` plus the
  reserve plus one cut, no write fails, the gate visits `slowdown`,
  `delayed_ms > 0`, the longest wait is under the cap; after the writer
  stops the gate returns to `open`; the whole working set reads back and
  `fsck` is clean.
- **No GC** (the test switch): writes fail with `ENOSPC` within
  `full_timeout` (3 s in the test) plus one second, later writes fail at
  once; a discard of half the working set brings the space back within the
  documented bound and writes succeed.
- **E1 replayed.** Fill to the limit with `on_full = wait`, leave writes
  parked, then: (a) `lunix space --on-full error` releases them at once; (b)
  a quiesce completes within `quiesce_timeout_ms`; (c) drop the stack as a
  kill would, reopen with `on_full = error` in `volume.json`, reissue the
  writes: each fails at once and nothing is parked.
- **Two volumes on one simulated file system:** the idle one keeps its
  minimum rate while the busy one is in `stop`.

### 9.4 M15 exit cells (root)

- Every `fio-verify` cell sets `cache_max` (proposed: 150 % of the volume,
  48 GiB for the 32 GiB exit volumes; the 4 GiB working-set profile also at
  24 GiB so the band is exercised). New gate lines, next to "zero errors,
  zero verify failures": the store directory, sampled every 10 s, stays
  under `cache_max` plus the reserve plus `commit.dirty_bytes`; no write
  returned `ENOSPC`; fio's maximum completion latency is under
  `full_timeout`; the kernel log has no hung-task line; the report gives
  `throttled_ms`, `delayed_writes` and `delay_max_ms` per cell.
- **One full-volume cell** (new): a cap below the incompressible working
  set. Expected and checked: fio sees `ENOSPC` within `full_timeout` plus
  5 s; no hung-task line; the status shows `full`; `blkdiscard` of half the
  device brings writes back within 90 s; a `SIGTERM` with writes parked
  exits within `quiesce_timeout_ms`; a `kill -9` with writes parked,
  `on_full = error` written to `volume.json`, and a restart ends with fio's
  errors and a usable device, **with no forced device deletion**. The cell
  also records what ext4 and xfs do with the error (the UNVERIFIED claims of
  §5.5).
- "No device in a stuck state at the end" keeps its meaning; this design is
  what makes it reachable on a full store.

### 9.5 M16 and DESIGN §20

- **The open path costs nothing new.** The 4 KiB random-write and 1 MiB
  ingest cells are compared before and after on the same host; the
  expectation is no difference outside noise, because the change on that
  path is one comparison under a lock already taken.
- **A §20 cell is valid only if it was not throttled:** `throttled_ms` and
  `delayed_writes` are zero for the cell, or the cell is reported as
  "throttled; not a §20 measurement". Each write cell states the garbage
  room it needs (`target rate × runtime`, since nothing written during a
  60 s cell outlives the 60 s grace) and sets `cache_max` to give it:
  about 70 GiB for the 4 KiB random-write cell at the target, about 170 GiB
  for a 60 s rewrite at 3 GB/s. A first fill makes no garbage and needs
  none.
- **One sustained cell is added** (reported, not a §20 gate): 4 KiB random
  write for 15 min on a 4 GiB working set with a 48 GiB cap. It reports the
  steady IOPS, p99, p99.9 and maximum latency, the store's maximum, GC's
  write amplification and the time per gate state, and compares the steady
  rate with `(cap - live store) / τ`.
- **§20 needs one sentence** if the ADR is accepted: the write targets are
  burst targets; the sustained rate of a volume is its garbage room over `τ`
  (§3). Decision M7.

## 10. Phases

**Phase 1, needed for the M15 and M16 gates:**

| step | content | closes |
|---|---|---|
| T1 | The bounded stop: the default `full_timeout`, the latch, `reconfigure` re-judging, the timer thread apart from the ladder, the ladder back-off, deadlines for ring-full waits, parked writes failed when the engine stops, the quiesce rule | P1, P2, P3, P5, P6; E1 |
| T2 | Discards admitted from the reserve and not queued behind parked writes; the cut and window turn in `stop` and `full` | P4 |
| T3 | The band: curve, bucket, delay cap, FUA class, the `lunixd.toml` keys | E2, E3 |
| T4 | GC pressure from the engine tick | E3 |
| T5 | Status fields and log lines | |
| T6 | `store_max_percent` at volume creation (if M2 is accepted) | E2 |
| T7 | The M15 and M16 harness changes of §9.4 and §9.5 | the gates |

T1 and T2 are worth merging first and on their own: they are alternative A,
they fix a defect that needs a forced device deletion today, and they do not
depend on `p1/gcrate`. T3 to T5 should land after `p1/gcrate` merges, since
T4 edits the same lines of `coord.rs`.

**Later:**

- the incremental garbage estimate as a second pressure source, and the
  adaptive rate step of alternative D if the M15 cells show oscillation;
- per-volume throttle keys in `volume.json` (F2);
- `UBLK_IO_F_META` passed through, so metadata writes bypass the band;
- lowering the device's `nr_requests` while throttled (alternative F);
- a daemon-wide budget for boosted GC I/O across volumes;
- `/metrics`;
- phase 2: the unsynced bytes against `B_max` and the cache tier's eviction
  as further pressure sources and ladder rungs of the same gate.

## 11. Implementation sketch

For the implementer. Rust work in this repository follows the `rust-skills`
rules where the project's conventions do not say otherwise; load them before
writing or reviewing. No backward compatibility, no shims.

**`lunix-space`** (most of the work)

- `policy.rs`: `Policy` gains the resolved throttle settings (`ThrottleCfg`:
  soft percent and clamps, `r_min`, `r_mid`, delay cap, enabled) and a
  non-optional `full_timeout`. `SpaceConfig` carries the daemon defaults.
- new `throttle.rs`, a plain state machine like `ledger.rs` (no locks, no
  clock of its own): `fn soft(limit) -> u64`, `fn rate(headroom, soft) ->
  Option<u64>`, `struct Bucket { tokens: i64, last: Duration }` with
  `refill(now, rate)`, `take(bytes)`, `overdraw(bytes)`; `enum GateState {
  Open, Slowdown, Stop, Full }` with the latch and its hysteresis.
- `ledger.rs`: `Parked` gains `arrived: Duration` and `class`; the deadline
  is always present. `grant_waiters(now)` admits the head when it fits and
  (the gate is open, or the bucket has its bytes, or it is past the delay
  cap). `expire(now)` latches on a space expiry and fails the rest.
  `next_wake(now) -> Option<Duration>` gives the timer its sleep: the
  earliest of the head's token time, its delay cap and the nearest deadline.
- `gate.rs`: two threads. `lunix-space-timer` owns grants, expiry and the
  file-system refresh, sleeps until `next_wake`, takes only the gate's lock.
  `lunix-space-ladder` runs the steps with the back-off. `reserve` takes the
  class; `reconfigure` re-judges; a new `begin_close()` admits what fits and
  fails the space waiters (the quiesce rule). `snapshot` fills the new
  fields. Wake fds are signalled once per distinct queue per timer pass.
- Tests: §9.1.

**`lunix-iface`**: I1 to I3 of §8. `lunix-testkit`'s fake gate follows.

**`lunix-engine`**

- `queue.rs`: `advance` passes the class (`Sync` for a FUA write of at most
  256 KiB); a hole record uses `reserve_headroom(JournalReserved, 64)`; a
  discard is not queued behind `self.parked` (it gets its own short queue
  for the ring-full case only). A ring-full park records its arrival;
  `retry_parked` fails a tag past `full_timeout`, and every parked tag when
  `read_only()` turns true.
- `coord.rs`: `gc_step` picks `GcBudget` and `GcParams` from the gate's
  state (`pressure(&SpaceSnapshot) -> Pressure`, with the 5 s hold);
  `wanted` cuts at most once a second while the gate is in `stop` or `full`
  and something is dirty; a `window_turn: u8` counter on `Coord`, set after
  a cut that held hole records in those states, forces that many further
  FULLs.
- `gc.rs`: `run_urgent` honours the back-off that `space_short` uses.
- `engine.rs`: `quiesce` calls the gate's `begin_close` before the drain.
- Tests: §9.2.

**`lunix-gc`**: nothing required. Wanted from `p1/gcrate`: a compaction rate
credit that carries across steps, and a directed test that a grace shortened
to the floor between condemn and verdict deletes nothing early.

**`lunixd`**: `config.rs` (the keys of §8 and their validation);
`stack/mod.rs` (hand the defaults to `SpaceConfig`; resolve a null
`full_timeout_ms`); `daemon.rs` (`store_max_percent` at create; the
`throttle` object in the status; the state line); `stack/ladder.rs` (the
`want`). Tests: §9.3.

**`lunix-cli`**: the status line and the JSON object.

**Docs, in the same change as the code:** DESIGN §2 (space policy), §4.3
(the ladder's order), §18, §19 (the "local FS full" row), §20 (one
sentence); GLOSSARY "Space ladder"; `docs/reference/gc.md` (pressure, the
ladder back-off); on-disk-formats.md §21 (F1's row); the plan's M8, M15 and
M16 gates.

**Size.** T1 + T2: about 600 lines of code and as many of tests, one
implementer, no dependency. T3 to T5: about 900 and 900. T6: small. T7:
harness scripts and the report tables.

**Open points for the implementer to settle with a measurement, not a
guess:**

1. `JournalReserved` moves by up to `commit.dirty_bytes` at each commit, so
   the headroom has a sawtooth. If the rate flaps with it in the slow-GC
   stack test, compute the curve on the headroom's minimum over the last
   commit interval.
2. The curve's middle (`r_mid = gc_rate`) is a starting point. The sustained
   M16 cell is the measurement that confirms or moves it.
3. Whether the quiesce rule ever fires in practice on the upgrade path, and
   what the file systems above do when it does (the full-volume cell).
