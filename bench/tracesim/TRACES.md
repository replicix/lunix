# P0.6 traces: what was fetched

Date: 2026-11-11. Nothing was downloaded. `/var/tmp/lunix-traces` on the host is empty.
All probes were plain HEAD/GET of landing pages from the host (no files saved).

| trace set | source checked | outcome | reason |
|---|---|---|---|
| MSR Cambridge (mandatory) | `https://iotta.snia.org/traces/block-io/388` (redirected from the http URL; trace page `https://iotta.snia.org/traces/388`) | not run: skipped | The repository answers with a proof-of-work "Verifying That You Are Human" page (JavaScript bot challenge) instead of content, and per the published FAQ every download requires accepting the SNIA Trace Data Files Download License with cookies. Solving the challenge is bot-detection bypass and accepting the licence is terms acceptance; both are off limits for the executing model. Mirror hosts listed by a web search (`ny.iotta.cs.hmc.edu`, `server2/3.iotta.snia.org`) are the same repository with the same licence step; not tried. |
| Alibaba block traces 2020 | `https://github.com/alibaba/block-traces` (README fetched via raw.githubusercontent.com) | not run: skipped | The README states the download link is given only after a survey (`yida.alibaba-inc.com/.../alibaba_block_traces_2020_survey`). Tarball is 181 GB gzip (751 GB raw), md5 `95780fc531a60fd4ca0513ef88ef469c`, licence CC-4.0. Also needs a streaming per-device splitter (not written). |
| Tencent CBS block traces 2018 | `https://iotta.snia.org/traces/parallel/27917` (search result; the plan's `block-io/27917` URL is the same collection family) | not run: skipped | Same SNIA challenge and licence step as MSR. |
| (checked, unusable) Tencent "Cloud-Volume-Workload-Traces" | `https://github.com/Tencent/Cloud-Volume-Workload-Traces` (CC-4.0, `Cloud_Disk_dataset.zip`, ~1.3 GB compressed) | not fetched | It is a different, 2025 dataset: 300-second aggregates of IOPS and bandwidth per disk, no offsets, lengths or per-request timestamps. Chunk-size and cache simulation need request-level traces. Not a substitute. |

No synthetic data is presented as a trace. The only data files in the repo are
`spikes/tracesim/tests/msr_format_smoke.csv` (40 hand-made lines in MSR column layout used to
exercise the CLI; labelled as a parser fixture, not a workload) and the unit-test fixtures.

To unblock: a person accepts the SNIA licence and downloads MSR (and Tencent CBS) parts onto the host
(`/var/tmp/lunix-traces/msr/`, `.../tencent/`), and/or obtains the Alibaba link through the survey;
then `spikes/tracesim/run.sh /var/tmp/lunix-traces out` produces everything below with the gate verdicts.
