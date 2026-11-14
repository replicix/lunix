#!/usr/bin/env python3
"""Applies the decision table D1-D7 (PREREGISTERED.md 1.2 with Amendment A1) to analyze.py's output and prints the SUMMARY tables.
usage: decide.py <analysis-dir> <raw-dir>      (raw-dir: shares.csv, db_shares.csv, db_sweep.csv, timing.csv, timing-db.csv, d5_objects.csv;
       `decide.py raw raw` works from bench/.../raw, where the analysis files are named analysis-a1-*)
writes <analysis-dir>/votes.csv and tables.md.  The first analysis' version is decide_v1.py.
Rules (A1): D2 against the counterfactual for S, LB, N, cores and cap, per volume and size, from the image's own timing cell; database
family judged on pv-cover if the median (pv-cover - pv) over the 15 images at 16 KiB exceeds 1 pp, else on pv; 16 KiB decides, 64 KiB is
confirmatory ("GO at 16 KiB only" if it does not also reach 8 of 15); D5 computed from the per-object pass, hard cap applied regardless of
N.  A volume passes D2 if either level passes (both levels were enumerated in families-a1.csv before the analysis ran)."""
import sys, os, csv, json, statistics
OUT, RAW = sys.argv[1:3]
def opn(n):
    for p in (n, 'analysis-a1-' + n):
        if os.path.exists(os.path.join(OUT, p)): return open(os.path.join(OUT, p))
    raise FileNotFoundError(n)
def load_desc():
    if os.path.exists(os.path.join(OUT, "desc.csv")): return list(csv.DictReader(open(os.path.join(OUT, "desc.csv"))))
    return [r for part in ("lunix-raw", "db") for r in csv.DictReader(open(os.path.join(OUT, "analysis-a1-desc-%s.csv" % part)))]   # raw/ keeps desc in two files (< 1 MiB each)
T = list(csv.DictReader(opn("tests.csv"))); DS = load_desc(); META = json.load(opn("meta.json"))
ELIG = ['host-ext4', 'host-xfs', 'host-btrfs', 'vm-ubuntu']; EXPL = ['vm-debian', 'vm-fedora']
DSETS = ['se-askubuntu', 'se-superuser', 'se-serverfault', 'geonames', 'tlc-2024q1']; ENG = ['pg', 'my', 'sq']
DB = ['db-%s-%s' % (d, e) for d in DSETS for e in ENG]; CTL = ['db-ctl-pgbench-pg', 'db-ctl-sysbench-my']; D5C = ['vm-ubuntu', 'vm-debian', 'vm-fedora', 'host-files']
KB = {'16384': '16', '65536': '64'}
fl = lambda x: float(x) if x not in ('', None) else float('nan')
def pp(x, d=2): return '-' if x in ('', None) else '%.*f' % (d, 100 * fl(x))
def num(x, d=2): return '%.*f' % (d, fl(x)) if x not in ('', None) else '-'
def tests(**k): return [r for r in T if all(str(r[a]) == str(b) for a, b in k.items())]
def one(**k):
    r = tests(**k); assert len(r) == 1, (k, len(r)); return r[0]
def desc(key, arm, mask='full', cls='all', shift='0', comp=None):
    r = [d for d in DS if d['key'] == key and d['arm'] == arm and d['mask'] == mask and d['cls'] == cls and d['shift'] == str(shift) and (comp is None or d['comp'] == comp)]
    return r[0] if r else None
def ci(key, arm, mask='full', cls='all', comp=None):
    """'S [lo, hi / lo16, hi16]': BCa 95 % with 1 MiB blocks (files) and with 16 MiB blocks (16 MiB file extents)."""
    a, b = desc(key, arm, mask, cls, 0, comp), desc(key, arm, mask, cls, 4, comp)
    if not a: return '-'
    if a.get('LB') in ('', None): return pp(a['S'])
    s = '%s [%s, %s' % (pp(a['S']), pp(a['LB']), pp(a['bca_hi']))
    return s + (' / %s, %s]' % (pp(b['LB']), pp(b['bca_hi'])) if b and b.get('LB') not in ('', None) else ']')
