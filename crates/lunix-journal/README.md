# lunix-journal

`FileJournal` implements `lunix_iface::journal::Journal` (the iface rustdoc is the contract).

- Files: `journal/q<lane:02x>-<ordinal:08x>.j`, 64 MiB, zero-filled, `O_DIRECT`, header per
  `on-disk-formats.md` 5.1. `journal_max / lanes / 64 MiB` files per lane, at least two (so a
  lane can roll while reclaim frees the first). Creation: tmp name, zero fill, fdatasync, header,
  fdatasync, rename, dir fsync. Reclaim reuses the inode (zero up to the written high water,
  fdatasync, new header with the next ordinal and a new generation, fdatasync, rename, dir fsync).
- Lane: one open batch in a pooled aligned buffer (1 MiB + 4 KiB, registered as a fixed buffer
  when RLIMIT_MEMLOCK allows, else plain writes). `try_reserve` places the record and takes the
  global `write_seq` under the lane's flight lock; `append` copies and encodes (`lunix-format`).
  A batch is written by an `append` that finds nothing in flight, by `poll` while fewer than
  `max_inflight` (2) are in flight, or when the next record does not fit.
- Flush coordinator: one thread, its own ring; waits for every lane's pending set to clear
  below `upto`, then fdatasyncs every dirty file in parallel. Poison = lowest failed or
  abandoned `write_seq`; flushes at or above it fail.
- Flushed watermark (on-disk-formats §5.2, amendment 2026-11-14, pending re-sign): a completed group flush
  raises `Shared::flushed` to its `upto`; every record appended carries it (bytes 56-63). `prepare` raises it
  to the last recovered record once those are synced. Recovery reports the highest watermark found
  (`Recovered::flushed`), so the engine refuses only gaps or damage at or below it.
- Recovery (`scan.rs`): normative reader `lunix_format::journal::scan_file` per file, merge,
  stop at the first gap or duplicate. It also plans a **wipe**: everything a lane holds after
  its last record below `next_seq` is zeroed by the first `open_lane` (not by `recover`), so
  stale valid records beyond the stop point cannot return when sequence numbers are reused.
  The wipe, re-initialisation of bad-header files and an fdatasync of every file holding
  records run there too.
- Not supported: reducing the lane count over existing files (`Unsupported`); files shorter
  than expected are scanned as they are and fixed on reuse.
- Tests: `tests/` (contract, model, recovery, flush, align, reclaim, crashpoints with
  `--features crashpoints`); bench: `journal-lane-bench`. Test knobs: `JournalConfig::file_len`,
  `trace`, `FileJournal::fail_next_append`.
