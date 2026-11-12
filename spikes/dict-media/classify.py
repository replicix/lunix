#!/usr/bin/env python3
"""classify.py <corpus> : merge manifests -> files.tsv (fmt, set, source, origin, licence, size, sha256), capped per format.
Also builds the derived 7z set. Deterministic (seed 1)."""
import os, random, subprocess, sys, glob, csv, hashlib
C = os.path.expanduser(sys.argv[1]); CAPF, CAPB = 400, 300_000_000
G = {"jpeg_web":"jpeg","jpeg_q75base":"jpeg","jpeg_q90opt":"jpeg","jpeg_q85prog":"jpeg","jpeg_photo":"jpeg","jpeg_thumb":"jpeg","jpeg_thumb2":"jpeg","jpeg_thumbd":"jpeg","png_thumbd":"png","webp_thumbd":"webp","ico":"ico",
 "png_screenshot":"png","png_icon":"png","png_misc":"png","kodak":"png","png_thumb":"png","webp":"webp","webp_lossy":"webp","webp_lossless":"webp",
 "tiff":"tiff","img_tiff":"tiff-raw","img_bmp":"bmp","img_heic":"heic","img_avif":"avif","gif_derived":"gif","gif":"gif",
 "audio_mp3":"mp3","audio_vorbis":"ogg-vorbis","audio_ogg":"ogg-vorbis","jar":"jar","apk":"apk","docx":"docx","docx_lo":"docx","xlsx":"xlsx","pptx":"pptx",
 "odt":"odf","ods":"odf","odp":"odf","epub":"epub","pdf":"pdf","gz_small":"gz","gz_pkg":"tar.gz","xz_pkg":"xz","zst":"zstd","sevenz":"7z"}
def fmt(group, path):
    if group in G: return G[group]
    n = path.lower()
    if group == "gnu_tar": return "tar.gz" if n.endswith(".gz") else "xz" if n.endswith(".xz") else "tar.bz2"
    if group == "audio_derived":
        for k, v in ((".m4a","aac"),(".opus","opus"),(".mp3","mp3"),(".flac","flac"),(".wav","wav")): 
            if n.endswith(k): return v
    if group == "video_derived":
        for k, v in (("_h264-copy-ts.","ts-h264"),("_h264-copy.","mp4-h264"),("_h265.","mp4-h265"),("_vp9.","webm-vp9"),("_av1.","mp4-av1")):
            if k in n: return v
    return None
# derived 7z archives from groups of ~20 gz man pages (decompressed)
if not glob.glob(C + "/manifest-sevenz.tsv"):
    gz = [l.split("\t") for l in open(C + "/manifest-gz_small.tsv")]
    os.makedirs(C + "/sevenz", exist_ok=True); tmp = C + "/_7z"; out = open(C + "/manifest-sevenz.tsv", "w")
    for i in range(0, 800, 20):
        subprocess.call(["rm", "-rf", tmp]); os.makedirs(tmp)
        for j, r in enumerate(gz[i:i+20]):
            subprocess.call("zcat %s > %s/f%d 2>/dev/null" % (os.path.join(C, r[0]), tmp, j), shell=True)
        p = f"{C}/sevenz/man{i//20}.7z"; subprocess.call(["7z", "a", "-bd", "-y", p, tmp + "/*"], stdout=subprocess.DEVNULL)
        d = open(p, "rb").read()
        out.write("\t".join([f"sevenz/man{i//20}.7z", "sevenz", "s:%d" % (i//20), "derived", "Ubuntu docs; 7z default", "derived://7z", str(len(d)), hashlib.sha256(d).hexdigest()]) + "\n")
    out.close()
rows = []
for mf in sorted(glob.glob(C + "/manifest-*.tsv")):
    for l in open(mf):
        p = l.rstrip("\n").split("\t")
        if p[1] in ("blender_src",): continue
        f = fmt(p[1], p[0])
        if p[2].startswith(('p:poi', 'p:lo/')): p[2] += '/' + os.path.basename(p[0]).rsplit('_', 1)[-1][:6]   # pseudo-source: file-name tail (weak)
        if p[1].endswith('thumbd'): p[2] = 'c:%d' % (int(p[5].rsplit('/', 1)[1].split('.')[0]) // 5000)   # same cluster as the COCO original
        if f and int(p[6]) > 0: rows.append([f] + p)
rnd = random.Random(1); rnd.shuffle(rows); cnt, byt, keep = {}, {}, []
for r in rows:
    f, sz = r[0], int(r[7])
    if cnt.get(f, 0) >= CAPF or byt.get(f, 0) + sz > CAPB: continue
    cnt[f] = cnt.get(f, 0) + 1; byt[f] = byt.get(f, 0) + sz; keep.append(r)
keep.sort(key=lambda r: (r[0], r[1]))
with open(C + "/../files.tsv", "w") as o:
    o.write("fmt\tpath\tset\tsource\torigin\tlicence\turl\tsize\tsha256\n")
    for r in keep: o.write("\t".join(r) + "\n")
for f in sorted(cnt): print(f, cnt[f], byt[f] // 1000000, "MB")
