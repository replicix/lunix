#!/usr/bin/env python3
"""Phase 5b: load one (dataset, engine) pair into its own ext4 image and quiesce it (section 2.3).
usage: dbload.py <dataset> <pg|my|sq> <n> [fs]    n = image number (UUID ...0010 + n); fs ext4 (default) | xfs | btrfs
Needs the CSV staging dir work/stage-<dataset> (dbconv.py).  Servers run unprivileged on a Unix socket under the mount, port 55432 / 33306."""
import sys, os, json, time, subprocess, shutil, sqlite3, csv, glob
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import lib, dbschema as S
ds, eng, n = sys.argv[1], sys.argv[2], int(sys.argv[3]); fs = sys.argv[4] if len(sys.argv) > 4 else 'ext4'
name = 'db-%s-%s' % (ds, eng) + ('' if fs == 'ext4' else '-' + fs)
img = os.path.join(lib.WORK, name + '.raw'); mnt = os.path.join(lib.MNT, name); stage = os.path.join(lib.WORK, 'stage-' + ds)
uuid = '00000000-0000-4000-8000-%012d' % (10 + n)
PGB = '/usr/lib/postgresql/18/bin'; MYS = os.path.join(lib.ROOT, 'bin', 'mysqld')
run = lambda c, **k: subprocess.run(c, check=True, capture_output=True, text=True, **k)
lib.guard(16, name); run(['rm', '-f', img]); run(['truncate', '-s', '16G', img])
mk = {'ext4': ['mkfs.ext4', '-q', '-F', '-b', '4096', '-U', uuid, '-E', 'lazy_itable_init=0,lazy_journal_init=0', img],
      'xfs': ['mkfs.xfs', '-q', '-f', '-b', 'size=4096', '-m', 'uuid=' + uuid, img],
      'btrfs': ['mkfs.btrfs', '-q', '-f', '-d', 'single', '-m', 'single', '-n', '16384', '-U', uuid, img]}[fs]
run(mk); t0 = time.time(); info = {'name': name, 'dataset': ds, 'engine': eng, 'fs': fs}
if ds == 'tlc-2024q1':
    import pyarrow.parquet as pq
    sc = pq.ParquetFile(os.path.join(lib.DL, 'yellow_tripdata_2024-01.parquet')).schema_arrow
    cols = []
    for f in sc:
        t = str(f.type); k = 'i' if t.startswith('int') else 'd' if t.startswith(('double', 'float')) else 't' if t.startswith('timestamp') else 's'
        cols.append('%s %s' % (f.name, k))
    schema, idx = S.tlc_schema(cols), S.TLC_IDX; tables = {'trips': 'trips.csv'}
else:
    schema, idx = S.DATASETS[ds]; tables = {t: t + '.csv' for t in schema}
