#!/usr/bin/env python3 -I
"""Non-git corpora: synthetic logs, loghub real logs, Maven jars (.class), npm wasm, apt ELF, derived sqlite. Appends to manifest.tsv.
usage: extras.py CORPUS_DIR STEP   (STEP in logs, loghub, jars, wasm, elf, sqlite)"""
import sys, os, re, io, json, random, hashlib, subprocess, urllib.request, zipfile, tarfile, shutil, glob, sqlite3, csv
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import taxonomy as T, gen_logs
corpus, step = sys.argv[1], sys.argv[2]
man = open(os.path.join(corpus, 'manifest.tsv'), 'a'); lock = open(os.path.join(corpus, 'sources.lock.extra.tsv'), 'a')
seen = {l.split('\t')[5] for l in open(os.path.join(corpus, 'manifest.tsv')).read().splitlines()[1:]}
def add(project, ty, name, data, spdx, eco='main'):
    h = hashlib.sha256(data).hexdigest()
    if h in seen or not data: return False
    seen.add(h); d = os.path.join(corpus, 'files', project); os.makedirs(d, exist_ok=True)
    open(os.path.join(d, h[:12]), 'wb').write(data)
    man.write(f'{h[:12]}\t{project}\t{ty}\t{name}\t{len(data)}\t{h}\t{spdx}\t{T.policy(spdx)}\t{eco}\n'); return True
def get(url):
    req = urllib.request.Request(url, headers={'User-Agent': 'dictft-study'})
    return urllib.request.urlopen(req, timeout=120).read()
def log(*a): sys.stderr.write(' '.join(map(str, a)) + '\n')

if step == 'logs':
    for ty, g in gen_logs.GEN.items():
        for prof in range(5):
            r = random.Random(f'{ty}{prof}')
            for i, sz in enumerate(gen_logs.sizes(r, 30)):
                add(f'gen-{ty}-{prof}', ty, f'{ty}.log.{i}' if i else f'{ty}.log', g(r, prof, sz), 'CC0-1.0', 'new' if prof == 4 else 'main')
    log('logs done')
elif step == 'loghub':
    d = '/tmp/dictft-loghub'; shutil.rmtree(d, ignore_errors=True)
    subprocess.run(['git', 'clone', '-q', '--depth', '1', 'https://github.com/logpai/loghub', d], check=True)
    commit = subprocess.run(['git', '-C', d, 'rev-parse', 'HEAD'], capture_output=True, text=True).stdout.strip()
    lock.write(f'loghub\thttps://github.com/logpai/loghub\t{commit}\tresearch-only\tmain\n')
    SYS = {'Linux', 'OpenSSH', 'Mac', 'Android', 'Windows', 'Thunderbird', 'BGL', 'HPC'}
    for p in glob.glob(d + '/*/*.log'):
        sub = p.split('/')[-2]; data = open(p, 'rb').read()
        ty = 'syslog' if sub in SYS else ('access' if sub == 'Apache' and False else 'applog')
        # real logs are test only (licence: research use) -> spdx 'research-only' -> policy excluded
        for k in range(0, len(data), 1 << 18):   # split in 256 KiB rotated-file-like pieces
            add(f'loghub-{sub}', ty, os.path.basename(p), data[k:k + (1 << 18)], 'research-only', 'new')
    shutil.rmtree(d, ignore_errors=True)
elif step == 'jars':
    J = [('org/apache/commons/commons-lang3/3.17.0/commons-lang3-3.17.0.jar', 'Apache-2.0'), ('commons-io/commons-io/2.17.0/commons-io-2.17.0.jar', 'Apache-2.0'),
     ('org/apache/commons/commons-collections4/4.4/commons-collections4-4.4.jar', 'Apache-2.0'), ('com/google/code/gson/gson/2.11.0/gson-2.11.0.jar', 'Apache-2.0'),
     ('com/google/guava/guava/33.3.1-jre/guava-33.3.1-jre.jar', 'Apache-2.0'), ('com/fasterxml/jackson/core/jackson-databind/2.18.0/jackson-databind-2.18.0.jar', 'Apache-2.0'),
     ('org/slf4j/slf4j-api/2.0.16/slf4j-api-2.0.16.jar', 'MIT'), ('org/jsoup/jsoup/1.18.1/jsoup-1.18.1.jar', 'MIT'), ('joda-time/joda-time/2.12.7/joda-time-2.12.7.jar', 'Apache-2.0'),
     ('org/yaml/snakeyaml/2.3/snakeyaml-2.3.jar', 'Apache-2.0'), ('org/apache/commons/commons-compress/1.27.1/commons-compress-1.27.1.jar', 'Apache-2.0'),
     ('org/apache/logging/log4j/log4j-core/2.24.1/log4j-core-2.24.1.jar', 'Apache-2.0'), ('org/jetbrains/kotlin/kotlin-stdlib/2.0.20/kotlin-stdlib-2.0.20.jar', 'Apache-2.0'),
     ('com/h2database/h2/2.3.232/h2-2.3.232.jar', 'MPL-2.0'), ('junit/junit/4.13.2/junit-4.13.2.jar', 'EPL-1.0'), ('org/ow2/asm/asm/9.7/asm-9.7.jar', 'BSD-3-Clause'),
     ('org/hibernate/orm/hibernate-core/6.6.1.Final/hibernate-core-6.6.1.Final.jar', 'LGPL-2.1-only'), ('org/postgresql/postgresql/42.7.4/postgresql-42.7.4.jar', 'BSD-2-Clause'),
     ('io/netty/netty-buffer/4.1.114.Final/netty-buffer-4.1.114.Final.jar', 'Apache-2.0'), ('org/eclipse/jetty/jetty-server/11.0.24/jetty-server-11.0.24.jar', 'Apache-2.0')]
    for path, lic in J:
        name = path.split('/')[-1][:-4]
        try: z = zipfile.ZipFile(io.BytesIO(get('https://repo1.maven.org/maven2/' + path)))
        except Exception as e: log('jar fail', name, e); continue
        lock.write(f'jar:{name}\thttps://repo1.maven.org/maven2/{path}\t{hashlib.sha256(open(os.devnull,"rb").read()).hexdigest()[:0]}\t{lic}\tmain\n')
        cl = [n for n in z.namelist() if n.endswith('.class')]; random.Random(name).shuffle(cl)
        for n in cl[:1500]: add('jar-' + name, 'class', os.path.basename(n), z.read(n), lic)
        log('jar', name, len(cl))
