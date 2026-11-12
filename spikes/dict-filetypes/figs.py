#!/usr/bin/env python3 -I
"""SVG figures from the summary CSVs.  usage: figs.py CSV_DIR FIG_DIR"""
import sys as _s, os as _o; _s.path.insert(0, _o.path.dirname(_o.path.abspath(__file__)))
import sys, os, csv, collections
from xml.sax.saxutils import escape
src, dst = sys.argv[1], sys.argv[2]; os.makedirs(dst, exist_ok=True)
PAL = ['#4e79a7', '#f28e2b', '#59a14f', '#b07aa1', '#9c755f', '#e15759', '#76b7b2']
def rd(fn): return list(csv.DictReader(open(os.path.join(src, fn))))
def f(x):
    try: return float(x)
    except Exception: return None
def svg_open(w, h, title):
    return [f'<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" viewBox="0 0 {w} {h}" font-family="sans-serif" font-size="11">',
            f'<title>{escape(title)}</title>', f'<rect width="{w}" height="{h}" fill="#ffffff"/>',
            f'<text x="{w/2}" y="18" text-anchor="middle" font-size="13" font-weight="bold">{escape(title)}</text>']
def legend(o, names, x, y):
    for i, n in enumerate(names):
        o.append(f'<rect x="{x}" y="{y+i*15}" width="10" height="10" fill="{PAL[i%len(PAL)]}"/><text x="{x+14}" y="{y+9+i*15}">{escape(n)}</text>')
def vbars(fname, title, cats, series, ylabel, ymin=None, ymax=None, w=820, h=420):
    o = svg_open(w, h, title); L, R, Tp, B = 60, 170, 35, 70; pw, ph = w - L - R, h - Tp - B
    vals = [v for s in series.values() for v in s if v is not None]
    lo = min(0, min(vals)) if ymin is None else ymin; hi = max(vals) * 1.1 if ymax is None else ymax
    y = lambda v: Tp + ph * (1 - (v - lo) / (hi - lo))
    step = (hi - lo) / 5
    for i in range(6):
        v = lo + i * step; o.append(f'<line x1="{L}" x2="{L+pw}" y1="{y(v)}" y2="{y(v)}" stroke="#ddd"/><text x="{L-6}" y="{y(v)+4}" text-anchor="end">{0 if abs(v) < 0.5 else round(v)}</text>')
    o.append(f'<text transform="translate(14,{Tp+ph/2}) rotate(-90)" text-anchor="middle">{escape(ylabel)}</text>')
    gw = pw / len(cats); bw = gw * 0.8 / len(series)
    for ci, c in enumerate(cats):
        for si, (sn, vs) in enumerate(series.items()):
            v = vs[ci]
            if v is None: continue
            x = L + ci * gw + gw * 0.1 + si * bw
            o.append(f'<rect x="{x:.1f}" y="{min(y(v), y(0)):.1f}" width="{bw:.1f}" height="{abs(y(v)-y(0)):.1f}" fill="{PAL[si%len(PAL)]}"/>')
        o.append(f'<text x="{L+ci*gw+gw/2}" y="{Tp+ph+14}" text-anchor="middle">{escape(c)}</text>')
    o.append(f'<line x1="{L}" x2="{L+pw}" y1="{y(0)}" y2="{y(0)}" stroke="#333"/>'); legend(o, list(series), L + pw + 12, Tp + 10)
    open(os.path.join(dst, fname), 'w').write('\n'.join(o + ['</svg>']))
def hbars(fname, title, cats, series, xlabel):
    rowh = 12 * len(series) + 6; h = 60 + rowh * len(cats); w = 760; L = 90; R = 150; pw = w - L - R
    o = svg_open(w, h, title); vals = [v for s in series.values() for v in s if v is not None]; hi = max(vals) * 1.08; lo = min(0, min(vals))
    x = lambda v: L + pw * (v - lo) / (hi - lo)
    for i in range(0, int(hi) + 1, 10 if hi > 30 else 5):
        o.append(f'<line x1="{x(i)}" x2="{x(i)}" y1="30" y2="{h-30}" stroke="#ddd"/><text x="{x(i)}" y="{h-16}" text-anchor="middle">{i}</text>')
    o.append(f'<text x="{L+pw/2}" y="{h-3}" text-anchor="middle">{escape(xlabel)}</text>')
    for ci, c in enumerate(cats):
        y0 = 34 + ci * rowh; o.append(f'<text x="{L-4}" y="{y0+rowh/2}" text-anchor="end">{escape(c)}</text>')
        for si, (sn, vs) in enumerate(series.items()):
            v = vs[ci]
            if v is None: continue
            o.append(f'<rect x="{x(0):.1f}" y="{y0+si*12}" width="{max(x(v)-x(0),0):.1f}" height="10" fill="{PAL[si%len(PAL)]}"/>')
    legend(o, list(series), L + pw + 10, 40)
    open(os.path.join(dst, fname), 'w').write('\n'.join(o + ['</svg>']))
