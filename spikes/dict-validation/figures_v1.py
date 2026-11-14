#!/usr/bin/env python3
"""SVG figures for SUMMARY.md section 8, from analyze.py / decide.py output only (aggregates).
usage: figures.py <ana/out> <raw-dir> <figdir>"""
import sys, os, csv, statistics
import matplotlib
matplotlib.use('Agg')
import matplotlib.pyplot as plt
OUT, RAW, FIG = sys.argv[1:4]; os.makedirs(FIG, exist_ok=True)
plt.rcParams.update({'svg.hashsalt': 'dictval', 'font.size': 9, 'axes.spines.top': False, 'axes.spines.right': False, 'axes.edgecolor': '#8a8984',
                     'axes.labelcolor': '#0b0b0b', 'xtick.color': '#52514e', 'ytick.color': '#52514e', 'grid.color': '#e4e3df', 'axes.grid': True, 'grid.linewidth': 0.6,
                     'figure.facecolor': '#fcfcfb', 'axes.facecolor': '#fcfcfb', 'legend.frameon': False})
C = ['#2a78d6', '#eb6834', '#1baf7a', '#eda100', '#e87ba4', '#008300', '#6250d6', '#e34948']; GREY = '#8a8984'; INK = '#0b0b0b'
def opn(n): return open(os.path.join(OUT, n) if os.path.exists(os.path.join(OUT, n)) else os.path.join(OUT, 'analysis-' + n))     # ana/out or raw/ (analysis-*)
DS = list(csv.DictReader(opn('desc.csv'))); T = list(csv.DictReader(opn('tests.csv')))
def desc(key, arm, mask='full', cls='all', comp=None):
    r = [d for d in DS if d['key'] == key and d['arm'] == arm and d['mask'] == mask and d['cls'] == cls and d['shift'] == '0' and (comp is None or d['comp'] == comp)]
    return r[0] if r else None
