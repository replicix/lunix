#!/usr/bin/env python3 -I
"""Render the summary CSVs as Markdown tables (pasted into SUMMARY.md).  usage: md_tables.py CSV_DIR"""
import sys as _s, os as _o; _s.path.insert(0, _o.path.dirname(_o.path.abspath(__file__)))
import sys, os, csv, collections
d = sys.argv[1]
def rd(fn):
    p = os.path.join(d, fn)
    return list(csv.DictReader(open(p))) if os.path.exists(p) else []
def tab(headers, rows):
    print('| ' + ' | '.join(headers) + ' |'); print('|' + '|'.join('---' for _ in headers) + '|')
    for r in rows: print('| ' + ' | '.join(str(x) for x in r) + ' |')
    print()
B = ['<1KiB', '1-4KiB', '4-16KiB', '16-64KiB', '64-256KiB', '256KiB-4MiB', '>4MiB', 'all']
g = rd('gains_overall.csv')
def cell(x): return f"{float(x['gain_bytes_pct']):.1f} ({float(x['ci_lo']):.1f} to {float(x['ci_hi']):.1f})" if x else ''
def head(title, grp, eco, arm, lvl=3):
    print(f'**{title}**\n')
    rows = []
    for kind in ('generic text', 'generic all', 'family', 'type', 'ext+type'):
        r = [kind]
        for b in B:
            x = next((x for x in g if x['group'] == grp and x['eco'] == eco and x['arm'] == arm and x['kind'] == kind and x['level'] == str(lvl) and x['bucket'] == b and x['vs'] == f'nodict z{lvl}'), None)
            r.append(cell(x))
        rows.append(r)
    tab(['dictionary'] + B, rows)
def refs(grp, eco):
    rows = []
    for kind, lvl, nm in (('nodict', 9, 'zstd:9, no dict'), ('nodict', 19, 'zstd:19, no dict'), ('br11', 0, 'brotli q11 (built-in dictionary), objects <= 1 MiB'), ('xz9e', 0, 'xz -9e (raw LZMA2), objects <= 1 MiB'), ('type', 3, 'zstd:3 + type dict (arm C), for comparison')):
        r = [nm]
        for b in B:
            if kind == 'type': x = next((x for x in g if x['group'] == grp and x['eco'] == eco and x['arm'] == 'C' and x['kind'] == 'type' and x['level'] == '3' and x['bucket'] == b and x['vs'] == 'nodict z3'), None)
            else: x = next((x for x in g if x['group'] == grp and x['eco'] == eco and x['kind'] == kind and x['level'] == str(lvl) and x['bucket'] == b and x['vs'] == 'nodict z3'), None)
            r.append(f"{float(x['gain_bytes_pct']):.1f}" if x else '')
        rows.append(r)
    tab(['saving vs plain zstd:3 (%)'] + B, rows)
def levels(grp, arm):
    print(f'**Dictionary at each level against the same level without a dictionary ({grp}, project-disjoint folds, arm {arm}, type dictionary)**\n')
    rows = []
    for L in (1, 3, 9, 19):
        r = [f'zstd:{L}']
        for b in B:
            x = next((x for x in g if x['group'] == grp and x['eco'] == 'main' and x['arm'] == arm and x['kind'] == 'type' and x['level'] == str(L) and x['bucket'] == b and x['vs'] == f'nodict z{L}'), None)
            r.append(f"{float(x['gain_bytes_pct']):.1f}" if x else '')
        rows.append(r)
    tab(['level'] + B, rows)
sec = sys.argv[2] if len(sys.argv) > 2 else 'all'
print('### T-headline-P\n'); head('arm P (permissive-only training), text types, project-disjoint folds, zstd:3, saving in % of stored bytes (95 % CI over projects)', 'text', 'main', 'P')
print('### T-headline-C\n'); head('arm C (permissive + allowed copyleft training), same test sets', 'text', 'main', 'C')
print('### T-new-P\n'); head('new ecosystem (projects never used for training or fold assignment), arm P', 'text', 'new', 'P')
print('### T-new-C\n'); head('new ecosystem, arm C', 'text', 'new', 'C')
print('### T-bin\n'); head('binary types (ELF, class, wasm, sqlite, pdf), arm C, project-disjoint folds', 'binary', 'main', 'C')
print('### T-refs\n'); refs('text', 'main')
print('### T-levels\n'); levels('text', 'C'); levels('text', 'P')
for fn, title in (('gains_by_type_lt64k.csv', 'Per type, objects under 64 KiB, project-disjoint folds, zstd:3 (saving in % of stored bytes vs plain zstd:3)'), ('gains_by_type_lt64k_new_ecosystem.csv', 'Per type, objects under 64 KiB, new-ecosystem projects')):
    t = rd(fn)
    if not t: continue
    print(f'### {fn}\n\n**{title}**\n')
    tab(['type', 'files', 'proj', 'median B', 'P generic', 'P family', 'P type', 'P ext', 'C type', 'z9', 'z19', 'C type z3 vs z9'],
        [[x['type'], x['n_files'], x['n_projects'], x['median_size'], x.get('P.gt', ''), x.get('P.fam', ''), x.get('P.typ', ''), x.get('P.ext', ''), x.get('C.typ', ''), x.get('nodict_z9', ''), x.get('nodict_z19', ''), x.get('C.typ.z3_vs_z9', '')] for x in t])