def tci(r, r16, scaled=False):
    S, LB = (r['S_cf'], r['LB_cf']) if scaled and r.get('S_cf') else (r['S'], r['LB'])
    LB16 = (r16.get('LB_cf') if scaled and r16.get('S_cf') else r16['LB']) if r16 else ''
    return '%s [%s / %s]' % (pp(S), pp(LB), pp(LB16))
VOTES = []; md = []
def vote(dec, reading, size, n, need, verdict, note=''): VOTES.append(dict(decision=dec, reading=reading, size=size, count=n, needed=need, verdict=verdict, note=note))
def t16(r): return one(test_id=r['test_id'], shift='4')
# ---------------------------------------------------------------- D1
D1 = {}
for sh in ('0', '4'):
    for cs in ('16384', '65536'):
        arms = ['ft@1', 'ft@3', 'bs-dist@1', 'bs-dist@3']; tag = '' if sh == '0' else ' (16 MiB blocks)'
        per_arm = {a: sum(int(one(family='D1', corpus=c, size=cs, arm=a, shift=sh)['pass']) for c in ELIG) for a in arms}
        any_arm = sum(any(int(one(family='D1', corpus=c, size=cs, arm=a, shift=sh)['pass']) for a in arms) for c in ELIG)
        best = max(per_arm.values()); go = best == 4
        orc = {c: max(fl(desc('%s-%s' % (c, cs), 'oracle@%d' % L, shift=sh)['LB']) for L in (1, 3)) for c in ELIG}
        decisive = sum(v < 0.03 for v in orc.values()) >= 4
        vote('D1', 'same set and level on all 4 eligible' + tag, cs, best, 4, 'GO' if go else 'NO-GO',
             'decisive among the measured candidate sets (oracle LB < 3 pp on all 4)' if decisive else 'oracle LB >= 3 pp on %d corpora: not decisive' % sum(v >= 0.03 for v in orc.values()))
        vote('D1', 'any set/level per corpus' + tag, cs, any_arm, 4, 'GO' if any_arm == 4 else 'NO-GO')
        vote('D1', 'sensitivity 3 of 4 (lead ruling: never decides)' + tag, cs, best, 3, 'GO' if best >= 3 else 'NO-GO')
        D1[(cs, sh)] = dict(go=go, orc=orc, decisive=decisive)
D1_fires = D1[('16384', '0')]['go'] or D1[('65536', '0')]['go']
# ---------------------------------------------------------------- D2 (A1)
def d2rows(vols, cs, sh, arm):
    return [r for r in tests(family='D2', size=cs, shift=sh) if r['corpus'] in vols and r['arm'].startswith(arm + '@')]
def d2count(vols, cs, sh, arm, key='pass', level=None):
    return sorted({r['corpus'] for r in d2rows(vols, cs, sh, arm) if int(r[key]) and (level is None or r['arm'].endswith('@' + level))})
COV = {}
for L in ('1', '3'):
    d = []
    for n in DB:
        a = desc('%s-16384' % n, 'pvcover-s1@' + L, mask='pv-s1'); b = desc('%s-16384' % n, 'pv-s1@' + L, mask='pv-s1')
        d.append(100 * (fl(a['S']) - fl(b['S'])))
    COV[L] = (statistics.median(d), min(d), max(d), d)
