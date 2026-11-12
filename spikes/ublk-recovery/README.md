# ublk-recovery (Phase 0, P0.3 and P0.4)

Throwaway spike, not product code. `ublk-recovery run` serves a recoverable
file-backed ublk device (libublk 0.4.8, O_DIRECT backing file, flags
`CMD_IOCTL_ENCODE | USER_RECOVERY | USER_RECOVERY_REISSUE | QUIESCE |
UPDATE_SIZE`, optional `--batch`). State lives in the backing file, so
`kill -9` + `run --recover --dev-id N` keeps a mounted file system alive.
Signals: SIGUSR1 prints FLUSH/FUA/write counters, SIGUSR2 quiesces and exits
(planned handover), SIGHUP toggles "ack but drop writes" (negative control),
SIGTERM stops. Other subcommands: `resize`, `quiesce`, `try-stop`, `stop`,
`del`, `features`, `probe-add` (raw ADD_DEV with arbitrary flags; libublk
rejects NO_AUTO_PART_SCAN, SAFE_STOP_DEV and FAIL_IO, so `src/rawctl.rs`
talks to /dev/ublk-control directly).

Reproduce (root, scratch only; fio, jq, python3, e2fsprogs, xfsprogs, btrfs-progs):

    cargo build --release
    sudo ./run-features.sh                      # feature table + live probes
    sudo ./run-crash-loop.sh                    # kill -9 loop, ext4 (FS=xfs for xfs)
    sudo KILL_MODE=quiesce PHASES="handover:20:0" ./run-crash-loop.sh
    sudo CONTROL=noreissue PHASES="immediate:3:0" ./run-crash-loop.sh   # negative control (a)
    sudo CONTROL=fault PHASES="immediate:2:0" ./run-crash-loop.sh       # negative control (b)
    sudo ./run-resize.sh && sudo ./run-flush-count.sh

All scripts use `$LUNIX_SPIKE_DIR` (default /var/tmp/lunix-spike), mount only
below it with `nosuid,nodev`, run fio under `timeout -k`, clean up on exit and
call `../common/ublk-leftovers.sh` at start and end. See
`docs/plans/v1/CONVENTIONS.md` for the safety rules.

P0.12 (never run): `run --sink mem --keeper SOCK`, `keeper/keeper.py`, `run-lost-keeper.sh`, `run-ramsink.sh`.
