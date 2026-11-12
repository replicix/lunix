# dict-media: zstd and zstd dictionaries on already-compressed media and archives

Measures (1) plain zstd, (2) the raw-store share under Constellation's "compressed >= original -> raw"
rule and Lunix's 4 KiB LZ4 probe, (3) per-format zstd dictionaries with source-grouped held-out testing.
Format-aware recompression (JPEG XL, Lepton, preflate, packMP3) is out of scope by maintainer decision.
Results and write-up: `bench/dict-study/media/SUMMARY.md`, CSVs in `bench/dict-study/media/raw/`.

Files (everything runs on the remote test host, never locally; corpus data never enters the repo):
- `fetch.py <group>`: downloads one corpus group, writes `manifest-<group>.tsv` (sha256, licence, source id).
- `classify.py corpus`: merges manifests, maps files to formats, caps at 400 files / 300 MB per format.
- `measure.py corpus out.csv.gz`: stored bytes per file (zstd 1/3/7/19, Lunix 16/64 KiB, dictionaries).
- `analyze.py`, `figs.py`: summary CSVs (paired Wilcoxon, Holm, source-bootstrap) and SVGs.
- `run.sh`: the whole pipeline in order. `hostinfo.sh`: the only host description allowed in outputs.

Pins: python-zstandard 0.25.0 (libzstd 1.5.7), python3-lz4 (Ubuntu 26.04 package), scipy 1.16.3,
numpy 2.3.5, ImageMagick 7.1.2, ffmpeg 8.0.1, cwebp 1.5.0, heif-enc 1.21.2, avifenc 1.3.0, 7-Zip (Ubuntu).
Seeds: random.Random(1) in classify, fold = sha1(source) mod 5, bootstrap numpy seed 1.
Install: `sudo apt-get install -y python3-zstandard python3-lz4 python3-numpy python3-scipy imagemagick webp
ffmpeg libheif-examples libavif-bin 7zip`.
