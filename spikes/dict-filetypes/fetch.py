#!/usr/bin/env python3 -I
"""Clone sources shallowly, sample files per (project,type), write corpus + manifest.tsv + sources.lock.tsv.
usage: fetch.py PROJECTS.tsv CORPUS_DIR [--lock sources.lock.tsv] [--only name,name]"""
import sys, os, re, subprocess, hashlib, random, shutil, csv, json
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import taxonomy as T
MAXF, MAXB, MAXFILE = 1500, 32 << 20, 24 << 20
SKIP_DIRS = {'.git','node_modules','vendor','third_party','thirdparty','3rdparty','deps','external','.svn'}

def sh(cmd, cwd=None, check=True):
    return subprocess.run(cmd, cwd=cwd, check=check, capture_output=True, text=True)

LIC_PATTERNS = [  # (regex on LICENSE text, SPDX)  -- first match wins; heuristic, recorded as 'detected'
 (r'GNU AFFERO GENERAL PUBLIC LICENSE', 'AGPL-3.0'),
 (r'GNU LESSER GENERAL PUBLIC LICENSE\s+Version 3', 'LGPL-3.0'),
 (r'GNU (LESSER|LIBRARY) GENERAL PUBLIC LICENSE\s+Version 2', 'LGPL-2.1-or-later'),
 (r'GNU GENERAL PUBLIC LICENSE\s+Version 3', 'GPL-3.0-or-later'),
 (r'GNU GENERAL PUBLIC LICENSE\s+Version 2', 'GPL-2.0-only'),
 (r'Mozilla Public License,? [Vv]ersion 2', 'MPL-2.0'),
 (r'Apache License\s+Version 2\.0', 'Apache-2.0'),
 (r'Eclipse Public License', 'EPL-2.0'),
 (r'Permission is hereby granted, free of charge', 'MIT'),
 (r'This is free and unencumbered software', 'Unlicense'),
 (r'Redistribution and use in source and binary forms', 'BSD-3-Clause'),
 (r'Permission to use, copy, modify, and/or distribute this software', 'ISC'),
 (r'Permission to use, copy, modify, and distribute this software', 'ISC'),
 (r'zlib License|freely, subject to the following restrictions', 'Zlib'),
 (r'Creative Commons|CC0', 'CC-other'),
 (r'GNU Free Documentation', 'GFDL'),
 (r'The Artistic License|Artistic License', 'Artistic-2.0'),
 (r'PostgreSQL', 'PostgreSQL'),
]
def detect_project_license(root):
    best = []
    for n in sorted(os.listdir(root)):
        if re.match(r'(?i)(licen[sc]e|copying|unlicense)', n) and os.path.isfile(os.path.join(root,n)):
            try: txt = open(os.path.join(root,n), errors='replace').read(60000)
            except Exception: continue
            for rx, spdx in LIC_PATTERNS:
                if re.search(rx, txt): best.append(spdx); break
    return best[0] if best else 'unknown'
