# Lunix phase 0 baseline (P0.11)

Dates: 2026-11-11 and 2026-11-12. Every number below is copied from the
`RESULTS.md` file named in its row; nothing here was measured again. The
gate outcomes are in [DECISIONS.md](../docs/explanation/DECISIONS.md) and
the spec corrections are in [DESIGN.md](../docs/explanation/DESIGN.md).

## Host

AMD EPYC (Zen 5), 16 vCPUs, SMT off, 1 NUMA node, ~30 GiB RAM, local NVMe
SSD (write cache reported as "write through"), ext4 root. Kernel 7.0.0
(Ubuntu), Ubuntu 26.04. fio 3.41, rustc 1.96.0. The host is a virtual
machine with no cpufreq, so the CPU governor and turbo state were not
exposed and not set. It was shared with other work during the runs.

Pinning: ublk queue threads on CPUs 0-3 and fio on 4-7 (P0.1-P0.4),
journal thread on CPU 0 (P0.5), CPUs 8-15 for the unprivileged spikes.
Crate pins: libublk 0.4.8, io-uring 0.7.16, blake3 1.8.7, aws-lc-rs 1.19.0,
object_store 0.14.1. Full header: `ublk-baseline/raw/hostinfo.md`.

## Headline numbers

### ublk framework (P0.1, P0.2)

fio: `io_uring`, `direct=1`, 4 active queues of 16, 5 s runs, 3
repetitions (4 s for P0.2). Source:
[ublk-baseline](ublk-baseline/RESULTS.md),
[ublk-datapath](ublk-datapath/RESULTS.md).

| measure | kernel reference | libublk 0.4.8 | note |
|---|---|---|---|
| 4k randread, 8 jobs, QD128, IOPS | `null_blk` 4 900 347 | `ublk-null` 3 829 625 (0.78×) | gate met; daemon CPU 0.85 µs per I/O |
| 4k randread, 1 job, QD32, IOPS | 1 217 194 | 978 373 (0.80×), p99 38.7 µs | one queue |
| 4k randread, QD1, lat p50 / p99 | 0.7 µs | 3.1 µs / 9.9 µs | +2.4 µs |
| 4k randwrite, 8 jobs, QD128, IOPS | 4 691 779 | 3 616 265 (0.77×) | |
| 64k write, 8 jobs, QD32, IOPS | 2 434 786 | 991 563 (0.41×) | copy path, 4.0 µs per I/O |
| 1M read, 8 jobs, QD32, IOPS | 152 028 | 25 254 (0.17×) | copy path, 158 µs per I/O |
| loop (16 GiB file) | ~5.8-6.0 K IOPS | ~5.8-6.0 K IOPS | disk-bound, inconclusive |

Copy against `USER_COPY` against `BATCH_IO`, with the server hashing every
write (BLAKE3) and filling every read, 8 jobs unless noted:

| cell | copy IOPS | `USER_COPY` / copy | `BATCH_IO` / copy |
|---|---|---|---|
| 4k randread QD128 | 3 710 358 | 1.10 | 1.09 |
| 4k randwrite QD128 | 2 134 923 | 0.91 | 1.07 |
| 4k randwrite QD32, 1 job | 553 968 | 0.89 | 1.02 |
| 4k randwrite QD1, 1 job, lat p50 | 4.9 µs | +0.1 µs | +0.3 µs |
| 64k write QD32 | 427 558 | 1.12 | 0.73 |
| 64k write QD128 | 235 317 | 1.43 | 0.97 |
| 64k randread QD128 | 279 189 | 2.55 | 0.89 |
| daemon CPU, 64k write QD128, µs per I/O | 17.02 | 11.92 | 17.49 |
| resident set | 148 MB | 26 MB | 95 MB |

Verdicts: the `USER_COPY` rule is not met (4k writes lose 8-12 %), so copy
stays the default. The `BATCH_IO` rule is not met, so it is opt-in and is
not used with `QUIESCE`. A worker pool costs about +4 µs p50 per hop at
QD1 (4k randwrite p50 4.9 µs inline, 8.8 µs with 4 workers).

Hash plus LZ4 of every 64 KiB write, inline in four queue threads: 239 895
IOPS at QD32 (about 15.7 GB/s, 16.7 µs of CPU per I/O).

