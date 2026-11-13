# Validation study: zstd dictionaries on raw devices, per-volume training and database pages

Status: pre-registered protocol, written 2026-11-12 before any measurement. Decision rules are not changed after data. Deviations
go into `SUMMARY.md` under "Deviations", never into this file. Builds on `../filetypes/` (GO, about 14 % under 64 KiB on text
and code) and `../media/` (NO-GO); it reuses their conventions (manifest and lock-file formats, bootstrap over blocks, Holm within a
family, "stored bytes" as the unit, raw summaries under `raw/`, hosts described by category only).

**Relation to the phase 1 plan.** This study takes over the core questions of milestone M18b (`docs/plans/v1/wip/01-p1-local-volume.md`:
arm 1 H1-H3, arm 2 C1-C3, the adoption thresholds and the built-in set). What M18b keeps after this study: the Lunix validation
cell (offline population = stored population, after M12a), and the ZFS, F2FS and container-store rows, which this study does not
run. M18a (`replicix-zdict`, codec 3) is unchanged. This study does not edit the plan or the ADRs; it reports proposals, and the lead
reconciles M18b's text.

Products, as verified in source: **Lunix** stores 16 or 64 KiB LBA-aligned chunks, all-zero chunks are holes, chunks are deduplicated by
plaintext hash, a 4 KiB LZ4 probe sends a chunk raw if it saves under 6 %, else zstd-1 (ADR-5, ADR-12). ADR-30 reserves codec 3 (zstd
with dictionary; `dictID` with the top bit set = built-in) and plans `lunix compress estimate|train|apply`. **Constellation** stores a
file under its chunk size (4 MiB default, configurable 1-64 MiB) as one object, larger files as chunks, each compressed alone with
zstd:3 and stored raw if not smaller; 16 B header. **zdicts** ships 33 file-type dictionaries (110 KiB, ids `0x8000_0001-0x8000_0021`)
with a naive Bayes classifier over the first 4 KiB and the 64 KiB size rule.

## 1. Decisions

### 1.1 Common definitions

- **Stored bytes** of a chunk under an arm: the compressed payload if smaller than the chunk, else the chunk length (raw guard, and in
  Lunix the probe decision). Header overhead is constant per chunk and is left out; holes count 0. **Saving** of arm A against
  comparator B over a population: `S = 1 - stored(A) / stored(B)`, summed over the population (bytes-weighted). Every Lunix figure is
  **volume-wide**: all unique non-hole chunks of the image, raw and probe-raw chunks included; the hole share, the duplicate share and
  the probe-raw share are printed next to it. Compressible-only figures are secondary.
- **LB**: the lower end of the 95 % block-bootstrap interval of S (§4).
- **CPU cost**: single-core, warm, in-memory CPU-seconds per GB of input on the quiet host (compress, including the classifier or probe
  the arm needs; decompress separately). `ΔCPU = CPU(A) - CPU(B)` on the same chunks.
- **Ingest targets**: Lunix 5 GB/s per node (research note 2026-11-11 §6.5); Constellation has no documented figure, **assumed 1 GB/s
  per node** (recorded as an assumption). `ΔCores = max(0, ΔCPU) × target`.
- **Net saving**: `N = LB - λ · ΔCores`, with **λ = 0.1 pp per extra core at the ingest target**. Justification: a core is 1/16 of the
  study's node class, and λ is deliberately conservative because the compress stage is the ingest bottleneck (4-12 cores of 16
  already). No storage or compute prices enter the rule: they depend on the backend and many other factors (amended 2026-11-13,
  wording only; λ and every threshold are unchanged). A dictionary that costs
  one extra core per node loses 0.1 pp; the 34 % slowdown the file-type study saw on 16-64 KiB objects at zstd:3 (1.26 CPU-s/GB) would
  cost 6.3 cores at 5 GB/s and 0.63 pp. **Hard cap, independent of λ:** an arm whose compress or decompress throughput on its
  population is below 0.8× the comparator's is a NO-GO for that decision regardless of N (the read path is latency, not a budget line).
- **Counterfactual.** The comparator for every dictionary decision is the **best plain level that passes the hard cap** against the
  product's default (Lunix: zstd-1 behind the probe; zstd-3 behind the probe if it passes the cap; Constellation: zstd:3). Raising a
  level is free of code; a dictionary must beat it, not the default.
- **Verdicts are go or no-go.** "Undetermined" is allowed only when §4.4's sample-size requirement was not met, and the summary then
  says what data would decide it.

### 1.2 Decision table

