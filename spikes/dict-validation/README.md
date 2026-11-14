# dict-validation

Executor code for the pre-registered study `bench/dict-study/validation/PREREGISTERED.md`. Test host only; never run locally.
STATUS: complete (measurement, timing and analysis 2026-11-13; Amendment A1 follow-up and re-analysis 2026-11-14); verdicts in SUMMARY.md.

Phases (one command each, from `~/dictval` on the test host, `taskset -c 12-15`, `python3 -I code/<script>`):
1. fetch      `fetch.py dl`                      download + checksum, writes sources.lock.tsv
2. images     `mkhost.py ext4|xfs|btrfs`         OS tree into a fresh fs image;  `qemu-img convert -O raw -S 4k` for VM images
3. chunk      `corpus.py prep <name> <img> [db <pg|my|sq>]`  16/64 KiB chunks, dedup, 4 KiB labels (`label.py`, `fiemap.c`), chunk classes, plain+probe pass
4. pool       `pool.py alpine|rocky|suse|arch`   block-set training samples (4 fs views each)
5. databases  `dbconv.py`, `dbload.py <ds> <pg|my|sq> <n> [fs]`  CSV conversion and engine loads (queue_b.sh)
6. train      `train.py bs|pv|pvcover ...`       COVER / fastCover dictionaries via the zstd CLI
7. measure    `corpus.py measure <name> <cs> <tag> <levels> name=dict...`  uses `zmeasure` (C, libzstd + liblz4, `cls.h` cheap classifier)
8. timing     `tmsample.py` (test host: 1 GiB samples), then on the quiet host `ztime2.c`, `zdtool seltime`, `timing_run.py`, `timing_agg.py` -> raw/timing*.csv (done; `ztime.c` is the superseded first version)
9. analysis   test host, `~/dictval/ana/`: `families.py` (Holm families, run first) -> `analyze.py ana/out 10000` (block bootstrap/BCa,
              Wilcoxon, Holm, cap, delta-cores, N; per-chunk inputs never leave the host) -> `decide.py ana/out ana/raw` (D1-D7 votes,
              markdown tables) -> `figures.py ana/out ana/raw ana/fig` (SVG). First analysis: `*_v1.py` (outputs raw/analysis-*).
              Amendment A1 (current): `families.py ana/a1/families.csv a1`, then `analyze.py ana/a1 10000`, `decide.py ana/a1 ana/raw`,
              `figures.py ana/a1 ana/raw ana/a1fig`, all as `nice -n 19 ionice -c3 taskset -c 14-15 python3 -I`; outputs raw/analysis-a1-*,
              raw/families-a1.csv; `decide.py raw raw` and `figures.py raw raw <dir>` reproduce tables and SVGs from raw/ alone.
Queues: `queue_a.sh` (loop-device steps in sequence), `queue_b.sh` (database images).
Build: `gcc -O2 -o zmeasure zmeasure.c -lzstd -llz4 -lpthread -lm`; `gcc -O2 -o fiemap fiemap.c`; `gcc -O2 -o ztime ztime.c -lzstd -llz4 -lm`.
Every loop device and mount is tracked in `work/tracked.txt` (lib.py) and released at exit.