def lines(fname, title, xs, series, xlabel, ylabel, w=720, h=400):
    o = svg_open(w, h, title); L, R, Tp, B = 60, 190, 35, 50; pw, ph = w - L - R, h - Tp - B
    import math
    lx = [math.log2(v) for v in xs]; vals = [v for s in series.values() for v in s if v is not None]; hi = max(vals) * 1.1; lo = 0
    X = lambda v: L + pw * (math.log2(v) - lx[0]) / (lx[-1] - lx[0]); Y = lambda v: Tp + ph * (1 - (v - lo) / (hi - lo))
    for i in range(6):
        v = lo + i * (hi - lo) / 5; o.append(f'<line x1="{L}" x2="{L+pw}" y1="{Y(v)}" y2="{Y(v)}" stroke="#ddd"/><text x="{L-6}" y="{Y(v)+4}" text-anchor="end">{v:.0f}</text>')
    for v in xs: o.append(f'<text x="{X(v)}" y="{Tp+ph+14}" text-anchor="middle">{v:g}</text>')
    o.append(f'<text x="{L+pw/2}" y="{h-8}" text-anchor="middle">{escape(xlabel)}</text><text transform="translate(14,{Tp+ph/2}) rotate(-90)" text-anchor="middle">{escape(ylabel)}</text>')
    for si, (sn, vs) in enumerate(series.items()):
        pts = [(X(x), Y(v)) for x, v in zip(xs, vs) if v is not None]
        if pts: o.append(f'<polyline fill="none" stroke="{PAL[si%len(PAL)]}" stroke-width="2" points="{" ".join(f"{a:.1f},{b:.1f}" for a, b in pts)}"/>' + ''.join(f'<circle cx="{a:.1f}" cy="{b:.1f}" r="3" fill="{PAL[si%len(PAL)]}"/>' for a, b in pts))
    legend(o, list(series), L + pw + 10, Tp + 10)
    open(os.path.join(dst, fname), 'w').write('\n'.join(o + ['</svg>']))

B = ['<1KiB', '1-4KiB', '4-16KiB', '16-64KiB', '64-256KiB', '256KiB-4MiB', '>4MiB']
try:
    g = rd('gains_overall.csv')
    def pick(kind, lvl, arm='C', grp='text', eco='main'):
        d = {r['bucket']: f(r['gain_bytes_pct']) for r in g if r['kind'] == kind and r['level'] == str(lvl) and r['group'] == grp and r['eco'] == eco and r['arm'] in (arm, '-') and r['vs'] == f'nodict z{lvl}' or (r['kind'] == kind and r['level'] == str(lvl) and r['group'] == grp and r['eco'] == eco and r['arm'] in (arm, '-') and kind == 'nodict' and r['vs'] == 'nodict z3')}
        return [d.get(b) for b in B]
    vbars('fig1_gain_by_size_z3.svg', 'Saving vs plain zstd:3, text types, project-disjoint (stored bytes)', B,
          {'generic text dict': pick('generic text', 3), 'family dicts': pick('family', 3), 'per-type dicts': pick('type', 3), 'zstd:9, no dict': pick('nodict', 9), 'zstd:19, no dict': pick('nodict', 19)}, 'saving (%)')
except Exception as e: sys.stderr.write(f'fig1 skipped: {e}\n')
try:
    t = [r for r in rd('gains_by_type_z3.csv') if f(r.get('C.typ')) is not None and r['type'] not in ('png', 'jpeg', 'zip', 'gz', 'video')]
    t.sort(key=lambda r: -f(r['C.typ']))
    hbars('fig2_gain_by_type_z3.svg', 'Saving vs zstd:3 by type (arm C dictionaries, all sizes)', [r['type'] for r in t],
          {'generic text': [f(r.get('C.gt')) for r in t], 'family': [f(r.get('C.fam')) for r in t], 'per-type': [f(r.get('C.typ')) for r in t]}, 'saving of stored bytes (%)')
except Exception as e: sys.stderr.write(f'fig2 skipped: {e}\n')
try:
    s = rd('sweeps.csv'); names = ['type:python', 'type:json', 'type:html', 'type:c', 'generic_text', 'family:clike']
    xs = [16, 32, 64, 112, 256]
    ser = {n: [next((f(r['gain_pct']) for r in s if r['dict'] == n and r['sweep'] == f'sweep-size{x}'), None) for x in xs] for n in names}
    lines('fig3_dict_size_sweep.svg', 'Dictionary size sweep (zstd:3, objects <= 256 KiB, fold 0)', xs, ser, 'dictionary size (KiB, log scale)', 'saving (%)')
    xs = [0.25, 1, 4, 16]
    ser = {n: [next((f(r['gain_pct']) for r in s if r['dict'] == n and r['sweep'] == f'sweep-train{x}'), None) for x in xs] for n in names}
    lines('fig4_training_size_sweep.svg', 'Training-set size sweep (112 KiB dictionary, zstd:3)', xs, ser, 'training data (MiB, log scale)', 'saving (%)')
except Exception as e: sys.stderr.write(f'fig3/4 skipped: {e}\n')
try:
    sl = rd('selection_loss_lt64k.csv')
    hbars('fig5_selection_loss.svg', 'Loss against oracle, objects <= 64 KiB, zstd:3', [r['strategy'] for r in sl], {'extra stored bytes vs oracle (%)': [f(r['loss_vs_oracle_pct']) for r in sl]}, 'percent of oracle stored bytes')
except Exception as e: sys.stderr.write(f'fig5 skipped: {e}\n')