### Recovery, resize and FLUSH counts (P0.3, P0.4)

Source: [ublk-recovery](ublk-recovery/RESULTS.md). ext4 unless noted.

| measure | result |
|---|---|
| kill -9 cycles, ext4 / xfs | 36 / 10, all clean (fio, fsck, sha256, acknowledged fsyncs) |
| QUIESCE handover cycles, ext4 | 20 of 20 clean (after the fix: exit without `STOP_DEV`) |
| `BATCH_IO` kill -9 / QUIESCE | 20 of 20 clean / failed on the second cycle |
| kill to serving, immediate restart (kill to ready p50 / max) | 259 / 268 ms |
| heartbeat stall, immediate restart (p50 / p99 / max) | 221 / 238 / 238 ms (ext4), 233 / 245 / 245 ms (xfs) |
| heartbeat stall, 1 s restart delay (p50 / p99 / max) | 1128 / 1141 / 1141 ms (ext4), 1132 / 1162 / 1162 ms (xfs) |
| heartbeat stall, QUIESCE handover (p50 / p99 / max) | 180 / 2000 / 2000 ms |
| daemon exec to recovered, immediate restart (p50) | 167 ms |
| negative controls | both fail as expected |
| online grow 16 to 64 GiB under fio | ext4, xfs, btrfs: no errors, fsck clean |
| device cache attributes | `write back`, `fua 1` (without `VOLATILE_CACHE`: `write through`, `fua 0`) |

FLUSH and FUA commands per `fsync` (10 000 iterations):

| file system | writes per sync | FUA per sync | FLUSH per sync, 1 job | FLUSH per sync, 8 jobs |
|---|---|---|---|---|
| ext4 | 3.0 | 1.0 | 1.0 | 0.25 |
| xfs | 2.0 | 1.0 | 1.0 | 0.73-0.78 |
| btrfs | 11.6 | 1.0 | 1.0 | 0.25 |

No file system issues more than 1 FLUSH per sync. Source for the block
layer view (an fsync is one empty FLUSH then one FUA write, and a flush
group carries 4.1 MiB at a 200 MB/s stream and 19 MiB at 1 GB/s):
[fsync-trace](fsync-trace/RESULTS.md).

### Keeper-held RAM write buffer (P0.12)

Source: [ublk-recovery](ublk-recovery/RESULTS.md), section P0.12.

| measure | result |
|---|---|
| mounted ext4 kill -9 cycles | 3 + 20, all clean |
| exec to recovered, 20 cycles (p50 / p99 / max) | 314 / 1264 / 1264 ms |
| heartbeat stall, 20 cycles (p50 / p99 / max) | 594 / 1819 / 1819 ms |
| lost keeper | refused and deleted, never resumed; ext4 went read-only |
| ack latency, RAM sink, 4k randwrite QD1 (p50 / p99 / p99.9) | 3.7 / 11.7 / 155 µs |
| ack latency, RAM sink, QD32 (p50 / p99 / p99.9) | 42 / 52 / 514 µs, 712 347 IOPS |
| ack latency, O_DIRECT write to the backing file (not durable), QD1 | 3359 / 13697 / 16908 µs |
| replay of 256 MiB after kill -9 | 3.76 s (64 KiB records), 23.7 s (4 KiB records) |

### Journal (P0.5)

Group commit, 4 KiB records, one run per cell. Source:
[journal-bench](journal-bench/RESULTS.md).

| file system | closed-loop best group commit (rec/s, p50 / p99) | note |
|---|---|---|
| ext4 on loop | 14 990 (4.6 / 5.6 ms) | flush p50 about 1.8 ms |
| xfs on loop | 2 764 (21.7 / 53.0 ms) | |
| btrfs on loop | 380 (136 / 489 ms) | 6-13 ms flush |
| btrfs `nodatacow` on loop | 7 674 (7.9 / 11.0 ms) | 20× default btrfs |
| real-disk directory (ext4) | 36 511 (1.5 / 3.6 ms) | flush p50 about 3 µs: the disk reports write-through |