pt = rd('paired_tests.csv')
if pt:
    print('### T-paired\n\n**Paired Wilcoxon tests per type (objects under 64 KiB, project-disjoint folds, zstd:3), Holm-corrected over all tests; effect = bytes saved by B relative to A**\n')
    by = collections.defaultdict(list)
    for x in pt: by[x['comparison']].append(x)
    for c, xs in by.items():
        sig = [x for x in xs if x['significant_0.05'] == 'True']
        print(f"- {c}: {len(sig)} of {len(xs)} types significant after Holm; ", end='')
        big = [f"{x['type']} {float(x['gain_of_B_over_A_bytes_pct']):+.1f}" for x in sig if abs(float(x['gain_of_B_over_A_bytes_pct'])) >= 1]
        print('with |effect| >= 1 %: ' + (', '.join(big) if big else 'none'))
    print()
lv = rd('level_vs_dict_text.csv')
sw = rd('sweeps.csv')
if sw:
    names = list(dict.fromkeys(x['dict'] for x in sw))
    print('### T-sweep-size\n'); 
    cols = ['sweep-size16', 'sweep-size32', 'sweep-size64', 'sweep-size112', 'sweep-size256']
    tab(['dictionary (zstd:3, fold 0, objects <= 256 KiB)', '16 KiB', '32 KiB', '64 KiB', '112 KiB', '256 KiB'], [[n] + [next((x['gain_pct'] for x in sw if x['dict'] == n and x['sweep'] == c), '') for c in cols] for n in names])
    print('### T-sweep-train\n')
    cols = ['sweep-train0.25', 'sweep-train1', 'sweep-train4', 'sweep-train16']
    tab(['dictionary (110 KiB, zstd:3, fold 0)', '0.25 MiB', '1 MiB', '4 MiB', '16 MiB'], [[n] + [next((x['gain_pct'] for x in sw if x['dict'] == n and x['sweep'] == c), '') for c in cols] for n in names])
tp = rd('throughput_quiet_host.csv')
if tp:
    print('### T-throughput\n')
    tab(['object set', 'level', 'mode', 'ratio', 'compress MB/s (median; slowest-fastest of reps)', 'decompress MB/s (median)', 'CDict prepare ms', 'DDict prepare ms', 'CDict B', 'DDict B', 'load1'],
        [[x['set'], x['level'], x['mode'], x['ratio'], f"{x['comp_MBps_median']} ({x['comp_MBps_slowest_rep']}-{x['comp_MBps_fastest_rep']})", f"{x['decomp_MBps_median']} ({x['decomp_MBps_slowest_rep']}-{x['decomp_MBps_fastest_rep']})", x['cdict_prepare_ms'], x['ddict_prepare_ms'], x['cdict_bytes'], x['ddict_bytes'], x['load1_before']] for x in tp])
for fn, title in (('selection_accuracy.csv', 'Selection accuracy (exact type)'), ('selection_loss_lt64k_all.csv', 'Stored bytes against the oracle, objects <= 64 KiB, zstd:3, arm C dictionaries'), ('oracle_choice_by_type.csv', 'Oracle versus own-type dictionary, per type (objects <= 256 KiB in the matrix subset)')):
    t = rd(fn)
    if t:
        print(f'### {fn}\n\n**{title}**\n'); tab(list(t[0]), [list(x.values()) for x in t])
# ---- extra tables
sr = rd('gains_size_ranges.csv')
if sr:
    print('### T-ranges\n')
    rows = []
    for x in sr:
        if x['group'] == 'text' and x['range'] == '1-64KiB' and x['level'] == '3' and x['arm'] in ('P', 'C', '-') and x['kind'] != 'nodict':
            rows.append((x['eco'], x['arm'], x['kind'], x['n_files'], x['gain_bytes_pct'], f"{x['ci_lo']} to {x['ci_hi']}"))
    tab(['test set', 'arm', 'dictionary', 'files', 'saving % (1-64 KiB)', '95 % CI'], rows)
