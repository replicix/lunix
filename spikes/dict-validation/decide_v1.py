#!/usr/bin/env python3
"""Applies the decision table D1-D7 (PREREGISTERED.md 1.2) mechanically to analyze.py's output and prints the SUMMARY tables (markdown).
usage: decide.py <ana/out> <raw-dir>      (raw-dir holds shares.csv, db_shares.csv, db_sweep.csv, timing.csv; `decide.py raw raw` works from bench/.../raw)
writes <ana/out>/votes.csv, <ana/out>/tables.md.  Strict reading decides; every other reading is a sensitivity line."""
import sys, os, csv, json, statistics
OUT, RAW = sys.argv[1:3]
def opn(n): return open(os.path.join(OUT, n) if os.path.exists(os.path.join(OUT, n)) else os.path.join(OUT, 'analysis-' + n))     # ana/out or raw/ (analysis-*)
T = list(csv.DictReader(opn('tests.csv'))); DS = list(csv.DictReader(opn('desc.csv')))
META = json.load(opn('meta.json'))
ELIG = ['host-ext4', 'host-xfs', 'host-btrfs', 'vm-ubuntu']; EXPL = ['vm-debian', 'vm-fedora']
DSETS = ['se-askubuntu', 'se-superuser', 'se-serverfault', 'geonames', 'tlc-2024q1']; ENG = ['pg', 'my', 'sq']
DB = ['db-%s-%s' % (d, e) for d in DSETS for e in ENG]; CTL = ['db-ctl-pgbench-pg', 'db-ctl-sysbench-my']
fl = lambda x: float(x) if x not in ('', None) else float('nan')
def pp(x, d=2): return '%.*f' % (d, 100 * fl(x))
def num(x, d=2): return '%.*f' % (d, fl(x)) if x not in ('', None) else 'n/a'
def tests(**k): return [r for r in T if all(str(r[a]) == str(b) for a, b in k.items())]
def one(**k):
    r = tests(**k); assert len(r) == 1, (k, len(r)); return r[0]
def desc(key, arm, mask='full', cls='all', shift='0', comp=None):
    r = [d for d in DS if d['key'] == key and d['arm'] == arm and d['mask'] == mask and d['cls'] == cls and d['shift'] == shift and (comp is None or d['comp'] == comp)]
    return r[0] if r else None
VOTES = []; md = []
def vote(dec, reading, size, n, need, verdict, note=''): VOTES.append(dict(decision=dec, reading=reading, size=size, count=n, needed=need, verdict=verdict, note=note))
P = lambda r, rd: int(r['pass' if rd == 'strict' else 'len_pass'])
# ---------------------------------------------------------------- D1
D1 = {}
for sh in ('0', '4'):
    for cs in ('16384', '65536'):
        arms = ['ft@1', 'ft@3', 'bs-dist@1', 'bs-dist@3']
        per_arm = {a: sum(P(one(family='D1', corpus=c, size=cs, arm=a, shift=sh), 'strict') for c in ELIG) for a in arms}
        any_arm = sum(any(P(one(family='D1', corpus=c, size=cs, arm=a, shift=sh), 'strict') for a in arms) for c in ELIG)
        best = max(per_arm.values()); go = best == 4
        orc = {c: max(fl(desc('%s-%s' % (c, cs), 'oracle@%d' % L, shift=sh)['LB']) for L in (1, 3)) for c in ELIG}
        decisive = sum(v < 0.03 for v in orc.values()) >= 4
        tag = '' if sh == '0' else ' (16 MiB blocks)'
        vote('D1', 'strict: same set and level on all 4 eligible' + tag, cs, best, 4, 'GO' if go else 'NO-GO', 'decisive (oracle LB < 3 pp on all 4)' if decisive else 'oracle LB >= 3 pp on %d corpora' % sum(v >= 0.03 for v in orc.values()))
        vote('D1', 'any set/level per corpus' + tag, cs, any_arm, 4, 'GO' if any_arm == 4 else 'NO-GO')
        vote('D1', 'sensitivity 3 of 4 (never decides)' + tag, cs, best, 3, 'GO' if best >= 3 else 'NO-GO')
        if sh == '0': D1[cs] = dict(go=go, best=best, orc=orc, decisive=decisive)