At 5 000 offered rec/s, group B = 64: ext4 p50 4.9 ms and p99 7.9 ms; xfs
p50 4.2 ms and p99 6.1 ms. Linked `WRITE→FSYNC` reached 0.11-0.55× of
group commit's saturation throughput on ext4, xfs and the real disk.
`O_DSYNC` per write: 43-1041 rec/s.

### Block map (P0.7)

10 TiB, 64 KiB chunks, fanout 512, one core. Source:
[radixmap](radixmap/RESULTS.md).

| measure | result |
|---|---|
| depth | 3 levels to 8 TiB, 4 at 10 TiB |
| seal, 100 / 1 000 / 10 000 scattered dirty leaves | 9.4 ms / 59.7 ms / 275 ms |
| bytes written, 1 000 leaves | 23.1 MiB raw, 20.4 MiB stored (flat 1 MiB blocks: 873 MiB) |
| diff, 1 000 differing leaves | 1.04 ms, 998 nodes read |
| cold open and first lookup | 4 reads (25 ms per GET: 100.5 ms) |
| 256 MiB leaf cache, 1 M lookups | 294 MiB peak RSS |
| decoded dirty leaf | 24 KiB (sealed: 11.4 KiB); 100 000 leaves: 2.65 GiB RSS |
| encoded leaves, full volume | 3.5 GiB (358 MiB/TiB); flat table 7.5 GiB |
| map write amplification, scattered writes | 0.18-0.55 of dirty logical bytes |

### Crypto (P0.8)

One core, aws-lc-rs. Source: [crypto-kat](crypto-kat/RESULTS.md).

| ns per op (GB/s) | 4 KiB | 16 KiB | 64 KiB |
|---|---|---|---|
| AES-256-GCM seal, pre-keyed | 301 (13.6) | 838 (19.6) | 3442 (19.0) |
| full seal, ADR-11 as written | 652 (6.3) | 1201 (13.6) | 3766 (17.4) |
| full seal, proposed derivation and fresh salt | 920 (4.5) | 1471 (11.1) | 3975 (16.5) |
| full seal, FIPS profile | 1349 (3.0) | 1902 (8.6) | 4434 (14.8) |

Argon2id at 64 MiB, t = 3: 75-83 ms per wrap. 40 tests pass and 27
independent cross-checks agree. Six findings (F2, F5, F6, F7, F9, F10) are
pending the maintainer's decision.

### S3 (P0.10)

Concurrency 1, create-only or plain PUT, medians unless noted. Source:
[s3probe](s3probe/RESULTS.md).

| measure | AWS S3 Standard, same region (us-west-2) | OVH Milan, remote client | AWS, remote client |
|---|---|---|---|
| authority rule | met, 3 of 3 sweeps | met | met |
| 4 KiB PUT, p50 / p99 | 29.7 / 45 ms | 39.6 / 46-54 ms | 218 / 302 ms |
| 1 MiB PUT, p50 | 73 ms | 130-138 ms | 630 ms |
| 16 MiB PUT, p50 | 210 ms | 886-911 ms | 5.5 s |
| fit of p50 | 42 ms + 10.6 ms/MiB | 71 ms + 52.9 ms/MiB | 291 ms + 328 ms/MiB |
| two serial 4 KiB PUTs, p50 / p99 | 54 / 94 ms | 97 / 133-212 ms | 444 / 1059 ms |
| 1 MiB PUT then 4 KiB PUT, p50 / p99 | 81 / 133-189 ms | 192 / 362-378 ms | 695 / 2281 ms |
| `CompleteMultipartUpload`, 1 part, conditional | 87-97 ms | 76-78 ms | 285 ms |
| HEAD / LIST of 10 keys | 11.7 / 14.4 ms | 29.1 / 31.8 ms | |
| GET 4 KiB | 23.9 ms | 78.1 ms (slow stretch) | |
| 4 KiB PUT during 8 × 16 MiB uploads, p50 | 28.7-30.0 ms (idle 28.2) | 52.3-52.7 ms (idle 47.7) | |
| 16 MiB single PUT, 8 objects in flight | 535 MB/s | 28.5 MB/s | |
| 16 MiB, single PUT against 3 parts | 194 ms against 252 ms | 1424 ms against 1164 ms | |

