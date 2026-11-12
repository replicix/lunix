# zstd and dictionaries on media and archive formats

Date: 2026-11-12. Code: [`spikes/dict-media/`](../../../spikes/dict-media/).
Raw tables: [`raw/baseline.csv`](raw/baseline.csv),
[`raw/dictionaries.csv`](raw/dictionaries.csv), per-file results in
`raw/per_file.csv.gz`, and the corpus manifest (SHA-256, licence, source
cluster and URL per file) in `raw/corpus_manifest.tsv.gz`. Figures:
[ratio by format](fig-ratio-by-format.svg),
[dictionary gain](fig-dict-gain.svg).

## Abstract

On 3,875 media and archive files (1,414 MB measured), **zstd dictionaries
gain nothing worth having**. The held-out byte gain over all 3,349 files
that had a dictionary is **0.03 % (95 % CI -0.01 to 0.07 %) with a 16 KiB
dictionary** and 0.03 % (-0.02 to 0.09 %) with a 112 KiB one.
- **Where any gain exists:** only on files under 64 KiB, and only from
  container structure:
  - small office documents: 5-18 %, or 0.6-3.8 KB per file;
  - small PDFs: 4-19 %;
  - tiny JPEG and PNG thumbnails and icons: 5-6 %, or 30-150 B per file.
- **Where it turns negative:** above 1 MiB, dictionaries are neutral to
  slightly harmful.
- **Recommendation:** no dictionaries (codec id 3) for media. Keep the raw
  guard and the Lunix LZ4 probe as designed. Small office and PDF files
  belong in the text/XML file-type study.

## Question

1. How much does plain zstd gain per format and file size, and what
   share of objects ends up stored raw?
2. Does a per-format zstd dictionary gain anything, including on tiny
   files?

Format-aware lossless recompression (JPEG XL transcoding, Lepton,
preflate, packMP3) was **out of scope** by the maintainer's decision: the
products must store arbitrary, possibly invalid or byte-edited files, and
an edit must stay cheap per chunk. See Appendix B.

## Storage models

- **Constellation:** a file under 4 MiB is one object; a larger file is
  split into 4 MiB chunks. Each object is compressed with zstd and stored
  raw if the result is not smaller. Level 3 is assumed, since DESIGN.md
  names no default; level 7 was also run, since DESIGN.md uses `zstd:7` as
  its example.
- **Lunix:** 16 KiB and 64 KiB chunks. A 4 KiB LZ4 probe stores the chunk
  raw if it saves under 6 %; otherwise the chunk is compressed with zstd
  level 1.

## Corpora

| Group | Sources (licence) |
|---|---|
| JPEG | COCO/Flickr (CC), Wikimedia Commons thumbnails, Kodak set (derived) |
| PNG | Material icons (Apache-2.0), GNOME help screenshots (CC-BY-SA 4.0), Kodak, Commons thumbnails |
| WebP | libwebp test data (BSD-3), plus derived |
| Audio | LibriVox via the Internet Archive (public domain); AAC, Opus, FLAC and WAV derived with stock encoders |
| Video | Blender open movies (CC-BY), 8-second clips derived per codec |
| Archives and documents | Maven Central JARs, F-Droid APKs, Apache POI (Apache-2.0) and LibreOffice (MPL-2.0) test documents, Project Gutenberg EPUBs, pdf.js test PDFs (Apache-2.0), Ubuntu man pages and package members |

No corpus data is in the repository.

## Method

- **zstd:** python-zstandard 0.25.0 (libzstd 1.5.7), at levels 1, 3, 7
  and 19.
- **Dictionaries:** 16 KiB and 112 KiB, trained on the first 32 KiB plus
  the last 8 KiB of up to 600 training files per format.
- **Evaluation:** 5-fold cross-validation grouped by source, so the test
  files always come from sources not used for training. Test compression
  is at level 3 with the same raw guard.
- **Statistics:**
  - paired per-file Wilcoxon signed-rank tests, Holm-corrected within each
    comparison family: 507 comparisons, 300 significant at adjusted
    p < 0.05;
  - 95 % bootstrap intervals resampling source clusters, 1,000 draws;
  - gains reported both by bytes and by file count.
- **Caps:** at most 8 chunks (32 MiB) per file for the Constellation model,
  and the first 1 MiB per file for the Lunix model.

## Results

### Plain zstd (stored bytes / original bytes)