cd = rd('shipped_set_candidates.csv')
if cd:
    print('### T-candidates\n')
    bks = ['<1KiB', '1-4KiB', '4-16KiB', '16-64KiB', '64-256KiB', '>=256KiB', '<64KiB', 'all']
    for eco in ('main', 'new'):
        rows = []
        for v in ('generic text', 'family', 'type', 'hybrid'):
            r = [v]
            for b in bks:
                x = next((x for x in cd if x['eco'] == eco and x['variant'] == v and x['bucket'] == b), None)
                r.append(f"{x['saving_pct']} ({x['ci_lo']} to {x['ci_hi']})" if x else '')
            rows.append(r)
        print(f'**{"project-disjoint folds" if eco == "main" else "new ecosystem"}, arm P dictionaries, text and binary types, zstd:3**\n'); tab(['candidate set'] + bks, rows)
inv = rd('shipped_set_inventory.csv')
if inv:
    print('### T-inventory\n')
    tab(['dictionary', 'dictID', 'bytes', 'training projects', 'training MiB', 'training licence mix (share of bytes, top 3)'],
        [[x['name'], x['dict_id'], x['dict_bytes'], x['train_projects'], round(int(x['train_bytes']) / 2**20, 1), ', '.join(f'{k} {float(v)*100:.0f} %' for k, v in list(__import__('json').loads(x['licence_mix']).items())[:3])] for x in inv])
ch = rd('chunk_size_sensitivity.csv')
if ch:
    print('### T-chunk\n'); tab(list(ch[0]), [list(x.values()) for x in ch])
ct = rd('corpus_types.csv')
if ct:
    print('### T-corpus\n')
    tab(['type', 'family', 'files', 'MiB', 'projects', 'permissive-licensed projects', 'p10 / median / p90 size (B)', 'bytes by policy: perm / copyleft-ok / excluded (%)'],
        [[x['type'], x['family'], x['files'], round(int(x['bytes']) / 2**20, 1), x['projects'], x['perm_projects'], f"{x['p10']} / {x['median']} / {x['p90']}",
          f"{round(100*int(x['perm_bytes'])/int(x['bytes']))} / {round(100*int(x['copyleft_bytes'])/int(x['bytes']))} / {round(100*int(x['excluded_bytes'])/int(x['bytes']))}"] for x in ct])

sa = rd('selection_accuracy.csv'); sl = rd('selection_loss_lt64k_all.csv')
NAMES = {'none': 'no dictionary', 'generic_text': 'one generic text dictionary (text/binary known)', 'true': 'true-type dictionary (perfect selector)', 'oracle': 'oracle (best of all dictionaries per object)',
 'ext0': 'extension map, real file names', 'ext1': 'extension map, names without extension', 'ext2': 'extension map, misleading extension', 'magic': 'libmagic, hand-written description-to-type map',
 'magic_cal': 'libmagic, description-to-type map learned on training folds', 'nb': 'naive Bayes on content only', 'nbx0': 'naive Bayes + extension feature, real names', 'nbx1': 'naive Bayes + extension feature, no extension',
 'nbx2': 'naive Bayes + extension feature, misleading extension', 'cas0': 'cascade (extension, else naive Bayes), real names', 'cas1': 'cascade, no extension', 'cas2': 'cascade, misleading extension',
 'cas2b0': 'agreement cascade (naive Bayes overrides a disagreeing extension when confidence >= 0.9), real names', 'cas2b1': 'agreement cascade, no extension', 'cas2b2': 'agreement cascade, misleading extension'}
if sl:
    print('### T-selection\n')
    acc = {(x['strategy'], x['test_set']): x['accuracy_pct'] for x in sa}
    rows = []
    for st in ['none', 'generic_text', 'true', 'oracle', 'ext0', 'ext1', 'ext2', 'magic', 'magic_cal', 'nb', 'nbx0', 'nbx1', 'nbx2', 'cas0', 'cas1', 'cas2', 'cas2b0', 'cas2b1', 'cas2b2']:
        a = next((x for x in sl if x['strategy'] == st and x['test_set'] == 'project-disjoint'), None); n = next((x for x in sl if x['strategy'] == st and x['test_set'] == 'new-ecosystem'), None)
        if not a: continue
        rows.append([NAMES[st], acc.get((st, 'project-disjoint folds'), ''), acc.get((st, 'new-ecosystem'), ''), a['gain_vs_no_dict_pct'], f"{a['loss_vs_oracle_pct']} ({a['ci_lo']} to {a['ci_hi']})", n['loss_vs_oracle_pct'] if n else ''])
    tab(['selector', 'exact-type accuracy, folds (%)', 'new ecosystem (%)', 'saving vs no dictionary, folds (%)', 'extra stored bytes vs oracle, folds (%, 95 % CI)', 'extra vs oracle, new ecosystem (%)'], rows)
