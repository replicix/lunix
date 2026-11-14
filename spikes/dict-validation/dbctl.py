#!/usr/bin/env python3
"""Synthetic controls (section 2.3), labelled `ctl`: pgbench -i -s 100 (PostgreSQL) and sysbench oltp_common --tables=8 --table-size=1000000 prepare (MySQL), ext4, same server settings as dbload.py.
usage: dbctl.py <pgbench|sysbench> <n>      -> work/db-ctl-pgbench-pg.raw | db-ctl-sysbench-my.raw, out/<name>.json"""
import sys, os, json, time, subprocess
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import lib
kind, n = sys.argv[1], int(sys.argv[2]); eng = 'pg' if kind == 'pgbench' else 'my'; name = 'db-ctl-%s-%s' % (kind, eng)
img = os.path.join(lib.WORK, name + '.raw'); mnt = os.path.join(lib.MNT, name); uuid = '00000000-0000-4000-8000-%012d' % (10 + n)
PGB = '/usr/lib/postgresql/18/bin'; MYS = os.path.join(lib.ROOT, 'bin', 'mysqld')
run = lambda c, **k: subprocess.run(c, check=True, capture_output=True, text=True, **k)
lib.guard(16, name); run(['rm', '-f', img]); run(['truncate', '-s', '16G', img])
run(['mkfs.ext4', '-q', '-F', '-b', '4096', '-U', uuid, '-E', 'lazy_itable_init=0,lazy_journal_init=0', img]); t0 = time.time(); info = {'name': name, 'dataset': 'ctl-' + kind, 'engine': eng, 'fs': 'ext4', 'control': True}
with lib.loopmount(img, mnt, ro=False, opts='noatime,discard', direct=True) as dev:
    run(['sudo', 'chown', '%d:%d' % (os.getuid(), os.getgid()), mnt])
    d = os.path.join(mnt, 'pg' if eng == 'pg' else 'mysql'); sock = os.path.join(mnt, 'sock'); os.makedirs(sock, exist_ok=True)
    if eng == 'pg':
        run([PGB + '/initdb', '-D', d, '--no-locale', '-E', 'UTF8', '-A', 'trust'])
        opts = "-p 55432 -k %s -c listen_addresses= -c max_wal_size=1GB -c shared_buffers=1GB -c maintenance_work_mem=1GB -c fsync=off -c synchronous_commit=off" % sock
        run([PGB + '/pg_ctl', '-D', d, '-o', opts, '-l', os.path.join(mnt, 'pg.log'), 'start', '-w'])
        try:
            run(['pgbench', '-i', '-s', '100', '-h', sock, '-p', '55432', 'postgres'])
            run([PGB + '/psql', '-h', sock, '-p', '55432', '-d', 'postgres', '-c', 'VACUUM ANALYZE', '-c', 'CHECKPOINT'])
            info['version'] = run([PGB + '/postgres', '--version']).stdout.strip(); info['page_size'] = 8192
        finally: run([PGB + '/pg_ctl', '-D', d, 'stop', '-m', 'fast', '-w'])
    else:
        common = ['--no-defaults', '--basedir=/usr', '--lc-messages-dir=/usr/share/mysql', '--plugin-dir=/usr/lib/mysql/plugin', '--datadir=' + d]
        run([MYS] + common + ['--initialize-insecure', '--log-error=' + os.path.join(mnt, 'init.log')])
        sk = sock + '/m.sock'
        srv = subprocess.Popen([MYS] + common + ['--socket=' + sk, '--port=33306', '--skip-networking', '--skip-log-bin', '--innodb_file_per_table=1',
              '--innodb_buffer_pool_size=1G', '--innodb_redo_log_capacity=512M', '--innodb_flush_log_at_trx_commit=2', '--log-error=' + os.path.join(mnt, 'my.log')])
        try:
            for _ in range(120):
                if subprocess.run(['mysqladmin', '--no-defaults', '-uroot', '--socket=' + sk, 'ping'], capture_output=True).returncode == 0: break
                time.sleep(1)
            run(['mysql', '--no-defaults', '-uroot', '--socket=' + sk, '-e', 'CREATE DATABASE sbtest'])
            run(['sysbench', 'oltp_common', '--db-driver=mysql', '--mysql-socket=' + sk, '--mysql-user=root', '--mysql-db=sbtest', '--tables=8', '--table-size=1000000', '--threads=2', 'prepare'])
            run(['mysql', '--no-defaults', '-uroot', '--socket=' + sk, '-e', 'SET GLOBAL innodb_fast_shutdown=0; FLUSH TABLES'])
            info['version'] = run(['mysqld', '--version']).stdout.strip(); info['page_size'] = 16384
        finally:
            subprocess.run(['mysqladmin', '--no-defaults', '-uroot', '--socket=' + sk, 'shutdown'], capture_output=True); srv.wait(timeout=600)
    info['fstrim'] = subprocess.run(['sudo', 'fstrim', '-v', mnt], capture_output=True, text=True).stdout.strip().split(':')[-1]
info['load_seconds'] = round(time.time() - t0); run(['fallocate', '-d', img])
json.dump(info, open(os.path.join(lib.OUT, name + '.json'), 'w')); print(info)