OVERRIDE = {  # known project licences where the root text is ambiguous (engineering notes; re-check)
 'sqlite':'Public-Domain','curl':'curl','postgres':'PostgreSQL','zlib':'Zlib','lua':'MIT','wordpress':'GPL-2.0-or-later',
 'git':'GPL-2.0-only','busybox':'GPL-2.0-only','linux':'GPL-2.0-only','nginx':'BSD-2-Clause','libuv':'MIT','redis':'BSD-3-Clause',
 'openjdk':'GPL-2.0-only','ansible':'GPL-3.0-or-later','perl5':'Artistic-1.0-or-GPL-1.0+','vue':'MIT','llvm':'Apache-2.0-LLVM','gcc-ada':'GPL-3.0-or-later',
 'ffmpeg-asm':'LGPL-2.1-or-later','php-src':'PHP-3.01','python-peps':'Public-Domain','cpython':'Python-PSF','django':'BSD-3-Clause','django-po':'BSD-3-Clause',
 'fivethirtyeight':'CC-BY-4.0','covid-owid':'CC-BY-4.0','stacks-project':'GFDL','loghub':'research-only','r-source':'GPL-2.0-or-later',
 'pandoc':'GPL-2.0-or-later','shellcheck':'GPL-3.0-or-later','gnucobol':'GPL-3.0-or-later','kotlin-stdlib':'Apache-2.0','nodejs':'MIT','openssh':'BSD-2-Clause',
 'systemd':'LGPL-2.1-or-later','inkscape-po':'GPL-2.0-or-later','odoo':'LGPL-3.0','awesome':'GPL-2.0-or-later','ejabberd':'GPL-2.0-or-later',
 'exiftool':'Artistic-1.0-or-GPL-1.0+','knitr':'GPL-2.0-or-later','shiny':'GPL-3.0-or-later','discourse':'GPL-2.0-or-later','schemastore':'Apache-2.0',
 'bcd':'CC0-1.0','openapi-dir':'CC0-1.0','simple-icons':'CC0-1.0','font-awesome':'CC-BY-4.0','vega-datasets':'BSD-3-Clause','tidytuesday':'CC0-1.0',
 'k8s-website':'CC-BY-4.0','openblas':'BSD-3-Clause','alire':'GPL-3.0-or-later','gnatcoll':'GPL-3.0-or-later','als':'GPL-3.0-or-later','gprbuild':'GPL-3.0-or-later',
 'httpd':'Apache-2.0','pgf':'GPL-2.0-or-later','latex3':'LPPL-1.3c','latexindent':'GPL-3.0-or-later','mojolicious':'Artistic-2.0','moose':'Artistic-1.0-or-GPL-1.0+',
}
HDR = [  # per-file header heuristics (first 4 KiB of text)
 (re.compile(rb'SPDX-License-Identifier:\s*([A-Za-z0-9.\-+() ]+?)(?:\s*\*/|\s*-->|\n|$)'), None),
 (re.compile(rb'either version 2 of the License, or\s+\(?at your option\)? any later version', re.I|re.S), 'GPL-2.0-or-later'),
 (re.compile(rb'(either )?version 3 of the License|GNU General Public License.{0,60}version 3', re.I|re.S), 'GPL-3.0-or-later'),
 (re.compile(rb'GNU General Public License', re.I), 'GPL-2.0-only'),   # unclear version => treated as the stricter reading
 (re.compile(rb'GNU (Lesser|Library) General Public', re.I), 'LGPL-2.1-or-later'),
]
def norm_spdx(s):
    s = s.strip().replace('GPL-2.0+','GPL-2.0-or-later').replace('GPL-2.0 OR','GPL-2.0-or-later OR')
    if ' OR ' in s or ' or ' in s:      # dual licence: take the most permissive alternative present
        parts = [norm_spdx(p) for p in re.split(r'\s+OR\s+|\s+or\s+', s.strip('() '))]
        for rank in ('perm','copyleft'):
            for p in parts:
                if T.policy(p)==rank: return p
        return parts[0]
    s = s.strip('() ')
    s = {'GPL-2.0':'GPL-2.0-only','GPL-3.0':'GPL-3.0-only','LGPL-2.1':'LGPL-2.1-only','GPL-2.0-only WITH Linux-syscall-note':'GPL-2.0-only','MIT-0':'MIT'}.get(s, s)
    return re.sub(r' WITH .*','',s)
def file_license(head, proj):
    for rx, spdx in HDR:
        m = rx.search(head)
        if m:
            if spdx is None: return norm_spdx(m.group(1).decode('latin1'))
            if proj in ('MIT','BSD-2-Clause','BSD-3-Clause','Apache-2.0','ISC','Zlib') and spdx.startswith('GPL'): continue
            return spdx
    return None

def clone(name, url, ref, sparse, dest, lock):
    os.makedirs(dest, exist_ok=True)
    sh(['git','init','-q'], dest); sh(['git','remote','add','origin',url], dest)
    if sparse != '-':
        sh(['git','sparse-checkout','init','--cone'], dest)
        sh(['git','sparse-checkout','set']+sparse.split(','), dest)
    flt = ['--filter=blob:none'] if sparse != '-' else []
    want = lock.get(name) or (ref if ref != '-' else 'HEAD')
    r = sh(['git','fetch','-q','--depth','1']+flt+['origin',want], dest, check=False)
    if r.returncode != 0 and want != 'HEAD':
        sys.stderr.write(f'{name}: ref {want} failed ({r.stderr.strip()[:100]}), using HEAD\n')
        r = sh(['git','fetch','-q','--depth','1']+flt+['origin','HEAD'], dest, check=False)
    if r.returncode != 0: raise RuntimeError(r.stderr[:300])
    sh(['git','checkout','-q','FETCH_HEAD'], dest)
    return sh(['git','rev-parse','HEAD'], dest).stdout.strip()

