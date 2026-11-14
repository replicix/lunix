# Note: branch `lead/p1-iface2` (M12a review requests A-G, I; maintainer decisions 1 and 2)

The M12a implementer made this branch for the lead, from `origin/main` `30e4329`. `p1/m12a` is rebased onto it.
Ownership fails for this branch by design, so CI ran with `LUNIX_SKIP_OWNERSHIP=1`.

## Code

| request | change |
|---|---|
| A | `ctl::ErrorCode` gains `Rollback`, `Fork`, `ForkUnverifiable` (exit 6) and `JournalDamage` (exit 7, "run fsck"). |
| B | `VolumeStatusBody::rollback_floor: Option<RollbackFloorView { max_commit_seq, tip_hash (64 hex) }>` (serde default). |
| C | `Attached::first_attach: bool` (serde default), filled from the engine's `RecoveryReport::first_attach`. |
| D | `Credential::mode: Option<VolumeMode>`, the mode the caller expects. `validate` refuses key material with `plain`. |
| E | `Attach::on_journal_damage: Option<JournalDamagePolicy>`, plus `Attach::allow_unverifiable_fork: bool`, the audited operator override of decision 1. |
| other iface | `.` and `..` are no longer valid names. Size fields document 0 as "unknown" (a detached encrypted volume). |
| F (engine) | Decision 1, details below. |
| G (engine) | `Engine::tip_created_at()`. |
| I (M9) | `Partial::undeleted: Vec<SegId>`, the segments whose delete failed. `freed_bytes` is documented as informational (R3). |

Details of F:
- When the tip is above the floor, `recover` walks the chain from the tip down to the floor's `commit_seq` with
  `verify_chain`. The commit reached there must hash to `tip_hash`, otherwise `OpenError::Fork`.
- A walk that cannot be completed is `OpenError::ForkUnverifiable { floor, tip, source }`.
- `EngineConfig::allow_unverifiable_fork` (default false) is the override: when set, the engine logs a warning and
  opens.
- The existing R4 test has a new case (another commit at the floor's height below the tip's `chain_floor` is a
  `Fork`). A new engine test covers a deleted floor commit: `ForkUnverifiable`, and the override opens.
- The daemon's pre-check is gone from `p1/m12a`. Its test moved to the daemon's wiring tests and also pins a fork
  two commits above the floor.

## Docs

- **ADR-31:**
  - the fork window (decision 1);
  - the floor recorded at create;
  - a rejected `state/pins` is an error;
  - the new errors and the audited override.
- **DESIGN:**
  - §4.4 step 3: R4 now names `ForkUnverifiable`. It also documents the two housekeeping writes that come before
    R4 (review m5): new ring files of new lanes and `*.tmp` removal; existing records and runs are never touched.
  - §8.4: the R4 walk.
  - §8.7: GC keeps the commits at or above the floor's height.
  - §15: the walk, creation, and the rejected-pins rule.
- **on-disk-formats §20:**
  - the floor is written at create;
  - the walk replaces "attach proceeds" below `chain_floor`;
  - `ForkUnverifiable` and the override;
  - a rejected file is an error.
- **Plan:**
  - M17 rule 6, a requirement for M17.A: keep every commit at or above the floor's height while a floor exists,
    with a test.
  - Rule 3(c) and the journal section: `journal_max >= queues × 128 MiB`, with the 256 MiB, 512 MiB and 1 GiB
    figures tied to their queue counts.
  - M12 memory gate: measured after a workload, with idle heap trimming; the fresh-daemon reading and the
    journal's locked batch buffers are reported for information (decision 2).

## CI on the build host (12 CPUs, `LUNIX_JOBS=10`)

- **First run** (`7911531`): rustfmt diffs. They are fixed in the next commit.
- **Second run:** one failure, `lunix-journal --test model crash_recovery_prefix_lanes_1_2`.
  - The error was `open lane: ENOMEM`. The build host's hard `RLIMIT_MEMLOCK` is 8 MiB, and several test threads
    register io_uring buffers at once.
  - The same test passes alone with `--test-threads 1` (4 passed, 300 s).
  - Nothing in the journal changed. This is the host, not the branch.
- **Third run:** `RUST_TEST_THREADS=4` (the plan's own cap). The same ENOMEM, this time in
  `crash_recovery_prefix_lanes_3_4`.
- **Fourth run** at `f8d48e1` with `RUST_TEST_THREADS=1`: `ci: all green`, 1 354 tests passed, 0 failed
  (ownership skipped).
- `p1/m12a` on top of this branch is also green (1 394 tests).
- **Lead decision of 2026-11-13, now on this branch (`0eace35`):** `on_journal_damage = refuse` refuses only
  mid-journal damage, that is a damaged range with a later valid record in its lane, or `ignored_after_gap`.
  - A torn last record of a lane is crash residue, logged at warn and wiped. The reason: a flushed or FUA record is
    durable before its acknowledgement, so a crash cannot tear it.
  - The change is in `recover.rs` (`refuse_worthy`, with a unit test) and the `OnJournalDamage::Refuse` rustdoc.
  - The docs say the same in ADR-4, DESIGN §4.1 and §4.4 step 7, and the plan's M12 paragraph.
- **CI at `0eace35`:** `ci: all green`, 1 356 tests passed, 0 failed.
