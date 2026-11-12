# P0.5 results: journal microbenchmark

**Not run.** The session was stopped under CONVENTIONS rule 6 after a ublk device deadlocked in the kernel during P0.3
(see `../ublk-recovery/RESULTS.md`); P0.5 needs loop devices and mounted file systems on a host that now has a stuck ext4
whose global `sync` hangs, so it was not started.

Ready but never executed: `spikes/journal-bench` (`journal-bench`, io-uring `=0.7.16`, hdrhistogram `=7.6.0`; it builds cleanly on the
host), `run-journal.sh`, `matrix.py`, `analyze.py` (decision-rule evaluation). The 512 B record row is not a separate cell: it is
padded to 4 KiB and issues the same I/O as the 4 KiB row. Plan: ~116 cells per file system configuration for ext4, xfs, btrfs and
the root-directory case, and a primary subset (48 cells) for the `noatime` and btrfs `nodatacow` variants.