D1_fires = any(v['go'] for v in D1.values())
# ---------------------------------------------------------------- D2
D2 = {}
for sh in ('0', '4'):
    for rd in ('strict', 'lenient'):
        for cs in ('16384', '65536'):
            for fam, vols, need in (('raw', ELIG, 4), ('db', DB, 8)):
                rows = [r for r in tests(family='D2', size=cs, shift=sh) if r['corpus'] in vols]
                if sh == '4':                                                  # 16 MiB where it was run, 1 MiB elsewhere (database images: se-askubuntu-pg only)
                    have = {(r['corpus'], r['arm']) for r in rows}
                    rows += [r for r in tests(family='D2', size=cs, shift='0') if r['corpus'] in vols and (r['corpus'], r['arm']) not in have]
                if not rows: continue
                n = len({r['corpus'] for r in rows if P(r, rd)}); fixedL = '3' if cs == '16384' else '1'
                n42 = len({r['corpus'] for r in rows if P(r, rd) and r['arm'].endswith('@' + fixedL)})
                tag = (' (16 MiB blocks)' if sh == '4' else '')
                vote('D2-' + fam, rd + ' reading, either level' + tag, cs, n, need, 'reached' if n >= need else 'not reached')
                vote('D2-' + fam, rd + ' reading, level fixed to the CF level (42-test reading)' + tag, cs, n42, need, 'reached' if n42 >= need else 'not reached')
                if sh == '0': D2[(rd, cs, fam)] = n
                if sh == '0' and rd == 'strict':                               # what the cap does: strict N and Holm, cap not applied (never decides)
                    nc = len({r['corpus'] for r in rows if r['N'] != '' and fl(r['N']) >= 2.0 and fl(r['holm']) < 0.05 and r['underpowered'] == '0'})
                    vote('D2-' + fam, 'strict N, cap not applied (shows the cap; never decides)', cs, nc, need, 'reached' if nc >= need else 'not reached')
def d2_fires(rd, fam, need):
    prim = D2[(rd, '16384', fam)] >= need; conf = D2[(rd, '65536', fam)] >= need
    return prim and conf, prim
D2res = {(rd, fam): d2_fires(rd, fam, need) for rd in ('strict', 'lenient') for fam, need in (('raw', 4), ('db', 8))}
D2_fires = D2res[('strict', 'raw')][0] or D2res[('strict', 'db')][0]
# ---------------------------------------------------------------- D3
D3_fires = False
# ---------------------------------------------------------------- D4, D6 (per engine, median over the 5 datasets)
def engine_pass(dec, fam, arm, size, cls, thr, rd='strict', sh='0'):
    res = {}
    for e in ENG:
        rows = [one(family=fam, decision=dec, corpus='db-%s-%s' % (d, e), arm=arm, size=size, shift=sh) for d in DSETS]
        Ns = [fl(r['N'] if rd == 'strict' or not r.get('len_N') else r['len_N']) for r in rows]
        sig = sum(1 for r in rows if fl(r['holm']) < 0.05 and fl(r['S']) > 0 and r['cap_ok'] == '1')
        res[e] = (statistics.median(Ns), sig, all(r['cap_ok'] == '1' for r in rows))
    n = sum(1 for e in ENG if res[e][0] >= thr and res[e][1] >= 3 and res[e][2])
    return res, n