The OVH and remote-AWS columns come from a workstation on a Wi-Fi link
(about 270 Mbit/s link rate), whose single-stream rate (19 MiB/s to
Milan) bounds the large-object rows. OVH latency was 2-2.6× worse in one
of three runs; the table uses the other two. MinIO on loopback (not
representative of any cloud): 4 KiB PUT 3.8-5.5 ms, GET 0.23 ms, 16 MiB
PUT 54 ms; it meets the authority rule on unversioned and versioned
buckets.

## DESIGN §20 targets

| workload | target | ceiling measured | status |
|---|---|---|---|
| 4K random read | ≥ 500K IOPS, p99 ≤ 150 µs | `ublk-null`: 978 K IOPS, p99 38.7 µs on one queue; 3.83 M with four queue threads | framework headroom 2× (one queue) to 7× (four); product not built; measured at 4 active queues, not 8 |
| 4K random write, async/local | ≥ 300K IOPS (journal-bound) | `ublk-null` 3.6 M; with BLAKE3 of every write 2.13 M; with LZ4 1.69 M. Journal prototype, one thread: 36.5 K rec/s on the real disk, flush-bound | framework passes; the one-thread journal is below target on this disk; not a product measurement |
| sequential write ingest | ≥ 3 GB/s, `auto` compression, 8 workers | hash plus LZ4 inline at 64 KiB: about 15.7 GB/s on four queue threads; AES-256-GCM 17-19 GB/s per core | not validated: zstd-1, workers and the pipeline are not in the spike |
| sequential read, hits | ≥ 5 GB/s | 64k random read touching every byte: 48.8 GB/s (copy), 81 GB/s (`USER_COPY`) | framework headroom; product not built |
| cold sequential read, same region | ≥ 1 GB/s | AWS, 16 MiB GETs: 96 MB/s one stream, 788 MB/s at concurrency 8 (few objects, rough) | not shown yet |
| sync-mode fsync latency | ≈ 2 PUT RTT (ext4/xfs), 1 RTT (fdatasync) | AWS Standard: two serial 4 KiB PUTs p50 54 ms / p99 94 ms; one PUT p50 29.7 ms. OVH: 97 ms | consistent with 2 RTT (1.8×); Express One Zone not run |

## Verdicts

| ADR | milestone | verdict | source |
|---|---|---|---|
| 18 | P0.1 | accepted (gate) | [ublk-baseline](ublk-baseline/RESULTS.md) |
| 17 | P0.2 | `USER_COPY` and `BATCH_IO` rules not met; copy default | [ublk-datapath](ublk-datapath/RESULTS.md) |
| 16, 17 | P0.3 | accepted (gate), amended | [ublk-recovery](ublk-recovery/RESULTS.md) |
| 17, 20 | P0.4 | grow works; daemon validates sizes | [ublk-recovery](ublk-recovery/RESULTS.md) |
| 4 | P0.5 | accepted (gate): group commit | [journal-bench](journal-bench/RESULTS.md) |
| 5, 14 | P0.6 | not run | [tracesim](tracesim/RESULTS.md) |
| 8 | P0.7 | amended (gate) | [radixmap](radixmap/RESULTS.md) |
| 11 | P0.8 | partial; amendment awaiting a decision | [crypto-kat](crypto-kat/RESULTS.md) |
| 19 | P0.9 | executed outside the spike plan | (none) |
| 2 | P0.10 | partial: AWS, OVH, MinIO met | [s3probe](s3probe/RESULTS.md) |
| 26 | P0.12 | accepted (gate) for the keeper | [ublk-recovery](ublk-recovery/RESULTS.md) |
| 28 | P0.10 | partial; phase 3 gate open | [s3probe](s3probe/RESULTS.md), [fsync-trace](fsync-trace/RESULTS.md) |

## Known limitations

- **A shared virtual machine.** Other work ran on the host. Noise is
  flagged in each file; none of the headline ublk cells is flagged noisy.
- **Short runs.** ublk cells ran 4-5 s (the plan says 20 s) on 4 queue
  threads and 4 fio CPUs. Reduced matrices: cells not listed in a
  `RESULTS.md` were not run. A p99 at QD ≥ 32 with 8 jobs shows a 3 ms
  scheduler artefact from two fio jobs per CPU.