elif step == 'wasm':
    P = ['sql.js', 'esbuild-wasm', 'web-tree-sitter', 'tree-sitter-wasms', '@swc/wasm-web', 'brotli-wasm', '@sqlite.org/sqlite-wasm', 'vscode-oniguruma', 'yoga-wasm-web',
         '@dqbd/tiktoken', '@resvg/resvg-wasm', 'quickjs-emscripten', '@duckdb/duckdb-wasm', 'wabt', 'binaryen', 'argon2-browser', 'libsodium-sumo', '@ffmpeg/core', 'pyodide', 'z3-solver',
         '@jsquash/webp', '@jsquash/avif', '@jsquash/oxipng', 'shiki', 'lightningcss-wasm', 'sass-embedded-wasm', '@parcel/css-wasm', 'wasm-pack', 'typst', '@myriaddreamin/typst-ts-web-compiler']
    for pk in P:
        try:
            meta = json.loads(get('https://registry.npmjs.org/' + pk.replace('/', '%2F') + '/latest')); tb = meta['dist']['tarball']
            lic = meta.get('license', 'unknown'); lic = lic if isinstance(lic, str) else 'unknown'
            data = get(tb); lock.write(f'npm:{pk}\t{tb}\t{hashlib.sha256(data).hexdigest()}\t{lic}\tmain\n')
            t = tarfile.open(fileobj=io.BytesIO(data)); n = 0
            for m in t.getmembers():
                if m.isfile() and m.name.endswith('.wasm') and m.size < 40 << 20:
                    add('npm-' + pk.replace('/', '_').lstrip('@'), 'wasm', os.path.basename(m.name), t.extractfile(m).read(), lic); n += 1
            log('wasm', pk, n)
        except Exception as e: log('wasm fail', pk, str(e)[:80])