cov_fires = COV['3'][0] > 1.0 and COV['1'][0] > 1.0
cov_split = (COV['3'][0] > 1.0) != (COV['1'][0] > 1.0)
DBARM = 'pvcover-s1' if cov_fires else 'pv-s1'
raw16 = d2count(ELIG, '16384', '0', 'pv-s1'); raw64 = d2count(ELIG, '65536', '0', 'pv-s1')
vote('D2-raw', 'pv against the counterfactual, either level', '16384', len(raw16), 4, 'reached' if len(raw16) >= 4 else 'not reached')
vote('D2-raw', 'pv against the counterfactual, either level', '65536', len(raw64), 4, 'reached' if len(raw64) >= 4 else 'not reached')
D2 = {}
for arm in ('pv-s1', 'pvcover-s1'):
    for sh in ('0', '4'):
        for cs in ('16384', '65536'):
            tag = ('decides' if arm == DBARM else 'other arm (sensitivity)') + ('' if sh == '0' else ', 16 MiB blocks')
            n = d2count(DB, cs, sh, arm); D2[(arm, sh, cs)] = n
            vote('D2-db', '%s, either level; %s' % (arm.replace('-s1', ''), tag), cs, len(n), 8, 'reached' if len(n) >= 8 else 'not reached', ' '.join(x.replace('db-', '') for x in n))
            if sh == '0':
                n1 = d2count(DB, cs, sh, arm, level='3' if cs == '16384' else '1')
                vote('D2-db', '%s, one level (level of the counterfactual)' % arm.replace('-s1', ''), cs, len(n1), 8, 'reached' if len(n1) >= 8 else 'not reached')
                nc = d2count(DB, cs, sh, arm, key='pass_nocap')
                vote('D2-db', '%s, cap not applied (shows the cap; never decides)' % arm.replace('-s1', ''), cs, len(nc), 8, 'reached' if len(nc) >= 8 else 'not reached', ' '.join(x.replace('db-', '') for x in nc))
