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

## 2. What was built

* `crates/lunix-engine/src/pressure.rs`: `Pressure::Lead` (between `Normal` and `Raised`; the levels are now ordered), `lead_window`, `lead_bytes`, `Lead` (the snapshot's headroom, soft mark and lead),
  `PressureTracker::observe` takes the lead and keeps one hold rule for every fall of the level (`low_since`, replacing `open_since`); `Inner::update_pressure` computes the lead from the gate's snapshot
  and the collector's settings; `gc_yields_to_ring` keeps the yield at `Normal` and `Lead` (it was off for any level other than `Normal`).
* `crates/lunix-testkit/src/space.rs`: `MemSpace::set_throttle_figures` (scripted headroom, soft mark, ingest rate) for the engine tests.
* Status: one new enum value, `gc_pressure = "lead"` (`lunix-iface` `GcPressure::Lead`, `lunix status` rendering, the daemon's mapping, the ctl round-trip test). No new field.
* Docs: `docs/reference/gc.md`, DESIGN §4.3 / §5 / status text (the ingest rate is now one control input, which DESIGN and ADR-32 said it never would be; see the amendment).
* Untouched: `lunix-space` (the gate), `lunix-gc`, `coord.rs` (the share), `compact_per_pass`, the on-disk formats.

## 3. Rule to test

Unit tests are deterministic (manual clocks or plain arguments, no sleeps, no duration asserts). Mutation: the patch was applied to the host copy only, `cargo test --no-fail-fast` run on
`lunix-engine` (lib, `pressure`, `jstall`) and `lunix-space` (`pace`), the patch reverted. All 13 mutants were killed.

| rule | test(s) | mutation (host copy) that fails it |
|---|---|---|
| `lead = ingest x window` | `pressure::tests::the_lead_is_the_ingest_times_the_window`, `..._of_a_snapshot_...` | M1 window replaced by 1 s: 3 unit + 2 engine tests fail |
| window = floor grace + 2 pass periods | `the_lead_window_is_the_floor_grace_plus_two_pass_periods` | M10 `LEAD_PASS_PERIODS` 0: 2 unit + 2 engine tests fail |
| `lead <= soft` | `the_lead_is_bounded_by_the_soft_mark_and_a_quarter_of_the_limit` | M2 bound removed: unit and 3 engine tests fail |
| `lead <= limit/4` | the same | M3 bound removed: that unit test fails |
| lead 0 for an idle writer / no band | `the_lead_is_zero_for_an_idle_writer_and_with_the_band_off`, `a_writer_that_stops_ends_the_lead_zone_after_the_hold`, engine `the_lead_zone_ends_with_the_writer_and_keeps_its_hysteresis` | M13 ingest forced up: 1 unit + 1 engine test fail (the guard clauses are redundant with the product and the bound, so there is no mutant of them) |
| entry below `soft + lead` | `the_lead_zone_starts_below_soft_plus_lead_and_only_while_the_gate_is_open`, engine `the_floor_grace_starts_in_the_lead_zone_before_the_band` | M4 entry at `soft`: 5 unit + 2 engine tests fail |
| only while the gate is open (the band stays `Raised`) | `..._only_while_the_gate_is_open`, `the_level_falls_from_the_band_to_the_lead_zone_after_the_hold` | M11 lead also wins over `Slowdown`: 2 unit + 1 engine test fail |
| hysteresis: kept up to `soft + 1.25 lead` | `the_lead_zone_is_kept_up_to_a_quarter_of_the_lead_above_its_entry`, `a_headroom_that_wobbles_around_the_mark_does_not_flap_the_level` | M5 no margin: both fail |
| every fall waits for the hold (and `Urgent` to `Raised` is at once) | `pressure_rises_at_once_and_falls_after_the_hold`, `..._wobbles_...`, `the_level_falls_from_the_band_to_..._lead_zone_after_the_hold`, engine tests | M6 falls at once: 6 unit + 3 engine tests fail |
| `Lead` changes only the grace | `the_lead_level_changes_the_grace_only`, engine `the_floor_grace_starts_in_the_lead_zone_before_the_band` (budget, rate, retain, grace) | M7 grace kept normal: 1 unit + 2 engine tests; M8 budget boosted: 1 unit + 1 engine test |
| pressure and floor grace start in the lead zone (engine, through `Engine::tick`) | engine `the_floor_grace_starts_in_the_lead_zone_before_the_band`, `..._ends_with_the_writer_...` | M4, M7, M8, M10, M11, M13 |
| the ADR-33 yield holds in the lead zone | `jstall::the_lead_zone_keeps_the_yield_to_a_ring_parked_write` (two steps per cut, an eighth of the rate, with the level `Lead`) | M9 yield off for any level but `Normal`: fails |
| the gate does not pace in the lead zone | `lunix-space/tests/pace.rs::the_gate_does_not_pace_in_the_lead_zone_above_the_soft_mark` (100 MiB/s, headroom from 2 x soft down towards the mark: every write `Reserved`, state `open`, no rate, no delay counted) | M12 band widened to 2 x soft: this test and four more fail |
| names and order of the levels, status code round trip | `the_levels_are_ordered_and_their_names_are_the_status_names`, `lunix-iface` ctl property test | (type-checked) |

## 4. Gates

1. `tools/ci.sh` on the host (CPUs 0-15, `-j 16`): `ci: all green`, 2 264 tests passed, 0 failed, 30 ignored (sum of the `test result` lines). The run was on the tree of `ae0e40e`; the later commit changes only
   DESIGN, `gc.md`, `ownership.toml` and this report (the ownership check of the final tree is in the hand-back).
2. Kill loops by hand, one at a time, `LUNIX_KILL_CUT_MS=1 LUNIX_KILL_HITS=first LUNIX_STACK_REQUIRE_FSCK=1`, `cargo test --release -p lunixd --features testsupport,crashpoints --test stack_kill kill_loop_<mode>_16k`:

| loop | points reached | runs / kills | result |
|---|---|---|---|
| plain 16 KiB | 55 of 58 | 57 / 57 | ok (1 814 s) |
| integrity 16 KiB | 55 of 58 | 60 / 60 | ok (2 028 s) |

3. Measurement: section 5.

## 5. Before / after

Base = main `1738f2e`, head = this branch's code (`ae0e40e`; later commits are documentation), both built `cargo build --release -p lunixd -p lunix-cli -p lunix-ublk` on the host in separate target
directories, same host, same session. Cells as in the T6 RAM setup (`tests/perf/partA.sh` of `p1/m16`, run unchanged except for one local knob, see the deviations): tmpfs 20 GiB, 16 GiB volumes in integrity mode,
2 GiB working set, 4 queues on CPUs 0-3, daemon CPUs 0-7, fio on 8-15, `journal_max` 1 GiB, 128 iodepth x 8 jobs 4 KiB random write, 60 s per cell, 64 KiB chunks then 16 KiB chunks, 3 repetitions per
volume, status sampled every 2 s, status probe 1 Hz. **Three independent sessions per build, interleaved (base, head, base, head, base, head), each a fresh daemon and fresh volumes, so 9 cells per build and chunk size.**
`lunix fsck --deep` after every volume: clean (12 of 12; every cell fio exit 0, EIO 0, `overall FAIL=0`).

Aggregate (stops and delay figures are cell deltas of the gate's counters; `delay_max_ms` and the ring wait "since attach" are maxima since the volume was attached, so one value per volume, the maximum over the 3 volumes is given):

| cell | build | sessions x reps | IOPS median (min-max) | p99.9 s median (min-max) | max completion s: all values | median max s | stops per rep (sum) | delay_max_ms per volume (max) | ring wait max 60 s ms (max) | ring wait since attach ms (max) | first band entry reclaim MB/s (per volume) | reps with stop |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| 64 KiB w4k | base | 3 x 3 | 24,005 (21,782-30,382) | 1.99 (0.88-5.20) | 27.1 13.1 12.8 32.6 18.4 22.9 33.3 18.1 8.1 | 18.4 | 1 4 4 1 4 7 2 7 4 (34) | 29517 | 889 | 889 | 0 0 0 | 9/9 |
| 64 KiB w4k | head | 3 x 3 | 24,977 (22,379-28,582) | 2.02 (1.04-5.20) | 31.4 11.8 6.0 33.8 14.4 15.5 27.6 14.9 6.7 | 14.9 | 2 2 2 2 6 4 2 4 1 (25) | 29569 | 834 | 834 | 0 0 0 | 9/9 |
| 16 KiB w4k | base | 3 x 3 | 43,648 (36,888-47,671) | 0.81 (0.77-2.07) | 24.5 11.7 5.8 21.0 5.5 3.2 18.3 5.5 1.5 | 5.8 | 1 1 0 1 0 0 1 0 0 (4) | 12030 | 834 | 834 | 0 0 0 | 4/9 |
| 16 KiB w4k | head | 3 x 3 | 42,469 (35,673-45,936) | 1.10 (0.74-2.33) | 25.0 9.1 3.1 15.2 21.8 6.6 20.0 5.9 4.2 | 9.1 | 3 0 0 4 2 0 1 0 0 (10) | 11991 | 910 | 910 | 0 0 0 | 4/9 |

Reading, with the run-to-run spread in view (the base builds differ from each other by as much as base and head do):

* **No improvement outside the spread.** The 64 KiB maximum completion: base median 18.4 s (8.1-33.3), head 14.9 s (6.0-33.8); the repetition-1 cells, the ones that cross the band for the first time, are 27.1 / 32.6 / 33.3 s on
  base and 31.4 / 33.8 / 27.6 s on head. Stops over the nine 64 KiB cells: 34 base, 25 head, but the per-cell values 1-7 on both sides overlap and sessions of one build differ by up to 2 x; not claimed as a gain.
  The 16 KiB cell (no-regression check) has stops 4 (base) and 10 (head) in nine cells and maximum completions with the same range (1.5-24.5 s base, 3.1-25.0 s head): no regression claimed or excluded, the difference is within the spread
  (the 16 KiB repetition-1 cells are 24.5 / 21.0 / 18.3 s base against 25.0 / 15.2 / 20.0 s head). p99.9 and sustained IOPS (median and range) are the same on both builds.
* **Reclaim at band entry: unchanged, 0 MB/s at the first entry of every one of the 12 volumes, both builds** (`write throttle on` log line; the sequences of all entries are in the per-cell data below).
* **Ring wait maximum: not worse.** Since-attach maxima over the three volumes: 64 KiB 889 ms (base) against 834 ms (head), 16 KiB 834 ms against 910 ms; the per-cell 60 s maxima are 0.65-0.91 s on both builds.
* The lead zone was active, but briefly: `gc_pressure = lead` in 2-4 of the 2 s samples of repetition 1 at 64 KiB (the first fill passes the zone in about 4-8 s), 2-6 samples at 16 KiB, and not at all in
  repetitions 2 and 3 at 64 KiB, where the volume sits in the band (`raised`/`urgent`) for the whole cell.

Per cell (all six sessions; `samples per gc_pressure` are 2 s status samples of the cell):

| dir | chunk | rep | IOPS | p99 s | p99.9 s | max s | EIO | stops | delayed | throttled s | delay_max_ms | ring wait 60 s ms | ring wait since attach ms | min headroom GiB | samples per gc_pressure |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| base1 | 64 | 1 | 30382 | 0.33 | 0.88 | 27.1 | 0 | 1 | 178947 | 34.0 | 24495 | 709 | 709 | 0.0 | {'normal': 10, 'raised': 7, 'urgent': 11} |
| base1 | 64 | 2 | 22470 | 0.63 | 5.20 | 13.1 | 0 | 4 | 754200 | 57.8 | 24495 | 805 | 805 | 0.0 | {'raised': 24, 'urgent': 7} |
| base1 | 64 | 3 | 28289 | 0.51 | 0.94 | 12.8 | 0 | 4 | 805070 | 49.6 | 24495 | 761 | 805 | 0.0 | {'raised': 21, 'normal': 2, 'urgent': 5} |
| base1 | 16 | 1 | 36888 | 0.52 | 2.07 | 24.5 | 0 | 1 | 210709 | 32.8 | 9507 | 697 | 697 | 0.0 | {'normal': 13, 'raised': 11, 'urgent': 4} |
| base1 | 16 | 2 | 44703 | 0.41 | 1.17 | 11.7 | 0 | 1 | 322389 | 25.7 | 9507 | 727 | 733 | 0.06 | {'normal': 11, 'raised': 17} |
| base1 | 16 | 3 | 45966 | 0.28 | 0.78 | 5.8 | 0 | 0 | 1114488 | 40.0 | 9507 | 724 | 733 | 0.31 | {'raised': 25, 'normal': 3} |

| head1 | 64 | 1 | 28450 | 0.27 | 1.64 | 31.4 | 0 | 2 | 38475 | 33.2 | 25533 | 715 | 715 | 0.0 | {'normal': 8, 'lead': 2, 'raised': 6, 'urgent': 12} |
| head1 | 64 | 2 | 22502 | 0.68 | 4.01 | 11.8 | 0 | 2 | 709807 | 52.3 | 25533 | 834 | 834 | 0.0 | {'raised': 25, 'urgent': 3} |
| head1 | 64 | 3 | 23337 | 0.67 | 2.02 | 6.0 | 0 | 2 | 1006057 | 59.9 | 25533 | 731 | 834 | 0.07 | {'raised': 28} |
| head1 | 16 | 1 | 35827 | 0.40 | 2.16 | 25.0 | 0 | 3 | 223246 | 33.1 | 9630 | 747 | 747 | 0.0 | {'normal': 10, 'lead': 3, 'raised': 11, 'urgent': 4} |
| head1 | 16 | 2 | 42469 | 0.42 | 1.38 | 9.1 | 0 | 0 | 615438 | 33.8 | 9630 | 817 | 817 | 0.11 | {'normal': 3, 'lead': 6, 'raised': 19} |
| head1 | 16 | 3 | 43590 | 0.29 | 0.79 | 3.1 | 0 | 0 | 806940 | 34.7 | 9630 | 910 | 910 | 0.95 | {'lead': 5, 'raised': 23} |

| base2 | 64 | 1 | 25167 | 0.39 | 0.94 | 32.6 | 0 | 1 | 174524 | 38.2 | 29517 | 713 | 713 | 0.0 | {'normal': 7, 'raised': 7, 'urgent': 14} |
| base2 | 64 | 2 | 26775 | 0.55 | 2.67 | 18.4 | 0 | 4 | 954838 | 55.0 | 29517 | 744 | 744 | 0.0 | {'raised': 25, 'urgent': 3} |
| base2 | 64 | 3 | 23336 | 0.60 | 4.46 | 22.9 | 0 | 7 | 616527 | 53.5 | 29517 | 790 | 790 | 0.0 | {'raised': 20, 'normal': 1, 'urgent': 7} |
| base2 | 16 | 1 | 40067 | 0.35 | 1.62 | 21.0 | 0 | 1 | 275727 | 29.6 | 9632 | 694 | 694 | 0.0 | {'normal': 13, 'raised': 11, 'urgent': 4} |
| base2 | 16 | 2 | 45114 | 0.30 | 0.77 | 5.5 | 0 | 0 | 1117061 | 41.8 | 9632 | 692 | 694 | 0.27 | {'raised': 21, 'normal': 7} |
| base2 | 16 | 3 | 39955 | 0.32 | 0.81 | 3.2 | 0 | 0 | 1066662 | 41.6 | 9632 | 771 | 775 | 0.63 | {'raised': 25, 'normal': 3} |

| head2 | 64 | 1 | 24977 | 0.35 | 1.04 | 33.8 | 0 | 2 | 146424 | 38.6 | 29569 | 704 | 704 | 0.0 | {'normal': 6, 'lead': 2, 'raised': 6, 'urgent': 14} |
| head2 | 64 | 2 | 28582 | 0.63 | 3.41 | 14.4 | 0 | 6 | 577306 | 43.8 | 29569 | 707 | 707 | 0.0 | {'raised': 25, 'urgent': 3} |
| head2 | 64 | 3 | 23377 | 0.64 | 5.20 | 15.5 | 0 | 4 | 846358 | 59.6 | 29569 | 763 | 763 | 0.0 | {'raised': 24, 'urgent': 7, 'lead': 1} |
| head2 | 16 | 1 | 36766 | 0.55 | 2.16 | 15.2 | 0 | 4 | 223818 | 32.3 | 1646 | 651 | 651 | 0.0 | {'normal': 10, 'lead': 3, 'raised': 14, 'urgent': 1} |
| head2 | 16 | 2 | 35673 | 0.47 | 2.33 | 21.8 | 0 | 2 | 451542 | 38.2 | 8639 | 833 | 833 | 0.0 | {'lead': 5, 'normal': 4, 'raised': 16, 'urgent': 3} |
| head2 | 16 | 3 | 42657 | 0.37 | 0.97 | 6.6 | 0 | 0 | 602710 | 33.0 | 8639 | 674 | 833 | 0.22 | {'lead': 6, 'normal': 4, 'raised': 18} |

| base3 | 64 | 1 | 24005 | 0.39 | 1.72 | 33.3 | 0 | 2 | 103570 | 38.4 | 29430 | 731 | 731 | 0.0 | {'normal': 7, 'raised': 7, 'urgent': 14} |
| base3 | 64 | 2 | 22951 | 0.67 | 4.46 | 18.1 | 0 | 7 | 634905 | 48.0 | 29430 | 718 | 731 | 0.0 | {'raised': 22, 'urgent': 5, 'normal': 1} |
| base3 | 64 | 3 | 21782 | 0.72 | 1.99 | 8.1 | 0 | 4 | 852391 | 54.4 | 29430 | 889 | 889 | 0.01 | {'raised': 28} |
| base3 | 16 | 1 | 43648 | 0.23 | 1.02 | 18.3 | 0 | 1 | 310361 | 27.2 | 12030 | 722 | 722 | 0.0 | {'normal': 14, 'raised': 8, 'urgent': 6} |
| base3 | 16 | 2 | 47671 | 0.30 | 0.78 | 5.5 | 0 | 0 | 1040990 | 35.7 | 12030 | 703 | 722 | 0.37 | {'normal': 5, 'raised': 23} |
| base3 | 16 | 3 | 42400 | 0.26 | 0.78 | 1.5 | 0 | 0 | 1720359 | 50.3 | 12030 | 834 | 834 | 1.1 | {'normal': 1, 'raised': 27} |

| head3 | 64 | 1 | 28389 | 0.45 | 1.48 | 27.6 | 0 | 2 | 123749 | 34.9 | 20430 | 737 | 737 | 0.0 | {'normal': 5, 'lead': 4, 'raised': 9, 'urgent': 10} |
| head3 | 64 | 2 | 22379 | 0.67 | 4.01 | 14.9 | 0 | 4 | 805283 | 54.7 | 20430 | 723 | 737 | 0.0 | {'raised': 25, 'urgent': 3} |
| head3 | 64 | 3 | 25121 | 0.62 | 1.87 | 6.7 | 0 | 1 | 852772 | 55.0 | 20430 | 799 | 799 | 0.08 | {'raised': 28} |
| head3 | 16 | 1 | 41254 | 0.33 | 1.10 | 20.0 | 0 | 1 | 222324 | 27.8 | 11991 | 659 | 659 | 0.0 | {'normal': 8, 'lead': 5, 'raised': 9, 'urgent': 6} |
| head3 | 16 | 2 | 42679 | 0.34 | 0.81 | 5.9 | 0 | 0 | 883556 | 39.0 | 11991 | 713 | 713 | 0.37 | {'lead': 6, 'normal': 1, 'raised': 21} |
| head3 | 16 | 3 | 45936 | 0.33 | 0.74 | 4.2 | 0 | 0 | 1040457 | 38.9 | 11991 | 714 | 714 | 0.6 | {'lead': 2, 'raised': 26} |

band entries (reclaim_rate MB/s at `write throttle on`, `S` = writes wait for space):
- base1 lunixd.1.log: first entry reclaim 0; sequence: 0 S 0 364 S 489 431 653 S 124 172 559 S 223
- base1 lunixd.2.log: first entry reclaim 0; sequence: 0 S 0 0 526 0 0 218 282
- head1 lunixd.1.log: first entry reclaim 0; sequence: 0 S 0 S 268 388 626 206 461 473 303
- head1 lunixd.2.log: first entry reclaim 0; sequence: 0 S 0 2 280 0 98 318 362
- base2 lunixd.1.log: first entry reclaim 0; sequence: 0 S 0 444 S 446 339 553 S 588 491 S
- base2 lunixd.2.log: first entry reclaim 0; sequence: 0 S 0 0 198 357 0 631 497 211 265
- head2 lunixd.1.log: first entry reclaim 0; sequence: 0 S 9 0 S 591 641 656 S 604 311 132 S
- head2 lunixd.2.log: first entry reclaim 0; sequence: 0 S 0 6 S 43
- base3 lunixd.1.log: first entry reclaim 0; sequence: 0 S 0 482 S 400 494 552 141 513 12 314 301
- base3 lunixd.2.log: first entry reclaim 0; sequence: 0 S 0 0 1 402 0 0 301 297 309 276 269
- head3 lunixd.1.log: first entry reclaim 0; sequence: 0 S 0 S S 405 78 236 282 227 281 S 128
- head3 lunixd.2.log: first entry reclaim 0; sequence: 0 S 34 207 312 262 303 275 310

## 6. Why it does not move the measured stop (what the measurement says about the design)

The first stop of each volume comes in repetition 1 of the cell, the first overwrite of a freshly written working set, about 12-15 s after the overwrite starts, when the store reaches the band at about
300 MB/s. Garbage exists only from the start of that overwrite, and the earliest a byte of it can be deleted is one pass after its condemn plus the floor grace (30 s here). So by construction nothing is deletable
before about 35 s, and at 300 MB/s the volume has a room of `limit - live - soft` (about 13 GiB of 18) that the writes use up in 12-15 s. The lead zone can start the floor grace earlier, but a lead of at most `soft` (2.7 GiB)
is 4-9 s of this ingest; the dead time `R x tau` of 9-12 GB is more than the volume's garbage room in this cell, and no collector-side measure (the brief's constraints exclude the rest) can create a deletable byte before the
grace of the first condemned one has run out. In repetitions 2 and 3 the volume already lives in the band, where GC pressure is `Raised` anyway and the lead zone changes nothing. What remains is the model of ADR-32:
the sustained rate is fixed by `(limit - live) / tau`; a cap this small for this ingest needs pacing from further away (a gate decision, excluded here) or a larger cap.

## 7. Deviations

1. **The measured stop is not fixed** (section 6); the lead zone is correct, tested and harmless in the measurement, but not an improvement outside the run-to-run spread in these cells.
2. The `ingest` rate becomes a control input of the engine (ADR-32 said: "no rate estimate and no GC statistic is a control input in phase 1"); DESIGN's status text was changed to say so. It is the gate's own
   `throttle.ingest_rate`, so a volume that is idle has no lead.
3. `PASS_PERIOD` (5 s) is a constant mirroring `GcConfig::daemon().busy_interval`; the engine has no view of the collector's configuration.
4. Status: one new value of `gc_pressure` (`lead`), no field.
5. The `p1/m16` harness is not on main. It was copied into the measuring trees untracked (not committed) from `p1/m16`; the only change is a local `SKIP_SEQW` switch that skips the seqw cells.
   `STALL_PROFILE`, `SHARES` and `UPROBES` were off (they perturb the run); the daemon's `destage_workers` key does not exist on main, so both builds ran with the daemon default instead of T6's 8 workers.
   Sessions are three per build rather than one with three repetitions: the three repetitions of one volume are not independent (they run on the same filled volume).
6. The hosts' `noisy_host` flag was set in 5 of 6 sessions (as in the T6 runs); the interleaving of builds is the control for it.
7. CI ran on the tree of the code commit; the documentation commit that followed was not re-run through CI.
8. `tools/ownership.toml` has a `[leadzone]` entry listing the files changed (`lunix-space/tests/pace.rs` and `lunix-engine/tests/jstall.rs` were added for the gate and yield tests).

## 8. Proposed ADR-32 amendment (for DECISIONS.md, the lead's file)

> **Amendment (2026-11-22, `p1/leadzone`).** GC pressure has a lead zone before the band. While the gate is `open` and the headroom is below `soft + lead`, the collector gets the floor grace (10 s +
> 2 x `read_timeout`, never longer than `gc_grace_ms`) and nothing else of M8's concession: the step budget, the compaction rate and the ADR-33 yield to a full journal ring are those of `open`. The gate does not
> pace in the zone. `lead = min(ingest x (grace_floor + 2 x 5 s), soft, limit / 4)`, where `ingest` is the gate's admitted rate of the last 10 s; it is 0 for an idle writer. The zone is entered below `soft + lead` and left above
> `soft + 1.25 x lead`, and every fall of the pressure level (from the band, from stop, from the lead zone) waits for the lower level to be wanted for 5 s; stop to the band is at once. `lunix status` shows
> `gc_pressure = lead`. This makes the ingest rate a control input of the engine (it was reported only). Rationale: garbage condemned under the long grace becomes deletable earlier, so that reclaim at the soft mark is not zero for the
> whole grace. Measured (RAM, 64 KiB w4k, three sessions per build): no change in the stops, the maximum completion or the reclaim at the first band entry within the run-to-run spread; the first fill reaches the band within
> less than the grace of its first garbage, which no collector-side measure can change. The dead time `R x tau` still has to fit in the cap (ADR-32 Context).