elif step == 'elf':
    PK = {'zlib1g': 'Zlib', 'libzstd1': 'BSD-3-Clause', 'liblz4-1': 'BSD-2-Clause', 'libsqlite3-0': 'Public-Domain', 'libpcre2-8-0': 'BSD-3-Clause', 'libssl3t64': 'Apache-2.0',
          'libbrotli1': 'MIT', 'libyaml-0-2': 'MIT', 'liblua5.4-0': 'MIT', 'libuv1t64': 'MIT', 'libjansson4': 'MIT', 'libxxhash0': 'BSD-2-Clause', 'libcurl4t64': 'curl',
          'libexpat1': 'MIT', 'libffi8': 'MIT', 'libpng16-16t64': 'libpng', 'libjpeg-turbo8': 'BSD-3-Clause', 'jq': 'MIT', 'redis-server': 'BSD-3-Clause', 'nginx': 'BSD-2-Clause',
          'libc6': 'LGPL-2.1-or-later', 'libstdc++6': 'GPL-3.0-or-later', 'bash': 'GPL-3.0-or-later', 'coreutils': 'GPL-3.0-or-later', 'grep': 'GPL-3.0-or-later', 'sed': 'GPL-3.0-or-later',
          'gawk': 'GPL-3.0-or-later', 'libglib2.0-0t64': 'LGPL-2.1-or-later', 'libgnutls30t64': 'LGPL-2.1-or-later', 'libsystemd0': 'LGPL-2.1-or-later', 'libgmp10': 'LGPL-3.0',
          'python3.14-minimal': 'Python-PSF', 'libpython3.14-minimal': 'Python-PSF', 'openssh-client': 'BSD-2-Clause', 'e2fsprogs': 'GPL-2.0-only', 'busybox-static': 'GPL-2.0-only',
          'vim-tiny': 'Vim', 'util-linux': 'GPL-2.0-only', 'git': 'GPL-2.0-only', 'libxml2': 'MIT', 'libgcrypt20': 'LGPL-2.1-or-later', 'ffmpeg': 'GPL-2.0-or-later', 'libavcodec61': 'LGPL-2.1-or-later'}
    work = '/tmp/dictft-apt'; shutil.rmtree(work, ignore_errors=True); os.makedirs(work)
    for pk, lic in PK.items():
        r = subprocess.run(['apt-get', 'download', pk], cwd=work, capture_output=True, text=True)
        debs = glob.glob(f'{work}/{pk}_*.deb')
        if not debs: log('elf fail', pk); continue
        x = f'{work}/x-{pk}'; os.makedirs(x); subprocess.run(['dpkg-deb', '-x', debs[0], x], check=True)
        lock.write(f'deb:{pk}\tdistro-archive\t{os.path.basename(debs[0])}\t{lic}\tmain\n'); n = 0
        for d, _, fs in os.walk(x):
            for f in fs:
                p = os.path.join(d, f)
                if os.path.islink(p) or not os.path.isfile(p): continue
                with open(p, 'rb') as fh: hd = fh.read(4)
                if hd == b'\x7fELF' and os.path.getsize(p) < 40 << 20 and n < 200:
                    n += add('deb-' + pk, 'elf', f, open(p, 'rb').read(), lic)
        shutil.rmtree(x); os.remove(debs[0]); log('elf', pk, n)
    shutil.rmtree(work, ignore_errors=True)
elif step == 'sqlite':
    rows = [l.split('\t') for l in open(os.path.join(corpus, 'manifest.tsv')).read().splitlines()[1:]]
    byp = {}
    for r in rows:
        if r[2] in ('csv',) and r[7] in ('perm', 'copyleft', 'excluded') and int(r[4]) < (4 << 20): byp.setdefault(r[1], []).append(r)
    tmp = '/tmp/dictft-sql'; os.makedirs(tmp, exist_ok=True)
    for pj, fl in byp.items():
        rng = random.Random('sq' + pj); rng.shuffle(fl)
        for k in range(0, min(len(fl), 120), 4):
            grp = fl[k:k + 4]; path = f'{tmp}/{pj}-{k}.db'
            if os.path.exists(path): os.remove(path)
            con = sqlite3.connect(path); con.execute(f'PRAGMA page_size={rng.choice([1024, 4096, 4096, 8192])}')
            for j, r in enumerate(grp):
                try:
                    rd = list(csv.reader(open(f'{corpus}/files/{r[1]}/{r[0]}', newline='', errors='replace')))
                    h = rd[0]; w = len(h); con.execute(f'create table t{j} (' + ','.join(f'c{i} text' for i in range(w)) + ')')
                    con.executemany(f'insert into t{j} values (' + ','.join('?' * w) + ')', [x for x in rd[1:] if len(x) == w])
                    if rng.random() < .5: con.execute(f'create index i{j} on t{j}(c0)')
                except Exception: pass
            con.commit(); con.close()
            if os.path.getsize(path) > 8192: add('sqlite-from-' + pj, 'sqlite', os.path.basename(path), open(path, 'rb').read(), r[6], r[8])
            os.remove(path)
    shutil.rmtree(tmp, ignore_errors=True)
elif step == 'jsonl':      # derived: record-array JSON files rewritten one record per line (licence of the source file)
    rows = [l.split('\t') for l in open(os.path.join(corpus, 'manifest.tsv')).read().splitlines()[1:]]
    for r in rows:
        if r[2] != 'json' or not (4096 < int(r[4]) < (24 << 20)) or r[7] == 'excluded' and r[6] in ('research-only',): continue
        try: obj = json.load(open(f'{corpus}/files/{r[1]}/{r[0]}', encoding='utf-8'))
        except Exception: continue
        if isinstance(obj, dict) and len(obj) == 1 and isinstance(next(iter(obj.values())), list): obj = next(iter(obj.values()))
        if not (isinstance(obj, list) and len(obj) >= 40 and all(isinstance(x, dict) for x in obj[:20])): continue
        rng = random.Random(r[0]); n = len(obj)
        for k in range(rng.choice([1, 2, 3])):
            m = max(20, int(n * 2 ** -rng.uniform(0, 4))); a = rng.randrange(0, n - m + 1)
            data = ''.join(json.dumps(x, separators=(',', ':'), ensure_ascii=False) + '\n' for x in obj[a:a + m]).encode()
            add('jsonl-from-' + r[1], 'jsonl', r[3].rsplit('.', 1)[0] + f'-{k}.jsonl', data, r[6], r[8])
man.close()