D4L = {L: engine_pass('D4-L', 'D4', 'dbd@%d' % L, '16384', 'dbpage', 5.0) for L in (1, 3)}
D4C = engine_pass('D4-C', 'D4', 'dbd@3', '1048576', 'all', 5.0)
D6r = engine_pass('D6', 'D6', 'dbd@3', '1048576', 'all', 3.0); D6r4 = engine_pass('D6', 'D6', 'dbd@3', '4194304', 'all', 3.0)
D6_fires = D6r[1] >= 2
for L in (1, 3): vote('D4-L', 'engines with median N >= 5 pp (dbd@%d, DB-page class, 16 KiB)' % L, '16384', D4L[L][1], 2, 'GO' if D4L[L][1] >= 2 and D1_fires else 'NO-GO', 'consumer D1 %s' % ('fires' if D1_fires else 'does not fire'))
vote('D4-C', 'engines with median N >= 5 pp (dbd@3, 1 MiB)', '1048576', D4C[1], 2, 'GO' if D4C[1] >= 2 and D6_fires else 'NO-GO', 'consumer D6 %s' % ('fires' if D6_fires else 'does not fire'))
vote('D6', 'engines with median N >= 3 pp (dbd@3, 1 MiB)', '1048576', D6r[1], 2, 'GO' if D6_fires else 'NO-GO')
vote('D6', 'reported: 4 MiB', '4194304', D6r4[1], 2, 'reported only')
pgref = {n: desc('cv-%s-pg' % n, 'dbd@3', mask='all') for n in DB}
vote('D6', 'page-size reference: images with LB >= 15 pp', 'page', sum(1 for d in pgref.values() if d and fl(d['LB']) >= 0.15), 1, 'open question' if any(d and fl(d['LB']) >= 0.15 for d in pgref.values()) else 'no open question')
# ---------------------------------------------------------------- D5 (no saving measurement; cap from the timing cells)
TM = {(r['cell'], r['arm'], r['level'], r['op']): r for r in csv.DictReader(open(os.path.join(RAW, 'timing.csv')))}
def tcost(cell, arm, lv, op): return float(TM[(cell, arm, lv, op)]['median'])
def tst(cell, arm, lv): return float(TM[(cell, arm, lv, 'compress')]['stored_bytes'])
D5 = {}
for cell in ('vm-ubuntu-files', 'vm-ubuntu-files-lt64k'):
    D5[cell] = dict(cap_c=tcost(cell, 'z3', '3', 'compress') / tcost(cell, 'ft', '3', 'compress'), cap_d=tcost(cell, 'z3', '3', 'decompress') / tcost(cell, 'ft', '3', 'decompress'),
                    S=1 - tst(cell, 'ft', '3') / tst(cell, 'z3', '3'), dcores=max(0, tcost(cell, 'ft', '3', 'compress') - tcost(cell, 'z3', '3', 'compress')) * 1.0,
                    z4cap=tcost(cell, 'z3', '3', 'compress') / tcost(cell, 'z4', '4', 'compress'))
vote('D5', 'hard cap (ft@3 vs z3, vm-ubuntu file objects; the saving vote was not measured)', '4194304', 0, 3, 'NO-GO (hard cap)', 'compress ratio %.2f all objects, %.2f under 64 KiB' % (D5['vm-ubuntu-files']['cap_c'], D5['vm-ubuntu-files-lt64k']['cap_c']))
for dec, fires in (('D7-Lunix', not (D1_fires or D2_fires)), ('D7-Constellation', not D6_fires), ('D7-zdicts', not (D3_fires or (D4L[3][1] >= 2 and D1_fires) or (D4C[1] >= 2 and D6_fires)))):
    vote(dec, 'derived', '', '', '', 'fires (no dictionary work)' if fires else 'does not fire')
with open(os.path.join(OUT, 'votes.csv'), 'w', newline='') as fh:
    w = csv.DictWriter(fh, fieldnames=list(VOTES[0])); w.writeheader(); w.writerows(VOTES)
# ================================================================ markdown tables
def h(s): md.append('\n#### ' + s + '\n')
def row(*c): md.append('| ' + ' | '.join(str(x) for x in c) + ' |')
SH = {(r['corpus'], r['chunk']): r for r in csv.DictReader(open(os.path.join(RAW, 'shares.csv')))}
h('Q1 shares (raw devices)')
row('corpus', 'chunk', 'unique non-hole', '1 MiB blocks', 'holes (of chunks)', 'duplicates (of non-hole)', 'probe-raw (bytes)', 'vote'); row(*['---'] * 8)
for c in ELIG + EXPL:
    for cs in ('16384', '65536'):
        s = SH[(c, cs)]; nh = int(s['chunks_total']) - int(s['holes'])
        row(c, {'16384': '16', '65536': '64'}[cs] + ' KiB', '%.2f GiB' % (int(s['unique_bytes']) / 2**30), s['blocks_1MiB_nonhole'], '%.1f %%' % (100 * int(s['holes']) / int(s['chunks_total'])),
            '%.1f %%' % (100 * int(s['dups']) / nh), '%.1f %%' % (100 * int(s['probe_raw_bytes']) / int(s['unique_bytes'])), 'yes' if c in ELIG else 'no (exploratory)')
