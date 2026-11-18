# M15 crash points (2026-11-18T01:28:27Z, kernel 7.0.0, 16 cpus): 1 of 66 points, run cap 600 s, budget 72000 s

## counting run (120 s of the workload)
- hit counts: counted

## crashpoint/destage.after_assemble/first-1/ext4
- crashed after 3s (status 137)
- PASS crashpoint/destage.after_assemble/first-1/ext4 (crashed after 3s, recovery stall 1065 ms)

## crashpoint/destage.after_assemble/mid-369980/ext4
- crashed after 4s (status 137)
- PASS crashpoint/destage.after_assemble/mid-369980/ext4 (crashed after 4s, recovery stall 376 ms)

## crashpoint/destage.after_assemble/last-739959/ext4
- UNREACHED crashpoint/destage.after_assemble/last-739959/ext4: the workload finished without executing hit 739959 of destage.after_assemble
## crashpoint/destage.after_assemble/last90-665963/ext4 (last run did not reach the count)
- UNREACHED crashpoint/destage.after_assemble/last90-665963/ext4: the workload finished without executing hit 665963 of destage.after_assemble

## end
overall FAIL=0
