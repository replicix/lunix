# journal-bench (Phase 0, P0.5)

Throwaway spike, not product code. `journal-bench` appends fixed-size records
to a journal file with `O_DIRECT` + io_uring (registered file and buffer, one
thread) and measures per-record durable latency for four durability
primitives: `group` (B writes, one `FSYNC(DATASYNC)`), `linked` (chains of
B `WRITE_FIXED` + `FSYNC` with `IO_LINK`, up to 4 in flight), `dsync`
(`RWF_DSYNC` per write) and `odsync` (`O_DSYNC` file). Open-loop offered load
(`--rate`) or closed-loop saturation (`--rate 0`), optional concurrent 64 KiB
sequential reader (`--bg-read`), preallocated zero-filled file or growing file.
512 B records are padded to 4 KiB, so the I/O equals the 4 KiB case.

Reproduce (root; needs loop devices, mkfs.ext4/xfs/btrfs, python3):

    cargo build --release && sudo ./run-journal.sh      # all file systems
    python3 analyze.py out/results.jsonl > out/tables.md

`run-journal.sh` creates sparse 12 GiB images under `$LUNIX_SPIKE_DIR`, attaches
them with `losetup --direct-io=on`, mounts under `$LUNIX_SPIKE_DIR/mnt`
(`nosuid,nodev`), runs `matrix.py`, and cleans up on exit. Env knobs: `FSES`,
`SECS`, `OUT`, `QUICK=1` (tiny smoke matrix).