p16, p64 = len(D2[(DBARM, '0', '16384')]) >= 8, len(D2[(DBARM, '0', '65536')]) >= 8
D2db = 'GO' if p16 and p64 else 'GO at 16 KiB only' if p16 else 'NO-GO'
D2raw = 'GO' if len(raw16) >= 4 else 'NO-GO'
vote('D2', 'raw-device family', '', len(raw16), 4, D2raw); vote('D2', 'database family (judged on %s; 16 KiB decides, 64 KiB confirmatory)' % DBARM, '', len(D2[(DBARM, '0', '16384')]), 8, D2db)
D2_fires = D2raw == 'GO' or D2db != 'NO-GO'
# ---------------------------------------------------------------- D2c (only if D2 fires): N >= 1 pp on >= half of the volumes that made D2 fire
if D2_fires:
    fired = (raw16 if D2raw == 'GO' else []) + (D2[(DBARM, '0', '16384')] if D2db != 'NO-GO' else [])
    ok = [c for c in fired if any(int(r['pass']) for r in tests(family='D2c', corpus=c, size='16384', shift='0'))]
    vote('D2c', 'pv-class vs pv on the volumes that made D2 fire', '16384', len(ok), (len(fired) + 1) // 2, 'GO' if 2 * len(ok) >= len(fired) else 'NO-GO')
else: vote('D2c', 'not evaluated (D2 did not fire)', '', '', '', 'not evaluated')
D3_fires = False
vote('D3', 'D1 does not fire' if not D1_fires else 'D1 fires', '', '', '', 'NO-GO' if not D1_fires else 'see tests')
# ---------------------------------------------------------------- D4, D6
def engine_pass(dec, fam, arm, size, thr, sh='0'):
    res = {}
    for e in ENG:
        rows = [one(family=fam, decision=dec, corpus='db-%s-%s' % (d, e), arm=arm, size=size, shift=sh) for d in DSETS]
        Ns = [fl(r['N']) if r['N'] != '' else 100 * fl(r['S']) for r in rows]
        sig = sum(1 for r in rows if fl(r['holm']) < 0.05 and fl(r['S']) > 0 and r['cap_ok'] == '1')
        res[e] = (statistics.median(Ns), sig, all(r['cap_ok'] == '1' for r in rows))
    return res, sum(1 for e in ENG if res[e][0] >= thr and res[e][1] >= 3 and res[e][2])
D4L = {L: engine_pass('D4-L', 'D4', 'dbd@%d' % L, '16384', 5.0) for L in (1, 3)}
D4C = engine_pass('D4-C', 'D4', 'dbd@3', '1048576', 5.0)
D6r = engine_pass('D6', 'D6', 'dbd@3', '1048576', 3.0); D6r4 = engine_pass('D6', 'D6', 'dbd@3', '4194304', 3.0); D6r16 = engine_pass('D6', 'D6', 'dbd@3', '1048576', 3.0, '4')
D6_fires = D6r[1] >= 2
for L in (1, 3): vote('D4-L', 'engines with median N >= 5 pp (dbd@%d, DB pages, 16 KiB)' % L, '16384', D4L[L][1], 2, 'GO' if D4L[L][1] >= 2 and D1_fires else 'NO-GO', 'consumer D1 %s' % ('fires' if D1_fires else 'does not fire'))
vote('D4-C', 'engines with median N >= 5 pp (dbd@3, 1 MiB)', '1048576', D4C[1], 2, 'GO' if D4C[1] >= 2 and D6_fires else 'NO-GO', 'consumer D6 %s' % ('fires' if D6_fires else 'does not fire'))
vote('D6', 'engines with median N >= 3 pp (dbd@3, 1 MiB)', '1048576', D6r[1], 2, 'GO' if D6_fires else 'NO-GO')
vote('D6', 'same, 16 MiB file extents as the unit', '1048576', D6r16[1], 2, 'GO' if D6r16[1] >= 2 else 'NO-GO')
vote('D6', 'reported: 4 MiB', '4194304', D6r4[1], 2, 'reported only')
pgref = {n: desc('cv-%s-pg' % n, 'dbd@3', mask='all') for n in DB}
nref = sum(1 for d in pgref.values() if d and d.get('LB') not in ('', None) and fl(d['LB']) >= 0.15)
vote('D6', 'page-size reference: images with LB >= 15 pp', 'page', nref, 1, 'open question' if nref else 'no open question')
# ---------------------------------------------------------------- D5 (per-object pass, A1.5)
D5 = {}
for c in D5C:
    lt = one(family='D5', corpus=c, pop='lt64k', shift='0'); al = one(family='D5', corpus=c, pop='all', shift='0')
    loss = {m: 100 * (fl(desc('d5-' + c, 'oracle@3', mask=m, comp='z3')['S']) - fl(desc('d5-' + c, 'ft@3', mask=m, comp='z3')['S'])) for m in ('lt64k', 'all')}
    floors_nocap = fl(lt['N']) >= 5 and fl(al['N']) >= 1 and fl(lt['holm']) < 0.05 and fl(al['holm']) < 0.05
    D5[c] = dict(lt=lt, al=al, loss=loss, floors=int(lt['pass_nocap']) and int(al['pass_nocap']), cap=lt['cap_ok'] == '1' and al['cap_ok'] == '1', r5=loss['lt64k'] <= 1.5)
d5_floor_ok = sum(1 for c in D5C if D5[c]['floors'] and D5[c]['r5'])
d5_missed = sum(1 for c in D5C if not D5[c]['floors'])
d5_cap = all(D5[c]['cap'] for c in D5C)
D5v = 'GO' if d5_floor_ok >= 3 and d5_cap else 'NO-GO'
vote('D5', 'corpora meeting both floors and R5 (table rule, cap not applied)', '4194304', d5_floor_ok, 3, 'reached' if d5_floor_ok >= 3 else 'not reached', 'a floor missed on %d of 4 corpora' % d5_missed)
vote('D5', 'hard cap (select + dictionary vs zstd:3, compress)', '4194304', sum(1 for c in D5C if D5[c]['cap']), 4, 'passes' if d5_cap else 'fails (NO-GO regardless of N)',
     'compress %.2fx all objects, %.2fx under 64 KiB' % (fl(D5['vm-ubuntu']['al']['cap_c']), fl(D5['vm-ubuntu']['lt']['cap_c'])))
vote('D5', 'verdict', '', '', '', D5v)
D5_fires = D5v == 'GO'
for dec, fires in (('D7-Lunix', not (D1_fires or D2_fires)), ('D7-Constellation', not (D5_fires or D6_fires)), ('D7-zdicts', not (D3_fires or (D4L[3][1] >= 2 and D1_fires) or (D4C[1] >= 2 and D6_fires)))):
    vote(dec, 'derived', '', '', '', 'fires (no dictionary work)' if fires else 'does not fire')
with open(os.path.join(OUT, 'votes.csv'), 'w', newline='') as fh:
    w = csv.DictWriter(fh, fieldnames=list(VOTES[0])); w.writeheader(); w.writerows(VOTES)
# ================================================================ markdown tables
def h(s): md.append('\n#### ' + s + '\n')
def row(*c): md.append('| ' + ' | '.join(str(x) for x in c) + ' |')
SH = {(r['corpus'], r['chunk']): r for r in csv.DictReader(open(os.path.join(RAW, 'shares.csv')))}
for cs in ('16384', '65536'):
    cf = META['cf']['host-ext4-' + cs]
    h('Q1 %s KiB: volume-wide S against the counterfactual (%s behind the probe), %%, [BCa 95 %% with 1 MiB blocks / with 16 MiB blocks]' % (KB[cs], cf))
    arms = (['z3'] if cf == 'z1' else ['z1']) + ['ft@1', 'ft@3', 'bs-dist@1', 'bs-dist@3', 'bs-xfs@3', 'oracle-zd@3', 'oracle@3']
    row('corpus', *arms); row(*['---'] * (len(arms) + 1))
    for c in ELIG + EXPL: row(c + ('' if c in ELIG else ' (expl.)'), *[ci('%s-%s' % (c, cs), a) for a in arms])
    h('D1 %s KiB: N (pp) with 1 MiB blocks (16 MiB blocks); cap = compress / decompress ratio against %s on the vm-ubuntu cell' % (KB[cs], cf))
    row('arm', 'cap', 'delta-cores', *ELIG, 'max Holm p', 'passing'); row(*['---'] * 9)
    for a in ('ft@1', 'ft@3', 'bs-dist@1', 'bs-dist@3'):
        rs = [one(family='D1', corpus=c, size=cs, arm=a, shift='0') for c in ELIG]
        row(a, '%s / %s' % (num(rs[0]['cap_c']), num(rs[0]['cap_d'])), num(rs[0]['dcores'], 1), *['%s (%s)' % (num(r['N']), num(t16(r)['N'])) for r in rs], '%.1e' % max(fl(r['holm']) for r in rs), sum(int(r['pass']) for r in rs))
h('Q1 per class, 16 KiB: S against zstd-3 behind the probe, % [BCa LB 1 MiB / 16 MiB] where the class has >= 200 blocks')
row('corpus', 'class', 'bytes', 'blocks', 'ft@3', 'bs-dist@3', 'oracle@3'); row(*['---'] * 7)
for c in ELIG:
    tot = int(SH[(c, '16384')]['unique_bytes'])
    for cl in ('data', 'meta', 'dir', 'journal', 'unmapped', 'mixed', 'otherpart'):
        d = desc('%s-16384' % c, 'ft@3', cls=cl)
        if not d or int(d['bytes_in']) < 0.004 * tot: continue
        g = lambda a: (lambda x, y: (pp(x['S']) + ('' if x.get('LB') in ('', None) else ' [%s / %s]' % (pp(x['LB']), pp(y['LB']) if y and y.get('LB') not in ('', None) else '-'))) if x else '-')(desc('%s-16384' % c, a, cls=cl), desc('%s-16384' % c, a, cls=cl, shift=4))
        row(c, cl, '%.1f %%' % (100 * int(d['bytes_in']) / tot), d['blocks'], g('ft@3'), g('bs-dist@3'), g('oracle@3'))
h('Q2 raw devices: pv (seed 1, hold-out) against the counterfactual, S [LB 1 MiB / 16 MiB], N; cap = compress / decompress vs CF')
row('corpus', 'size', 'CF', 'pv@1', 'pv@3', 'cap pv@3', 'N pv@1', 'N pv@3', 'pass'); row(*['---'] * 9)
for c in ELIG + EXPL:
    for cs in ('16384', '65536'):
        r1, r3 = one(family='D2', corpus=c, size=cs, arm='pv-s1@1', shift='0'), one(family='D2', corpus=c, size=cs, arm='pv-s1@3', shift='0')
        row(c + ('' if c in ELIG else ' (expl.)'), KB[cs] + ' KiB', r3['cf'], tci(r1, t16(r1)), tci(r3, t16(r3)), '%s / %s' % (num(r3['cap_c']), num(r3['cap_d'])), num(r1['N']), num(r3['N']), max(int(r1['pass']), int(r3['pass'])))
for cs in ('16384', '65536'):
    h('Q2 database images, %s KiB: against the image\'s own counterfactual (own timing cell), S [LB 1 MiB / 16 MiB] in %%, cap compress / decompress, N in pp%s' % (KB[cs], '; zstd-2 counterfactuals: S and LB rescaled from zstd-1 by the cell\'s byte ratio' if cs == '65536' else ''))
    row('image', 'CF', 'pv@1', 'pv@3', 'pv-cover@1', 'pv-cover@3', 'cap pv@3', 'cap cover@3', 'cores pv@3 / cover@3', 'N pv@1 / pv@3', 'N cover@1 / cover@3', 'pass pv / cover'); row(*['---'] * 12)
    for n in DB:
        rr = {a: one(family='D2', corpus=n, size=cs, arm=a, shift='0') for a in ('pv-s1@1', 'pv-s1@3', 'pvcover-s1@1', 'pvcover-s1@3')}
        sc = rr['pv-s1@3'].get('S_cf') not in ('', None)
        row(n.replace('db-', ''), rr['pv-s1@3']['cf'], *[tci(rr[a], t16(rr[a]), sc) for a in rr], '%s / %s' % (num(rr['pv-s1@3']['cap_c']), num(rr['pv-s1@3']['cap_d'])),
            '%s / %s' % (num(rr['pvcover-s1@3']['cap_c']), num(rr['pvcover-s1@3']['cap_d'])), '%s / %s' % (num(rr['pv-s1@3']['dcores'], 1), num(rr['pvcover-s1@3']['dcores'], 1)),
            '%s / %s' % (num(rr['pv-s1@1']['N']), num(rr['pv-s1@3']['N'])), '%s / %s' % (num(rr['pvcover-s1@1']['N']), num(rr['pvcover-s1@3']['N'])),
            '%d / %d' % (max(int(rr['pv-s1@1']['pass']), int(rr['pv-s1@3']['pass'])), max(int(rr['pvcover-s1@1']['pass']), int(rr['pvcover-s1@3']['pass']))))
md.append('\nCOVER rule (A1.3), pv-cover minus pv against the counterfactual, 16 KiB, over the 15 images: level 3 median %.3f pp (range %.2f to %.2f), level 1 median %.3f pp (range %.2f to %.2f); rule fires: %s%s; database family judged on %s.' % (
    COV['3'][0], COV['3'][1], COV['3'][2], COV['1'][0], COV['1'][1], COV['1'][2], 'yes' if cov_fires else 'no', ' (the two levels disagree)' if cov_split else '', DBARM))
md.append('\nPer image, level 3: ' + ', '.join('%s %.2f' % (n.replace('db-', ''), v) for n, v in zip(DB, COV['3'][3])))
md.append('\nPer image, level 1: ' + ', '.join('%s %.2f' % (n.replace('db-', ''), v) for n, v in zip(DB, COV['1'][3])))
h('Q2 sweep, host-ext4 16 KiB (pv seed 1, hold-out): S against z3, %, level 3 (level 1)')
row('sample', '16 KiB dict', '32 KiB', '64 KiB', '110 KiB'); row(*['---'] * 5)
for f, m in (('0.001', 'sw-f0.001'), ('0.01', 'pv-s1'), ('0.1', 'sw-f0.1')):
    cells_ = []
    for dsz in (16, 32, 64, 110):
        a = 'pv-s1' if (f, dsz) == ('0.01', 110) else 'sw-f%s-d%d' % (f, dsz)
        d3, d1 = desc('host-ext4-16384', a + '@3', mask=m), desc('host-ext4-16384', a + '@1', mask=m)
        cells_.append('%s (%s)' % (pp(d3['S']), pp(d1['S'])) if d3 else '-')
    row({'0.001': '0.1 %', '0.01': '1 %', '0.1': '10 %'}[f], *cells_)
SW = list(csv.DictReader(open(os.path.join(RAW, 'db_sweep.csv'))))
def sw(pop, arm):
    r = [x for x in SW if x['pop'] == pop and x['class'] == 'all' and x['arm'] == arm]; return int(r[0]['stored_bytes']) if r else None
h('Q2 sweep, se-askubuntu x PostgreSQL 16 KiB (seed 1, hold-out): S against z3 behind the probe, %, level 3 (level 1)')
row('sample', '16 KiB dict', '32 KiB', '64 KiB', '110 KiB'); row(*['---'] * 5)
for f in ('0.001', '0.01', '0.1'):
    pop = 'smp-f' + f; z3 = sw(pop, 'z3')
    row({'0.001': '0.1 %', '0.01': '1 %', '0.1': '10 %'}[f], *['%.2f (%.2f)' % (100 * (1 - sw(pop, 'pv_f%s_d%d@3' % (f, d)) / z3), 100 * (1 - sw(pop, 'pv_f%s_d%d@1' % (f, d)) / z3)) for d in (16, 32, 64, 110)])
h('Q3a Constellation view, database-file bytes: S against z3, % [BCa: unit file / unit 16 MiB file extent]')
row('image', 'units 1 MiB view (files / extents / objects)', 'dbd@3 4 MiB', 'dbd@3 1 MiB', 'N 1 MiB (extent)', 'cap 1 MiB', 'dbs@3 1 MiB', 'pv@3 1 MiB', 'dbd@3 page', 'dbs@3 page'); row(*['---'] * 10)
for n in DB + CTL:
    t1 = (tests(decision='D6', corpus=n, size='1048576', shift='0') or tests(family='ctl', corpus=n, size='1048576', shift='0'))[0]
    t4 = one(test_id=t1['test_id'], shift='4'); u = META['units']['cv-%s-1m' % n]
    row(n.replace('db-', '') + (' (control)' if n in CTL else ''), '%d / %d / %d' % (u['files'], u['extents16'], u['objects']), ci('cv-%s-4m' % n, 'dbd@3', 'all'), ci('cv-%s-1m' % n, 'dbd@3', 'all'),
        '%s (%s)' % (num(t1['N']), num(t4['N'])), '%s / %s' % (num(t1['cap_c']), num(t1['cap_d'])), ci('cv-%s-1m' % n, 'dbs@3', 'all'), ci('cv-%s-1m' % n, 'pv-s1@3', 'pv-s1'), ci('cv-%s-pg' % n, 'dbd@3', 'all'), ci('cv-%s-pg' % n, 'dbs@3', 'all'))
h('Q3b Lunix view 16 KiB, DB-page class: S against z3 behind the probe, % [LB 1 MiB / 16 MiB] (D4-L uses dbd)')
row('image', 'dbpage share', 'ft@3', 'dbd@1', 'dbd@3', 'N dbd@3', 'dbs@3', 'pv@3 (hold-out)', 'pv-cover@3 (hold-out)', 'oracle@3'); row(*['---'] * 10)
for n in DB + CTL:
    key = '%s-16384' % n
    def g(a, m='full'):
        x, y = desc(key, a, mask=m, cls='dbpage'), desc(key, a, mask=m, cls='dbpage', shift=4)
        if not x: return '-'
        return pp(x['S']) + ('' if x.get('LB') in ('', None) else ' [%s / %s]' % (pp(x['LB']), pp(y['LB']) if y else '-'))
    dp = desc(key, 'dbd@3', cls='dbpage'); allb = desc(key, 'dbd@3')
    t = tests(decision='D4-L', corpus=n, arm='dbd@3', shift='0')
    row(n.replace('db-', '') + (' (control)' if n in CTL else ''), '%.0f %%' % (100 * int(dp['bytes_in']) / int(allb['bytes_in'])), g('ft@3'), g('dbd@1'), g('dbd@3'), num(t[0]['N']) if t else '-', g('dbs@3'),
        pp(desc(key, 'pv-s1@3', mask='pv-s1', cls='dbpage')['S']), (lambda x: pp(x['S']) if x else '-')(desc(key, 'pvcover-s1@3', mask='pv-s1', cls='dbpage')), g('oracle@3'))
h('D4 and D6 per engine: median N over the 5 datasets (pp); datasets with Holm p < 0.05, S > 0 and the cap passed')
row('rule', 'pg', 'my', 'sq', 'engines passing'); row(*['---'] * 5)
for lab, r in (('D4-L dbd@1 (16 KiB DB pages, >= 5)', D4L[1]), ('D4-L dbd@3', D4L[3]), ('D4-C dbd@3 (1 MiB, >= 5)', D4C), ('D6 dbd@3 (1 MiB, >= 3)', D6r), ('D6, 16 MiB file extents', D6r16), ('D6 dbd@3 at 4 MiB (reported)', D6r4)):
    row(lab, *['%.2f (%d/5%s)' % (r[0][e][0], r[0][e][1], '' if r[0][e][2] else ', cap fails') for e in ENG], r[1])
h('D5 per corpus (Constellation file objects at 4 MiB, zstd:3 counterfactual): S [BCa: unit file / 16 MiB file extent] in %, N in pp')
row('corpus', 'population', 'objects / files', 'ft@3 (select)', 'oracle@3', 'selector loss (pp)', 'cap c / d', 'delta-cores', 'N', 'floor', 'passes floor (no cap)', 'Holm p'); row(*['---'] * 12)
for c in D5C:
    for m, r in (('lt64k', D5[c]['lt']), ('all', D5[c]['al'])):
        u = META['units']['d5-' + c][m]
        row(c, 'under 64 KiB' if m == 'lt64k' else 'all objects', '%d / %d' % (u['objects'], u['files']), ci('d5-' + c, 'ft@3', m, comp='z3'), ci('d5-' + c, 'oracle@3', m, comp='z3'), '%.2f' % D5[c]['loss'][m],
            '%s / %s' % (num(r['cap_c']), num(r['cap_d'])), num(r['dcores'], 1), '%s (%s)' % (num(r['N']), num(t16(r)['N'])), r['threshold'], r['pass_nocap'], '%.1e' % fl(r['holm']))
h('Votes')
row('decision', 'reading', 'size', 'count', 'needed', 'verdict', 'note'); row(*['---'] * 7)
for v in VOTES: row(*v.values())
md.append('\nD1 oracle (best of 33 zdicts dictionaries and bs-dist per chunk; no DB set) LB, max over levels, 1 MiB (16 MiB) blocks: ' + '; '.join('%s KiB: %s' % (KB[cs], ', '.join('%s %s (%s)' % (c, pp(D1[(cs, '0')]['orc'][c]), pp(D1[(cs, '4')]['orc'][c])) for c in ELIG)) for cs in ('16384', '65536')))
flips = []
for r4 in tests(shift='4'):
    r0 = one(test_id=r4['test_id'], shift='0')
    if r0['pass'] != r4['pass']: flips.append('%s: %s -> %s' % (r4['test_id'], r0['pass'], r4['pass']))
w16 = [fl(one(test_id=r['test_id'], shift='4')['half']) / fl(r['half']) for r in tests(shift='0') if r['family'] != 'ctl' and r['half'] not in ('', None) and fl(r['half']) > 0 and one(test_id=r['test_id'], shift='4')['half'] not in ('', None)]
md.append('\n16 MiB blocks / extents: %d tests re-run (every test); pass flags that change: %s. Median ratio of the BCa half-width, 16 MiB to 1 MiB: %.2f (range %.2f-%.2f).' % (len(tests(shift='4')), '; '.join(flips) or 'none', statistics.median(w16), min(w16), max(w16)))
hw = [r for r in T if r['family'] != 'ctl' and r['underpowered'] == '1']
md.append('\nDecision cells with BCa half-width > 1.5 pp: %s; largest half-width of any family test: 1 MiB %s pp, 16 MiB %s pp.' % (', '.join('%s (shift %s)' % (r['test_id'], r['shift']) for r in hw) or 'none',
    pp(max(fl(r['half']) for r in T if r['shift'] == '0' and r['family'] != 'ctl' and r['half'] not in ('', None))), pp(max(fl(r['half']) for r in T if r['shift'] == '4' and r['family'] != 'ctl' and r['half'] not in ('', None)))))
md.append('\nCounterfactuals of the database images (own cells): ' + '; '.join('%s %s KiB %s' % (n.replace('db-', ''), KB[cs], META['cf']['%s-%s' % (n, cs)]['protocol']) for n in DB for cs in ('16384', '65536')))
open(os.path.join(OUT, 'tables.md'), 'w').write('\n'.join(md) + '\n')
print('\n'.join(v['decision'] + ' | ' + v['reading'] + ' | ' + str(v['size']) + ' | ' + str(v['count']) + '/' + str(v['needed']) + ' | ' + v['verdict'] + ' | ' + v['note'] for v in VOTES))
