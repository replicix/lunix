| point | hits in a 120 s run | runs ext4 | runs xfs | result | notes |
|---|---|---|---|---|---|
| destage.after_assemble | 732188 | pass 3 | - | PASS |  |
| destage.after_segment_append | 199270 | pass 3, unreached 1 | - | PASS, some variants unreached | unreached: last-159416/ext4 |
| destage.after_segment_seal | 804 | pass 3 | - | PASS |  |
| engine.c1.waited | 68 | pass 3, unreached 1 | pass 3 | PASS, some variants unreached | unreached: last-54/ext4 |
| engine.c2.cut | 68 | pass 3 | pass 3 | PASS |  |
| engine.c3.destaged | 68 | pass 3 | pass 3 | PASS |  |
| engine.c4.named | 68 | pass 3 | pass 3 | PASS |  |
| engine.c5.map_sealed | 68 | pass 3 | pass 3 | PASS |  |
| engine.c6.committed | 68 | pass 3 | pass 3 | PASS |  |
| engine.c7.checkpoint | 68 | pass 3 | pass 3 | PASS |  |
| engine.c7.floor | 68 | pass 3, unreached 1 | pass 3 | PASS, some variants unreached | unreached: last-54/ext4 |
| engine.c8.index | 68 | pass 3 | pass 3 | PASS |  |
| engine.c9.reclaimed | 68 | pass 3 | pass 3 | PASS |  |
| engine.recover.caught_up | 1 | pass 1 | - | PASS |  |
| engine.recover.floor | 1 | pass 1 | - | PASS |  |
| engine.recover.journal | 1 | pass 1 | - | PASS |  |
| engine.recover.swept | 1 | pass 1 | - | PASS |  |
| engine.recover.verified | 1 | pass 1 | - | PASS |  |
| gc.commit_delete.before_unlink | 16 | pass 3 | - | PASS |  |
| gc.compact.after_relocate | 34 | pass 3 | - | PASS |  |
| gc.compact.after_segment_seal | 34 | pass 3 | - | PASS |  |
| gc.condemn.after_list_write | 3 | pass 2 | - | PASS |  |
| gc.delete.after_dirsync | 10 | pass 3 | - | PASS |  |
| gc.delete.after_unlink | 10 | pass 3 | - | PASS |  |
| gc.delete.before_unlink | 10 | pass 3 | - | PASS |  |
| gc.evidence.after_put | 2 | pass 1 | - | PASS |  |
| gc.mark.end | 4 | pass 3 | - | PASS |  |
| gc.urgent.after_mark | 0 | - | - | NOT REACHED | the workload never executes it |
| index.after_condemn_flag | 529 | pass 3 | - | PASS |  |
| index.after_manifest_swap | 600 | pass 3 | - | PASS |  |
| index.before_manifest_swap | 600 | pass 3, unreached 1 | - | PASS, some variants unreached | unreached: last-480/ext4 |
| journal.after_append_submit | 26936 | pass 3 | - | PASS |  |
| journal.after_fdatasync | 11542 | pass 3 | - | PASS |  |
| journal.after_reclaim_delete | 68 | pass 3 | - | PASS |  |
| journal.before_fdatasync | 11542 | pass 3 | - | PASS |  |
| journal.before_reclaim_delete | 68 | pass 3 | - | PASS |  |
| journal.group_flush.after_watermark_wait | 11558 | pass 3 | - | PASS |  |
| journal.mark.after_sync | 60 | pass 3 | - | PASS |  |
| journal.mark.before_write | 60 | pass 3 | - | PASS |  |
| journal.reinit.before_rename | 68 | pass 3 | - | PASS |  |
| store.checkpoint.after_dirsync | 1 | pass 1 | - | PASS |  |
| store.checkpoint.after_fdatasync | 68 | pass 3 | - | PASS |  |
| store.checkpoint.before_fdatasync | 68 | pass 3 | - | PASS |  |
| store.checkpoint.before_write | 68 | pass 3 | - | PASS |  |
| store.commit.after_dirsync | 69 | pass 3 | - | PASS |  |
| store.commit.after_fdatasync | 69 | pass 3 | - | PASS |  |
| store.commit.after_rename | 69 | pass 3 | - | PASS |  |
| store.commit.before_fdatasync | 69 | pass 3 | - | PASS |  |
| store.delete.after_dirsync | 18 | pass 3 | - | PASS |  |
| store.delete.after_unlink | 18 | pass 3 | - | PASS |  |
| store.delete.before_unlink | 18 | pass 3 | - | PASS |  |
| store.evidence.after_dirsync | 2 | pass 1 | - | PASS |  |
| store.evidence.after_fdatasync | 2 | pass 1 | - | PASS |  |
| store.evidence.after_rename | 2 | pass 1 | - | PASS |  |
| store.evidence.before_fdatasync | 2 | pass 1 | - | PASS |  |
| store.leftover.after_dirsync | 1 | pass 1 | - | PASS |  |
| store.leftover.after_unlink | 0 | - | - | NOT REACHED | the workload never executes it |
| store.leftover.before_unlink | 0 | - | - | NOT REACHED | the workload never executes it |
| store.pack.after_dirsync | 67 | pass 2, unreached 1 | - | PASS, some variants unreached | unreached: mid-34/ext4 |
| store.pack.after_fdatasync | 67 | pass 3 | - | PASS |  |
| store.pack.after_rename | 67 | pass 3 | - | PASS |  |
| store.pack.before_fdatasync | 67 | pass 3 | - | PASS |  |
| store.segment.after_dirsync | 838 | pass 3 | - | PASS |  |
| store.segment.after_fdatasync | 838 | pass 3 | - | PASS |  |
| store.segment.after_rename | 838 | pass 3 | - | PASS |  |
| store.segment.before_fdatasync | 838 | pass 3 | - | PASS |  |
| store.segment_delete.after_dirsync | 0 | - | - | NOT REACHED | the workload never executes it |
| store.segment_delete.after_unlink | 0 | - | - | NOT REACHED | the workload never executes it |
| store.segment_delete.before_unlink | 0 | - | - | NOT REACHED | the workload never executes it |
| store.set_aside.after_dirsync | 0 | - | - | NOT REACHED | the workload never executes it |
| store.set_aside.after_rename | 0 | - | - | NOT REACHED | the workload never executes it |
| store.set_aside.before_rename | 0 | - | - | NOT REACHED | the workload never executes it |
| store.state.after_dirsync | 77 | pass 3 | - | PASS |  |
| store.state.after_fdatasync | 77 | pass 3 | - | PASS |  |
| store.state.after_rename | 77 | pass 3 | - | PASS |  |
| store.state.before_fdatasync | 77 | pass 3 | - | PASS |  |
| ublk.after_fetch | 342861 | pass 3 | - | PASS |  |
| ublk.after_update_size | 0 | - | - | NOT REACHED | the workload never executes it |
| ublk.before_commit | 342861 | pass 3 | - | PASS |  |
