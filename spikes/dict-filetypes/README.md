# dict-filetypes

Measurement study: do pre-trained zstd dictionaries for well-known file types improve compression of Constellation
chunk objects, at which granularity, and how well can the dictionary be chosen from the file name or the content?
Spike code, no product code. Results and the written analysis: `bench/dict-study/filetypes/SUMMARY.md`.

Reproduce (test host only; never run locally; about 8 h on 4 pinned cores, about 6 GB of disk):
`cd spikes/dict-filetypes && ./run_all.sh <out-dir>`

Pins: Python 3.14 stdlib `compression.zstd` (libzstd 1.5.7), zstd CLI 1.5.7 (`--train-cover` for training), brotli 1.2.0,
python3-numpy 2.3.5, python3-scipy 1.16.3, libmagic 5.46 (python3-magic 0.4.27), gcc for `tp.c`. Sources are
pinned by commit in `sources.lock.tsv` (written by the first run, copied to `bench/.../raw/`); seeds are fixed in the code.

Files: `projects.tsv` (git sources) · `fetch.py` (clone, licence tags, sampling) · `extras.py`, `gen_logs.py` (logs, jars,
wasm, ELF, derived sqlite and jsonl) · `taxonomy.py` (extension map, families) · `common.py` (folds, stored-size model) ·
`train.py` (dictionaries, 2 licence arms x 4 folds + full) · `measure.py` (every object x every variant) ·
`select_dict.py`, `analyze_sel.py` (extension map, libmagic, naive Bayes) · `sweep_eval.py` · `tp.c`, `throughput.py` ·
`analyze.py`, `chunk_sens_summary.py`, `decide.py` (pre-registered rules per type), `hybrid_eval.py`, `figs.py`, `md_tables.py` ·
`timing_quiet.sh` (timing cells on a quiet host) · `hostinfo.sh`.

Rules: no corpus data leaves the host; the host is described by category only; nothing here ships. Licence statements
are engineering assumptions for counsel to confirm.
