# Write throughput: the journal read-back and the ring stalls

Date: 2026-11-19. Status: design note for
[ADR-33](../DECISIONS.md#adr-33-write-throughput-a-resident-journal-window-and-bounded-cuts)
(accepted 2026-11-19; W6 and W9 as the ADR records them, not as recommended here). It carries the root-cause analysis, the options, their effect on
the guarantees, a recommendation and a throughput model.

Method and limits. The code claims come from reading `main` at `cda79fb`
(crates `lunix-journal`, `lunix-destage`, `lunix-engine`, `lunix-overlay`,
`lunix-store`, `lunix-space`, `lunix-gc`, `lunixd`, `lunix-iface`). Nothing
was compiled, run or measured for this note. The measurements are the M16
re-measurement of 2026-11-19 (report `p1-m16.md` on `p1/m16`, "Re-measurement
on current main"); every "why" attached to them is derived from the code and
marked where it is a hypothesis. The model in §6 is arithmetic on the
project's own stage figures (DESIGN §5) and on stated assumptions; none of
its outputs is a measurement.

## Summary

1. **The write path itself is not slow; the drain is.** A write is
   acknowledged when its journal batch write completes. The ring then drains
   only through destage and commit, and both are bounded by things the engine
   does to itself: it reads every record back from the journal file with one
   synchronous `O_DIRECT` read per piece, and it frees ring files only at the
   end of a commit that covers everything written so far.
2. **The read-back is avoidable.** The payload is in memory when the record
   is appended (it is copied into the lane's batch buffer) and is dropped when
   the batch write completes. Keeping it until the ring file is reclaimed
   removes the read. This is a cache of durable data: nothing about what is
   durable, and when, changes.
3. **The ring cannot be reclaimed before a commit.** The recovery floor is
   the `cut_seq` of the newest FULL, so a record may leave the ring only when
   a durable FULL covers it. "Reclaim per destaged prefix" is therefore
   "commit per destaged prefix": the fix for the stalls is **bounded cuts**,
   not an earlier reclaim.
4. **Recommended (§5):** a resident journal window in `lunix-journal` (W1,
   W2), a 1 MiB read-around on a miss (W3), cuts bounded by journal bytes with
   an adaptive bound (W4), a wake per freed ring file and ring counters (W7),
   and a status path that does not take the coordinator mutex (W8). **No
   on-disk format changes.** Two later options would touch a frozen rule and
   are listed for the maintainer, not recommended now (§4.3).
5. **What the fix does not do (§6).** It removes the 2.7 K pieces/s bound and
   the 15-35 s stalls. It does not change the device: on the measured network
   volume the sustained rate then sits at that volume's bandwidth, and a 4 KiB
   random write into a 64 KiB chunk still stores up to 16 times its size.
   The §20 targets need an NVMe-class device, and the sustained 4 KiB target
   needs the working set and the ring size stated.

## 1. The write path as built

In order, with the code:

- **Admission and append** (`lunix-engine/src/queue.rs:385-585`). The queue
  thread charges `JournalReserved`, calls `JournalLane::try_reserve`
  (`queue.rs:503`) and `append` (`queue.rs:554`). `append` copies the data into
  a pooled aligned batch buffer; the lane writes the batch with one `O_DIRECT`
  io_uring write (`lunix-journal/src/lane.rs:362-424`).
- **Acknowledgement** (`queue.rs:749-793`). On the batch's completion the
  record goes into the overlay as `lba -> JournalLoc` (`queue.rs:756`) and a
  non-FUA write completes. A FUA write joins the next group flush. The batch
  buffer goes back to the lane's pool at that moment (`lane.rs:475`): **the
  payload is no longer in memory**.
- **Commit trigger** (`lunix-engine/src/coord.rs:327-360`): the interval `T`
  (30 s), dirty bytes `B` (1 GiB), journal fill above 0.5, or a parked write
  (`journal_full`).
- **Cut** (`coord.rs:397`): `c = last_issued()`, everything written so far.
  The only bound is the map's dirty-leaf cap (`bound_cut`, `coord.rs:442-468`).
- **Destage, C3** (`lunix-destage/src/pipeline.rs:270-362`). Workers take jobs
  of up to 16 chunks; a worker handles its chunks one after the other
  (`worker.rs:207-215`) and assembles each chunk piece by piece
  (`assemble.rs:114-126`).
- **C4-C8** (`coord.rs:552-729`): ledger, map pack, the FULL commit (the
  durability point), checkpoint, floor, index run.
- **C9** (`coord.rs:730-750`): `Journal::reclaim`, release of the overlay,
  `journal_full = false`, wake of the queues.

## 2. Root causes

### 2.1 One synchronous read per piece (82.8 % of blocked time)

- `assemble.rs:124`: `PieceSource::Journal(loc) => deps.journal.read_at(loc, dst)?`.
  The engine's stand-in destager does the same (`lunix-engine/src/destage.rs:265`).
- `lunix-journal/src/journal.rs:340-364`: `read_at` rounds the range to 4 KiB,
  calls `sys::read_aligned` (`journal.rs:360`) and copies the bytes out.
- `lunix-journal/src/sys.rs:163-167`: `read_aligned` allocates and zeroes a
  fresh aligned buffer, then issues one blocking `read_exact_at`.

So each piece costs one allocation, one `O_DIRECT` `pread` and one copy, and
each worker waits for it before it issues the next. With `w` workers and a
read latency `t` the pipeline assembles at most `w / t` pieces per second:
8 / 3 ms is 2.7 K. The report's raw figure for the same volume is 2,732
random 4 KiB writes per second, so the volume's own operation limit gives the
same number; a deeper queue of the same reads would not pass it. A 4 KiB
random write is one piece, hence 2.4-3.2 K IOPS.

For the sequential cell the same bound at 64 KiB per piece allows about
0.17 GB/s; the measured 0.04 GB/s means the larger reads were slower than
3 ms or the volume capped their bandwidth. The report does not separate the
two (not verified).

Two side findings:

- **The read-back is not checked.** Neither `read_at` nor the lane's
  `submit_read` (`lane.rs:668`) verifies the record CRC: a piece is a
  sub-range of a record's data, and the CRC covers the whole record. Today a
  journal block damaged between the append and the destage is hashed, sealed
  and committed as the chunk's content. Recovery and `fsck` do check the CRC.
- **A partly covered chunk also reads its base** (`assemble.rs:102-106`,
  `read_base`): a 4 KiB write into an existing 64 KiB chunk reads, verifies
  and decodes the stored chunk first. The profile does not show it as a
  bottleneck in these cells (it goes through the store reader and its cache),
  but it is the next serial read in the same loop.

### 2.2 The ring is freed only at C9 of an unbounded cut

- A lane returns `Full` when it has no free ring file (`lane.rs:250-251`,
  `260-262`); the queue parks the write with `ring_wait` and raises
  `journal_full` (`queue.rs:508-524`).
- `Journal::reclaim` frees a file only when it is `Used`, its ordinal is
  below the tail, and every record in it is at or below a cut the engine
  asked about (`journal.rs:445-454`). The tail is computed from the cut
  (`tail_after`, `journal.rs:392-415`).
- The engine calls `reclaim` in one place, at C9 (`coord.rs:734-741`), and
  clears `journal_full` after it (`coord.rs:748`).
- The cut covers everything up to `last_issued()` (`coord.rs:397`). A commit
  therefore lasts `cut bytes / drain rate`; at 2.7 K pieces/s that is the
  measured 22-28 s, and a write that parks just after a cut began waits for
  the whole of it. This is the 15-35 s mode of the latency distribution.

Three things make it worse:

- **Reclaim rewrites the file before it frees it.** `reinit_slot`
  (`lunix-journal/src/ring.rs:389-423`) zeroes the used part of each file with
  `O_DIRECT` writes, `fdatasync`s twice, renames, and `reclaim` syncs the
  directory, file after file (`journal.rs:469-510`), on the coordinator
  thread, before any parked write is woken. Every journal byte is thus written
  twice to the journal device: once as data, once as zeros.
- **Granularity is the 64 MiB file.** A lane with `k` files can use `k - 1` of
  them before it needs a reclaim, and a commit frees a file of a lane only if
  its cut passed that file's last record. `lunixd` accepts 2 files per lane
  (`lunixd/src/stack/mod.rs:787-796`), which leaves one file, 64 MiB, of
  runway per lane.
- **One coordinator.** `tick` holds the coordinator mutex from the trigger
  check to the end of C10 and the GC share (`coord.rs:271-297`). Cuts do not
  overlap.

### 2.3 The control socket waits for the commit

`Daemon::status` (`lunixd/src/daemon.rs:666`) calls `engine.tip()`
(`daemon.rs:675`) and `engine.tip_created_at()` (`daemon.rs:710`); both lock
the coordinator mutex (`lunix-engine/src/engine.rs:429-437`), which the
`lunix-coord` thread holds for the length of a commit. `lunix status` returns
when the commit ends: the measured 10-15 s. `window_turn_left`
(`engine.rs:473-476`) has the same shape. Nothing else in the status path
blocks on the commit (`Journal::fill` takes short ring locks, the GC status
has its own mutex).

### 2.4 The `lunix-space` thread on a RAM-backed journal (hypothesis)

The thread is `lunix-space-ladder` (`lunix-space/src/gate.rs:315`); the kernel
truncates the name. It runs the ladder steps, and the compact step is the
engine's `run_urgent` (`lunix-engine/src/gc.rs:313-330`), which takes the
coordinator mutex and runs the urgent pass: a complete mark
(`lunix-gc/src/collector.rs:1819`, `Mark::run`, `mark.rs:468-471`) walking
every leaf of every retained root with an index lookup per entry
(`mark.rs:496-537`). `Mark::unit`, `LsmIndex::get` and the checksums of the
footers and nodes it reads are exactly the profile's top symbols.

Derived, not measured: while that pass runs, no commit can start, so the ring
is not freed and writes park, as in §2.2 but with the collector as the cause.
The RAM-backed run predates `p1/gcrate` and ADR-32 (which gave the ladder's
urgent pass a back-off), and it ran on a small volume directory, where
space pressure is expected. Whether it still limits `cda79fb` is not known.
The structural point stands either way: the urgent mark is the one collector
call that holds the coordinator for a whole mark instead of a budgeted step.

## 3. What must not change

- **Bytes.** A chunk must hold exactly the guest's bytes; newest `write_seq`
  wins per block (ADR-3).
- **Durability points.** A write is volatile until a FLUSH or FUA; those
  complete only after the group flush has `fdatasync`ed every lane up to
  their `upto` (ADR-4, ADR-28). A cut is durable at C6.
- **Reclaim rule.** A record leaves the ring only when a durable FULL covers
  it (DESIGN §4.1 "Reclaim", §8.2 "never reclaimed before step 5 is durable").
  Recovery replays the records above the newest FULL's `cut_seq`.
- **Prefix cuts.** Every commit is the state after all writes `<= c` (ADR-3).
- **Integrity and encryption.** Ids, tags and AEAD are computed at destage
  over the assembled chunk (ADR-6, ADR-11). The journal is local, plaintext
  and checksummed, not authenticated (on-disk-formats §5).
- **Formats.** Journal file and record, segments, commits and `volume.json`
  are frozen.

## 4. Options

Effects are given as: crash consistency (CC), durability and FUA/FLUSH (DUR),
integrity and encryption (INT), memory (MEM), on-disk format (FMT).

### 4.1 The read-back

**A1. Resident journal window (recommended).** The journal keeps a copy of
each batch's bytes from its submission until its ring file is reclaimed, in a
per-lane index keyed by `(ordinal, file offset)`. `read_at` and `submit_read`
look there first and fall back to the file. The copy is made when the batch
is sealed (one `memcpy` of the packed bytes; the registered pool buffer
cannot itself be kept, it is needed for the next batch). It becomes visible
when the batch write completes, before the `Append` completion is delivered,
so every location the overlay holds is resident or was skipped for the
budget.

- CC: none. The window is a cache of bytes already in the ring file; a crash
  loses it and recovery reads the files as today.
- DUR: none. Acknowledgement, group flush, FUA and the C6 point are untouched.
- INT: the bytes served are the bytes handed to the `O_DIRECT` write, so the
  chunk is byte-exact by construction. It is stricter than today: a ring
  block damaged after the write no longer reaches a chunk through the
  resident path (§2.1). Tags, ids and AEAD are computed as before. The copy is
  plaintext, like the batch buffers, the overlay's source and the read cache.
- MEM: at most the record bytes of the un-reclaimed ring files, so at most
  `journal_max` per volume, and a configurable budget below it. Past the
  budget new batches are not admitted to the window (oldest stay: the next
  cut needs them); they are read from the file.
- FMT: none. `lunix-iface` is unchanged (`Journal::read_at` keeps its
  contract); counters are added.
- Side effect: a guest read of just-written data is served from memory
  instead of an io_uring read.

**A2. Payload references in the overlay.** The overlay piece carries an
`Arc` of the data instead of a location. Same guarantees as A1, but it
changes `lunix-iface` (`PieceSource`), the overlay's 32-byte entry and its
memory gate, and after a restart there is nothing to reference. Rejected in
favour of A1, which hides the same thing behind `read_at`.

**A3. Read-around on a miss (recommended, with A1).** A miss reads the
1 MiB-aligned window of the ring file around the location with one `O_DIRECT`
read and puts it into the resident window, once per window however many
workers miss in it. A lane writes its records in order, so a cut's pieces
sit next to each other: 4 KiB records then cost about 1/250 of today's reads.
This is the path after a restart (the first cut holds up to a ring of
recovered records and nothing is resident) and past the budget.

- CC, DUR, FMT: none. INT: as today (file bytes, unchecked; see A5).
- MEM: within the A1 budget; a window that does not fit is used once and
  dropped.

**A4. io_uring or batched reads per piece.** Overlaps the reads instead of
removing them. It keeps one device operation per piece, so a volume with an
operation limit (the measured one) gains nothing, and an NVMe device still
pays a syscall and an interrupt per 4 KiB. Rejected as the main fix; A3
covers the miss path with fewer operations.

**A5. Check the CRC on read-back.** Possible only when whole records are
read (A3 could parse its window as the recovery scan does). It would close
the gap of §2.1 on the miss path. Not needed for the throughput fix; listed
as W6.

### 4.2 The ring stalls

**B1. Reclaim per destaged prefix, before the commit. Rejected: it loses
flushed data.** After a crash the tip is the previous FULL and its map does
not reference the destaged chunks; the records that held the data would be
gone. Making it safe needs a durable object between FULLs that carries the
map change (a local DELTA), which is a new format and a second recovery
path. The reclaim rule of §3 stays.

**B2. Bounded cuts (recommended).** Any `c'` in `(prev, c]` is a cut; the
coordinator already uses that for the map's leaf cap (`bound_cut`). Bound the
cut by **journal bytes** as well: the per-lane charge lists
(`lunix-engine/src/full.rs:268-291`) give the bytes up to any `write_seq`.
While the ring is above the fill trigger the coordinator commits back to
back. A parked write then waits for one bounded cut, not for the whole
backlog.

- The bound has a price: overwrites of a chunk in two cuts are destaged
  twice. For small random writes on large chunks the store writes per cut
  approach the working set, whatever the cut size, so small cuts multiply
  store writes and garbage (ADR-32's `R x tau`). The bound must therefore be
  as large as the stall target allows: **adaptive**, `cut_target` (1 s) times
  the measured drain rate of the last cuts, clamped to at least one ring file
  per lane in use and at most half the ring. On a fast device this is half
  the ring and nothing changes but the stall; on a slow one the cuts shrink
  and a parked write waits about a second.
- The cut is raised, when needed, to the lowest `c'` that lets `reclaim` free
  a file of the fullest lane, so every commit under pressure frees something
  (a read-only query on `Journal`).
- CC: none; every cut is a prefix, the C1-C9 order and the crash points are
  unchanged. DUR: none. INT: none. MEM: less (smaller cuts hold fewer sealed
  extents). FMT: none.
- Consequences: more FULLs under sustained overload (about one per second
  instead of one per 22-28 s), so more commit objects, map packs and index
  runs for GC, and `retain_last` covers less wall-clock time. Under a light
  load `T` and `B` still decide and nothing changes.

**B3. A fixed bound** (a quarter of the ring). Simpler than B2's adaptive
bound, but too small on fast devices (needless store writes) and too large on
slow ones (a quarter of 2 GiB at the measured drain rate is still minutes
before A1, seconds after). The alternative to W4.

**B4. Overlapping commits.** Destage cut `k+1` while cut `k` runs C5-C9. The
map transaction of `k+1` must not land before the map of `k` is sealed, or
the FULL of `k` would not be a prefix cut; so only the worker stage can
overlap. The gain is the share of C5-C9 in a cycle, which is small once A1
and B2 are in. Not now; revisit if the measurement shows C5-C9 above 20 % of
a cycle.

**B5. Wake per freed file, and count the waits (recommended).** `reclaim`
signals a lane as soon as one of its files is free instead of after all files
of all lanes; the status gains ring waiters, the longest ring wait, and the
resident window's hits, misses and bytes. Today no counter shows ring
parking. No effect on any guarantee.

### 4.3 Options that touch a frozen rule (not recommended now)

- **Reclaim without the zero rewrite.** The rewrite doubles the journal
  device's write load (§2.2). The reader rule "a zero `magic` ends the batch"
  and the treatment of a stale record as damage depend on it; dropping it is a
  reader-rule change of on-disk-formats §5.3 and needs the maintainer's
  **FREEZE sign-off**. `FALLOC_FL_ZERO_RANGE` gives the same bytes without the
  writes and is not a format change, but it leaves unwritten extents, which is
  the latency case ADR-4's gate rejected for growing files; it needs its own
  measurement.
- **Smaller ring files.** Would fix the granularity of §2.2. The format fixes
  64 MiB.

Neither is needed for the recommended set. Both are W7's later question.

### 4.4 The coordinator and the control path

- **C1. Status without the coordinator mutex (recommended).** The tip's
  `commit_seq`, `cut_seq` and `created_at` are published at C6 in a small
  view of their own; `tip`, `tip_created_at` and `window_turn_left` read it.
- **C2. The urgent mark in steps.** `run_urgent` would release the
  coordinator between mark units, as `run_pass` does. Only if the
  re-measurement still shows §2.4.

## 5. Recommendation

Stage 1, no format change:

1. A1, resident window in `lunix-journal`, budget `journal_max` by default,
   not locked memory.
2. A3, read-around on a miss.
3. B2, cuts bounded by journal bytes, adaptive, with the file-boundary floor.
4. B5, wake per freed file and the counters.
5. C1, status off the coordinator mutex.

Then measure (the M16 write cells with the stall profile, on an NVMe-class
device and on a RAM-backed directory) and decide stage 2 from the numbers:
the zero rewrite (§4.3), the urgent mark (C2), sealing segments off the
packer thread, overlapping commits (B4).

Order matters: A1 alone turns a 25 s cut into a short one and removes most of
the stall with it; B2 bounds what is left and is what holds when the device
is slow.

## 6. Throughput model

Symbols: `r` record bytes (4,160 for a 4 KiB write), `cs` chunk size, `W`
working set, `J` journal device write bandwidth, `S` store write bandwidth,
`D` destage rate in assembled bytes per second, `n` writes in a cut, `t_c`
the fixed cost of C4-C9.

**Today.** Drain = `min(w / t_read, device operations/s)` pieces per second,
2.7 K measured. Sustained IOPS equal the drain; the stall is
`cut bytes / drain`.

**After stage 1.**

- *Burst* (ring not full): the acknowledgement path alone, `J / r` and the
  queue threads. 300 K IOPS is 1.25 GB/s of journal writes. A burst lasts
  `usable ring / (ingest - drain)`.
- *Sustained*: `IOPS = n / (U(n) x cs / D + t_c)`, capped by `J / (2 r)`
  while the zero rewrite stays. `U(n) = (W / cs) x (1 - exp(-n x cs / W))` is
  the number of distinct chunks `n` uniform random writes touch.
- `D = min(CPU, S)`. CPU with 8 workers, from DESIGN §5: incompressible data
  (the `auto` probe stores it raw) is bounded by BLAKE3 and the seal, above
  10 GB/s; zstd-1 data 3-16 GB/s. The packer is one thread (copy, segment
  hash, buffered write, inline seal of each 16 MiB segment:
  `lunix-destage/src/pack.rs:109-125`, `lunix-store/src/container.rs:387-453`)
  and is estimated at 1.5-2 GB/s of stored bytes (estimate, not measured).

Worked cases, assuming `D` = 2 GB/s and `t_c` = 0.2 s (assumptions):

| case | per cut | sustained |
|---|---|---|
| w4k, 64 KiB, `W` 4 GiB, ring 1 GiB (cut 129 K writes) | 56 K chunks, 3.5 GiB, 1.9 s | about 65 K IOPS |
| w4k, 64 KiB, `W` 4 GiB, ring 2 GiB (cut 258 K writes) | 64 K chunks, 3.9 GiB, 2.1 s | about 110 K IOPS |
| w4k, 16 KiB, `W` 4 GiB, ring 2 GiB | 164 K chunks, 2.5 GiB, 1.3 s | about 170 K IOPS |
| w4k, 64 KiB, `W` much larger than the cut | every write a new chunk | `D / cs`, about 30 K IOPS |
| seqw, 1 MiB writes | no overwrite | `min(J / 2, D, S)`, 1.5-2 GB/s by the packer |

Reading the table:

- **300 K sustained 4 KiB IOPS at 64 KiB chunks needs `D` of about 6 GB/s
  with the 2 GiB ring, or a ring of about 6 GiB at 2 GB/s**, for a 4 GiB working
  set. For a working set far above the cut it is not reachable at 64 KiB
  (16 x amplification); at 16 KiB it needs `D` of 5 GB/s. The §20 row is a
  burst target as ADR-32 already says; a sustained cell must state `W`, the
  ring and the chunk size. This is an input to ADR-5's open default.
- **Sequential 3 GB/s** needs stage 2: the packer and its inline seals, and a
  journal device that takes 6 GB/s while the zero rewrite stays.
- **Stalls.** A parked write waits for one cut: `cut bound / D + t_c`, about
  1-2 s at the adaptive target, against 15-35 s. With ingest at or below the
  drain no write parks.
- **On the measured volume** (2.7-3 K operations/s, about 260 `fdatasync`/s,
  bandwidth `B` not reported): bursts run at the journal's bandwidth, and the
  sustained rate is about `B / (2 r + U(n) x cs / n)`, since journal, zero
  rewrite and store share the device. For 64 KiB chunks and the bench's 4 GiB
  working set that is `B / 36 KiB`: 3.5 K IOPS at an assumed 125 MB/s. The
  cells will show short bursts and the end of the long stalls, not the
  targets. The targets have to be measured on an NVMe-class device, with the
  RAM-backed directory as the engine-only reference.

## 7. Tests the change needs

- Resident window: a property test that `read_at` returns the same bytes
  from the window and from the file for random records, sub-ranges and lanes;
  concurrent readers during appends and reclaim; the budget (admission stops,
  bytes never exceed it); release at reclaim; a miss after restart.
- Read-around: one read per window under concurrent misses; windows at the
  file's ends; a location reclaimed meanwhile.
- Bounded cuts: every cut is a prefix (the existing crash-point and recovery
  suites at a small bound); a commit under pressure frees a file of the
  fullest lane; the bound follows the drain rate; `T` and `B` behave as
  before under a light load.
- Stack: the M16 w4k and seqw cells with the stall profile; no completion
  above a few seconds; `lunix status` under load in milliseconds;
  integrity-mode and encrypted volumes read back byte-exact after a kill
  during a bounded cut (the existing kill tests, run at the new cadence).

## 8. Open points

- The measured volume's bandwidth and whether the sequential cell's reads
  were latency- or bandwidth-bound (§2.1).
- Whether §2.4 still holds on `cda79fb`.
- The packer's real ceiling (§6).
- The cost of about one FULL per second for GC and for the index's run count
  (`LsmIndex::get` reads every run); the measurement must watch both.
