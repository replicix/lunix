# fsync-trace

Measures what a block device sees (FLUSH / PREFLUSH / FUA requests and the bytes completed
between them) when ext4, xfs and btrfs fsync, with and without a concurrent streaming writer.
Results: `bench/fsync-trace/RESULTS.md`; raw summaries: `bench/fsync-trace/raw/`.

Runs on the test host only (nothing is compiled or run locally). All load runs under
`taskset -c 8-15`; no fio; no null_blk/ublk.

- `gen.c`      load generators: `prep`, `small` (append 4 KiB + fsync|fdatasync at a rate), `stream`
               (buffered 1 MiB writes at MB/s, cyclic over a file, optional `wb` write-behind via
               `sync_file_range(WRITE)`, never fsync), `dsync` (O_DIRECT|O_DSYNC 4 KiB overwrite|append).
- `sqlite_wal.py` WAL + synchronous=FULL, one INSERT per transaction, paced.
- `run.sh`     setup (loads `scsi_debug`: blk-mq, write-back cache, FUA; brd is bio-based and
               write-through so it shows no requests/flushes), mkfs, mount, private tracefs instance
               filtered to the device, run a scenario, `filefrag -v -b512` for sector attribution, analyse.
               `run.sh all "ext4 xfs btrfs"`; `SECS`, `SCENS`, `WB` env knobs; `run.sh teardown` undoes everything.
- `analyze.py` parses block_bio_queue / block_rq_issue / block_rq_complete lines; flush points are
               bios with PREFLUSH or FUA; bytes = non-FUA write completions between consecutive
               points, split by file via the extent map (other = journal, metadata, unmapped).
- `merge.py`   combines the per-run summaries into `all.csv` and prints the tables.
- `hostinfo.sh` prints only the generic host description.

Scenarios: s1 fsync alone, s2a/s2b fsync + 200 MB/s / 1 GB/s stream, s3 fdatasync alone,
s3e fdatasync + 200 MB/s stream, s4 O_DSYNC overwrite, s4b O_DSYNC append, s6 SQLite WAL.