- **Loop-backed file systems** (P0.5) add a layer: the loop images pay
  2-13 ms per flush, the real-disk directory about 3 µs. The loop rows say
  more about ordering than about absolute latency. One run per journal cell.
- **No trace subset.** P0.6 has no data at all.
- **Provider vantage point.** AWS was measured from a client in the same
  region and from a remote workstation; OVH only from the remote
  workstation, on a Wi-Fi link that bounds large objects. Cell counts are
  small (n = 10-12 for multipart, about 45 for the lane test), so only
  p50 is resolved there.
- **The kernel table** (first kernel release per feature) is copied from
  the fact-check note and was not re-verified.
- **Spike revisions are uncommitted** in every results file.

## Not run

- P0.6: chunk-size and cache-policy simulation (ADR-5, ADR-14). No trace
  could be obtained without a licence acceptance.
- P0.10: Cloudflare R2, GCS, Backblaze B2, Scaleway, Wasabi, Hetzner,
  Tigris, DigitalOcean Spaces, Ceph RGW, S3 Express One Zone (no
  credentials); PUT latency per size with 8 parallel streams; a shaped
  200 MB/s lane test.
- P0.12: the systemd FD-store variant; replay against a real remote sink;
  the generation-mismatch refusal as a separate run.
- P0.3: flipping a live device to fail-I/O; shrink under a mounted file
  system.
- P0.2: the full cross product of the matrices; `BATCH_IO` with
  `USER_COPY`; a worker pool that completes from any thread.
- P0.1: `null_blk` with `memory_backed=1`; libublk 0.5.0 (not published).
- P0.7: random-order build at 10 TiB; gates 2-4 at fanouts 256 and 1024.
- P0.8: the FIPS-validated build, a real KMS, parser fuzzing, AArch64.

## Rerun commands

Run on the test host only. The ublk spikes need root and follow the safety
rules in [CONVENTIONS.md](../docs/plans/v1/CONVENTIONS.md). Phase 1 and 8
CI reuse these.

| milestone | command |
|---|---|
| P0.1 | `cd spikes/ublk-baseline && cargo build --release && sudo RUNTIME=5 RAMP=1 REPS=3 ./run-baseline.sh` |
| P0.2 | `cd spikes/ublk-datapath && cargo build --release && sudo ./run-datapath.sh && python3 analyze.py <out>` |
| P0.3, P0.4 | `cd spikes/ublk-recovery && cargo build --release && sudo ./run-features.sh && sudo ./run-crash-loop.sh && sudo KILL_MODE=quiesce PHASES="handover:20:0" ./run-crash-loop.sh && sudo ./run-resize.sh && sudo ./run-flush-count.sh` |
| P0.3 negative controls | `sudo CONTROL=noreissue PHASES="immediate:3:0" ./run-crash-loop.sh` and `sudo CONTROL=fault PHASES="immediate:2:0" ./run-crash-loop.sh` |
| P0.12 | `cd spikes/ublk-recovery && sudo ./run-ramsink.sh && sudo ./run-lost-keeper.sh` (keeper: `keeper/keeper.py`) |
| P0.5 | `cd spikes/journal-bench && cargo build --release && sudo ./run-journal.sh && python3 analyze.py out/results.jsonl` |
| P0.6 | `cd spikes/tracesim && cargo build --release -j 4 && ./run.sh /var/tmp/lunix-traces out_dir` (needs traces) |
| P0.7 | `cd spikes/radixmap && cargo test --release && bash run.sh && python3 summarize.py out` |
| P0.8 | `cd spikes/crypto-kat && ./run.sh` |
| P0.10 | `cd spikes/s3probe && cargo build --release`, export `LUNIX_PROBE_<NAME>_*`, then `./target/release/s3probe --out out --providers AWS`; MinIO: `./run-minio.sh`; multipart: `s3probe mpu --out DIR --providers AWS` |
| fsync-trace | `cd spikes/fsync-trace && ./run.sh all "ext4 xfs btrfs"` |

P0.9 has no spike: the crates are in
<https://github.com/replicix/replicix-core> (commit `caa47b4`).
