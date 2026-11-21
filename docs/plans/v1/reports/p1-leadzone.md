# p1/leadzone: a lead zone before the admission band (ADR-32)

Branch `p1/leadzone` from main `1738f2e`. This first section is the design, written and committed before any code.

## 1. Design

### The problem

Under sustained random overwrite at a small cap the store rises until the space gate enters the band, and the first entry has `reclaim_rate = 0`. GC pressure and the
floor grace start at the soft mark (`Pressure::Raised` follows `ThrottleState::Slowdown`). Garbage condemned at that moment becomes deletable only after the grace (floor
30 s: 10 s + 2 x `read_timeout`) and the next pass (pipelined: one pass period, `busy_interval` 5 s while garbage keeps coming) have elapsed. For that time reclaim is
zero, while writes keep coming at the pace of the curve (unbounded at the soft mark): `L = R x tau`. A cap smaller than that bridge runs into `stop`, and every queued
write then waits (completions up to 32 s in the RAM-backed 64 KiB w4k cell, also the M15 8G-cap cell).

The collector reads the grace per verdict (`view.params.grace` against the stamp of the condemn), so a shortened grace also applies to what was condemned earlier. Starting the
floor grace earlier therefore makes the garbage that was condemned (under the long grace) earlier deletable earlier, and the garbage newly found is condemned with the short
grace from then on.

### The rule

A new pressure level `Lead` between `Normal` and `Raised`, entered while the gate is `open` and the headroom is below `soft + lead`:

```
tau_lead = grace_now + 2 x PASS_PERIOD          grace_now = min(gc_grace, 10 s + 2 x read_timeout)  (30 s by default)
                                                PASS_PERIOD = 5 s (the daemon's busy_interval)
lead     = min( ingest x tau_lead , soft , limit / 4 )   0 when ingest = 0 or soft = 0
```

* `ingest` is the gate's measured admitted rate (`throttle.ingest_rate`, the 10 s average of bytes admitted). Inside the lead zone nothing paces, so the admitted rate is the
  offered rate.
* `tau_lead` is the dead time of one reclaim: a pass finds the garbage (up to one period after it died), its grace runs, and the next pass deletes it (one more period).
  With the defaults 40 s.
* Bounds: `lead <= soft` keeps the zone at most twice the band's width; `lead <= limit/4` keeps it small on small limits (the soft mark is half the limit there). `lead = 0`
  for an idle writer and with the band off (`soft = 0`), so an idle volume never has pressure from this rule and the existing behaviour is unchanged there.
* `Lead` changes the collector's grace only: the floor grace of ADR-32 M8 (never longer than the configured one). Budget and compaction rate stay at their normal
  figures; `Raised` and `Urgent` are unchanged. The gate does not pace in the lead zone (nothing in `lunix-space` changes).

### Hysteresis

* Entry: `headroom < soft + lead`.
* Stay: while the level is `Lead` or higher and the gate is `open`, `headroom < soft + lead + lead/4` keeps `Lead` (a 25 % margin of the lead above the entry mark).
* Fall: every fall of the level (Urgent or Raised to Lead or Normal, Lead to Normal) needs the lower level to be wanted for [`HOLD`] (5 s) without a break, the rule `Raised` and
  `Urgent` already had for `Normal`. `Urgent` to `Raised` (the gate left stop) stays immediate, as before.
* So the level can not flap on a headroom that oscillates around the mark (the margin) nor on an ingest that wobbles (the time hold).

### Why it addresses `reclaim_rate = 0`

At the soft mark the garbage of the last `tau_lead` seconds of writing has been condemned under the floor grace already, so the first deletions come due while the volume is
still approaching the band instead of `tau_lead` after it; reclaim at band entry is no longer zero when the work was there to do. With lead sized as `ingest x tau_lead`, the
store grows by at most `lead` over the dead time, so the headroom at which the first deletes arrive is about `soft`.

### What does not change (the constraints of the rejected part 2)

* The ADR-33 GC yield to ring-parked writes is not disabled or narrowed: it applies at `Normal` and at `Lead` (not at `Raised` and `Urgent`, as before). While yielding at
  `Lead` the compaction rate is the yield's (an eighth of the configured rate), only the grace is the floor.
* `compact_per_pass` and `lunix-gc` are untouched: no pass takes more from the cut's reservation pool.
* The collector's share of the coordinator after a cut (`gc_share`, `GC_SHARE_*`) is untouched; `Lead` does not change the step budget, so a step is not longer.

### Risks

* A shorter grace earlier means a shorter safety window for in-flight reads and cuts at `Lead`. It is the floor that ADR-32 M8 and the urgent pass already rely on (DESIGN
  "why a stale mark is safe"); only the time at which it applies moves.
* `ingest` is the admitted journal rate; the store grows faster than that with write amplification (a 4 KiB overwrite of a 64 KiB chunk), so `lead` can undershoot in
  bytes of store. The bound `soft` is the cap in any case. Measured, not assumed (section 4 of this report).
* A lead zone that is permanently on (small caps, `soft = limit/2`) means the floor grace runs all the time under writes. The cost is garbage that waits less, never more.
* `PASS_PERIOD` is a constant mirroring the daemon's collector (`GcConfig::daemon().busy_interval`); a collector configured otherwise is sized a little off, the effect is
  only the length of the zone.
* A new `lead` value of the status's `gc_pressure` (`lunix status`): the one status change, an enum value, no new field.