ddl, ix = S.ddl(eng, schema, idx)
with lib.loopmount(img, mnt, ro=False, opts='noatime,discard', direct=True) as dev:
    run(['sudo', 'chown', '%d:%d' % (os.getuid(), os.getgid()), mnt])
    d = os.path.join(mnt, {'pg': 'pg', 'my': 'mysql', 'sq': 'sqlite'}[eng]); sock = os.path.join(mnt, 'sock'); os.makedirs(sock, exist_ok=True)
    if eng == 'pg':
        run([PGB + '/initdb', '-D', d, '--no-locale', '-E', 'UTF8', '-A', 'trust'])
        opts = "-p 55432 -k %s -c listen_addresses= -c max_wal_size=1GB -c shared_buffers=1GB -c maintenance_work_mem=1GB -c fsync=off -c synchronous_commit=off" % sock
        run([PGB + '/pg_ctl', '-D', d, '-o', opts, '-l', os.path.join(mnt, 'pg.log'), 'start', '-w'])
        try:
            ps = lambda sql, **k: run([PGB + '/psql', '-h', sock, '-p', '55432', '-d', 'postgres', '-v', 'ON_ERROR_STOP=1', '-c', sql], **k)
            for s in ddl: ps(s)
            for t, f in tables.items(): ps("COPY %s FROM '%s' WITH (FORMAT csv, NULL 'NULL')" % (t, os.path.join(stage, f)))
            for s in ix: ps(s)
            ps('VACUUM ANALYZE'); ps('CHECKPOINT')
            info['version'] = run([PGB + '/postgres', '--version']).stdout.strip(); info['page_size'] = 8192
        finally: run([PGB + '/pg_ctl', '-D', d, 'stop', '-m', 'fast', '-w'])
    elif eng == 'my':
        common = ['--no-defaults', '--basedir=/usr', '--lc-messages-dir=/usr/share/mysql', '--plugin-dir=/usr/lib/mysql/plugin', '--datadir=' + d]
        run([MYS] + common + ['--initialize-insecure', '--log-error=' + os.path.join(mnt, 'init.log')])
        srv = subprocess.Popen([MYS] + common + ['--socket=' + sock + '/m.sock', '--port=33306', '--skip-networking', '--skip-log-bin', '--innodb_file_per_table=1', '--local-infile=1',
              '--innodb_buffer_pool_size=1G', '--innodb_redo_log_capacity=512M', '--innodb_flush_log_at_trx_commit=2', '--log-error=' + os.path.join(mnt, 'my.log')])
        try:
            my = lambda sql, **k: subprocess.run(['mysql', '--no-defaults', '-uroot', '--socket=' + sock + '/m.sock', '--local-infile=1', '-e', sql], check=True, capture_output=True, text=True, **k)
            for _ in range(120):
                if subprocess.run(['mysqladmin', '--no-defaults', '-uroot', '--socket=' + sock + '/m.sock', 'ping'], capture_output=True).returncode == 0: break
                time.sleep(1)
            my('CREATE DATABASE ds CHARACTER SET utf8mb4'); 
            for s in ddl: subprocess.run(['mysql', '--no-defaults', '-uroot', '--socket=' + sock + '/m.sock', 'ds', '-e', s], check=True, capture_output=True, text=True)
            for t, f in tables.items():
                subprocess.run(['mysql', '--no-defaults', '-uroot', '--socket=' + sock + '/m.sock', '--local-infile=1', 'ds', '-e',
                  "SET unique_checks=1; LOAD DATA LOCAL INFILE '%s' INTO TABLE `%s` CHARACTER SET utf8mb4 FIELDS TERMINATED BY ',' ENCLOSED BY '\"' ESCAPED BY '' LINES TERMINATED BY '\\n'" % (os.path.join(stage, f), t)],
                  check=True, capture_output=True, text=True)
            for s in ix: subprocess.run(['mysql', '--no-defaults', '-uroot', '--socket=' + sock + '/m.sock', 'ds', '-e', s], check=True, capture_output=True, text=True)
            subprocess.run(['mysql', '--no-defaults', '-uroot', '--socket=' + sock + '/m.sock', '-e', 'SET GLOBAL innodb_fast_shutdown=0; FLUSH TABLES'], check=True, capture_output=True)
            info['version'] = run(['mysqld', '--version']).stdout.strip(); info['page_size'] = 16384
        finally:
            subprocess.run(['mysqladmin', '--no-defaults', '-uroot', '--socket=' + sock + '/m.sock', 'shutdown'], capture_output=True); srv.wait(timeout=600)
    else:
        os.makedirs(d); db = sqlite3.connect(os.path.join(d, ds + '.db')); db.execute('PRAGMA journal_mode=DELETE'); db.execute('PRAGMA synchronous=OFF')
        for s in ddl: db.execute(s)
        for t, f in tables.items():
            nc = len(schema[t][0]); rows = []
            with open(os.path.join(stage, f), newline='', encoding='utf-8') as fh:
                for r in csv.reader(fh, quotechar='"', doublequote=True):
                    rows.append([None if v == 'NULL' else v for v in r])
                    if len(rows) == 20000: db.executemany('INSERT INTO "%s" VALUES (%s)' % (t, ','.join('?' * nc)), rows); rows = []
            if rows: db.executemany('INSERT INTO "%s" VALUES (%s)' % (t, ','.join('?' * nc)), rows)
        db.commit()
        for s in ix: db.execute(s)
        db.commit(); db.execute('VACUUM'); db.close(); info['version'] = 'sqlite ' + sqlite3.sqlite_version; info['page_size'] = 4096
    info['fstrim'] = subprocess.run(['sudo', 'fstrim', '-v', mnt], capture_output=True, text=True).stdout.strip().split(':')[-1]
info['load_seconds'] = round(time.time() - t0); run(['fallocate', '-d', img])
json.dump(info, open(os.path.join(lib.OUT, name + '.json'), 'w')); print(info)
