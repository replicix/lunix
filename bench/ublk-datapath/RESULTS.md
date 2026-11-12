# P0.2 results: copy vs USER_COPY vs BATCH_IO (ADR-17)

Spike: `spikes/ublk-datapath` (`ublk-datapath`, libublk `=0.4.8`, io-uring `=0.7.16`, blake3 `=1.8.7`, lz4_flex `=0.14.0`).
Reproduce: `cd spikes/ublk-datapath && cargo build --release && sudo ./run-datapath.sh && python3 analyze.py <out>`.
Date 2026-11-11; spike revision uncommitted. Host and tool header: see `../ublk-baseline/RESULTS.md` (same host, same session,
AMD EPYC Zen 5, 16 vCPUs, SMT off, kernel 7.0.0 Ubuntu, fio 3.41). Governor not exposed.

## What was run, and deviations

- Modes: `copy` (default per-tag buffers), `user-copy` (`UBLK_F_USER_COPY`, `pread`/`pwrite` on `/dev/ublkcN` from the
  queue thread), `batch` (`UBLK_F_BATCH_IO` through libublk 0.4.8's `UblkBatchQueue`; **no raw-ring fallback was needed**).
  `GET_FEATURES` reports BATCH_IO and USER_COPY on this kernel.
- Work: `none`, `hash` (BLAKE3 of every WRITE payload; READ filled with an LBA-derived pattern, so the server touches every
  byte), `hash+lz4` (adds LZ4 of every 64 KiB chunk of a write). Write payload comes from fio with
  `--buffer_compress_percentage=50`; the daemon measured LZ4 out/in = 0.507 (1.97:1) for `copy` and 0.507 for user-copy.
- CPU split 4 + 4 as in P0.1: queue threads on CPUs 0-3, fio on 4-7, 16 queues x depth 256 (4 active) for the main matrices,
  1 queue x depth 128 for the QD1 rows. `runtime=4, ramp=1, reps=3` (plan: 20/3).
- **Reduced matrices (not the plan's full cross product).** `none`: bs {4k,64k,1M} x rw {randread,randwrite,write} x
  {QD32,QD128} at nj8 + QD1 nj1. `hash`: bs {4k,64k} x same rw x {QD32 nj1, QD32 nj8, QD128 nj8} + QD1 nj1. `hash+lz4`:
  bs {4k,64k} x rw {randwrite,write} x {QD32 nj8, QD128 nj8} + QD1 nj1. Cells not in these lists are **not run** (time). The
  decision rules below only use cells that exist.
- `--workers 0` is the primary configuration; the headline cells were repeated with `--workers 4` (bounded OS-thread pool,
  completions returned to the queue thread through a mutexed vec + eventfd, the only commit model libublk 0.4.8 offers).
- Constraints found: libublk's batch transport refuses `USER_COPY`, zero-copy and auto-buf-reg (`UNSUPPORTED_BATCH_FLAGS`)
  although the kernel accepts the flag combination at `ADD_DEV` (P0.3 probe table), so **BATCH_IO + USER_COPY could not be
  combined**; and batch completions can only be committed from the queue thread (`UblkBatchQueue` is bound to the
  thread-local ring), so "any worker completes any tag" is **not exposed by libublk 0.4.8** (the kernel interface allows it;
  using it would need the raw `sys` bindings, not attempted).
- Memory (resident set at exit, `VmHWM`; buffer reservation `max_io_buf_bytes x depth x queues` = 1 MiB x 256 x 16 = 4 GiB of
  virtual space for copy and batch, none for user-copy): work=hash: copy 148 MB, user-copy 26 MB, batch 95 MB; work=none (1M
  writes touch every buffer): copy 1.07 GB, user-copy 23 MB, batch 1.07 GB.
- Daemon CPU per I/O is queue-thread CPU including the kernel copy that task_work runs in the daemon's context.

## Decision rule (ADR-17, fixed in advance; work=hash, workers=0)

* **USER_COPY becomes the default** if at 64k writes QD32/128 it gives >= 10 % more IOPS or >= 10 % less daemon CPU per I/O
  than copy, **and** 4k randwrite QD1 p50 <= copy + 2 us, **and** no cell regresses by more than 5 % IOPS.
* **BATCH_IO becomes the default** if at QD128 nj8 (4k and 64k) it gives >= 10 % more IOPS or >= 10 % less CPU per I/O,
  **and** QD1 p50 <= +2 us and p99 <= +10 %, **and** P0.3's recovery cycle passes with BATCH_IO.

# Tables

### work = none

| cell | copy IOPS (min-max) | user-copy IOPS (min-max) | batch IOPS (min-max) | copy lat p50/p99 us | user-copy lat p50/p99 us | batch lat p50/p99 us | copy CPU us/IO | user-copy CPU us/IO | batch CPU us/IO | user-copy/copy IOPS | batch/copy IOPS |
|---|---|---|---|---|---|---|---|---|---|---|---|
| 1M_randread_qd128_nj8 | 41740 (41738-41794) | 291127 (289461-291484) | 41936 (41929-41979) | 24510.5/24510.5 | 4685.8/4882.4 | 16711.7/70778.9 | 95.14 | 4.46 | 94.62 | 6.97 | 1.00 |
| 1M_randread_qd1_nj1 | 30839 (30763-31246) | 68495 (68298-68700) | 29954 (28931-30417) | 31.6/39.2 | 13.8/22.1 | 31.6/54.0 | 22.03 | 4.65 | 22.55 | 2.22 | 0.97 |
| 1M_randread_qd32_nj8 | 45412 (45263-45464) | 350466 (348708-353285) | 44578 (44578-44590) | 5668.9/5865.5 | 362.5/3391.5 | 5734.4/5799.9 | 88.02 | 4.47 | 89.58 | 7.72 | 0.98 |
| 1M_randwrite_qd128_nj8 | 15254 (15225-15417) | 36592 (36303-36655) | 15759 (15735-16418) | 66846.7/69730.3 | 28704.8/29229.1 | 64749.6/68681.7 | 261.24 | 6.33 | 239.59 | 2.40 | 1.03 |
| 1M_randwrite_qd1_nj1 | 11014 (11013-11038) | 17202 (17162-17257) | 9683 (9516-9690) | 43.3/52.5 | 12.4/20.4 | 51.5/134.1 | 35.50 | 4.92 | 43.54 | 1.56 | 0.88 |
| 1M_randwrite_qd32_nj8 | 16680 (16637-16711) | 40623 (40592-40699) | 28561 (23592-31367) | 15007.7/18743.3 | 6062.1/9109.5 | 7045.1/16580.6 | 239.75 | 5.90 | 89.90 | 2.44 | 1.71 |
| 1M_write_qd128_nj8 | 15334 (15328-15363) | 36357 (36309-36357) | 15698 (15597-15803) | 66846.7/70778.9 | 28966.9/29229.1 | 64749.6/68681.7 | 260.21 | 6.34 | 249.84 | 2.37 | 1.02 |
| 1M_write_qd1_nj1 | 11022 (10977-11026) | 17213 (17092-17227) | 10729 (10288-10776) | 43.3/52.5 | 12.4/20.4 | 45.3/55.6 | 35.60 | 4.93 | 38.11 | 1.56 | 0.97 |
| 1M_write_qd32_nj8 | 16699 (16509-16796) | 40317 (40216-40347) | 16587 (16487-23876) | 15138.8/18481.2 | 6062.1/9109.5 | 15269.9/18743.3 | 239.72 | 5.90 | 187.31 | 2.41 | 0.99 |
| 4k_randread_qd128_nj8 | 3873516 (3816542-3875721) | 3655943 (3454648-3738568) | 4019767 (4010284-4077507) | 132.1/3129.3 | 136.2/3162.1 | 126.5/3129.3 | 0.84 | 0.76 | 0.99 | 0.94 | 1.04 |
| 4k_randread_qd1_nj1 | 263802 (261089-265119) | 255659 (252710-256214) | 224953 (224267-227025) | 3.1/6.6 | 3.0/9.9 | 3.7/10.3 | 1.65 | 1.61 | 2.42 | 0.97 | 0.85 |
| 4k_randread_qd32_nj8 | 3891058 (3844445-3929915) | 3458068 (3450441-3634925) | 3982136 (3921450-4302760) | 33.0/3031.0 | 37.1/3031.0 | 31.9/3031.0 | 0.84 | 0.77 | 0.98 | 0.89 | 1.02 |
| 4k_randwrite_qd128_nj8 | 3008186 (2954026-3026575) | 2777287 (2748276-2856717) | 3219129 (3216331-3378018) | 171.0/3162.1 | 185.3/3194.9 | 158.7/3162.1 | 0.87 | 0.82 | 1.22 | 0.92 | 1.07 |
| 4k_randwrite_qd1_nj1 | 241493 (240954-243253) | 231539 (229573-238483) | 222244 (219032-227630) | 3.4/6.9 | 3.4/6.8 | 3.8/10.4 | 1.68 | 1.61 | 2.36 | 0.96 | 0.92 |
| 4k_randwrite_qd32_nj8 | 2963315 (2947006-2973967) | 2759253 (2756174-2760886) | 3205790 (3202552-3310099) | 42.8/3063.8 | 46.3/3063.8 | 39.2/3031.0 | 0.86 | 0.83 | 1.23 | 0.93 | 1.08 |
| 4k_write_qd128_nj8 | 3029856 (3014427-3031361) | 2749053 (2745866-2838375) | 3226473 (3222345-3376191) | 169.0/3162.1 | 187.4/3194.9 | 156.7/3162.1 | 0.87 | 0.83 | 1.22 | 0.91 | 1.06 |
| 4k_write_qd1_nj1 | 241620 (229031-243107) | 236429 (232493-243206) | 220410 (219777-221219) | 3.4/6.9 | 3.3/6.8 | 3.8/10.6 | 1.67 | 1.61 | 2.38 | 0.98 | 0.91 |
| 4k_write_qd32_nj8 | 2995193 (2990378-3002301) | 2751385 (2744314-2767506) | 3196171 (3193336-3204309) | 42.2/3063.8 | 46.3/3063.8 | 39.7/3031.0 | 0.86 | 0.83 | 1.25 | 0.92 | 1.07 |
| 64k_randread_qd128_nj8 | 945368 (945090-946644) | 2312924 (2309584-2346052) | 1004617 (998252-1008365) | 1073.2/1138.7 | 220.2/3227.6 | 708.6/5799.9 | 4.23 | 1.16 | 3.98 | 2.45 | 1.06 |
| 64k_randread_qd1_nj1 | 212645 (211283-212811) | 214055 (211826-231175) | 185398 (173017-187874) | 4.4/5.5 | 3.9/7.3 | 5.3/7.0 | 2.59 | 1.80 | 3.36 | 1.01 | 0.87 |
| 64k_randread_qd32_nj8 | 2547460 (2515449-2561805) | 2313457 (2299402-2418280) | 2734928 (2692760-2778494) | 98.8/132.1 | 54.5/3063.8 | 46.8/3063.8 | 1.57 | 1.14 | 1.46 | 0.91 | 1.07 |
| 64k_randwrite_qd128_nj8 | 543051 (540176-548092) | 690563 (682952-691195) | 634038 (581418-657767) | 946.2/3981.3 | 757.8/3850.2 | 815.1/4079.6 | 3.84 | 1.68 | 4.31 | 1.27 | 1.17 |
| 64k_randwrite_qd1_nj1 | 106922 (106236-107806) | 132884 (132796-133635) | 96877 (96712-96941) | 5.9/9.9 | 4.0/8.2 | 6.3/12.4 | 3.36 | 1.90 | 4.07 | 1.24 | 0.91 |
| 64k_randwrite_qd32_nj8 | 551760 (536042-854967) | 856129 (849834-856465) | 860563 (859914-871674) | 226.3/3227.6 | 146.4/3162.1 | 144.4/3162.1 | 3.64 | 1.63 | 3.39 | 1.55 | 1.56 |
| 64k_write_qd128_nj8 | 542080 (529225-545436) | 686854 (685734-696572) | 486945 (480741-593107) | 946.2/3981.3 | 749.6/3883.0 | 1056.8/4079.6 | 3.86 | 1.67 | 5.14 | 1.27 | 0.90 |
| 64k_write_qd1_nj1 | 107098 (106520-107539) | 134351 (133861-134381) | 96921 (96675-99652) | 5.9/9.9 | 4.0/8.0 | 6.3/12.2 | 3.33 | 1.88 | 4.06 | 1.25 | 0.90 |
| 64k_write_qd32_nj8 | 552184 (540014-553872) | 852765 (845960-852909) | 861546 (859476-864161) | 226.3/3227.6 | 146.4/3162.1 | 144.4/3162.1 | 3.73 | 1.64 | 3.40 | 1.54 | 1.56 |

### work = hash

| cell | copy IOPS (min-max) | user-copy IOPS (min-max) | batch IOPS (min-max) | copy lat p50/p99 us | user-copy lat p50/p99 us | batch lat p50/p99 us | copy CPU us/IO | user-copy CPU us/IO | batch CPU us/IO | user-copy/copy IOPS | batch/copy IOPS |
|---|---|---|---|---|---|---|---|---|---|---|---|
| 4k_randread_qd128_nj8 | 3710358 (3704254-3729337) | 4082361 (4050378-4545780) | 4053126 (3980923-4184834) | 138.2/3129.3 | 124.4/3129.3 | 127.5/3129.3 | 0.94 | 0.94 | 0.98 | 1.10 | 1.09 |
| 4k_randread_qd1_nj1 | 250680 (249679-252092) | 237895 (230408-253368) | 225221 (224033-232823) | 3.2/10.0 | 3.4/7.6 | 3.5/10.6 | 1.79 | 1.86 | 2.47 | 0.95 | 0.90 |
| 4k_randread_qd32_nj1 | 946803 (945210-950214) | 988960 (985496-1028736) | 1011582 (1000635-1106686) | 33.5/40.2 | 31.9/38.1 | 31.4/37.6 | 0.92 | 0.98 | 0.96 | 1.04 | 1.07 |
| 4k_randread_qd32_nj8 | 3710838 (3709959-3716428) | 4073655 (4059939-4226954) | 3982336 (3864773-3982608) | 34.0/3031.0 | 31.1/3031.0 | 31.9/3031.0 | 0.94 | 0.96 | 1.01 | 1.10 | 1.07 |
| 4k_randwrite_qd128_nj8 | 2134923 (2134831-2138207) | 1952631 (1942929-1961627) | 2288283 (2273647-2316865) | 510.0/561.2 | 544.8/561.2 | 456.7/585.7 | 1.87 | 2.05 | 1.74 | 0.91 | 1.07 |
| 4k_randwrite_qd1_nj1 | 188389 (187865-191670) | 173599 (173356-174262) | 176894 (176551-178601) | 4.9/8.0 | 5.0/7.8 | 5.2/8.4 | 3.15 | 3.22 | 3.73 | 0.92 | 0.94 |
| 4k_randwrite_qd32_nj1 | 553968 (553623-554079) | 491265 (490541-491355) | 566909 (559365-567967) | 60.2/68.1 | 65.3/73.2 | 57.6/66.0 | 1.80 | 2.03 | 1.75 | 0.89 | 1.02 |
| 4k_randwrite_qd32_nj8 | 2177903 (2177543-2179905) | 1924316 (1919785-1940181) | 2269251 (2267371-2295472) | 123.4/136.2 | 136.2/146.4 | 117.2/129.5 | 1.84 | 2.07 | 1.75 | 0.88 | 1.04 |
| 4k_write_qd128_nj8 | 2136347 (2134254-2137243) | 1948657 (1948130-1959029) | 2272363 (2271722-2274376) | 510.0/561.2 | 544.8/561.2 | 460.8/585.7 | 1.87 | 2.05 | 1.76 | 0.91 | 1.06 |
| 4k_write_qd1_nj1 | 191642 (188242-192127) | 181375 (174677-182098) | 177362 (176521-179319) | 4.8/7.9 | 5.0/8.0 | 5.2/8.4 | 3.13 | 3.21 | 3.75 | 0.95 | 0.93 |
| 4k_write_qd32_nj1 | 552992 (551403-553654) | 486479 (485065-491756) | 555957 (555782-556326) | 60.2/69.1 | 66.0/74.2 | 57.6/68.1 | 1.81 | 2.04 | 1.75 | 0.88 | 1.01 |
| 4k_write_qd32_nj8 | 2177780 (2175389-2178685) | 1922812 (1921169-1945928) | 2271001 (2266242-2302560) | 123.4/136.2 | 136.2/146.4 | 117.2/129.5 | 1.84 | 2.07 | 1.75 | 0.88 | 1.04 |
| 64k_randread_qd128_nj8 | 279189 (278885-279480) | 710654 (706741-711588) | 249779 (249756-250496) | 3424.3/4816.9 | 1613.8/1744.9 | 2506.8/10420.2 | 14.32 | 5.63 | 15.99 | 2.55 | 0.89 |
| 64k_randread_qd1_nj1 | 146951 (146819-149636) | 133605 (133453-140213) | 142935 (142583-145432) | 6.5/10.3 | 6.6/11.6 | 6.6/10.6 | 4.44 | 4.56 | 4.87 | 0.91 | 0.97 |
| 64k_randread_qd32_nj1 | 329111 (325878-329500) | 303714 (303461-305153) | 344438 (343867-351223) | 96.8/112.1 | 110.1/120.3 | 89.6/136.2 | 3.05 | 3.28 | 2.86 | 0.92 | 1.05 |
| 64k_randread_qd32_nj8 | 743967 (739913-780096) | 1236127 (1233237-1243903) | 370866 (370618-371037) | 309.2/522.2 | 220.2/238.6 | 651.3/1003.5 | 5.30 | 3.23 | 10.78 | 1.66 | 0.50 |
| 64k_randwrite_qd128_nj8 | 234701 (234591-235550) | 333702 (329742-336581) | 228767 (227071-229126) | 4554.8/5668.9 | 3293.2/3555.3 | 4489.2/7569.4 | 17.02 | 11.99 | 17.51 | 1.42 | 0.97 |
| 64k_randwrite_qd1_nj1 | 64027 (64017-64064) | 58280 (58044-62498) | 61201 (61071-61245) | 12.0/18.3 | 12.4/22.4 | 12.5/18.6 | 9.71 | 9.71 | 10.33 | 0.91 | 0.96 |
| 64k_randwrite_qd32_nj1 | 120556 (115881-122058) | 123724 (121523-123894) | 120145 (118229-120854) | 280.6/333.8 | 257.0/268.3 | 280.6/333.8 | 8.21 | 8.11 | 8.25 | 1.03 | 1.00 |
| 64k_randwrite_qd32_nj8 | 429666 (427930-430579) | 478488 (478450-482053) | 311201 (309208-311890) | 602.1/823.3 | 536.6/553.0 | 815.1/1663.0 | 9.30 | 8.34 | 12.87 | 1.11 | 0.72 |
| 64k_write_qd128_nj8 | 235317 (234172-235582) | 335822 (334608-336041) | 228738 (228172-229058) | 4554.8/5668.9 | 3293.2/3489.8 | 4489.2/7569.4 | 17.02 | 11.92 | 17.49 | 1.43 | 0.97 |
| 64k_write_qd1_nj1 | 64294 (63855-64318) | 61049 (58187-62950) | 62583 (62503-62699) | 12.0/18.3 | 12.4/18.6 | 12.4/18.8 | 9.68 | 9.69 | 10.29 | 0.95 | 0.97 |
| 64k_write_qd32_nj1 | 120615 (114408-121004) | 123068 (120672-123736) | 120535 (120506-121089) | 280.6/333.8 | 255.0/501.8 | 280.6/333.8 | 8.23 | 8.04 | 8.21 | 1.02 | 1.00 |
| 64k_write_qd32_nj8 | 427558 (427300-428351) | 479391 (478362-480021) | 312087 (310173-314351) | 602.1/815.1 | 536.6/553.0 | 806.9/1663.0 | 9.35 | 8.34 | 12.81 | 1.12 | 0.73 |

### work = hash+lz4

| cell | copy IOPS (min-max) | user-copy IOPS (min-max) | batch IOPS (min-max) | copy lat p50/p99 us | user-copy lat p50/p99 us | batch lat p50/p99 us | copy CPU us/IO | user-copy CPU us/IO | batch CPU us/IO | user-copy/copy IOPS | batch/copy IOPS |
|---|---|---|---|---|---|---|---|---|---|---|---|
| 4k_randwrite_qd128_nj8 | 1694244 (1693166-1697242) | 1584724 (1584406-1601249) | 1789854 (1789688-1791217) | 651.3/725.0 | 675.8/692.2 | 585.7/733.2 | 2.36 | 2.51 | 2.23 | 0.94 | 1.06 |
| 4k_randwrite_qd1_nj1 | 174088 (173912-174954) | 161835 (161460-162362) | 161975 (161044-162968) | 5.3/8.6 | 5.5/9.0 | 5.7/9.0 | 3.65 | 3.79 | 4.28 | 0.93 | 0.93 |
| 4k_randwrite_qd32_nj8 | 1720418 (1719616-1723056) | 1565980 (1564377-1566978) | 1774073 (1770802-1774658) | 158.7/175.1 | 171.0/179.2 | 152.6/166.9 | 2.32 | 2.55 | 2.25 | 0.91 | 1.03 |
| 4k_write_qd128_nj8 | 1689124 (1684494-1696282) | 1583307 (1583248-1586625) | 1788784 (1787982-1789579) | 651.3/725.0 | 675.8/692.2 | 585.7/733.2 | 2.37 | 2.52 | 2.24 | 0.94 | 1.06 |
| 4k_write_qd1_nj1 | 170548 (169619-170675) | 167033 (163494-169586) | 161979 (161403-162536) | 5.5/8.8 | 5.5/8.9 | 5.7/9.0 | 3.69 | 3.77 | 4.28 | 0.98 | 0.95 |
| 4k_write_qd32_nj8 | 1720690 (1718988-1723578) | 1566390 (1565351-1575461) | 1777278 (1776579-1778001) | 158.7/177.2 | 171.0/179.2 | 152.6/166.9 | 2.32 | 2.55 | 2.25 | 0.91 | 1.03 |
| 64k_randwrite_qd128_nj8 | 174931 (173142-175870) | 208862 (208698-209509) | 171268 (170729-171526) | 6455.3/8028.2 | 5079.0/5406.7 | 6127.6/9764.9 | 22.90 | 19.14 | 23.37 | 1.19 | 0.98 |
| 64k_randwrite_qd1_nj1 | 42919 (42778-42951) | 42348 (42277-42413) | 40754 (40353-41696) | 19.6/27.0 | 19.8/26.0 | 19.8/28.8 | 17.17 | 17.25 | 17.83 | 0.99 | 0.95 |
| 64k_randwrite_qd32_nj8 | 241192 (241010-241284) | 254612 (253440-254953) | 194874 (194734-196400) | 1089.5/1482.8 | 1011.7/1036.3 | 1433.6/2072.6 | 16.54 | 15.72 | 20.47 | 1.06 | 0.81 |
| 64k_write_qd128_nj8 | 173068 (171672-173882) | 208608 (208323-208616) | 171147 (170899-171215) | 5734.4/8716.3 | 5079.0/5406.7 | 6127.6/9764.9 | 23.14 | 19.18 | 23.38 | 1.21 | 0.99 |
| 64k_write_qd1_nj1 | 42735 (42673-42781) | 42470 (42412-42537) | 40445 (40268-41320) | 19.6/26.8 | 19.6/26.2 | 19.8/28.5 | 17.22 | 17.20 | 17.73 | 0.99 | 0.95 |
| 64k_write_qd32_nj8 | 239895 (239823-239974) | 253737 (253344-254520) | 195441 (194892-195450) | 1105.9/1499.1 | 1019.9/1036.3 | 1433.6/2039.8 | 16.67 | 15.75 | 20.47 | 1.06 | 0.81 |


## Decision-rule inputs (work=hash, workers=0)

#### user-copy vs copy

| 64k write cell | IOPS ratio | daemon CPU/IO ratio |
|---|---|---|
| 64k_write_qd32_nj8 | 1.12 | 0.89 |
| 64k_write_qd128_nj8 | 1.43 | 0.70 |
| 64k_randwrite_qd32_nj8 | 1.11 | 0.90 |
| 64k_randwrite_qd128_nj8 | 1.42 | 0.70 |

- 64k write clause (>=10% more IOPS or >=10% less CPU/IO at QD32/128): **met**
- 4k randwrite QD1 p50 delta: +0.10 us (allowed <= +2 us); p99 ratio 0.97 (batch rule allows <= 1.10)
- worst IOPS ratio over all hash cells: 0.88 at 4k_write_qd32_nj1 (user-copy rule: no cell below 0.95)

#### batch vs copy

| 64k write cell | IOPS ratio | daemon CPU/IO ratio |
|---|---|---|
| 64k_write_qd32_nj8 | 0.73 | 1.37 |
| 64k_write_qd128_nj8 | 0.97 | 1.03 |
| 64k_randwrite_qd32_nj8 | 0.72 | 1.38 |
| 64k_randwrite_qd128_nj8 | 0.97 | 1.03 |

- 64k write clause (>=10% more IOPS or >=10% less CPU/IO at QD32/128): **not met**
- 4k randwrite QD1 p50 delta: +0.30 us (allowed <= +2 us); p99 ratio 1.05 (batch rule allows <= 1.10)
- worst IOPS ratio over all hash cells: 0.50 at 64k_randread_qd32_nj8 (user-copy rule: no cell below 0.95)

#### batch vs user-copy

| 64k write cell | IOPS ratio | daemon CPU/IO ratio |
|---|---|---|
| 64k_write_qd32_nj8 | 0.65 | 1.54 |
| 64k_write_qd128_nj8 | 0.68 | 1.47 |
| 64k_randwrite_qd32_nj8 | 0.65 | 1.54 |
| 64k_randwrite_qd128_nj8 | 0.69 | 1.46 |

- 64k write clause (>=10% more IOPS or >=10% less CPU/IO at QD32/128): **not met**
- 4k randwrite QD1 p50 delta: +0.20 us (allowed <= +2 us); p99 ratio 1.08 (batch rule allows <= 1.10)
- worst IOPS ratio over all hash cells: 0.30 at 64k_randread_qd32_nj8 (user-copy rule: no cell below 0.95)


## Headline cells with --workers 0 / 4 (work=hash)

| mode | workers | cell | IOPS | lat p50/p99 us | CPU us/IO |
|---|---|---|---|---|---|
| copy | 0 | 64k_write_qd32_nj8 | 427257 | 602.1/823.3 | 9.36 |
| copy | 0 | 4k_randwrite_qd1_nj1 | 184307 | 4.9/8.0 | 3.11 |
| copy | 4 | 64k_write_qd32_nj8 | 351220 | 733.2/2900.0 | 11.29 |
| copy | 4 | 4k_randwrite_qd1_nj1 | 107101 | 8.8/15.6 | 7.94 |
| user-copy | 0 | 64k_write_qd32_nj8 | 481391 | 536.6/553.0 | 8.31 |
| user-copy | 0 | 4k_randwrite_qd1_nj1 | 177643 | 5.0/8.0 | 3.24 |
| user-copy | 4 | 64k_write_qd32_nj8 | 350192 | 725.0/749.6 | 11.42 |
| user-copy | 4 | 4k_randwrite_qd1_nj1 | 69915 | 9.9/25.7 | 10.94 |
| batch | 0 | 64k_write_qd32_nj8 | 317842 | 782.3/1679.4 | 12.59 |
| batch | 0 | 4k_randwrite_qd1_nj1 | 179167 | 5.2/8.4 | 3.70 |
| batch | 4 | 64k_write_qd32_nj8 | 236146 | 1171.5/2572.3 | 16.28 |
| batch | 4 | 4k_randwrite_qd1_nj1 | 100884 | 9.3/15.3 | 9.03 |

## Outcome

**USER_COPY: rule not met, so copy stays the default and USER_COPY stays unused.** The first two clauses are met (64k
writes: +11 % to +43 % IOPS and 10-30 % less CPU per I/O; QD1 4k randwrite p50 +0.1 us), but the third fails: 4k writes
lose 8-12 % IOPS (4k_randwrite_qd32_nj8 0.88, 4k_write_qd32_nj1 0.88, 4k_randwrite_qd128_nj8 0.91) and several QD1 4k cells lose 5-10 %,
because every request pays an extra `pread`/`pwrite` syscall from the queue thread. The data show a clear crossover:
USER_COPY is a large win for reads and writes of >= 64k (64k randread QD128 2.55x, QD32 1.66x; CPU per I/O 0.4-0.7x) and a
loss for 4k. It also removes the per-tag buffers (resident set 26 MB vs 148 MB). This is a **finding for phase 1**: if the
copy were issued through io_uring (batched with the backend SQEs) instead of blocking syscalls, the 4k loss may disappear;
that variant was not tested. As recorded the rule says: copy default.

**BATCH_IO: rule not met; BATCH_IO is "used when present, opt-in only".** With libublk 0.4.8's batch transport the 64k write
cells give 0.73x (QD32) and 0.97x (QD128) of copy's IOPS at 1.03-1.37x the CPU per I/O; 64k randread QD32 is 0.50x; 4k cells
are within -10 % to +9 %; QD1 p50 +0.3 us, p99 +5 %. No cell shows a gain of 10 %, so the recovery re-run in P0.3 was run for
information only. Caveat: the result is for this libublk implementation (small fetch buffers of 64 tags, 2 fetch commands, queue-thread-only commit), not the
kernel interface's limit.

**ADR-18 thread-model note:** worker-pool completion is possible only by returning completions to the queue thread (eventfd
+ queue). `--workers 4` was slower than `--workers 0` in every mode at both headline cells (64k write QD32 nj8: copy
351 K vs 427 K IOPS; 4k randwrite QD1: p50 8.8 us vs 4.9 us), because each cross-thread hop costs a wakeup; with 4 daemon CPUs
shared between queue threads and workers the pool also competes for CPU. Hash + LZ4 at 64k writes is 4-10 us/IO of CPU,
so a real pool is needed at high write rates, but its hand-off cost (about +4 us p50 at QD1) must be avoided for small
synchronous requests (do them inline, as ADR-18 already says).