for cs in ('16384', '65536'):
    cf = META['cf']['host-ext4-' + cs]
    h('Q1 volume-wide saving against the counterfactual (%s behind the probe), %s KiB: S [BCa 95 %%] in %%' % (cf, {'16384': '16', '65536': '64'}[cs]))
    arms = (['z3'] if cf == 'z1' else ['z1']) + ['ft@1', 'ft@3', 'bs-dist@1', 'bs-dist@3', 'bs-xfs@3', 'oracle-zd@3', 'oracle@3']
    row('corpus', *arms); row(*['---'] * (len(arms) + 1))
    for c in ELIG + EXPL:
        cells = []
        for a in arms:
            d = desc('%s-%s' % (c, cs), a)
            cells.append('%s [%s, %s]' % (pp(d['S']), pp(d['LB']), pp(d['bca_hi'])) if d else '-')
        row(c + ('' if c in ELIG else ' (expl.)'), *cells)
    h('D1 tests, %s KiB: N = LB - 0.1 x delta-cores (pp); cap = worst throughput ratio compress / decompress against %s' % ({'16384': '16', '65536': '64'}[cs], cf))
    row('arm', 'cap compress', 'cap decompress', 'delta-cores at 5 GB/s', *['N ' + c for c in ELIG], 'Holm p (max over the 4)', 'corpora passing'); row(*['---'] * 10)
    for a in ('ft@1', 'ft@3', 'bs-dist@1', 'bs-dist@3'):
        rs = [one(family='D1', corpus=c, size=cs, arm=a, shift='0') for c in ELIG]
        row(a, num(rs[0]['cap_c']), num(rs[0]['cap_d']), num(rs[0]['dcores'], 1), *[num(r['N']) for r in rs], '%.1e' % max(fl(r['holm']) for r in rs), sum(int(r['pass']) for r in rs))
h('Q1 per class, 16 KiB, eligible corpora: share of unique bytes and saving against z3 (behind the probe), %')
row('corpus', 'class', 'bytes', 'blocks', 'ft@3', 'bs-dist@3', 'bs-xfs@3', 'oracle@3'); row(*['---'] * 8)
for c in ELIG:
    tot = int(SH[(c, '16384')]['unique_bytes'])
    for cl in ('meta', 'dir', 'journal', 'data', 'unmapped', 'mixed', 'otherpart', 'ptable'):
        d = desc('%s-16384' % c, 'ft@3', cls=cl)
        if not d: continue
        g = lambda a: (lambda x: (pp(x['S']) + ('' if x.get('LB') in ('', None) else ' [%s]' % pp(x['LB']))) if x else '-')(desc('%s-16384' % c, a, cls=cl))
        row(c, cl, '%.1f %%' % (100 * int(d['bytes_in']) / tot), d['blocks'], g('ft@3'), g('bs-dist@3'), g('bs-xfs@3'), g('oracle@3'))
