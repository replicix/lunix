#!/usr/bin/env python3
"""Hypothesis families for Holm (PREREGISTERED.md section 4.2), enumerated before any test is run.
usage: families.py <out.csv> [a1]      (a1: Amendment A1 families, written before the A1 analysis)
One row per test.  Holm runs within `family` over every row of that family (m = rows of the family).  Rows with `votes=0` are tested
and adjusted with their family but never counted in a vote (vm-debian, vm-fedora: below the 1,024-block floor, lead ruling 2026-11-13).
Comparator rules (section 1.1):
  CF        the counterfactual: best plain level passing the 0.8x cap against the product default, behind the ADR-12 probe on Lunix
            (z1 or z3; chosen from raw/timing.csv before any test), z3 on Constellation
  BEST      D2: the comparator with the fewest stored bytes on the pv hold-out among CF and the measured shipped sets
            (raw devices: ft@1, ft@3, bs-dist@1, bs-dist@3; database images: ft@1, ft@3, dbd@1, dbd@3)
  <arm>     that arm on the same chunks
Sensitivity runs (16 MiB blocks, alternative readings) re-test the same rows with the family's m; they add no rows."""
import sys, csv
RAW = ['host-ext4', 'host-xfs', 'host-btrfs', 'vm-ubuntu', 'vm-debian', 'vm-fedora']
ELIG = {'host-ext4', 'host-xfs', 'host-btrfs', 'vm-ubuntu'}
DB = ['db-%s-%s' % (d, e) for d in ('se-askubuntu', 'se-superuser', 'se-serverfault', 'geonames', 'tlc-2024q1') for e in ('pg', 'my', 'sq')]
A1 = len(sys.argv) > 2 and sys.argv[2] == "a1"                        # Amendment A1: D2 against the counterfactual, pv-cover rows, D5 per-object pass
FILES = ["vm-ubuntu", "vm-debian", "vm-fedora", "host-files"] if A1 else ["vm-ubuntu-files", "vm-debian-files", "vm-fedora-files", "host-files"]
rows = []
def add(fam, dec, corpus, view, size, pop, cls, arm, comp, votes, note=''):
    rows.append([fam, '|'.join([dec, corpus, str(size), pop, cls, arm, comp]), dec, corpus, view, size, pop, cls, arm, comp, votes, note])
for c in RAW:                                                      # D1: 6 corpora x 2 sizes x 2 sets x 2 levels = 48
    for cs in (16384, 65536):
        for s in ('ft', 'bs-dist'):
            for L in (1, 3): add('D1', 'D1', c, 'lunix', cs, 'full', 'all', '%s@%d' % (s, L), 'CF', int(c in ELIG))
vols = [(c, 'lunix', int(c in ELIG), 'raw') for c in RAW] + [(d, 'lunix', 1, 'db') for d in DB]
for c, v, vt, kind in vols:                                        # D2: 21 volumes x 2 sizes x 2 levels = 84 (level not named by the protocol)
    for cs in (16384, 65536):
        for L in (1, 3): add("D2", "D2", c, v, cs, "pv-s1", "all", "pv-s1@%d" % L, "CF" if A1 else "BEST", vt, kind + (" primary" if cs == 16384 else " confirmatory"))
        if A1 and kind == "db":                                        # A1.3: pv-cover on the 15 database images, same family (+60)
            for L in (1, 3): add("D2", "D2", c, v, cs, "pv-s1", "all", "pvcover-s1@%d" % L, "CF", vt, "db cover " + ("primary" if cs == 16384 else "confirmatory"))
for c, v, vt, kind in vols:                                        # D2c: per-class PV vs single PV, same level, 84 (evaluated only if D2 fires)
    for cs in (16384, 65536):
        for L in (1, 3): add('D2c', 'D2c', c, v, cs, 'pv-s1', 'all', 'pvc-s1@%d' % L, 'pv-s1@%d' % L, vt, kind + ' conditional on D2')
for c in RAW:                                                      # D3: bs-dist vs ft, 6 x 2 x 2 = 24 (bs-xfs retention is a ratio, no test)
    for cs in (16384, 65536):
        for L in (1, 3): add('D3', 'D3', c, 'lunix', cs, 'full', 'all', 'bs-dist@%d' % L, 'ft@%d' % L, int(c in ELIG))
for d in DB:                                                       # D4-L: 15 images x 2 levels on the DB-page class at 16 KiB = 30
    for L in (1, 3): add('D4', 'D4-L', d, 'lunix', 16384, 'full', 'dbpage', 'dbd@%d' % L, 'CF', 1)
for d in DB:                                                       # D4-C: 15 images, Constellation 1 MiB, database-file bytes = 15
    add('D4', 'D4-C', d, 'constellation', 1048576, 'all', 'all', 'dbd@3', 'CF', 1)
for f in FILES:                                                    # D5: 4 file corpora x 2 populations = 8
    add("D5", "D5", f, "constellation-files", 4194304, "lt64k", 'all', 'ft@3', 'CF', 1)
    add("D5", "D5", f, "constellation-files", 4194304, "all", 'all', 'ft@3', 'CF', 1)
for d in DB:                                                       # D6: 15 images x (1 MiB decides, 4 MiB reported) = 30
    for sz in (1048576, 4194304): add('D6', 'D6', d, 'constellation', sz, 'all', 'all', 'dbd@3', 'CF', 1, 'decides' if sz == 1048576 else 'reported')
w = csv.writer(open(sys.argv[1], 'w', newline=''))
w.writerow(['family', 'test_id', 'decision', 'corpus', 'view', 'size', 'pop', 'class', 'arm', 'comparator', 'votes', 'note']); w.writerows(rows)
print(len(rows), 'tests', {f: sum(1 for r in rows if r[0] == f) for f in ('D1', 'D2', 'D2c', 'D3', 'D4', 'D5', 'D6')})
