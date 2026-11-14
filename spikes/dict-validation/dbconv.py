#!/usr/bin/env python3
"""Phase 5a: convert a downloaded dump to CSV (strings always quoted, NULL unquoted as NULL), streaming.  usage: dbconv.py <dataset> <dl-dir> <stage-dir>
Run with python3 -I from outside the download directory.  PostHistory is skipped (size)."""
import sys, os, subprocess, csv, io
import xml.etree.ElementTree as ET
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import dbschema as S
ds, dl, st = sys.argv[1:4]; os.makedirs(st, exist_ok=True)
def q(v, num):
    if v is None or v == '': return 'NULL'
    return v if num else '"' + v.replace('"', '""') + '"'
def conv_se():
    site = ds.split('-', 1)[1] + '.com'; z = os.path.join(dl, site + '.7z')
    subprocess.run(['7z', 'x', '-y', '-o' + st, z] + [t + '.xml' for t in S.SE], check=True, capture_output=True)
    for t, (cols, pk) in S.SE.items():
        names = [c.split()[0] for c in cols]; kinds = [c.split()[1] for c in cols]
        with open(os.path.join(st, t + '.csv'), 'w', newline='') as o:
            for ev, el in ET.iterparse(os.path.join(st, t + '.xml')):
                if el.tag != 'row': continue
                a = el.attrib
                o.write(','.join(q(a.get(n), k in 'ib d'.replace(' ', '')) for n, k in zip(names, kinds)) + '\n'); el.clear()
        os.remove(os.path.join(st, t + '.xml'))
def conv_geo():
    import zipfile
    zipfile.ZipFile(os.path.join(dl, 'allCountries.zip')).extract('allCountries.txt', st)
    cols = S.GEO['geoname'][0]; kinds = [c.split()[1] for c in cols]
    with open(os.path.join(st, 'allCountries.txt'), encoding='utf-8', newline='') as f, open(os.path.join(st, 'geoname.csv'), 'w') as o:
        for line in f:
            p = line.rstrip('\n').split('\t')
            o.write(','.join(q(v, k in 'ibd') for v, k in zip(p, kinds)) + '\n')
    os.remove(os.path.join(st, 'allCountries.txt'))
def conv_tlc():
    import pyarrow.parquet as pq
    with open(os.path.join(st, 'trips.csv'), 'w') as o:
        for m in ('01', '02', '03'):
            pf = pq.ParquetFile(os.path.join(dl, 'yellow_tripdata_2024-%s.parquet' % m))
            for b in pf.iter_batches(batch_size=200000):
                cols = [c.to_pylist() for c in b.columns]; ty = [str(f.type) for f in b.schema]
                num = [t.startswith(('int', 'double', 'float')) for t in ty]
                for row in zip(*cols):
                    o.write(','.join('NULL' if v is None else (repr(v) if n else '"%s"' % (v.isoformat(sep=' ') if hasattr(v, 'isoformat') else str(v))) for v, n in zip(row, num)) + '\n')
{'se-askubuntu': conv_se, 'se-superuser': conv_se, 'se-serverfault': conv_se, 'geonames': conv_geo, 'tlc-2024q1': conv_tlc}[ds]()
