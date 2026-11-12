# P0.6 traces

Provenance of the MSR set: MSR Cambridge block traces (SNIA IOTTA), provided by the maintainer.
No trace data is in the repo; no download location is recorded here on purpose.

## Used
| set | content | size | notes |
|---|---|---|---|
| MSR Cambridge, 2007, one week | 36 volumes on 13 servers, files `<host>_<disk>.csv.gz` (hm_0 ... web_3), CSV `Timestamp(100 ns filetime),Hostname,DiskNumber,Type,Offset,Size,ResponseTime` | 5.0 GB compressed (files from 6 KB to 2.07 GB), MD5s verified by the maintainer | Staged by the maintainer in the host's trace directory. Streamed through `gzip -dc`; nothing decompressed to disk. Left in place after the run. |

## Not run
| set | reason |
|---|---|
| Alibaba block traces 2020 | the download link is only given after a survey; not obtained |
| Tencent CBS 2018 (SNIA IOTTA) | download needs a bot challenge and licence acceptance; not obtained. The Tencent GitHub "Cloud-Volume-Workload-Traces" dataset is 300 s aggregates without request offsets and is not usable for these simulations |

No synthetic data is presented as a trace. The CLI smoke fixture `spikes/tracesim/tests/msr_format_smoke.csv`
is 40 made-up lines in MSR layout used only to exercise the parser.