def save(fig, name): fig.savefig(os.path.join(FIG, name), format='svg', metadata={'Date': None}, bbox_inches='tight'); plt.close(fig)
ELIG = ['host-ext4', 'host-xfs', 'host-btrfs', 'vm-ubuntu']; MK = ['o', 's', '^', 'D']
# 1. saving by arm per corpus
fig, axs = plt.subplots(1, 2, figsize=(9, 3.6), sharey=True)
arms = ['ft@1', 'ft@3', 'bs-dist@1', 'bs-dist@3', 'oracle@3']
for ax, cs, cf, alt in ((axs[0], 16384, 'z3', 'z1'), (axs[1], 65536, 'z1', 'z3')):
    al = [alt] + arms
    for i, c in enumerate(ELIG):
        for j, a in enumerate(al):
            d = desc('%s-%d' % (c, cs), a)
            s, lo, hi = (100 * float(d[k]) for k in ('S', 'LB', 'bca_hi'))
            ax.errorbar(s, j + (i - 1.5) * 0.15, xerr=[[s - lo], [hi - s]], fmt=MK[i], color=C[i], ms=4, lw=1, label=c if j == 0 else None)
    ax.axvline(0, color=GREY, lw=0.8); ax.axvline(3, color=C[7], lw=1, ls='--'); ax.text(3.05, len(al) - 0.6, 'D1 floor 3 pp', color=INK, fontsize=8)
    ax.set_yticks(range(len(al))); ax.set_yticklabels(['other plain level\n(z1 at 16 KiB, z3 at 64 KiB)'] + arms); ax.set_xlabel('volume-wide saving against %s behind the probe (%%)' % cf); ax.set_title('%d KiB chunks (counterfactual %s)' % (cs // 1024, cf), fontsize=9)
fig.legend(*axs[0].get_legend_handles_labels(), loc='lower center', ncol=4, fontsize=8, bbox_to_anchor=(0.5, -0.08)); save(fig, 'fig-q1-arms.svg')
# 2. per-class saving (16 KiB)
fig, axs = plt.subplots(1, 4, figsize=(10, 3), sharey=True)
for ax, c in zip(axs, ELIG):
    cls = [k for k in ('data', 'dir', 'meta', 'journal', 'unmapped', 'mixed') if desc('%s-16384' % c, 'ft@3', cls=k)]
    for j, a in enumerate(('ft@3', 'bs-dist@3', 'oracle@3')):
        v = [100 * float(desc('%s-16384' % c, a, cls=k)['S']) for k in cls]
        ax.bar([x + (j - 1) * 0.27 for x in range(len(cls))], v, width=0.25, color=C[j], label=a)
    ax.axhline(0, color=GREY, lw=0.8); ax.set_xticks(range(len(cls))); ax.set_xticklabels(cls, rotation=45, fontsize=8); ax.set_title(c, fontsize=9)
axs[0].set_ylabel('saving against z3 (%), 16 KiB'); fig.legend(*axs[0].get_legend_handles_labels(), loc='lower center', ncol=3, fontsize=8, bbox_to_anchor=(0.5, -0.12)); save(fig, 'fig-q1-class.svg')
# 3. pv against shipped per volume (16 KiB): N strict and N lenient
rows = [r for r in T if r['family'] == 'D2' and r['size'] == '16384' and r['shift'] == '0' and r['arm'] == 'pv-s1@3']
fig, ax = plt.subplots(figsize=(9, 3.4))
for i, r in enumerate(rows):
    ax.plot([i, i], [float(r['N']), float(r['len_N'])], color=GREY, lw=0.8)
    ax.plot(i, float(r['N']), 'o', color=C[0] if r['cap_ok'] == '1' else C[7], ms=5, label=None)
    ax.plot(i, float(r['len_N']), 's', color=C[1], ms=4, mfc='none')
ax.plot([], [], 'o', color=C[0], label='N strict (cap vs counterfactual passes)'); ax.plot([], [], 'o', color=C[7], label='N strict, cap vs counterfactual fails (0.78)')
ax.plot([], [], 's', color=C[1], mfc='none', label='N lenient (cap and cores vs the shipped-set comparator)')
ax.axhline(2, color=INK, lw=1, ls='--'); ax.text(len(rows) - 0.5, 2.15, 'D2 floor 2 pp', ha='right', fontsize=8); ax.axhline(0, color=GREY, lw=0.8)
ax.set_xticks(range(len(rows))); ax.set_xticklabels([r['corpus'].replace('db-', '') for r in rows], rotation=60, ha='right', fontsize=7.5)
ax.set_ylabel('N of pv@3 vs comparator (pp)'); ax.legend(fontsize=7.5, loc='upper left'); ax.set_title('Per-volume dictionary (seed 1, level 3) against the best of counterfactual and shipped sets, 16 KiB', fontsize=9)
save(fig, 'fig-q2-pv.svg')
# 4. sweep
SW = list(csv.DictReader(open(os.path.join(RAW, 'db_sweep.csv'))))
def sw(pop, arm): return int([x for x in SW if x['pop'] == pop and x['class'] == 'all' and x['arm'] == arm][0]['stored_bytes'])
fig, axs = plt.subplots(1, 2, figsize=(8, 3), sharex=True)
ds = [16, 32, 64, 110]
for j, (f, lab) in enumerate((('0.001', '0.1 % sample'), ('0.01', '1 %'), ('0.1', '10 %'))):
    m = 'pv-s1' if f == '0.01' else 'sw-f' + f
    v = [100 * float(desc('host-ext4-16384', ('pv-s1' if (f, d) == ('0.01', 110) else 'sw-f%s-d%d' % (f, d)) + '@3', mask=m)['S']) for d in ds]
    axs[0].plot(ds, v, marker='o', color=C[j], lw=2, ms=5, label=lab)
    v = [100 * (1 - sw('smp-f' + f, 'pv_f%s_d%d@3' % (f, d)) / sw('smp-f' + f, 'z3')) for d in ds]
    axs[1].plot(ds, v, marker='o', color=C[j], lw=2, ms=5, label=lab)
for ax, t in zip(axs, ('host-ext4, 16 KiB', 'se-askubuntu x PostgreSQL, 16 KiB')):
    ax.axhline(0, color=GREY, lw=0.8); ax.set_xticks(ds); ax.set_xlabel('dictionary size (KiB)'); ax.set_title(t, fontsize=9)
axs[0].set_ylabel('pv@3 saving against z3 (%)'); axs[1].legend(fontsize=8); save(fig, 'fig-q2-sweep.svg')
# 5. database: saving by chunk/object size (median over the 15 images)
DB = ['db-%s-%s' % (d, e) for d in ('se-askubuntu', 'se-superuser', 'se-serverfault', 'geonames', 'tlc-2024q1') for e in ('pg', 'my', 'sq')]
pts = [('page', lambda n, a: desc('cv-%s-pg' % n, a, mask='all')), ('16 KiB\n(Lunix, pages)', lambda n, a: desc('%s-16384' % n, a, cls='dbpage')),
       ('64 KiB\n(Lunix, pages)', lambda n, a: desc('%s-65536' % n, a, cls='dbpage')), ('1 MiB', lambda n, a: desc('cv-%s-1m' % n, a, mask='all')), ('4 MiB', lambda n, a: desc('cv-%s-4m' % n, a, mask='all'))]
fig, ax = plt.subplots(figsize=(7, 3.2))
for j, a in enumerate(('dbd@3', 'dbs@3')):
    def vz3(g, n, a):
        d = g(n, a); z = g(n, 'z3')
        return 100 * (1 - int(d['stored']) / (int(z['stored']) if z else int(d['comp_stored'])))
    v = [statistics.median(vz3(g, n, a) for n in DB) for _, g in pts]
    ax.plot(range(len(pts)), v, marker='o', color=C[j], lw=2, ms=5, label={'dbd@3': 'DB set, schema-disjoint (decides)', 'dbs@3': 'DB set, same schema (reported only)'}[a])
ax.axhline(0, color=GREY, lw=0.8); ax.axhline(3, color=C[7], lw=1, ls='--'); ax.text(4, 3.2, 'D6 floor 3 pp', ha='right', fontsize=8)
ax.set_xticks(range(len(pts))); ax.set_xticklabels([p[0] for p in pts], fontsize=8); ax.set_ylabel('median saving over 15 images (%)')
ax.set_title('Database files: dictionary at level 3 against plain z3 by object size (Lunix: z3 behind the probe, CF)', fontsize=9); ax.legend(fontsize=8); save(fig, 'fig-q3-size.svg')
# 6. compress CPU against saving (timing samples)
TM = list(csv.DictReader(open(os.path.join(RAW, 'timing.csv'))))
fig, axs = plt.subplots(1, 3, figsize=(11, 3.4))
for ax, cell in zip(axs, ('vm-ubuntu-16384', 'vm-ubuntu-65536', 'db-se-askubuntu-pg-16384')):
    rs = [r for r in TM if r['cell'] == cell and r['op'] == 'compress' and r['stored_bytes']]
    z1 = float([r for r in rs if r['arm'] == 'z1'][0]['stored_bytes'])
    for r in rs:
        plain = r['arm'].startswith('z'); x = float(r['median']); y = 100 * (1 - float(r['stored_bytes']) / z1)
        ax.plot(x, y, 'o' if plain else 's', color=GREY if plain else C[0 if r['level'] == '1' else 1], ms=4 if plain else 5)
        if not plain or r['arm'] in ('z1', 'z3', 'z9'): ax.annotate('%s@%s' % (r['arm'], r['level']) if not plain else r['arm'], (x, y), fontsize=6.5, xytext=(3, 2), textcoords='offset points')
    zc = float([r for r in rs if r['arm'] == 'z1'][0]['median']); ax.axvline(zc / 0.8, color=C[7], lw=1, ls='--')
    ax.set_xscale('log'); ax.set_xlabel('compress CPU-s per GB (one core)'); ax.set_title(cell, fontsize=9)
axs[0].set_ylabel('saving against z1 behind the probe (%)')
axs[0].plot([], [], 'o', color=GREY, label='plain z1..z9'); axs[0].plot([], [], 's', color=C[0], label='dictionary arm, level 1'); axs[0].plot([], [], 's', color=C[1], label='dictionary arm, level 3')
axs[0].plot([], [], color=C[7], ls='--', label='0.8x cap against z1'); axs[0].legend(fontsize=7, loc='lower right'); save(fig, 'fig-cpu.svg')
print('figures written')
