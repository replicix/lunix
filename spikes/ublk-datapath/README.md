# ublk-datapath (Phase 0, P0.2)

Throwaway spike, not product code. One ublk target (libublk 0.4.8) with three
data-transfer modes and three work levels, so only the transfer differs:

- `--mode copy | user-copy | batch` (default per-tag buffers; `UBLK_F_USER_COPY` with
  `pread`/`pwrite` on `/dev/ublkcN`; `UBLK_F_BATCH_IO` via libublk's `UblkBatchQueue`)
- `--work none | hash | hash+lz4` (BLAKE3 of every write payload, LBA pattern for
  reads, LZ4 of every 64 KiB chunk)
- `--workers N` moves the work to a bounded OS-thread pool; completions return to the
  queue thread through an eventfd (libublk's batch transport is queue-thread-only)

Reproduce (root; fio, python3; scratch only; the driver cleans up on exit):

    cargo build --release
    sudo OUT=$PWD/out ./run-datapath.sh        # main + QD1 matrices, workers 0/4
    python3 analyze.py out                     # tables and the ADR-17 decision rules

Knobs: `MODES`, `WORKS`, `DAEMON_CPUS` (0-3), `FIO_CPUS` (4-7), `RUNTIME`, `RAMP`,
`REPS`, `PHASE=main|workers|both`. Matrices: `../common/matrices/dp-*.txt`.
Safety rules: `docs/plans/v1/CONVENTIONS.md`.