h('Q1 data chunks by zdicts type (ft selection on the chunk head), host-ext4 16 KiB, ten largest: saving against z3, %')
row('type', 'bytes', 'ft@3', 'oracle-zd@3 (type oracle bound)'); row(*['---'] * 4)
ty = sorted([d for d in DS if d['key'] == 'host-ext4-16384' and d['cls'].startswith('data:') and d['arm'] == 'ft@3' and d['shift'] == '0'], key=lambda d: -int(d['bytes_in']))[:10]
for d in ty: row(d['cls'][5:], '%.2f GiB' % (int(d['bytes_in']) / 2**30), pp(d['S']), pp(desc('host-ext4-16384', 'oracle-zd@3', cls=d['cls'])['S']))
# ---- Q2
for cs in ('16384', '65536'):
    h('Q2 per volume, %s KiB, pv hold-out (seed 1): S against CF, %%; D2 = pv against the strict comparator' % {'16384': '16', '65536': '64'}[cs])
    row('volume', 'CF', 'pv@1', 'pv@3', 'pv-class@3', 'oracle@3', 'pv@3 seeds 1-3', 'comparator', 'level', 'S vs comp [LB]', 'cap c/d vs CF', 'N strict', 'N lenient (comparator)', 'pass strict / lenient'); row(*['---'] * 14)
    for c in ELIG + EXPL + DB:
        key = '%s-%s' % (c, cs); cf = META['cf'][key]
        g = lambda a, m='pv-s1': (lambda x: pp(x['S']) if x else '-')(desc(key, a, mask=m))
        seeds = [desc(key, 'pv-s%d@3' % s, mask='pv-s%d' % s) for s in (1, 2, 3)]
        sp = '%s..%s' % (pp(min(fl(x['S']) for x in seeds if x)), pp(max(fl(x['S']) for x in seeds if x))) if all(seeds) else '-'
        rs = sorted(tests(family='D2', corpus=c, size=cs, shift='0'), key=lambda r: -fl(r['N']))
        b = rs[0]
        row(c + ('' if c in ELIG or c in DB else ' (expl.)'), cf, g('pv-s1@1'), g('pv-s1@3'), g('pvc-s1@3'), g('oracle@3'), sp, b['comp_used'], b['arm'][-1], '%s [%s]' % (pp(b['S']), pp(b['LB'])),
            '%s/%s' % (num(b['cap_c']), num(b['cap_d'])), num(b['N']), '%s (%s)' % (num(b.get('len_N')), b.get('len_comp', '')), '%s / %s' % (max(int(r['pass']) for r in rs), max(int(r['len_pass']) for r in rs)))
h('Q2 sweep, host-ext4 16 KiB (pv seed 1, hold-out of each sample): S against z3, %, level 3 (level 1 in brackets)')
row('sample', '16 KiB dict', '32 KiB', '64 KiB', '110 KiB'); row(*['---'] * 5)
for f, m in (('0.001', 'sw-f0.001'), ('0.01', 'pv-s1'), ('0.1', 'sw-f0.1')):
    cells = []
    for dsz in (16, 32, 64, 110):
        a = 'pv-s1' if (f, dsz) == ('0.01', 110) else 'sw-f%s-d%d' % (f, dsz)
        d3, d1 = desc('host-ext4-16384', a + '@3', mask=m), desc('host-ext4-16384', a + '@1', mask=m)
        cells.append('%s (%s)' % (pp(d3['S']), pp(d1['S'])) if d3 else '-')
    row(f.replace('0.001', '0.1 %').replace('0.01', '1 %').replace('0.1', '10 %') if False else {'0.001': '0.1 %', '0.01': '1 %', '0.1': '10 %'}[f], *cells)
pc = [desc('host-ext4-16384', 'pv-cover@%d' % L, mask='pv-s1', comp='pv-s1@%d' % L) for L in (1, 3)]
md.append('\npv-cover against pv (host-ext4 16 KiB, same hold-out): level 1 %s %%, level 3 %s %%.' % (pp(pc[0]['S']), pp(pc[1]['S'])))
SW = list(csv.DictReader(open(os.path.join(RAW, 'db_sweep.csv'))))
def sw(pop, arm):
    r = [x for x in SW if x['pop'] == pop and x['class'] == 'all' and x['arm'] == arm]; return int(r[0]['stored_bytes']) if r else None
h('Q2 sweep, se-askubuntu x PostgreSQL 16 KiB (seed 1, hold-out): S against z3 behind the probe, %, level 3 (level 1 in brackets)')
row('sample', '16 KiB dict', '32 KiB', '64 KiB', '110 KiB'); row(*['---'] * 5)
for f in ('0.001', '0.01', '0.1'):
    pop = 'smp-f' + f; z3 = sw(pop, 'z3')
    row({'0.001': '0.1 %', '0.01': '1 %', '0.1': '10 %'}[f], *['%.2f (%.2f)' % (100 * (1 - sw(pop, 'pv_f%s_d%d@3' % (f, d)) / z3), 100 * (1 - sw(pop, 'pv_f%s_d%d@1' % (f, d)) / z3)) for d in (16, 32, 64, 110)])
