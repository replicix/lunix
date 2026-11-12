# ublk-baseline (Phase 0, P0.1)

Throwaway spike, not product code. `ublk-spike` is a libublk 0.4.8 server with
a `null` target (completes every request immediately; READ returns whatever is
in the buffer, so it is a throughput probe only) and a `loop` target
(io_uring on an O_DIRECT backing file, FLUSH = fdatasync, FUA = RWF_DSYNC).
The logic follows libublk's `examples/null.rs` and `examples/loop.rs` (sync
queue handlers); changes: 4 KiB blocks, `VOLATILE_CACHE | FUA`, explicit
queue-thread pinning, fdatasync flush.

Subcommands: `add --target null|loop [--backing F] --queues N --depth D
--cpus LIST`, `del --id N`, `list`, `features`.

Reproduce (needs root, `ublk_drv` + `null_blk` modules, fio, python3):

    cargo build --release && sudo ./run-baseline.sh

The driver script writes fio JSON and summaries under `$OUT`
(default `~/lunix-p0/out/ublk-baseline`). Environment knobs: `DAEMON_CPUS`,
`FIO_CPUS`, `RUNTIME`, `RAMP`, `REPS`. Safety rules: see
`docs/plans/v1/CONVENTIONS.md`; the script only touches devices it creates and
files under `$LUNIX_SPIKE_DIR` and cleans up on exit.