- **Already compressed** (0.96-1.00 at zstd-3): JPEG, PNG, WebP, GIF,
  AVIF, Vorbis, Opus, FLAC, all video, xz, zstd, gzip and 7z. MP3 is
  0.963, AAC 0.987 and MPEG-TS 0.976. Between 80 and 100 % of these
  objects hit Constellation's raw rule.
- **Compressible containers:** docx 0.91, xlsx 0.76, pptx 0.86, ODF 0.82,
  PDF 0.81, JAR 0.92 and APK 0.80. The gain comes from how the ZIP members
  were written, not from the media inside them.
- **All 3,875 files pooled:**
  - stored/original is 0.937, 0.924, 0.918 and 0.907 at levels 1, 3, 7
    and 19;
  - 44 % of objects and 30 % of bytes are stored raw.
- **Level 19 against level 3:** 1.84 % fewer bytes (CI 1.56-2.13 %). CPU
  was not measured.
- **The Lunix LZ4 probe on 16 KiB chunks:** 86 % of chunks go raw, against
  64 % under a plain "zstd-1 result ≥ original" rule. The probe forfeits
  2.6 % of the measured bytes against that rule: WAV 15.9 %, BMP 9.7 %, raw
  TIFF 8.8 %, pptx 4.4 %, and under 2 % on JPEG, PNG, audio and video.

### Dictionaries

| Scope | Held-out byte gain, 16 KiB dictionary | 112 KiB dictionary |
|---|---|---|
| All files with a dictionary (3,349) | 0.03 % (CI -0.01 to 0.07 %) | 0.03 % (CI -0.02 to 0.09 %) |
| Office documents under 64 KiB | — | 5-18 % (0.6-3.8 KB per file) |
| PDFs under 64 KiB | — | 4-19 % |
| JPEG and PNG thumbnails and icons | — | 5-6 % (30-150 B per file) |
| Files over 1 MiB | — | neutral to -0.2 % |
| BMP and TIFF (uncompressed) | — | -1.8 % |
| WAV | — | -0.7 to -1.1 % |

Counted by file instead of by byte, the pooled gain is about 5 %, driven
by office files and tiny images. Measured in bytes, it vanishes.

## Answers

1. **Dictionaries for media:** no. They gain nothing measurable on media
   bytes and lose slightly on large and uncompressed files.
2. **Small office and PDF files:** a real gain of 5-19 %, but it is XML
   and text structure. It belongs to the text/XML dictionaries of the
   file-type study (`../filetypes/`), applied to the contents of those
   containers where possible.
3. **The Lunix probe:** keep it. It forfeits 2.6 % of bytes against an
   always-try-zstd rule, mostly on uncompressed audio and images. Those
   are rare on real volumes, and the probe saves CPU on the far more
   common already-compressed data.
4. **Raw guard:** keep it in both products.

## Threats to validity

- **Throttled downloads:** Wikimedia rate-limited the host for a while.
  The corpus has only one real camera JPEG and no real GIF, TIFF, MP3,
  FLAC or WebM from Commons.
- **Derived formats:** AAC, Opus, FLAC, WAV, all video, BMP, TIFF, AVIF,
  GIF and ICO were derived with stock encoders, so they are controls, not
  field data. HEIC is missing because the encoder failed, and the GNU
  tarball download stalled after 8 files.
- **Office and PDF sources:** these are small test files, and pseudo-source
  clusters were used, so the office dictionary gains are probably
  optimistic.
- **Too few sources:** TIFF, the video formats, `tar.*` and xz had too few
  sources for a dictionary result.
- **CPU, throughput and memory** were not measured; the host was heavily
  loaded during the run.

## Appendix A: reproducibility

- **Host:** AMD EPYC (Zen 5), 16 vCPUs, SMT off, 1 NUMA node, about 30 GiB
  RAM, local NVMe SSD, ext4 root; kernel 7.0.0 (Ubuntu), Ubuntu 26.04.1
  LTS. All runs were pinned with `taskset -c 12-15`.
- **Tools:** Python 3.14.4, scipy 1.16.3, numpy 2.3.5, python-zstandard
  0.25.0 (libzstd 1.5.7), ImageMagick 7.1.2-18, ffmpeg 8.0.1.
- **Seeds:** `random.Random(1)`, fold = SHA-1(source) mod 5, numpy seed 1.
- **Commands:** `spikes/dict-media/run.sh` (fetch, classify, measure,
  analyze, figures); see its README.

## Appendix B: out of scope

preflate-rs 0.7.6, lepton_jpeg_rust 0.5.8, packJPG 2.5k and packMP3
(commit `e61c119`) were built on the host before the scope change. None was
run on corpus data, and the binaries were deleted.