md.append('\npv-cover against pv (se-askubuntu x PostgreSQL 16 KiB, 1 %% hold-out): level 1 %.2f %%, level 3 %.2f %%.' % tuple(100 * (1 - sw('smp-f0.01', 'pv-cover@%d' % L) / sw('smp-f0.01', 'pv_f0.01_d110@%d' % L)) for L in (1, 3)))
# ---- Q3
h('Q3a Constellation view, database-file bytes: S against z3, % [BCa lower end]; resampling unit = file')
row('image', 'dbd@3 4 MiB', 'dbd@3 1 MiB', 'N 1 MiB', 'cap 1 MiB (c/d)', 'dbs@3 1 MiB', 'pv@3 1 MiB', 'dbd@3 page', 'dbs@3 page'); row(*['---'] * 9)
for n in DB + CTL:
    g = lambda tag, a, m='all': (lambda x: '%s [%s]' % (pp(x['S']), pp(x['LB'])) if x and x.get('LB') else (pp(x['S']) if x else '-'))(desc('cv-%s-%s' % (n, tag), a, mask=m))
    t1 = tests(decision='D6', corpus=n, size='1048576', shift='0') or tests(family='ctl', corpus=n, size='1048576')
    row(n + (' (control)' if n in CTL else ''), g('4m', 'dbd@3'), g('1m', 'dbd@3'), num(t1[0]['N']) if t1 else '-', '%s/%s' % (num(t1[0]['cap_c']), num(t1[0]['cap_d'])) if t1 else '-',
        g('1m', 'dbs@3'), g('1m', 'pv-s1@3', 'pv-s1'), g('pg', 'dbd@3'), g('pg', 'dbs@3'))
h('Q3b Lunix view 16 KiB, DB-page class: S against z3 behind the probe, % (D4-L uses dbd)')
row('image', 'dbpage share', 'ft@3', 'dbd@1', 'dbd@3', 'N dbd@3', 'dbs@3 (same schema)', 'pv@3 (hold-out)', 'oracle@3', 'wal: dbd@3', 'dbother: dbd@3'); row(*['---'] * 11)
DSH = {(r['corpus'], r['view'], r['size']): r for r in csv.DictReader(open(os.path.join(RAW, 'db_shares.csv')))}
for n in DB + CTL:
    key = '%s-16384' % n; g = lambda a, cl='dbpage', m='full': (lambda x: pp(x['S']) if x else '-')(desc(key, a, mask=m, cls=cl))
    dp = desc(key, 'dbd@3', cls='dbpage'); allb = desc(key, 'dbd@3')
    t = tests(decision='D4-L', corpus=n, arm='dbd@3', shift='0')
    row(n + (' (control)' if n in CTL else ''), '%.0f %%' % (100 * int(dp['bytes_in']) / int(allb['bytes_in'])), g('ft@3'), g('dbd@1'), g('dbd@3'), num(t[0]['N']) if t else '-', g('dbs@3'), g('pv-s1@3', m='pv-s1'), g('oracle@3'), g('dbd@3', 'wal'), g('dbd@3', 'dbother'))
h('D4 and D6 per engine: median N over the 5 datasets (pp), datasets with Holm p < 0.05 and S > 0 and the cap passed')
row('rule', 'pg', 'my', 'sq', 'engines passing'); row(*['---'] * 5)
for lab, r in (('D4-L dbd@1 (16 KiB DB pages, >= 5)', D4L[1]), ('D4-L dbd@3', D4L[3]), ('D4-C dbd@3 (1 MiB, >= 5)', D4C), ('D6 dbd@3 (1 MiB, >= 3)', D6r), ('D6 dbd@3 at 4 MiB (reported)', D6r4)):
    row(lab, *['%.2f (%d/5%s)' % (r[0][e][0], r[0][e][1], '' if r[0][e][2] else ', cap fails') for e in ENG], r[1])
