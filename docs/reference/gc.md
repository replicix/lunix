# Garbage collection (phase 1, local volumes)

This page describes how `lunix-gc` reclaims space on a local volume: what it
keeps, when it deletes, the two files it writes and the settings it reads. The
design is DESIGN §11 and plan M17; the byte layouts of the envelopes are in
[on-disk-formats.md §16](on-disk-formats.md#16-gc-files-gcpass-and-stategccondemned-freeze-item-15).

## Where it runs

Every call of the collector runs under the engine's coordinator mutex,
between cuts, with the in-memory map equal to the tip's root and no dirty
leaf; that mutex makes each caller the single caller. The background calls
are the coordinator's own: one budgeted `step` after every commit (C10), on
every idle tick, and after a commit that failed before it changed the map (no
space for the cut above all, so a full volume can still free what its
condemned objects hold): one rule, the step runs whenever the map is the
tip's. Moves of a call that failed, and a cut the collector asks for
(`GcProgress::cut_wanted`), reach the next cut whichever path called it.
A step does small units of work until its budget (`gc_step_budget_ms`, default
50 ms) is spent, so a step never runs for longer than one unit past the
budget. While the pass in progress has work a step can do now (it is not idle,
not waiting out its grace, and not compaction at its rate cap) the background
thread ticks every 10 ms instead of every 100 ms, and after each cut the
collector steps on for up to a third of that cut's time, at most 1 s
(`gc_share`): under writes that keep the coordinator cutting it gets about a
quarter of it, and the next cut waits that long. Under writes, pending
compaction moves cut at most once a second while a pass is busy (on an idle
volume at once). The urgent pass
and a dry run are one call each and hold the coordinator for a whole mark
(and the urgent pass's compaction).

A pass runs when the
previous one ended at least 30 s ago and something may have changed: a new
tip, different pins, or condemned objects waiting after a pass that changed
something. A pass that changed nothing (its only lines are deferrals that wait
for a FULL) is not repeated until the tip or the pins change; the pins are
then looked at once per interval, not at every tick. A pass abandoned on an
error, or one that deferred for the store or the rollback floor, is retried
one interval later (a deferral for the store or the floor writes no evidence).

**The daemon's schedule is pipelined** (`GcConfig::daemon`, milestone gcrate; the
order inside a pass is mark, condemn, delete, compact).
A scheduled pass does not wait out the grace in the middle of the pass: its
one mark is the recheck of every listed object whose grace ended before the
mark began (the urgent pass's rule, with the full grace): after condemning
what is newly dead it deletes those, then compacts. Objects whose grace still runs
are left alone without an evidence line. A pass that changed something is
followed by the next after 5 s (`busy_interval`) instead of 30 s, and a grace
that ends starts a pass without waiting for a new tip or the 30 s. "Changed
something" means reclaim work: a segment condemned, deleted or compacted, or
64 MiB freed; the commit objects and map packs every cut leaves dead do not
count, so a lightly written volume keeps the 30 s pace. Whatever the pace, a
pass follows the last one at the earliest after three times that pass's mark
time, so marking a large map takes at most about a quarter of the coordinator
(deletes and compaction are not capped: they are the reclaim). After an abandoned pass the next waits 30 s.
`condemned_pending` in `lunix gc --status` is normally non-zero under this
schedule: it holds about one grace's worth of dead objects waiting for it,
which is not a backlog (a backlog shows as a store that keeps growing, or as
`condemned_pending` growing from pass to pass). A dead
object therefore waits one grace plus at most two pass periods; in the M17
sequence (every pass waits out its grace, then 30 s) it waits up to two graces
plus 30 s plus two passes, and under sustained overwrite the store holds that
many seconds of writes. A pass asked for (`lunix gc NAME`, `request_pass`)
still runs the whole sequence: condemn, grace, recheck, delete. The
collector's default (`GcConfig::default`, which the tests and the offline
`fsck --repair --gc` use) is the whole sequence for every pass.

The daemon reaches the collector in three ways (the engine's `GcControl`; the
calling thread runs the collector under the coordinator mutex):

- **`lunix gc NAME`** (`Gc`) asks for a pass now (due or not) and steps it to
  its end. Between two steps it releases the coordinator and pauses (5 ms, or
  100 ms in the grace or when compaction is at its rate cap), so cuts, a
  quiesce and the ladder get in. It returns once a pass that started after
  the call ended: when the collector was idle, the next pass to end (the
  requested one, or an urgent pass); when a pass was in progress (its view
  predates the call), the one after it (an urgent pass that aborts the pass
  in progress counts as that pass). The answer covers **every pass that ended
  while the call waited**: the counts summed, `bytes_freed` and
  `segments_compacted` from the collector's counters, `pass` the newest
  evidence number. A call that finds a pass waiting out its grace therefore
  reports what that pass deleted too. It takes at least the grace and is not
  cut off by the daemon's `call_timeout`; the client's `--timeout` (default
  600 s) still applies. When the client hangs up, the daemon stops waiting for
  it within a second and the pass goes on in the daemon. One such call per
  volume runs at a time; a second answers `Busy`.
  Other answers: `Invalid` when the volume is not attached; `Busy` when the
  volume is detached or quiesced meanwhile, or when the map stays ahead of the
  tip for 50 steps; `Refused` when the collector dropped the pass four times
  (errors, see the log). After a quiesce no collector call starts (requested,
  urgent or background step). A detach waits for the call (and for a running
  compact step) to let go of the engine, so the engine's threads are stopped
  and `LOCK` is free when the detach answers, **except** when such a call is
  still running after 30 s (a collector unit stuck in I/O): the detach then
  answers with a warning in the log and `LOCK` is released only when that
  call ends. **`--dry-run`** is one call that marks and reports what a pass
  would condemn, delete and compact, changing nothing.
- **`lunix gc NAME --status`** (`GcStatus`) reads the status the collector
  published after its last call, without waiting for the coordinator or the
  engine. A volume that is not attached is idle, with empty counters.
- **The space ladder's compact step** is the urgent pass (below): the gate
  runs it on its ladder thread when a reservation does not fit, and asks
  again each second while requests wait for space (ADR-32). That path has a
  back-off of its own: not sooner than 2 s after the end of the last urgent
  pass (either kind), doubling up to 60 s while ladder passes free nothing,
  at most the floor grace after one that condemned something, back to 2 s
  after one that freed bytes or when a write is admitted again; a successful
  commit does not reset it. Within the pause the step frees nothing and
  returns at once, and the ladder thread then waits 100 ms before its next
  run. The gate's deadlines never wait for it: they run on its timer thread,
  which also asks the ladder each second while the volume is latched full. A commit that
  fails for space at any step (a destage `NoSpace` from a full host file
  system, an index run that cannot be written) runs it too, from the
  coordinator, at least 2 s after the end of the last urgent pass (either
  kind). The pause doubles, up to 60 s, while those passes free nothing; it
  is at most the floor grace after a pass that condemned something, and back
  to 2 s after a successful commit or a pass that freed bytes.
  Compaction re-hints are applied only after a cut's destage succeeded, so a
  cut that fails for space leaves the map at the tip and GC can run. They
  share the map's dirty-leaf cap with the cut: they are applied in leaf order,
  as many leaves as fit (at most `max_leaves_per_cut`, halved on `MapFull`),
  and the rest wait; moves that wait for want of room trigger a cut on their
  own only once per commit interval.

## GC under write pressure (ADR-32)

The engine reads the space gate's state at every coordinator tick and hands
the collector more while writes are slowed or held. No collector interface
changes: `GcBudget` and `GcParams` are built per step.

| gate state | step budget | compaction rate | grace | passes |
|---|---|---|---|---|
| `open` | `gc_step_budget_ms` (50 ms), `gc_rate` bytes | `gc_rate` | `gc_grace_ms` (60 s) | the pipelined schedule |
| `slowdown` (paced) | 4 x (200 ms, 4 x the bytes) | `gc_rate x gc_pressure_rate_factor` (4 x) | the floor: 10 s plus twice `read_timeout` (30 s), never longer than `gc_grace_ms` | the same, always busy |
| `stop`, `full` | as `slowdown` | uncapped | the floor | and the ladder's urgent pass, with its back-off |

- The pressure rises at once and ends with a hold: the engine returns to the
  normal figures after the gate has been `open` for 5 s (`lunix status` shows
  `gc_pressure`: `normal`, `raised`, `urgent`).
- The floor grace is the urgent pass's rule, argued safe below ("Why a stale
  mark is safe"): the recheck's roots are pinned after the flag and the
  grace, and the floor covers in-flight cuts, destage and reads. The
  pipelined pass compares the grace it is handed at verdict time with the same
  monotonic clock that stamped the condemn, so a grace shortened between the
  two only moves the delete earlier by what the floor allows. The default
  `gc_grace_ms` (60 s) is not changed.
- A cut may wait one unit past a 200 ms step instead of a 50 ms one: writes
  are being delayed already, and this is the trade the urgent pass makes for a
  whole mark.
- `gc_pressure_rate_factor` (1 ..= 64) is a `lunixd.toml` key. The budget's
  multiple is fixed at 4.
- Tests: `lunix-engine/tests/pressure.rs` (the exact figures per state and the
  5 s hold), `lunixd/tests/stack_throttle.rs` (the slow-GC cell: garbage is
  deleted inside a run shorter than the configured grace, only possible with
  the floor grace).

## What is kept

**Roots** are pinned when a mark starts:

- GC's **retained set**: every FULL from the lowest `chain_floor` any
  retained FULL records up to the tip (N24, lead decision of 2026-11-14).
  With `retain_last` = 2 the engine retains the tip and its predecessor and
  sets `chain_floor` to the older one; the predecessor's own `chain_floor` is
  one commit lower, so GC keeps three FULLs. An offline `fsck --repair
  --truncate-tip` then leaves a complete volume: both roots of the
  predecessor are readable and every object they name exists. GC reads the
  FULLs below the tip's `chain_floor` from the commit store (the view
  carries only the engine's window); a missing one ends that walk (after a
  truncation the floor commit's own floor may be gone), one that exists but
  does not open stops the mark;
- the commits that snapshot, reader and hold pins name. In phase 1 these
  directories are empty, but they are read on every pass, and a pin that
  cannot be read stops the pass.

An object is **live** when a root reaches it:

| object | live when |
|---|---|
| segment | it holds the live copy of a chunk some root references (below) |
| map pack | it stores a node of some root (a full node walk of every root) |
| index run | the index manifest names it |
| commit | `commit_seq >=` the lowest `chain_floor` any retained FULL records (GC's retained set), or a pin names it, or it is in the rollback-floor window (below) |

**The live copy of a chunk.** A leaf entry resolves through its hint when the
hint's segment exists and its authenticated footer holds, at exactly that
offset and length, a chunk id that starts with the entry's id prefix;
otherwise through the index by prefix (a stale hint, left behind when
compaction moved the chunk). The live copy is the index's current copy of
that id, confirmed against the footer of its segment; an older duplicate is
dead. When the index has no usable entry, the hinted extent is the live copy.
Volumes with `id_len = 0` keep the hinted extent of every entry (hint-only
liveness) and never compact, since a stale hint could not be resolved.

**Per-segment live bytes** are the sum of `clen` of the live copies in the
segment. The live-copy set is a sorted vector of 24 bytes per live extent
(the hint plus one chunk number that reaches it); a pass holds one at a time.

Besides liveness, these are never condemned:

- an object whose id is above the tip's **high-water mark** (the highest id
  any retained commit names): an open segment, sealed but uncommitted
  output or an orphan the open-time sweep will remove;
- an object a FULL of GC's **current** retained set names (`new_segments`,
  `map_packs`, `index_packs`, the root's pack);
- a compaction output whose moves the engine has not yet named in a FULL.
  The collector keeps that set itself and clears an output when a FULL
  names it (it reads every FULL committed since its last step).

**The rollback-floor window** (ADR-31, maintainer decision of 2026-11-13).
While `state/pins` records a rollback floor, no commit object with
`commit_seq >= max_commit_seq` of the floor is condemned, also below
`chain_floor`: an attach whose tip is above the floor walks the chain back to
the floor and refuses the volume (`ForkUnverifiable`) when a link is missing.
The floor is not part of the per-step view; the collector reads it from the
volume directory it was built with. If `state/pins` cannot be read, no commit
object is deleted in that pass. These commits are kept as chain links only:
their map roots are not roots, so their data may go.

## Deleting: condemn, grace, recheck, delete

1. **Condemn.** The dead objects of the mark are added to the condemned list
   (`state/gc/condemned`, written atomically), then all its segments are
   flagged in the index in one durable update (`mark_condemned_all`: one
   manifest swap however many), after which dedup never returns them. The
   grace of each object starts when the flags are set.
2. **Grace.** `gc_grace_ms` (default 60 s) on the collector's monotonic clock.
   It covers in-flight cuts, destage and reads; it may not be shorter than
   10 s plus twice `read_timeout_ms` (30 s with the defaults), and a shorter
   setting is refused. An object read back from the list after a restart
   starts its grace again.
3. **Recheck.** A new mark from roots pinned after the grace. Each listed
   object is then decided in its own unit, against the facts of that step: an
   object the mark finds referenced (or named by a retained commit, above the
   high-water mark, or an unnamed output) is un-condemned: it leaves the list in
   memory, and at the end of the step the flags of all the segments so
   un-condemned are cleared in one index update and the list is stored once (the
   flags first: a crash in between leaves a live segment flagged and listed,
   which the next recheck un-condemns again).
4. **Delete.** Unlink and directory `fsync` in the store, the ledger
   `release` of the bytes freed. The index follow-up of the deleted segments
   is queued and made in one durable update (`retire_segments`: the entries
   dropped and the flags cleared by the same manifest swap) at the end of the
   delete phase, at the end of the step that deleted them, or when 256 are
   queued: **a segment's flag is never cleared
   while index entries may still point into it.** A failed call, an abandoned
   pass or a crash leaves the objects listed or flagged; the next pass finds
   them gone, drops their entries, clears the flags, and releases the listed
   size if the earlier call may have deleted it. The index update runs after the step's time check, so a step that deletes may overrun its budget by one manifest swap. A segment that stale hints point into is deleted only
   while the live copy of every id they resolve to is in a segment a durable
   FULL has named (the delete-time index invariant): at or below the highest
   id named by a FULL that has a successor FULL (the collector reads each new
   commit once) and not an unnamed compaction output; otherwise it stays
   condemned for a later pass. **The tip's own names never count**: a copy
   only the tip names would go with the tip at `fsck --repair --truncate-tip`
   (the mount sweep deletes what the new chain does not name above its mark),
   so a compaction source is deleted one commit after the FULL that names its
   output (M17.B review B-1). On an idle volume the collector asks the
   engine for that cut (`GcProgress::cut_wanted`, one empty FULL), so the
   source goes at the next pass instead of being deferred at every pass
   (M17.B stack review S-1), and only when a cut helps: the blocking copy is
   in an unnamed output or named by the tip (re-review M-1). This rule is exact only while the engine
   persists the index entries of a named segment before its naming FULL
   leaves the retained window (spec gap N13). Commit objects go through
   `CommitStore::delete` with the durable tip's `chain_floor`; GC itself keeps
   them down to the lowest `chain_floor` of a retained FULL, together with
   everything those FULLs reach, so that `fsck --repair --truncate-tip`
   leaves a complete volume (N24). The truncation itself deletes the commit
   files below the new tip's `chain_floor` that would join the set only under
   the new tip (they were outside it under the old one and may be half
   collected), refuses when a root of the new chain resolves into an
   object only the dropped tip names, and rebuilds the index for the
   previous commit before it sets the tip aside: GC moved the entries of
   relocated ids into the output the next open sweeps. A crash or an error
   between the two leaves the old tip, and its mount's catch-up adds the
   entries of the segments only that tip names; a rebuild that fails refuses
   the truncation.

The **urgent pass** (the space ladder's compact step) marks, deletes the
condemned objects whose grace has run for the floor (10 s plus twice
`read_timeout`), with that mark as their recheck, condemns what is newly dead,
and compacts if still short. It never deletes without a recheck. When it stops
on an error after it freed bytes, it reports the bytes freed (and writes its
evidence) rather than an error.

### Why a stale mark is safe

Mark, condemn and recheck are separate steps with cuts between them, so the
mark a condemn uses may predate a cut that added a reference (a dedup hit on a
segment that was not yet condemned).

1. A mark walks exactly the root set pinned at its start, and only objects
   listed at that moment are candidates.
2. Condemn skips any object a currently retained commit names.
3. After an object is condemned no new reference to it can arise: dedup skips
   condemned segments, compaction writes new segments, and re-hints only
   point at compaction output. A recheck whose roots are pinned after the
   condemn therefore finds every reference: any later root derives from a
   pinned one and adds no reference to a condemned object.
4. If the roots change during the recheck walk, the walk is still sufficient
   for the same reason.

## Compaction

Candidates are segments with live bytes, a live fraction `live / file size`
below `gc_live_threshold` (default 0.5), and an output at least 1 KiB smaller
than the source (the output holds the live payload plus 128 bytes per extent
and 512 bytes per segment of container overhead): a small segment whose dead
bytes are mostly overhead would be rewritten into an output as large at every
pass, and is left alone. Candidates are also at or below the high-water mark,
not condemned, not an unnamed output, and not introduced by the tip FULL
(younger than one commit interval). With `u = live / file size` they are
ranked by LFS cost-benefit `(1 - u) · age / (1 + u)`, where `age` is the tip's high-water
mark minus the segment id, best first; up to 1 GiB of live bytes per pass
(256 MiB under the daemon's schedule).

Live extents are copied in chunk-number order (LBA locality). Each is read,
opened with its full chunk id and re-sealed through the extent codec, which
draws a fresh salt; the chunk id does not change. One output segment is open
at a time. After the output is sealed, the index is told (`relocate`, which
keeps the entry in memory until a durable commit names the output) and the
moves are handed to the engine, which re-hints the map at its next cut and
names the output in that FULL. Pending moves are a cut trigger even on an
idle volume. These cuts, and the one the collector asks for before a source
can go, are ordinary FULLs: on an idle volume they consume the retained
window, so the retained FULLs may soon all be GC cuts of the same data (M17.B
stack review M-5). Compaction reads at most `gc_rate` bytes per second
(default 64 MiB/s) plus one step's allowance, and at most that allowance per
step: the daemon allows one second of `gc_rate` per step (so a pass may read up
to 64 MiB ahead of the rate), and the step's time budget bounds it; a `gc_rate`
of 0 compacts nothing.

The source segments become dead once the index's current copies are in the
output; the next pass condemns them.

Only the index's current copy of an id is moved, and a segment holding a live
extent that no confirmed index entry names is no candidate (it could not be
relocated and would be picked every pass). An extent that does not open
(`Auth`, `Corrupt`, `Unsupported`) is skipped, reported with a `skip` line,
and its segment is never picked again by the process. Any other compaction
error ends compaction for that pass; the pass still finishes. Live extents are
queued 65 536 at a time (plus at most one segment's), so the compactor's
scratch is bounded.

## Settings (`volume.json`)

| key | default | meaning |
|---|---|---|
| `retain_last` | 2 | FULL commits kept as roots; the engine derives `chain_floor` from it |
| `gc_grace_ms` | 60 000 | grace between condemn and delete; at least `10 000 + 2 × read_timeout_ms` |
| `read_timeout_ms` | 10 000 | the longest a read may hold a hint; sets the grace floor |
| `gc_rate` | 67 108 864 | compaction bytes per second |
| `gc_live_threshold` | 0.5 | compact segments whose live fraction is below this |
| `gc_step_budget_ms` | 50 | time one step may spend |

Not in `volume.json` (`GcConfig` in the code): the pipelined schedule (the
daemon's), 30 s between passes and 5 s after a pass that did reclaim work, 1 GiB of
live bytes compacted per pass (256 MiB for the daemon), a root walked in at most 64 leaf batches of at
least 16 leaves, 32 MiB of decoded footers cached (`footer_cache_bytes`).

## `state/gc/condemned`, body version 1

Local only, never uploaded, replaced atomically. It is a hint: an unknown
version, a bad CRC or any decode error means the list is lost; the collector
then adopts the segments the index still flags (with a fresh grace) and
re-marks everything else. Integers are little endian.

| field | offset | width | encoding | notes |
|---|---|---|---|---|
| `count` | 0 | 4 | u32 | at most 10 737 418 entries (the 256 MiB body cap) |
| entries | 4 | 25 × `count` | | strictly ascending by (`kind`, `id`); nothing after the last |

Each entry:

| field | offset | width | encoding | notes |
|---|---|---|---|---|
| `kind` | 0 | 1 | u8 | 1 segment, 2 map pack, 3 index run, 4 commit |
| `id` | 1 | 8 | u64 | object id (kinds 1-3, never 0) or `commit_seq` (kind 4) |
| `condemned_at` | 9 | 8 | u64 | ms since the epoch; informational, the grace runs on the monotonic clock |
| `bytes` | 17 | 8 | u64 | size when condemned |

## `gc/<pass:016x>`: the evidence object

One sealed, create-only object per pass that condemned, un-condemned,
deleted or compacted anything, or deferred for the grace; a pass that found
nothing, or only deferred for a FULL, the store or the rollback floor, writes
none, and its number is used by the
next. **Local bound:** the volume keeps the newest 1 024 objects; writing a
new one deletes the oldest beyond that, at most 16 per pass
(`GcStateStore::delete_evidence`). Under the daemon's pipelined schedule a
busy volume writes one object every few seconds, so the newest 1 024 cover
about one to two hours of a sustained overwrite (a day or more when the
volume is quiet), and an object's `condemn` lines and the `delete` lines of
the same objects are in different evidence objects (a grace apart).
Evidence objects are not in the ledger; the bound keeps them under 1 024 x
48 MiB at worst, in practice a few KiB each. `pass` is one above the highest
`gc/` object present. The body is the frozen opaque body with `ver` = 1 and
`bytes` = the pass's lines, written once at pass end (and once per urgent
pass). A pass abandoned on an error (not a crash) writes what it has, best
effort, so the record of deletes already made is not lost. A crash before the
write loses only that pass's evidence. Phase 2 uploads the objects verbatim.

Each line is one JSON object, keys in this order, no spaces:

```
{"time":1700000000000,"action":"delete","kind":"segment","id":"000000000000002a","bytes":1048768,"reason":"unreferenced after grace and recheck"}
```

| key | value |
|---|---|
| `time` | ms since the epoch |
| `action` | `condemn`, `uncondemn`, `defer`, `delete` (also for an object found already gone: reason `already gone`), `compact` (a source segment; `bytes` = its live bytes), `output` (a compaction output; `bytes` = its size), `skip` (a source with an extent that does not open), `truncated` (see below) |
| `kind` | `segment`, `pack`, `run`, `commit` (`pass` for `truncated`) |
| `id` | 16 lowercase hex digits: the object id, or the `commit_seq` |
| `bytes` | as above; for `delete` the bytes freed (0 when the file was already gone) |
| `reason` | a fixed phrase: `unreferenced`, `referenced at recheck`, `named by a retained commit`, `above the high-water mark`, `grace not over`, `rollback floor unreadable`, `live copies of its ids are not durable yet`, ... |

Lines beyond 48 MiB are counted, not kept; a last `truncated` line gives the
count in `bytes`.

## Crash points and recovery

| point | after it |
|---|---|
| `gc.mark.end` | the mark is complete (nothing durable yet) |
| `gc.urgent.after_mark` | the urgent pass's mark is complete (nothing durable yet; one name per call site) |
| `gc.condemn.after_list_write` | the list is durable, the index flags may be missing: they are set again (with a fresh grace) when the list is loaded |
| `gc.delete.before_unlink` | nothing deleted |
| `gc.delete.after_unlink` | the file is gone (the store unlinks and fsyncs the directory in one call); the index may still hold entries of this segment and of the segments unlinked since the last index update (those of one step, up to 256): `catch_up` drops them at open, a later pass removes the list entry |
| `gc.delete.after_dirsync` | the ledger is updated and the object's index follow-up is queued (see `gc.delete.after_unlink`); the list still names the object until the end of the delete phase (an un-condemn is stored at once) |
| `gc.commit_delete.before_unlink` | the commit object is not deleted yet |
| `gc.compact.after_segment_seal` | the output is sealed, nothing relocated: above the high-water mark, the open-time sweep removes it |
| `gc.compact.after_relocate` | relocations are in the index memtable only, the moves never reached a commit: the sweep removes the output, the old segment stays named and indexed |
| `gc.evidence.after_put` | the pass is complete |

Every state between two points is recoverable: the list is a hint, deletes
are idempotent, a listed object that is gone is dropped from the list.

## Ledger

The collector `release`s each deleted file's bytes under its kind
(`Segments`, `Map`, `Index`, `Commits`) after the delete is durable. A
compaction output is charged from the headroom while it is written and trued
up when it is sealed. `gc/` evidence objects are not in the ledger (startup
accounting does not count them either).

## Steps and their bounds

A step runs units until its budget is spent, so it lasts at most the budget
plus one unit. Units: the mark's init (the root set and four listings); one
full `walk_nodes` of a root (one callback per stored node; interior nodes are
resident); 32 new leaves of a root (`× fanout` entries resolved, plus one
skip-set lookup per leaf of the root); the sort of about 262 144 live extents;
the condemn list (one write); one index flag; one recheck-and-delete decision;
one compaction copy or seal; the evidence write. The step-wide facts (retained
names, the index manifest, the rollback floor) are read once per step.

**The cost of the leaf walk grows with the square of the root (N25).** A
leaf unit resolves only its 32 new leaves, but it reaches them through
`walk_leaves_skipping`, which traverses the whole root and does one skip-set
lookup per leaf of the root. A mark therefore costs about `leaves² / 32`
lookups per root. At 16 KiB chunks and fanout 512 a 1 TiB volume has about
125 000 leaves: about 4 000 units of about 125 000 lookups each, fine per
unit at 1 TiB, over the step budget per unit near 8 TiB, and tens of seconds
of CPU per mark well before that. The fix is a range walk in the map
interface (walk leaves `from..from + n` of a root, or a resumable cursor), so
a unit costs its own leaves only; it is a plan item (N25), and M17.B measures
a 64 M-chunk mark against the budget.

## What the wiring must do

- Build the collector once, with the volume directory as its floor source,
  and keep it as long as the engine: the unnamed outputs and the moves not yet
  handed over live in memory only.
- Give every move a step or an urgent pass returns to the next cut
  (`MapTxn::rehint`) and name the output segments in that FULL.
- Call it only between cuts, with the map equal to the tip's root.