def walk(root):
    for d, ds, fs in os.walk(root):
        ds[:] = [x for x in ds if x not in SKIP_DIRS]
        for f in fs:
            p = os.path.join(d, f)
            if os.path.islink(p): continue
            yield p

def main():
    projfile, corpus = sys.argv[1], sys.argv[2]
    lockfile = sys.argv[sys.argv.index('--lock')+1] if '--lock' in sys.argv else None
    only = set(sys.argv[sys.argv.index('--only')+1].split(',')) if '--only' in sys.argv else None
    lock = {}
    if lockfile and os.path.exists(lockfile):
        for l in open(lockfile):
            p = l.rstrip('\n').split('\t')
            if len(p) >= 3: lock[p[0]] = p[2]
    os.makedirs(corpus, exist_ok=True)
    seen = set()
    man = open(os.path.join(corpus,'manifest.tsv'), 'a')
    lk = open(os.path.join(corpus,'sources.lock.new.tsv'), 'a')
    if man.tell()==0: man.write('id\tproject\ttype\tname\tsize\tsha256\tfile_spdx\tpolicy\teco\n')
    for l in open(os.path.join(corpus,'manifest.tsv')).read().splitlines()[1:]:
        seen.add(l.split('\t')[5])
    done = {l.split('\t')[0] for l in open(os.path.join(corpus,'sources.lock.new.tsv'))}
    for line in open(projfile):
        if line.startswith('#') or not line.strip(): continue
        name, url, ref, sparse, eco = line.rstrip('\n').split('\t')[:5]
        if (only and name not in only) or name in done: continue
        tmp = os.path.join(corpus, '_clone', name)
        try:
            shutil.rmtree(tmp, ignore_errors=True)
            commit = clone(name, url, ref, sparse, tmp, lock)
        except Exception as e:
            sys.stderr.write(f'FAIL {name}: {e}\n'); shutil.rmtree(tmp, ignore_errors=True); continue
        proj = OVERRIDE.get(name) or detect_project_license(tmp)
        groups = {}
        for p in walk(tmp):
            ty = T.classify_name(p)
            if not ty: continue
            try: sz = os.path.getsize(p)
            except OSError: continue
            if sz == 0 or sz > MAXFILE: continue
            groups.setdefault(ty, []).append((p, sz))
        n = 0
        for ty, fl in groups.items():
            rng = random.Random(f'{name}/{ty}'); fl.sort(); rng.shuffle(fl)
            tot = cnt = 0
            cf, cb = (MAXF, MAXB) if ty not in T.CONTROL_TYPES+['pdf'] else (12, 6 << 20)
            for p, sz in fl:
                if cnt >= cf or tot + sz > cb: continue
                data = open(p,'rb').read()
                h = hashlib.sha256(data).hexdigest()
                if h in seen: continue
                seen.add(h)
                lic = file_license(data[:4096], proj) if ty in T.TEXT_TYPES else None
                spdx = lic or proj
                if ty in T.TEXT_TYPES and not lic and proj == 'unknown': spdx = 'unknown'
                fid = h[:12]
                d = os.path.join(corpus, 'files', name); os.makedirs(d, exist_ok=True)
                open(os.path.join(d, fid), 'wb').write(data)
                man.write(f'{fid}\t{name}\t{ty}\t{os.path.basename(p)}\t{sz}\t{h}\t{spdx}\t{T.policy(spdx)}\t{eco}\n')
                tot += sz; cnt += 1; n += 1
        man.flush()
        lk.write(f'{name}\t{url}\t{commit}\t{proj}\t{eco}\n'); lk.flush()
        shutil.rmtree(tmp, ignore_errors=True)
        sys.stderr.write(f'{name}: {n} files, licence {proj}, {commit[:10]}\n')
main()