| id | product | population and view | measurement (arm vs comparator, metric) | fires (GO) if | NO-GO if |
|---|---|---|---|---|---|
| **D1** | Lunix writes codec 3 with built-in dictionaries | 6 raw-device corpora (§2.1-2.2), Lunix view, per chunk size (16, 64 KiB) | best *shipped* set (file-type set via classifier, or block set BS-dist), at zstd-1 and zstd-3, vs counterfactual; volume-wide `N` | `N ≥ 3 pp` and Holm p < 0.05 on **≥ 4 of 6** corpora at that chunk size; verdict per chunk size | fewer than 4 corpora, or the **oracle** best built-in has `LB < 3 pp` on ≥ 4 corpora (then no set can ever pass: decisive no) |
| **D2** | Lunix builds per-volume `train`/`apply` (ADR-30) | the 6 raw-device corpora and the 15 database images (§2.3), Lunix view, 16 KiB primary, 64 KiB confirmatory | per-volume dictionary (PV, 1 % sample, fastCover) vs **the best shipped set on that volume** (file-type, BS-dist or DB set); volume-wide `N` | `N ≥ 2 pp` on **≥ 4 of 6** raw-device corpora **or** on **≥ 8 of 15** database images (then `train` is recommended for that family only) | neither vote reached; `estimate` with built-ins may still ship (D1), `train`/`apply` leave v1 |
| **D2c** | `--classes` (per-class volume dictionaries) | same, only if D2 fires | per-class PV (3 classes, ADR-30 classifier) vs single PV; volume-wide `N` | `N ≥ 1 pp` on ≥ half of the volumes that made D2 fire | otherwise ship single-dictionary `train` only |
| **D3** | zdicts adds a block or file-system set | the 6 raw-device corpora, Lunix view | BS-dist (held out by distribution family) vs file-type set via classifier; volume-wide `N`; also BS-xfs (held out by distribution **and** file system) | D1 fires at some chunk size **and** (D1 fires only with BS-dist, or BS-dist beats the file-type set by `N ≥ 2 pp` on ≥ 4 of 6); BS-xfs keeps ≥ half of BS-dist's saving | D1 does not fire, or the file-type set alone carries D1 within 2 pp of BS-dist |
| **D4** | zdicts adds database-page dictionaries | database images (D4-L: Lunix view, 16 KiB, DB-page class) and database files (D4-C: Constellation view at 1 MiB, §2.3) | DB set trained on other datasets, **schema-disjoint** (§4.3), vs counterfactual; `N` on the DB-page class (D4-L) and on database-file bytes (D4-C) | `N ≥ 5 pp` on ≥ 2 of 3 engines, median over test datasets, **and** the consumer fires (D1 for D4-L, D6 for D4-C) | fails on 2 engines, or no consumer; the same-schema arm is reported but never decides |
| **D5** | Constellation adopts the zdicts file-type set | files of the 3 VM images and of the OS tree (§2.5: 4 corpora), Constellation view, 4 MiB chunks | `select(name, head, len)` dictionary at zstd:3 vs zstd:3; `N` on objects under 64 KiB **and** corpus-wide (all objects, raw guard) | under-64 KiB `N ≥ 5 pp` and corpus-wide `N ≥ 1 pp` on ≥ 3 of the 4 file corpora (§2.5); selector loss vs oracle ≤ 1.5 pp (R5 of the file-type study) | either floor missed on 2 corpora; R5 failure means "fix the selector first", reported as NO-GO for this release |
| **D6** | Constellation uses database-page dictionaries | database files, Constellation view at **1 MiB** (the configurable floor) and 4 MiB; page-sized objects as a **reference only** | DB set (schema-disjoint) at zstd:3 vs zstd:3; `N` on database-file bytes | `N ≥ 3 pp` at 1 MiB on ≥ 2 of 3 engines, median over datasets | below 3 pp at 1 MiB. If page-sized objects show `LB ≥ 15 pp`, record "chunk-size floor is the obstacle" as an open question for Constellation, not a decision |
| **D7** | no dictionary work for that product | derived | D7-Lunix: neither D1 nor D2 fires (codec 3 stays reserved; ADR-30's commands leave v1). D7-Constellation: neither D5 nor D6. D7-zdicts: neither D3 nor D4 (the file-type set stays as is) | | |

### 1.3 Why these numbers

- **3 pp for D1.** ADR-30 lets a *volume* adopt a dictionary at 1 % volume-wide, but a built-in set is a different cost: a decoder in
  the read path for ever, a per-chunk classifier at ingest, 3.5 MiB of binary plus about 22 MB of prepared state at zstd:3 (file-type
  study §6.7), permanent ids, and a second codec in fsck, GC and mirrors. Moving from zstd-1 to zstd-3 typically buys 10-15 % at
  about 20 % CPU and no code; a dictionary that adds under 3 pp on top of the best free level is not worth the surface. Votes of 4 of
  6 require the gain on both image families (VM images and the OS-tree images, three each), so one family cannot carry it.
- **2 pp for D2 and D3.** The granularity step of the file-type study (R2). `train`/`apply` is the largest item in ADR-30 (sampling
  with ranged GETs, background training in `lunixd`, sealed `dicts/` objects, a GC liveness rule, `--rewrite`, mirror ordering, an
  operator command); a new zdicts set is permanent ids, a block classifier and an image-training pipeline. Below 2 pp over what ships
  already, the maintainer's position holds: use zdicts out of the box.
- **5 pp for D4.** ADR-30's own per-class threshold. Database pages are one class; a dictionary that cannot clear it on the class it
  was built for has no case.
- **5 pp under 64 KiB and 1 pp corpus-wide for D5.** The file-type study's R1 floor was 8 % on 1-64 KiB *text*; a real file system's
  files are mixed (ELF, locales, docs, caches), so the small-object floor is lower, and the 1 pp corpus-wide floor (ADR-30's
  volume-wide rule) guarantees the gain is visible on the bill, not only on a subset.
- **3 pp for D6.** Constellation would need a database detector and a per-path chunk-size policy; the gain must show at a size the
  product can configure today (1 MiB), not at a page size it cannot.
- **Expected results, stated now so they can be falsified:** dictionaries lose on objects of 256 KiB and more (file-type study §6.6,
  media study), so D6 at 1 MiB and 4 MiB is expected NO-GO and D4-C with it; D4-L and D1 at 16 KiB are the open questions; D2 is
  expected to fail against a block set on OS images and is open on database images.

## 2. Corpora

Everything is fetched on the test host from official sources with checksums verified against the publisher's file; the executor
records URL, file name, size and SHA-256 in `raw/sources.lock.tsv` (file-type study format). No corpus data, image, file list or chunk
enters the repository. All images are sparse files under the study's scratch directory.

### 2.1 Public VM images (test corpora: raw devices)

| corpus | image | obtain | notes |
|---|---|---|---|
| `vm-ubuntu` | Ubuntu 26.04 LTS server cloud image, amd64, `*-server-cloudimg-amd64.img` (qcow2), release serial recorded | `cloud-images.ubuntu.com/releases/26.04/release-<serial>/`, `SHA256SUMS` + `SHA256SUMS.gpg` | ext4 root, GPT with ESP and BIOS-boot partitions |
| `vm-debian` | Debian 13 (trixie) `debian-13-genericcloud-amd64.qcow2`, latest build at execution | `cloud.debian.org/images/cloud/trixie/latest/`, `SHA512SUMS` | ext4 root |
| `vm-fedora` | Fedora Cloud Base Generic, current stable release at execution (expected 45), `Fedora-Cloud-Base-Generic-<N>-<build>.x86_64.qcow2` | `download.fedoraproject.org/pub/fedora/linux/releases/<N>/Cloud/x86_64/images/`, `*-CHECKSUM` (GPG) | btrfs root (verify; record) |

Conversion: `qemu-img convert -O raw -S 4k in.qcow2 out.raw`; `qemu-img info` and `sfdisk -J out.raw` recorded (partition offsets and
file-system types). The image is **not booted** (booting would add host-specific keys and a machine-id); the chunked population is
the device exactly as distributed, which is also what a provisioning write to a Lunix volume stores.

**Block-set training pool** (never tested): Alpine Linux current `generic_cloud` or `virt` image (apk family), Rocky Linux 10
`GenericCloud` (rhel family), openSUSE Leap current `Minimal-VM` (suse family), Arch Linux `cloudimg` (arch family). Each pool
image is additionally unpacked (`guestmount --ro` or loop-mount of the converted raw) and copied with the §2.2 procedure into fresh
ext4, xfs and btrfs images, so the pool has 4 distributions × 4 file-system views. Distribution families, fixed now: **deb** (Debian,
Ubuntu, the host's own tree), **rhel** (Fedora, Rocky), **suse**, **apk**, **arch**.

### 2.2 The host's OS tree in fresh file systems (test corpora: raw devices)

Source: the test host's own tree (Ubuntu 26.04 LTS; recorded as a category only), copied as root with
`rsync -aHAX --numeric-ids --one-file-system --exclude-from=exclude.txt / <mnt>/`, size capped at 24 GiB (abort and record if the
cap is hit). `exclude.txt`, fixed now:

```
/home/** /root/** /srv/** /mnt/** /media/** /opt/*/data/** /tmp/** /var/tmp/**   # user data and scratch
/etc/shadow /etc/shadow- /etc/gshadow /etc/gshadow- /etc/ssh/ssh_host_* /etc/ssl/private/** /etc/machine-id
/var/lib/dbus/machine-id /etc/hostname /etc/hosts /etc/fstab /etc/crypttab /etc/NetworkManager/system-connections/**
/etc/netplan/** /etc/wireguard/** /etc/sudoers.d/** /etc/cloud/** /var/lib/cloud/**                    # keys, identity, network, cloud-init
/var/log/** /var/crash/** /var/spool/** /var/mail/** /var/backups/** /var/cache/apt/archives/*.deb /var/cache/private/**
/var/lib/apt/lists/** /var/lib/docker/** /var/lib/containers/** /var/lib/libvirt/images/** /var/lib/snapd/snaps/*.snap
/var/lib/systemd/random-seed /var/lib/systemd/coredump/** /var/lib/private/** /var/lib/lxd/** /var/lib/postgresql/** /var/lib/mysql/**
/proc/** /sys/** /dev/** /run/** /boot/efi/**  **/.ssh/** **/*.pem **/*.key **/*.gpg **/*.kdbx **/id_rsa* **/id_ed25519*
/usr/local/src/** /usr/src/**                                                                            # local builds and checkouts
```

After the copy and before unmount the executor runs `grep -rIl` for the host's hostname, its primary IPv4/IPv6 addresses, its MAC
addresses and every login name in `/etc/passwd` with uid ≥ 1000 over `<mnt>/etc` and `<mnt>/var`, deletes any hit, and records only
the count of deleted files. Three images from the same tree, created one at a time:

| corpus | mkfs (4 KiB blocks, fixed UUIDs, compression off) | mount |
|---|---|---|
| `host-ext4` | `mkfs.ext4 -b 4096 -U 00000000-0000-4000-8000-000000000001 -E lazy_itable_init=0,lazy_journal_init=0 img` | `-o noatime,discard` |
| `host-xfs` | `mkfs.xfs -f -b size=4096 -m uuid=00000000-0000-4000-8000-000000000002 img` (defaults: crc=1, reflink=1, inode 512 B) | `-o noatime,discard` |
| `host-btrfs` | `mkfs.btrfs -d single -m single -n 16384 -U 00000000-0000-4000-8000-000000000003 img` | `-o noatime,discard` (no `compress`) |

Image file: `truncate -s 32G`; loop device via `losetup --find --show --direct-io=on --sector-size 512`. After the copy: `sync`,
`fstrim -v <mnt>`, `umount`, `losetup -d`. `fstrim` makes freed blocks holes, which models a guest that issues discards (the data path
honours them); the un-trimmed population is not measured. The VM images are used as distributed and not trimmed again.

### 2.3 Database images (test corpora for D2, D4, D6; 15 images)

Datasets, all loaded with one schema per dataset into every engine:

| dataset | source and licence | tables and rows | fixed indexes beyond primary keys |
|---|---|---|---|
| `se-askubuntu`, `se-superuser`, `se-serverfault` | Stack Exchange data dump, `archive.org/details/stackexchange`, `<site>.7z` (CC BY-SA 4.0) | Posts, Comments, Users, Votes, Badges, PostLinks, Tags (PostHistory skipped for size; say so) | Posts(OwnerUserId), Posts(ParentId), Posts(CreationDate), Comments(PostId), Votes(PostId), Badges(UserId) |
| `geonames` | `download.geonames.org/export/dump/allCountries.zip` (CC BY 4.0) | geoname, 19 columns, about 13 M rows | (country_code), (feature_class, feature_code), (latitude, longitude), (name) |
| `tlc-2024q1` | NYC TLC yellow taxi trip records, 2024-01 to 2024-03, parquet converted to CSV with pyarrow (NYC open-data terms; licence UNVERIFIED, data used locally only) | trips, 19 columns, about 9 M rows | (tpep_pickup_datetime), (PULocationID), (DOLocationID) |

Schema families for hold-out (§4.3): **se** (three sites, one schema), **geonames**, **tlc**. XML and CSV are converted once to CSV
by a streaming script run with `python3 -I` from outside the download directory; the same CSV feeds all engines.

| engine | version (record exact) | page size | load | quiescing before imaging |
|---|---|---|---|---|
| PostgreSQL | 18.x from the distribution | 8 KiB | `initdb -D <mnt>/pg --no-locale -E UTF8`; `COPY ... FROM ... CSV`; defaults otherwise (`default_toast_compression=pglz`, `full_page_writes=on`, `max_wal_size=1GB`) | `VACUUM ANALYZE; CHECKPOINT;` then `pg_ctl stop -m fast` |
| MySQL (InnoDB) | 8.4 LTS from the distribution, or MariaDB stated | 16 KiB | `mysqld --initialize-insecure --datadir=<mnt>/mysql`; `--skip-log-bin --innodb_file_per_table=1 --local-infile=1`; `LOAD DATA LOCAL INFILE` | `FLUSH TABLES;` then `mysqladmin shutdown` (slow shutdown, `innodb_fast_shutdown=0`) |
| SQLite | 3.x from the distribution | 4 KiB (default) | `sqlite3 <mnt>/sqlite/<ds>.db`, `.mode csv`, `.import` | `PRAGMA journal_mode=DELETE; VACUUM;` |

Servers run **unprivileged** as the study user on a non-default port and a Unix socket under scratch, never as a system service, and
are stopped before imaging. Every (dataset, engine) pair gets its own `truncate -s 16G` ext4 image (§2.2 parameters, UUID
`...000000000010` + n), mounted `noatime,discard`, chowned to the study user; after quiescing: `sync`, `fstrim`, `umount`. Sensitivity:
`se-askubuntu` × PostgreSQL is repeated on xfs and btrfs. Synthetic controls, run once each on ext4 and labelled "control" in every
table: `pgbench -i -s 100` (PostgreSQL) and `sysbench oltp_common --tables=8 --table-size=1000000 prepare` (MySQL). Controls never
enter a vote.

### 2.4 Chunking and class labels (Lunix view)

- The whole raw device (partition table included) is cut into aligned chunks from offset 0 at 16 KiB and at 64 KiB (two passes).
  All-zero chunks are **holes**; the rest are deduplicated by BLAKE3-256 of the plaintext, keeping the first occurrence (by LBA). The
  population is the unique non-hole chunks; holes, duplicates and the probe-raw share are recorded in `raw/shares.csv`. Each chunk
  carries its origin **block** (the 1 MiB-aligned extent of the first occurrence; §4). Partition start offsets are recorded; a root
  partition not aligned to 64 KiB is noted because chunks then straddle file-system blocks.
- **Class label per 4 KiB block**, mapped to chunks by majority of non-zero 4 KiB blocks (class `mixed` if no class reaches 75 %):
  - ext4: `filefrag -v` (FIEMAP) over every regular file and directory (mounted read-only) → `data` (with the file's zdicts type from
    `select_by_content` on the file head, so the "type oracle" is known per block) or `dir`; `dumpe2fs` group descriptors → `meta`
    (superblocks, GDT, bitmaps, inode tables); the journal from `debugfs -R "stat <8>"` → `journal`; other non-zero blocks →
    `unmapped`. A 1 % random sample of `data` blocks is cross-checked with `debugfs -R "icheck"`; the disagreement rate is reported.
  - xfs: `filefrag -v` for files and directories; `xfs_db -r -c "sb 0" -c "print"` for the log (`logstart`, `logblocks` → `journal`)
    and `-c "agf N"`, `-c "agi N"` for the AG headers and free-space and inode B-trees → `meta`; inode chunks from `xfs_db -r -c
    "blockget -n" -c "blockuse"` → `meta`; if `blockget` fails on the image, the remaining non-zero blocks are `meta-or-unmapped` and
    the summary says so.
  - btrfs: `btrfs inspect-internal dump-tree -t chunk` gives the logical→physical map of each block group (`single` profiles make it
    linear); block groups flagged DATA → `data`, METADATA → `meta`, SYSTEM → `meta`. File-level attribution inside DATA via FIEMAP
    translated through the chunk map; if the translation fails, DATA stays unattributed.
  - database images: files under the data directory are labelled by path: `dbpage` (heap and index files, `.ibd`, `.db`), `wal`
    (`pg_wal/`, `#innodb_redo/`, `undo_*`), `dbother` (catalog, `pg_xact`, `ib_buffer_pool`, temporary files), else as above.
- **Constellation views** (§2.5, §2.3): objects are whole files under the chunk size, else chunks of the file; file lists come from the
  mounted image, not from the live host.

### 2.5 Constellation file populations (D5)

The files of the three VM images (mounted read-only from the raw conversion) and the three OS-tree images (`host-*`; the same bytes
in each, so one file population, labelled `host-files`, and 4 test corpora in total; D5's vote is "≥ 3 of 4"). Files with identical
content are kept once (dedup). Objects at 4 MiB chunking; names are available to `select`. Database files (D6) are taken from the
database images' data directories at 4 MiB, 1 MiB and the engine's page size.

## 3. Arms

Per chunk (Lunix view) or object (Constellation view). Lunix arms sit behind the ADR-12 probe (4 KiB LZ4, raw under 6 %) unless named
"probe off"; `probe off` is run for the plain levels only, to report forfeited bytes as the media study did.

| arm | definition | levels | views |
|---|---|---|---|
| `none` | stored raw | | all |
| `probe` | ADR-12: probe → raw or zstd-1; its raw share reported | 1 | Lunix |
| `z1`, `z3` | plain zstd | 1, 3 | Lunix (1, 3), Constellation (3) |
| `ft` | zdicts file-type dictionary chosen by `select_by_content` on the chunk's first 4 KiB (Lunix) or `select(name, head, len)` with the 64 KiB rule (Constellation) | 1, 3 | all |
| `oracle` | the best of the 33 zdicts dictionaries, the block set and the DB set, or none, per chunk; the upper bound for any selector | 3 (and 1 on Lunix) | all |
| `bs-dist` | block set: 3 class dictionaries (`meta`, `data-text`, `data-binary`; chunk class from the ADR-30 cheap classifier, metadata by a magic check on ext4/xfs/btrfs structures) trained on the pool images of **other distribution families** (and the pool's file systems); trained once per test family | 1, 3 | Lunix |
| `bs-xfs` | as `bs-dist`, additionally excluding the test image's file system from training | 1, 3 | Lunix (host-* corpora and vm-fedora) |
| `db` | database set: one dictionary per engine, trained on the database images of **other schema families** (schema-disjoint, primary) and, secondarily, on other datasets of any family (same-schema, reported only) | 1, 3 (Lunix), 3 (Constellation) | database corpora |
| `pv` | per-volume: one dictionary trained with fastCover (`--train-fastcover=k=1024,d=8,f=20,accel=1`, no optimiser, the ADR-30 product path) on a uniform sample of `max(1 %, 8 MiB)` capped at 64 MiB of the unique non-hole chunks, tested on the remaining chunks only; seed 1 decides, seeds 2 and 3 give the spread | 1, 3 | Lunix, and database files in the Constellation view |
| `pv-class` | per-volume with 3 classes from the ADR-30 cheap classifier (magic, text/binary, byte entropy), one dictionary each, unclassified chunks plain | 1, 3 | Lunix |

- **Dictionary size** 110 KiB everywhere (zdicts' convention), so sets are comparable. Shipped-candidate sets (`bs-*`, `db`) are trained
  with `zstd --train-cover=steps=16` (the zdicts pipeline), on an equalised 24 MiB per class drawn from the training images.
- **Sweeps, only where they move a decision:** on `host-ext4` and `se-askubuntu`×PostgreSQL at 16 KiB: `pv` sample 0.1, 1, 10 % ×
  dictionary 16, 32, 64, 110 KiB (D2: GET volume and memory per volume), and `pv` retrained with the COVER optimiser (`pv-cover`; if it
  beats `pv` by over 1 pp, the summary says the product should use COVER, and D2 is judged on `pv-cover`). No other sweeps.
- **Timing** (quiet sister host, same libzstd and zstd crate, one pinned core, `performance` governor if possible, 1 warm-up and 7
  timed passes, median and min/max, 1-minute load recorded, cells with max/min > 1.2 flagged): a 1 GiB block-stratified in-memory
  sample per corpus and view, compress and decompress for every arm, including the classifier (`ft`), the probe and the cheap
  classifier (`pv-class`, `bs-*`). Per-volume dictionaries of the `host-*` corpora and any host-tree chunk sample **never leave the
  test host**; their timing cells use the `vm-ubuntu` sample and a same-size dictionary trained on it (timing depends little on content).

## 4. Statistics

1. **Unit and resampling.** The chunk (or object) is the unit of measurement; the **1 MiB origin block** (file for Constellation
   objects) is the unit of resampling and of pairing, because neighbouring chunks are not independent. Volume-wide `S` gets a block
   bootstrap of 10,000 resamples with percentile and BCa 95 % intervals; `LB` is the BCa lower end. Sensitivity: `host-ext4` and one
   database image are re-run with 16 MiB blocks; if any D1-D6 conclusion changes, the 16 MiB result rules and the summary says so.
2. **Paired tests.** Per block, the difference in stored bytes between arm and comparator; Wilcoxon signed-rank over blocks; the
   rank-biserial correlation as effect size. **Holm** correction within one family per decision: D1 = 6 corpora × 2 chunk sizes × 2 sets
   × 2 levels (48 tests); D2 = 21 volumes × 2 sizes; D2c, D3, D4, D5, D6 likewise, enumerated in `raw/families.csv` before the first
   test is run. A vote counts a corpus only if both `N ≥ threshold` and the Holm-adjusted p < 0.05.
3. **Hold-out rules, enforced by code (an assertion that fails the run):** no dictionary is tested on any chunk of an image it was trained
   on; `bs-dist` excludes the test image's distribution family; `bs-xfs` also excludes its file system; `db` (primary) excludes the
   test dataset's schema family; `pv` tests only the chunks outside its sample; `host-*` corpora are **deb** family, so their `bs-*`
   dictionaries are trained on rhel, suse, apk and arch pool images only; the zdicts file-type set was trained on GitHub projects
   and is used as shipped. Duplicate chunks across test and training images (shared packages between distributions) are counted and
   reported as the "cross-image duplicate share"; they are not removed, because a shipped set would meet them too.
4. **Required sample sizes.** Each corpus and view must have **≥ 1,024 non-hole 1 MiB blocks** (≥ 1 GiB unique non-zero data). With a
   per-block saving standard deviation of 12 pp, that gives a half-width of about 0.7 pp (1.96·12/√1024), and 80 % power to see a
   true saving of threshold + 1 pp clear the threshold at the lower bound. The executor checks the achieved half-width per cell; a
   decision cell with half-width > 1.5 pp is "underpowered" and the verdict is "undetermined" with the needed size stated. A class
   (`meta`, `dbpage`, `wal`) is reported with an interval only from ≥ 200 blocks; below that, descriptively.
5. **Seeds and reproducibility.** One master seed (`20261112`) derives every sample, fold, trainer and bootstrap seed; every image, CSV
   and export has a recorded SHA-256; one analysis command regenerates every table and figure from `raw/` byte for byte; tool versions
   are pinned in `raw/environment.lock`.

## 5. Feasibility

- **Hosts, categories only:** test host 16 vCPU, about 30 GiB RAM, local NVMe, about 150 GB for this study; a quiet sister host of the
  same class for timing. Study processes pinned to 4 cores (`taskset`), 6 at most; `LUNIX_MIN_FREE_GIB`-style guard: every `mkfs`,
  download and export is preceded by a free-space check requiring 40 GiB free after the step.
- **Disk budget (peak ≤ 110 GB, sequential):** downloads 12 GB (7 qcow2, 3 SE dumps about 6 GB, GeoNames 0.4 GB, TLC 0.2 GB); one
  live image at a time (≤ 32 GiB sparse, data ≤ 24 GiB); one chunk export at a time (≤ 24 GB, deleted after its arms are run); pool
  images reduced to training samples (≤ 0.5 GB each) immediately; per-chunk result tables (about 20 arms × 8 B × up to 1.6 M chunks,
  gzip, ≤ 0.3 GB per corpus and size) kept until analysis; CSV staging for a dataset ≤ 8 GB, deleted after the third engine.
- **Compute (estimate; actuals reported):** chunk bytes about 105 GB (raw-device corpora, both sizes) + 90 GB (database images) + 60 GB
  (Constellation files, three sizes); about 50 codec passes including the oracle (34 dictionaries) at 0.3-0.5 GB/s per core: 40-60
  core-hours; trainings: 24 shipped-candidate COVER trainings (3 families × 2 variants × 3 classes + 6 DB) at ≤ 10 min, about 600
  fastCover trainings (21 volumes × 2 sizes × 3 seeds × (1 + 3 classes) + sweeps) at ≤ 1 min: about 15 core-hours; timing 3 core-hours
  on the quiet host. Total about 60-80 core-hours; wall clock 3-4 days on 4 cores, dominated by downloads, 15 database loads (5-8 h)
  and image I/O.
- **Root** (through the host-wide root lock, leftovers check before and after, `tools/remote.sh root` conventions): `losetup`, `mkfs`,
  `mount`/`umount`, `fstrim`, `rsync` of the OS tree, `chown` of mount points. Everything else, including the database servers, the
  chunker, training and analysis, runs unprivileged. One loop device and one mounted image at a time; a wedged mount is reported,
  never retried.
- **ublk ban:** no ublk device is created, loaded or probed in this study. The offline chunker defines the population; its equivalence
  to a stored Lunix volume is M18b's validation cell and is listed as a threat here (§6).
- **Privacy rule:** no hostname, IP or MAC address, login name, machine-id, SSH host key, real file-system UUID or absolute path under
  `/home` appears in any file under this directory or in `SUMMARY.md`; hosts are described by category. Per-file and per-chunk rows,
  file lists, dictionaries trained on the host tree and chunk samples stay on the test host and are deleted when the summary is
  accepted; only aggregates (per corpus, class, size bucket and arm; counts, bytes, ratios, intervals, p-values, timings) leave the
  host into `raw/`. Dictionaries trained on public images may be kept as candidate artefacts for zdicts, with their training manifest.

## 6. Threats to validity and mitigations

| threat | mitigation |
|---|---|
| Offline chunking is not what a Lunix volume stores (RMW merges, partial writes, discard timing) | aligned chunks from offset 0 and `fstrim` reproduce the steady state of a provisioned image; M18b's validation cell checks the chunk-id set after M12a; stated as unverified here |
| VM images are fresh installs: no logs, caches or churn; the OS tree is one developer host | two image families (3 distributions, 3 file systems of one tree) and a 4-of-6 vote; the host tree carries years of churn but is one tree; both named as limits |
| The zdicts classifier was trained on file heads, but Lunix sees mid-file chunks | the `oracle` arm bounds the selector's loss; the loss is reported per class, and a chunk-trained classifier is named as the fix if the gap exceeds 1.5 pp |
| Shared packages between distributions leak across hold-out | distribution families held out, cross-image duplicate share reported, `bs-xfs` as the stricter variant |
| Stack Exchange sites share a schema; a DB dictionary could learn the schema, not the engine | schema-disjoint training is primary for D4 and D6; same-schema is reported only |
| Database contents are text-heavy (SE) or numeric (GeoNames, TLC); other workloads differ | three schema families and two synthetic controls, labelled; the summary states the class mix per dataset |
| TOAST, InnoDB and SQLite settings change page content | engine defaults, versions and page sizes recorded; no compression features enabled |
| Trainer randomness and parameters | COVER with 16 steps for shipped candidates, fixed fastCover parameters for the product path, 3 seeds, spread reported; `pv-cover` sensitivity |
| Block independence in the bootstrap | 1 MiB blocks, 16 MiB sensitivity run on two corpora, decision follows the wider interval |
| Multiple comparisons and vote rules | Holm within pre-enumerated families, votes fixed here, no post hoc buckets |
| CPU cost measured on another host with partly substituted dictionaries | same libzstd version, one core, warm, in-memory; host-tree cells use the Ubuntu stand-in and are marked; λ is small and the 0.8× cap uses ratios, not absolute speeds |
| Lunix extent-header and encryption overhead ignored | constant per chunk, cancels in differences; stated |
| Constellation's streaming `encode_all` frames differ from one-shot frames | a few bytes per object; not corrected (file-type study §3) |
| The licences of the TLC data and of the images' bundled software | recorded as engineering assumptions; nothing is redistributed; dictionaries derived from images are candidates for counsel's review before any release |

## 7. Required output: `SUMMARY.md`

The executor writes `bench/dict-study/validation/SUMMARY.md` (file-type study structure, under about 400 lines) with these sections in
this order; every number carries its interval, population, hole, duplicate and probe-raw shares, and the Holm-adjusted p where a test
applies.

1. **Abstract** (10 lines): the three answers and the D1-D7 verdicts.
2. **Question 1, raw devices** (one page): per corpus and chunk size, a table of volume-wide saving of `z3`, `ft`, `bs-dist`, `bs-xfs`,
   `oracle` against the counterfactual, with `N`, CPU and the vote; the per-class table (`meta`, `dir`, `journal`, `data` by zdicts
   type, `unmapped`); the hole and duplicate shares; the answer in one sentence per chunk size.
3. **Question 2, per-volume training against shipped sets** (one page): per volume `pv`, `pv-class`, best shipped set, `oracle`; the
   two votes (raw-device, database); the sweep table (sample × dictionary size, GET bytes a 1 % sample implies for a 1 TiB volume,
   memory per volume); `pv-cover` against `pv`; the answer and what it means for ADR-30's `train`/`apply`, `estimate` and the 5 %/1 %
   adoption rule.
4. **Question 3, database pages** (one page): (a) Constellation view at 4 MiB, 1 MiB and page size, per engine and dataset, `z3`, `db`
   (schema-disjoint and same-schema), `pv`; (b) Lunix view at 16 and 64 KiB on the database images, per class (`dbpage`, `wal`,
   `dbother`, `meta`), same arms; controls in a separate marked row; the answer per engine.
5. **Decision table applied**: D1-D7 with the measured `LB`, `ΔCPU`, `ΔCores`, `N`, votes and the verdict (GO, NO-GO, or undetermined
   with the missing sample size); the §1.3 expectations marked confirmed or refuted.
6. **Proposals** (not edits): changes to ADR-30, ADR-12 and ADR-5's chunk-size evidence, the zdicts sets to add, Constellation
   settings; each marked proposal.
7. **Deviations** from this protocol, with reasons. **Threats to validity**, updated with what was observed. **Reproducibility
   appendix** (hosts by category, versions, seeds, hashes, the one analysis command, actual compute and disk, `raw/` inventory).
8. **Figures** (SVG under `figures/`): saving by arm per corpus (Q1), per-class saving (Q1), `pv` against shipped per volume (Q2), the
   sweep (Q2), saving by chunk size for database files (Q3), compress CPU against saving for every arm (all).

## Amendment A1 (2026-11-13, maintainer ruling before any follow-up data): D2 on database images

The independent review of the first analysis found the D2 comparator ambiguous for the cap and ΔCores, and three measurements
missing. The maintainer ruled the following **before** any of the follow-up data below existed. It replaces deviation 22 of the
first analysis and fixes D2 for both families; no threshold changes.

1. **Comparator.** Built-in sets did not pass (D1 NO-GO), so the realistic alternative to a per-volume dictionary is the plain
   counterfactual. For D2, `S`, `LB`, `N`, ΔCPU, ΔCores and the 0.8× hard cap (compress and decompress) are all taken against the
   **counterfactual of §1.1** (the best plain level, behind the probe, that passes the cap), per volume and chunk size.
2. **Timing per volume.** The cap and ΔCPU of every database image come from that image's own timing (volume-wide, block-stratified
   1 GiB sample of all its unique non-hole chunks, quiet host, §3 method), at 16 and 64 KiB, for the plain levels needed to fix the
   counterfactual (at least zstd-1, zstd-2, zstd-3), `pv@1`, `pv@3`, `pv-cover@1` and `pv-cover@3`. No stand-in cell decides.
3. **COVER.** `pv-cover` is trained (same 1 % sample, same size) and measured on all 15 database images at both sizes. Per §3, if
   `pv-cover` beats `pv` by more than 1 pp (median over the 15 images at 16 KiB), the database family's D2 is judged on `pv-cover`;
   otherwise on `pv`. The raw-device family keeps its first-analysis verdict (0 of 4 in every reading).
4. **Sizes.** The 16 KiB vote decides. The 64 KiB vote is reported as confirmatory; if it does not also reach 8 of 15, the verdict is
   recorded as "GO at 16 KiB only" and the summary states what that means for the product.
5. **D5.** The per-object Constellation pass (`select(name, head, len)` + dictionary at zstd:3 against zstd:3, and the oracle) is
   measured on the four file corpora, so that the decision table carries D5; the hard cap still applies regardless of `N`.
6. **Intervals.** 1 MiB blocks remain the pre-registered resampling unit and decide; the 16 MiB-block intervals are printed next to
   them in every table, because neighbouring 1 MiB blocks are measurably dependent.