h('Votes')
row('decision', 'reading', 'size', 'count', 'needed', 'verdict', 'note'); row(*['---'] * 7)
for v in VOTES: row(*v.values())
md.append('\nD5 from the timing cells: ' + '; '.join('%s: ft@3 vs z3 S %.2f %%, compress ratio %.2f, decompress %.2f, delta-cores at 1 GB/s %.1f, z4 vs z3 compress ratio %.2f' % (k, 100 * v['S'], v['cap_c'], v['cap_d'], v['dcores'], v['z4cap']) for k, v in D5.items()))
md.append('\nD1 oracle (best of 33 zdicts dictionaries and bs-dist per chunk, no DB set) LB, max over levels: ' + '; '.join('%s KiB: %s' % ({'16384': '16', '65536': '64'}[cs], ', '.join('%s %s' % (c, pp(v)) for c, v in D1[cs]['orc'].items())) for cs in D1))
flips = []
for r4 in tests(shift='4'):
    r0 = [r for r in T if r['test_id'] == r4['test_id'] and r['shift'] == '0'][0]
    for k in ('pass', 'len_pass'):
        if r0[k] != r4[k]: flips.append('%s %s: %s -> %s (LB %s -> %s)' % (r4['test_id'], k, r0[k], r4[k], pp(r0['LB']), pp(r4['LB'])))
md.append('\n16 MiB blocks: %d tests re-run (all raw-device corpora, se-askubuntu x PostgreSQL); pass flags that change: %s' % (len(tests(shift='4')), '; '.join(flips) or 'none'))
hw = [r for r in T if r['shift'] == '0' and r['family'] != 'ctl' and r['underpowered'] == '1']
md.append('\nDecision cells with BCa half-width > 1.5 pp: %s; largest half-width of any family test: %s pp.' % (', '.join(r['test_id'] for r in hw) or 'none', pp(max(fl(r['half']) for r in T if r['shift'] == '0' and r['family'] != 'ctl'))))
cov = [(desc('host-ext4-16384', 'pv-cover@%d' % L, mask='pv-s1'), desc('host-ext4-16384', 'pv-s1@%d' % L, mask='pv-s1')) for L in (1, 3)]
z3 = sw('smp-f0.01', 'z3')
md.append('\npv-cover minus pv in pp of the counterfactual: host-ext4 16 KiB L1 %.2f, L3 %.2f; se-askubuntu x PostgreSQL 16 KiB L1 %.2f, L3 %.2f.' % (
    100 * (fl(cov[0][0]['S']) - fl(cov[0][1]['S'])), 100 * (fl(cov[1][0]['S']) - fl(cov[1][1]['S'])),
    *[100 * (sw('smp-f0.01', 'pv_f0.01_d110@%d' % L) - sw('smp-f0.01', 'pv-cover@%d' % L)) / z3 for L in (1, 3)]))
sz = lambda cell, a, lv: tst(cell, a, lv)
md.append('\nCounterfactual sensitivity from the timing samples (plain levels that pass the cap but were not measured volume-wide): z2 vs z1 at 64 KiB on vm-ubuntu %.2f %% (z2 compress ratio vs z1 %.2f); z4 vs z3 on Constellation database objects: SE 1 MiB %.2f %%, TLC 1 MiB %.2f %%, SE 4 MiB %.2f %%, TLC 4 MiB %.2f %% (ratios %.2f, %.2f, %.2f, %.2f).' % (
    100 * (1 - sz('vm-ubuntu-65536', 'z2', '2') / sz('vm-ubuntu-65536', 'z1', '1')), tcost('vm-ubuntu-65536', 'z1', '1', 'compress') / tcost('vm-ubuntu-65536', 'z2', '2', 'compress'),
    *[100 * (1 - sz(c, 'z4', '4') / sz(c, 'z3', '3')) for c in ('db-se-askubuntu-pg-1m', 'db-tlc-2024q1-pg-1m', 'db-se-askubuntu-pg-4m', 'db-tlc-2024q1-pg-4m')],
    *[tcost(c, 'z3', '3', 'compress') / tcost(c, 'z4', '4', 'compress') for c in ('db-se-askubuntu-pg-1m', 'db-tlc-2024q1-pg-1m', 'db-se-askubuntu-pg-4m', 'db-tlc-2024q1-pg-4m')]))
open(os.path.join(OUT, 'tables.md'), 'w').write('\n'.join(md) + '\n')
print('\n'.join(md[-40:]))
