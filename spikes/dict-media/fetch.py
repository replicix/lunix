#!/usr/bin/env python3
"""Corpus fetcher (runs on the test host only). Usage: fetch.py <group> [outdir]
Every file is recorded in <out>/manifest-<group>.tsv:
 path  group  source_id  origin(real|derived)  licence  url  size  sha256
source_id is the cluster used for source-disjoint splits (uploader, project, ...)."""
import csv, hashlib, json, os, random, sys, time, urllib.error, urllib.parse, urllib.request, re, subprocess

UA = {"User-Agent": "replicix-compression-study/0.1 (non-commercial research; bulk-polite)"}
OUT = os.path.expanduser(sys.argv[2] if len(sys.argv) > 2 else "~/dictmedia/corpus")
rnd = random.Random(20261112)

def http(url, tries=7, timeout=90):
    for i in range(tries):
        try:
            with urllib.request.urlopen(urllib.request.Request(url, headers=UA), timeout=timeout) as r:
                return r.read()
        except urllib.error.HTTPError as e:
            if e.code in (429, 500, 502, 503, 504):
                time.sleep(min(120, float(e.headers.get("Retry-After") or 4 * 2 ** i)))
            else:
                raise
        except (urllib.error.URLError, TimeoutError, ConnectionError):
            time.sleep(3 * 2 ** i)
    raise RuntimeError("giving up " + url)

class Manifest:
    def __init__(self, group):
        self.group = group
        self.dir = os.path.join(OUT, group)
        os.makedirs(self.dir, exist_ok=True)
        self.p = os.path.join(OUT, f"manifest-{group}.tsv")
        self.seen = set()
        if os.path.exists(self.p):
            for l in open(self.p):
                self.seen.add(l.split("\t")[5])
        self.f = open(self.p, "a")
    def add(self, name, data, source, origin, lic, url):
        if url in self.seen:
            return
        name = re.sub(r"[^A-Za-z0-9._-]", "_", name)[:120]
        path = os.path.join(self.dir, name)
        with open(path, "wb") as f:
            f.write(data)
        self.f.write("\t".join([os.path.relpath(path, OUT), self.group, source, origin, lic, url,
                                str(len(data)), hashlib.sha256(data).hexdigest()]) + "\n")
        self.f.flush(); self.seen.add(url)

FREE = re.compile(r"^(CC0|CC[ -]BY|Public domain|PD|CC-BY|Attribution|GFDL|GNU|Copyrighted free use|FAL|OGL)", re.I)

def commons(group, query, want, per_user=8, thumb=None, maxbytes=None, words=None, minbytes=0):
    """Random-sorted Commons search; one query per seed word for diversity."""
    m = Manifest(group)
    got, users = len(m.seen), {}
    for w in (words or [""]):
        if got >= want: break
        off, tries_here = 0, 0
        while got < want and tries_here < 4:
            p = dict(action="query", format="json", generator="search", gsrsearch=f"{query} {w}".strip(),
                     gsrnamespace=6, gsrlimit=50, gsroffset=off, gsrsort="random", prop="imageinfo",
                     iiprop="url|size|mime|user|extmetadata", iiextmetadatafilter="LicenseShortName")
            if thumb: p["iiurlwidth"] = thumb
            try:
                r = json.loads(http("https://commons.wikimedia.org/w/api.php?" + urllib.parse.urlencode(p)))
            except Exception as e:
                print("api fail", e, flush=True); break
            time.sleep(1.0)
            pages = list(r.get("query", {}).get("pages", {}).values())
            if not pages: break
            for pg in pages:
                if got >= want: break
                ii = (pg.get("imageinfo") or [None])[0]
                if not ii: continue
                lic = ii.get("extmetadata", {}).get("LicenseShortName", {}).get("value", "")
                if not FREE.match(lic): continue
                u = ii["user"]
                if users.get(u, 0) >= per_user: continue
                url = ii["thumburl"] if thumb and ii.get("thumburl") else ii["url"]
                if url in m.seen: continue
                if maxbytes and not thumb and ii["size"] > maxbytes: continue
                try:
                    d = http(url)
                except Exception as e:
                    print("dl fail", url, e, flush=True); continue
                if len(d) < minbytes or (maxbytes and len(d) > maxbytes): continue
                users[u] = users.get(u, 0) + 1
                m.add(os.path.basename(urllib.parse.unquote(urllib.parse.urlparse(url).path)) if not thumb else
                      f"{pg['pageid']}_" + os.path.basename(urllib.parse.unquote(urllib.parse.urlparse(url).path)), d, "u:" + u, "real", lic, url)
                got += 1
                time.sleep(1.0)
            off += 50; tries_here += 1
        print(group, w, got, flush=True)

W = ["landscape","portrait","building","bird","street","church","flower","mountain","river","bridge","car","train",
     "museum","map","diagram","logo","statue","castle","city","sunset","market","animal","insect","painting",
     "school","airport","harbour","forest","festival","sport","food","tree","road","station","garden","lake","house","village","tower","island"]
S = " filesize:{a},{b}"
GROUPS = {
 "jpeg_photo":  lambda: commons("jpeg_photo", "filemime:image/jpeg" + S.format(a=150, b=6000), 420, words=W, maxbytes=6_500_000),
 "jpeg_thumb":  lambda: commons("jpeg_thumb", "filemime:image/jpeg" + S.format(a=100, b=6000), 360, words=W, thumb=160, per_user=10),
 "jpeg_thumb2": lambda: commons("jpeg_thumb2", "filemime:image/jpeg" + S.format(a=100, b=6000), 120, words=W[::-1], thumb=480, per_user=10),
 "png_misc":    lambda: commons("png_misc", "filemime:image/png" + S.format(a=1, b=3000), 360, words=W, maxbytes=4_000_000),
 "png_icon":    lambda: commons("png_icon", "filemime:image/png" + S.format(a=0, b=12), 240, words=["icon","logo","flag","symbol","button","emblem","coat of arms"], maxbytes=14_000, per_user=15),
 "png_screenshot": lambda: commons("png_screenshot", "filemime:image/png screenshot" + S.format(a=20, b=3000), 160, words=["Firefox","Linux","Windows","website","software","GNOME","KDE","terminal","editor","Wikipedia","game","app"], maxbytes=4_000_000),
 "png_thumb":   lambda: commons("png_thumb", "filemime:image/png" + S.format(a=5, b=3000), 160, words=W, thumb=200),
 "webp":        lambda: commons("webp", "filemime:image/webp" + S.format(a=1, b=6000), 160, words=W, maxbytes=6_000_000),
 "gif":         lambda: commons("gif", "filemime:image/gif" + S.format(a=0, b=4000), 160, words=W, maxbytes=4_500_000),
 "tiff":        lambda: commons("tiff", "filemime:image/tiff" + S.format(a=300, b=12000), 40, words=W, maxbytes=13_000_000),
 "audio_mp3":   lambda: commons("audio_mp3", "filemime:audio/mpeg" + S.format(a=30, b=6000), 100, words=["speech","music","song","recording","voice","pronunciation","interview","radio","concert","anthem"], maxbytes=7_000_000),
 "audio_ogg":   lambda: commons("audio_ogg", "filemime:application/ogg" + S.format(a=20, b=6000), 100, words=["speech","music","song","recording","voice","pronunciation","bird","anthem","opus","vorbis"], maxbytes=7_000_000),
 "audio_flac":  lambda: commons("audio_flac", "filemime:audio/x-flac" + S.format(a=20, b=12000), 60, words=["speech","music","recording","voice","pronunciation","bird"], maxbytes=13_000_000),
 "audio_wav":   lambda: commons("audio_wav", "filemime:audio/wav" + S.format(a=10, b=8000), 40, words=["speech","music","recording","voice","sound"], maxbytes=9_000_000),
 "video_webm":  lambda: commons("video_webm", "filemime:video/webm" + S.format(a=100, b=15000), 80, words=["demo","tutorial","animation","nature","city","talk","lecture","experiment"], maxbytes=16_000_000, per_user=6),
 "odf":         lambda: commons("odf", "filemime:application/vnd.oasis.opendocument.text", 40, words=[""], maxbytes=8_000_000) or None,
 "pdf":         lambda: commons("pdf", "filemime:application/pdf" + S.format(a=20, b=5000), 120, words=["report","law","manual","guide","map","paper","history","statistics","minutes","catalogue"], maxbytes=6_000_000, per_user=10),
}

def simple_list(group, items, origin="real"):
    m = Manifest(group)
    for name, url, source, lic in items:
        if url in m.seen: continue
        try: d = http(url)
        except Exception as e: print("fail", url, e, flush=True); continue
        m.add(name, d, source, origin, lic, url); time.sleep(0.3)
    print(group, len(m.seen), flush=True)

def maven():
    base = "https://repo1.maven.org/maven2/"
    coords = """org/apache/commons/commons-lang3 commons-io/commons-io org/apache/commons/commons-collections4 com/google/guava/guava
com/google/code/gson/gson org/slf4j/slf4j-api ch/qos/logback/logback-classic junit/junit org/yaml/snakeyaml
com/fasterxml/jackson/core/jackson-core com/fasterxml/jackson/core/jackson-databind org/apache/httpcomponents/httpclient
org/apache/commons/commons-compress org/apache/commons/commons-text commons-codec/commons-codec org/jsoup/jsoup
joda-time/joda-time org/apache/logging/log4j/log4j-api org/apache/logging/log4j/log4j-core org/hamcrest/hamcrest
org/mockito/mockito-core com/squareup/okhttp3/okhttp com/squareup/okio/okio-jvm org/jetbrains/kotlin/kotlin-stdlib
org/antlr/antlr4-runtime org/apache/commons/commons-math3 org/apache/commons/commons-csv commons-cli/commons-cli
org/apache/poi/poi org/apache/pdfbox/pdfbox org/bouncycastle/bcprov-jdk18on com/h2database/h2 org/xerial/sqlite-jdbc
org/apache/lucene/lucene-core io/netty/netty-buffer io/netty/netty-common org/eclipse/jetty/jetty-server
org/springframework/spring-core org/apache/maven/maven-core org/apache/tika/tika-core com/zaxxer/HikariCP
org/postgresql/postgresql org/apache/commons/commons-pool2 com/opencsv/opencsv org/ow2/asm/asm
net/bytebuddy/byte-buddy org/projectlombok/lombok org/reflections/reflections com/github/ben-manes/caffeine/caffeine
io/micrometer/micrometer-core org/apache/zookeeper/zookeeper org/apache/kafka/kafka-clients org/apache/avro/avro
org/scala-lang/scala-library org/apache/velocity/velocity-engine-core org/freemarker/freemarker commons-beanutils/commons-beanutils""".split()
    items = []
    for c in coords:
        art = c.rsplit("/", 1)[1]
        try:
            meta = http(base + c + "/maven-metadata.xml").decode()
            ver = re.search(r"<release>([^<]+)</release>", meta) or re.search(r"<latest>([^<]+)</latest>", meta)
            v = ver.group(1)
        except Exception as e:
            print("skip", c, e); continue
        items.append((f"{art}-{v}.jar", f"{base}{c}/{v}/{art}-{v}.jar", "p:" + c, "Apache-2.0/MIT/other OSS (Maven Central)"))
        items.append((f"{art}-{v}-sources.jar", f"{base}{c}/{v}/{art}-{v}-sources.jar", "p:" + c, "OSS (Maven Central)"))
    simple_list("jar", items)

def fdroid():
    idx = json.loads(http("https://f-droid.org/repo/index-v1.json", timeout=300))
    pk = {}
    for a in idx["packages"]:
        pk[a] = idx["packages"][a]
    meta = {a["packageName"]: a for a in idx["apps"]}
    cands = []
    for name, vs in pk.items():
        v = vs[0]
        if 20_000 < v.get("size", 0) < 4_000_000 and meta.get(name, {}).get("license") and \
           re.match(r"(GPL|Apache|MIT|BSD|LGPL|MPL|AGPL|CC0|Unlicense|ISC)", meta[name]["license"]):
            cands.append((name, v, meta[name]["license"]))
    rnd.shuffle(cands)
    items = [(v["apkName"], "https://f-droid.org/repo/" + v["apkName"], "p:" + n, lic) for n, v, lic in cands[:70]]
    simple_list("apk", items)

def gnu():
    pk = """coreutils grep sed gawk make bash diffutils findutils tar gzip m4 bison flex gettext libtool autoconf automake
readline ncurses wget nano less bc cpio patch which indent texinfo gperf units hello mtools sharutils gsl glpk gdbm libiconv
libunistring libidn2 libmicrohttpd lightning gmp mpfr mpc gnutls cflow ed ddrescue datamash dico recutils parted gsasl
mailutils gss rcs cim ccd2cue freeipmi gawk-doc gnugo gnuchess xorriso""".split()
    items = []
    for p in pk:
        try:
            html = http(f"https://ftp.gnu.org/gnu/{p}/").decode()
        except Exception: continue
        for ext in ("tar.gz", "tar.xz", "tar.bz2"):
            fs = sorted(set(re.findall(r'href="(%s-[0-9][0-9.]*\.%s)"' % (re.escape(p), re.escape(ext)), html)),
                        key=lambda s: [int(x) for x in re.findall(r"\d+", s.rsplit(".tar", 1)[0])])
            if fs: items.append((fs[-1], f"https://ftp.gnu.org/gnu/{p}/{fs[-1]}", "p:" + p, "GPL (GNU)"))
    # keep download total reasonable: HEAD sizes are skipped, rely on package selection
    simple_list("gnu_tar", items)

def gutenberg():
    ids = sorted(rnd.sample(range(100, 72000), 160))
    m = Manifest("epub")
    for i in ids:
        if len(m.seen) >= 90: break
        u = f"https://www.gutenberg.org/cache/epub/{i}/pg{i}.epub"
        try: d = http(u, tries=2)
        except Exception: continue
        if len(d) > 6_000_000: continue
        m.add(f"pg{i}.epub", d, "b:%d" % i, "real", "Public domain (Project Gutenberg)", u); time.sleep(1.0)
    print("epub", len(m.seen))

def poi():
    d = os.path.join(OUT, "_poi"); 
    if not os.path.isdir(d):
        subprocess.check_call(["git", "clone", "-q", "--depth", "1", "--filter=blob:none", "--sparse", "https://github.com/apache/poi", d])
        subprocess.check_call(["git", "-C", d, "sparse-checkout", "set", "test-data/document", "test-data/spreadsheet", "test-data/slideshow"])
    rev = subprocess.check_output(["git", "-C", d, "rev-parse", "HEAD"]).decode().strip()
    for grp, ext in (("docx", ".docx"), ("xlsx", ".xlsx"), ("pptx", ".pptx")):
        m = Manifest(grp)
        files = []
        for root, _, fs in os.walk(os.path.join(d, "test-data")):
            files += [os.path.join(root, f) for f in fs if f.endswith(ext)]
        files.sort(); rnd.shuffle(files)
        for f in files[:200]:
            data = open(f, "rb").read()
            if len(data) > 8_000_000: continue
            m.add(os.path.basename(f), data, "p:poi", "real", "Apache-2.0 (apache/poi test-data @" + rev[:10] + ")", "git://poi/" + os.path.relpath(f, d))
        print(grp, len(m.seen))

def blender():
    items = [("bbb_320x180.mp4", "https://download.blender.org/peach/bigbuckbunny_movies/BigBuckBunny_320x180.mp4", "m:bbb", "CC-BY 3.0 (Blender Foundation)"),
             ("sintel_trailer_480p.mp4", "https://download.blender.org/durian/trailer/sintel_trailer-480p.mp4", "m:sintel", "CC-BY 3.0 (Blender Foundation)"),
             ("elephants_dream_480.avi", "https://download.blender.org/ED/ED_1024.avi", "m:ed", "CC-BY 2.5 (Blender Foundation)"),
             ("tears_of_steel_720p.mov", "https://download.blender.org/demo/movies/ToS/tears_of_steel_720p.mov", "m:tos", "CC-BY 3.0 (Blender Foundation)")]
    simple_list("blender_src", items)


def sparse(name, repo, paths, tag=None):
    d = os.path.join(OUT, "_" + name)
    if not os.path.isdir(d):
        subprocess.check_call(["git", "clone", "-q", "--depth", "1"] + (["--filter=blob:none", "--sparse"] if paths else []) + (["--branch", tag] if tag else []) + [repo, d])
        if paths: subprocess.check_call(["git", "-C", d, "sparse-checkout", "set"] + paths)
    return d, subprocess.check_output(["git", "-C", d, "rev-parse", "HEAD"]).decode().strip()

def walk_files(d, exts, sub=""):
    out = []
    for r, _, fs in os.walk(os.path.join(d, sub)):
        if "/.git" in r: continue
        out += [os.path.join(r, f) for f in fs if f.lower().endswith(exts)]
    out.sort(); return out

def from_repo(group, d, rev, repo, files, lic, want, maxbytes=8_000_000, source_fn=None):
    m = Manifest(group); rnd2 = random.Random(7); files = list(files); rnd2.shuffle(files)
    for f in files:
        if len(m.seen) >= want: break
        data = open(f, "rb").read()
        if len(data) > maxbytes or len(data) == 0: continue
        rel = os.path.relpath(f, d)
        src = source_fn(rel) if source_fn else "p:" + repo
        m.add(rel.replace("/", "_"), data, src, "real", lic + " @" + rev[:10], "git://" + repo + "/" + rel)
    print(group, len(m.seen), flush=True)

def coco():
    z = os.path.join(OUT, "_coco_val2017.zip")
    if not os.path.exists(z):
        open(z, "wb").write(http("http://images.cocodataset.org/zips/val2017.zip", timeout=900))
    import zipfile
    zf = zipfile.ZipFile(z); names = sorted(n for n in zf.namelist() if n.endswith(".jpg"))
    rnd.shuffle(names)
    m = Manifest("jpeg_web")
    for n in names[:700]:
        # source = image id bucket; Flickr photographers unknown here -> cluster by id//50
        m.add(os.path.basename(n), zf.read(n), "c:%d" % (int(os.path.basename(n)[:-4]) // 5000), "real", "Flickr CC (COCO val2017; per-image CC licence)", "coco://val2017/" + n)
    print("jpeg_web", len(m.seen))

def gnome_docs():
    d, rev = sparse("gnomedocs", "https://gitlab.gnome.org/GNOME/gnome-user-docs", ["gnome-help"])
    fs = walk_files(d, (".png",)); fs2 = walk_files(d, (".jpg", ".jpeg"))
    from_repo("png_screenshot", d, rev, "gnome-user-docs", fs, "CC-BY-SA 4.0 (GNOME docs)", 200, source_fn=lambda r: "p:" + r.split("/")[1] if r.count("/") > 1 else "p:gnome")

def icons():
    d, rev = sparse("matic", "https://github.com/google/material-design-icons", ["png/action", "png/communication", "png/maps", "png/image", "png/social", "png/file"])
    fs = walk_files(d, (".png",))
    from_repo("png_icon", d, rev, "material-design-icons", fs, "Apache-2.0 (material-design-icons)", 600, source_fn=lambda r: "p:" + r.split("/")[1] + "/" + r.split("/")[2] if r.count("/") > 3 else "p:mdi", maxbytes=60_000)

def webp_test():
    d, rev = sparse("webptest", "https://chromium.googlesource.com/webm/libwebp-test-data", [])
    fs = walk_files(d, (".webp",))
    from_repo("webp", d, rev, "libwebp-test-data", fs, "BSD-3 (libwebp-test-data)", 200, source_fn=lambda r: "p:webptest/" + os.path.basename(r)[:3])

def libtiff():
    d = os.path.join(OUT, "_libtiffpics"); os.makedirs(d, exist_ok=True)
    tgz = os.path.join(d, "pics.tar.gz")
    if not os.path.exists(tgz):
        open(tgz, "wb").write(http("https://download.osgeo.org/libtiff/pics-3.8.0.tar.gz", timeout=300))
        subprocess.check_call(["tar", "xzf", tgz, "-C", d])
    from_repo("tiff", d, "pics-3.8.0", "libtiff", walk_files(d, (".tif", ".tiff")), "libtiff licence (test pictures)", 60)

def pdfjs():
    d, rev = sparse("pdfjs", "https://github.com/mozilla/pdf.js", ["test/pdfs"])
    from_repo("pdf", d, rev, "pdf.js", walk_files(d, (".pdf",)), "Apache-2.0 repo test PDFs (mixed origin, public test corpus)", 250, maxbytes=6_000_000, source_fn=lambda r: "p:pdfjs/" + os.path.basename(r)[:2])

def libreoffice():
    d, rev = sparse("lo", "https://github.com/LibreOffice/core", ["sw/qa/extras/odfimport/data", "sw/qa/extras/uiwriter/data", "sc/qa/unit/data/ods", "sd/qa/unit/data/odp", "sw/qa/extras/ooxmlexport/data", "sw/qa/extras/ooxmlimport/data"])
    for g, ext in (("odt", ".odt"), ("ods", ".ods"), ("odp", ".odp")):
        from_repo(g, d, rev, "libreoffice-core", walk_files(d, (ext,)), "MPL-2.0 (LibreOffice core test data)", 150, source_fn=lambda r: "p:lo/" + r.split("/")[0] + r.split("/")[1])
    from_repo("docx_lo", d, rev, "libreoffice-core", walk_files(d, (".docx",)), "MPL-2.0 (LibreOffice core test data)", 150, source_fn=lambda r: "p:lo/" + r.split("/")[0])

def archive_org():
    q = "https://archive.org/advancedsearch.php?q=collection%3Alibrivoxaudio+AND+mediatype%3Aaudio&fl%5B%5D=identifier&sort%5B%5D=downloads+desc&rows=60&page=1&output=json"
    ids = [d["identifier"] for d in json.loads(http(q))["response"]["docs"]]
    rnd.shuffle(ids)
    mm, mo = Manifest("audio_mp3"), Manifest("audio_vorbis")
    for i in ids[:40]:
        try: md = json.loads(http(f"https://archive.org/metadata/{i}"))
        except Exception: continue
        fl = md.get("files", [])
        mp3 = [f for f in fl if f["name"].endswith("_64kb.mp3") and 100_000 < int(f.get("size", 0)) < 12_000_000]
        ogg = [f for f in fl if f["format"] == "Ogg Vorbis" and 100_000 < int(f.get("size", 0)) < 12_000_000]
        for tgt, lst in ((mm, mp3[:2]), (mo, ogg[:1])):
            for f in lst:
                u = f"https://archive.org/download/{i}/" + urllib.parse.quote(f["name"])
                try: tgt.add(f["name"], http(u, timeout=300), "a:" + i, "real", "Public domain (LibriVox via Internet Archive)", u)
                except Exception as e: print("fail", u, e)
        time.sleep(1)
    print("audio", len(mm.seen), len(mo.seen))

def kodak_derived():
    """JPEG/WebP/GIF/PNG-recompressed variants from Kodak: encoder diversity (derived)."""
    tmp = os.path.join(OUT, "_tmp"); os.makedirs(tmp, exist_ok=True)
    kd = os.path.join(OUT, "kodak"); fs = sorted(os.listdir(kd))
    mj = {k: Manifest("jpeg_" + k) for k in ("q75base", "q90opt", "q85prog")}
    mw = {k: Manifest("webp_" + k) for k in ("lossy", "lossless")}
    mg = Manifest("gif_derived")
    for f in fs:
        p = os.path.join(kd, f); sid = "k:" + f
        base = os.path.splitext(f)[0]
        for k, args in (("q75base", ["-quality", "75"]), ("q90opt", ["-quality", "90", "-define", "jpeg:optimize-coding=true"]), ("q85prog", ["-quality", "85", "-interlace", "Plane"])):
            t = os.path.join(tmp, "o.jpg"); subprocess.check_call(["convert", p] + args + [t])
            mj[k].add(base + ".jpg", open(t, "rb").read(), sid, "derived", "Kodak; ImageMagick/libjpeg " + k, "derived://jpeg" + k + "/" + f)
        t = os.path.join(tmp, "o.webp"); subprocess.check_call(["cwebp", "-quiet", "-q", "80", p, "-o", t]); mw["lossy"].add(base + ".webp", open(t, "rb").read(), sid, "derived", "Kodak; cwebp", "derived://webplossy/" + f)
        subprocess.check_call(["cwebp", "-quiet", "-lossless", p, "-o", t]); mw["lossless"].add(base + ".webp", open(t, "rb").read(), sid, "derived", "Kodak; cwebp", "derived://webpll/" + f)
        t = os.path.join(tmp, "o.gif"); subprocess.check_call(["convert", p, "-resize", "512x512>", t]); mg.add(base + ".gif", open(t, "rb").read(), sid, "derived", "Kodak; ImageMagick", "derived://gif/" + f)
    print("kodak_derived done")

def thumbs_derived():
    """Small web-thumbnail style JPEG/PNG/WebP/ICO derived from COCO JPEGs and Material icons (ImageMagick defaults)."""
    tmp = os.path.join(OUT, "_tmp"); os.makedirs(tmp, exist_ok=True)
    jd = os.path.join(OUT, "jpeg_web"); fs = sorted(os.listdir(jd)); random.Random(3).shuffle(fs)
    mj, mp, mw = Manifest("jpeg_thumbd"), Manifest("png_thumbd"), Manifest("webp_thumbd")
    for f in fs[:150]:
        p = os.path.join(jd, f); sid = "c:" + f[:7]
        for w, q in ((96, 80), (200, 75)):
            t = os.path.join(tmp, "t.jpg"); subprocess.check_call(["convert", p, "-thumbnail", f"{w}x{w}", "-quality", str(q), "-strip", t])
            mj.add(f"{w}_{f}", open(t, "rb").read(), sid, "derived", "COCO CC; thumbnail", f"derived://thumbj{w}/{f}")
        t = os.path.join(tmp, "t.png"); subprocess.check_call(["convert", p, "-thumbnail", "128x128", t]); mp.add(f"128_{f}.png", open(t, "rb").read(), sid, "derived", "COCO CC; thumbnail", "derived://thumbp/" + f)
        t = os.path.join(tmp, "t.webp"); subprocess.check_call(["cwebp", "-quiet", "-q", "75", "-resize", "160", "0", p, "-o", t]); mw.add(f"160_{f}.webp", open(t, "rb").read(), sid, "derived", "COCO CC; thumbnail", "derived://thumbw/" + f)
    mi = Manifest("ico")
    idir = os.path.join(OUT, "png_icon"); fs = sorted(os.listdir(idir)); random.Random(4).shuffle(fs)
    for f in fs[:200]:
        t = os.path.join(tmp, "t.ico"); 
        try: subprocess.check_call(["convert", os.path.join(idir, f), "-resize", "32x32", t])
        except Exception: continue
        mi.add(f + ".ico", open(t, "rb").read(), "i:" + f[:12], "derived", "Material icons Apache-2.0; ico", "derived://ico/" + f)
    print("thumbs_derived")

def kodak():
    items = [(f"kodim{i:02d}.png", f"https://r0k.us/graphics/kodak/kodak/kodim{i:02d}.png", "k:kodak", "Kodak, released for unrestricted research use") for i in range(1, 25)]
    simple_list("kodak", items)

def local_gz():
    """Ubuntu packages already on the host: man pages and changelogs (.gz), deb data members."""
    m = Manifest("gz_small")
    cands = []
    for root in ("/usr/share/man", "/usr/share/doc", "/usr/share/info"):
        for r, _, fs in os.walk(root):
            cands += [os.path.join(r, f) for f in fs if f.endswith(".gz") and os.path.isfile(os.path.join(r, f)) and not os.path.islink(os.path.join(r, f))]
    cands.sort(); rnd.shuffle(cands)
    n = 0
    for f in cands:
        if n >= 1600: break
        d = open(f, "rb").read()
        pkg = f.split("/")[4] if f.startswith("/usr/share/doc") else f.split("/")[-1].split(".")[0]
        m.add(f.replace("/", "_"), d, "p:" + pkg, "real", "Ubuntu package docs (various OSS licences)", "file://" + f); n += 1
    print("gz_small", n)

def debs():
    d = os.path.join(OUT, "_debs"); os.makedirs(d, exist_ok=True)
    pk = "zlib1g bash-completion nano less tree jq vim-tiny htop lsof rsync curl git-man python3-minimal perl-base fonts-dejavu-core tzdata ca-certificates libjpeg-turbo8 libpng16-16t64 liblzma5 man-db groff-base info texinfo gawk sed".split()
    subprocess.call(["apt-get", "download"] + pk, cwd=d, stderr=subprocess.DEVNULL)
    mz, mx, mg = Manifest("zst"), Manifest("xz_pkg"), Manifest("gz_pkg")
    for f in sorted(os.listdir(d)):
        if not f.endswith(".deb"): continue
        w = os.path.join(d, "x_" + f); os.makedirs(w, exist_ok=True)
        subprocess.call(["ar", "x", os.path.join(d, f)], cwd=w)
        for g in os.listdir(w):
            data = open(os.path.join(w, g), "rb").read()
            tgt = {"data.tar.zst": mz, "data.tar.xz": mx, "control.tar.zst": mz, "control.tar.xz": mx, "data.tar.gz": mg}.get(g)
            if tgt: tgt.add(f"{f}.{g}", data, "p:" + f.split("_")[0], "real", "Ubuntu package (various OSS)", "deb://" + f + "/" + g)
        subprocess.call(["rm", "-rf", w])

def blender_clips():
    """Derived clips from Blender open movies (CC-BY): copy-cut H.264/MP4, TS segments, re-encodes."""
    m = Manifest("video_derived")
    src = {n: os.path.join(OUT, "blender_src", n) for n in os.listdir(os.path.join(OUT, "blender_src"))}
    tmp = os.path.join(OUT, "_tmp"); os.makedirs(tmp, exist_ok=True)
    lic = "CC-BY (Blender Foundation), derived by ffmpeg"
    def run(args): subprocess.check_call(["ffmpeg", "-v", "error", "-y", "-threads", "2"] + args)
    def put(name, tmpf, srcid, tag):
        m.add(name, open(tmpf, "rb").read(), srcid, "derived", lic, "derived://" + tag + "/" + name); os.remove(tmpf)
    for n, p in src.items():
        sid = "m:" + n.split("_")[0]
        dur = float(subprocess.check_output(["ffprobe", "-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0", p]))
        starts = [dur * f for f in (0.1, 0.35, 0.6, 0.8)]
        for i, s in enumerate(starts):
            for tag, args, ext in (
                ("h264-copy", ["-c", "copy", "-an"], "mp4"),
                ("h264-copy-ts", ["-c:v", "copy", "-an", "-bsf:v", "h264_mp4toannexb"], "ts"),
                ("h265", ["-c:v", "libx265", "-crf", "28", "-preset", "fast", "-an", "-x265-params", "log-level=error"], "mp4"),
                ("vp9", ["-c:v", "libvpx-vp9", "-crf", "34", "-b:v", "0", "-deadline", "good", "-cpu-used", "4", "-an"], "webm"),
                ("av1", ["-c:v", "libsvtav1", "-crf", "36", "-preset", "9", "-an"], "mp4")):
                if tag.startswith("h264-copy") and not (subprocess.check_output(["ffprobe","-v","error","-select_streams","v:0","-show_entries","stream=codec_name","-of","csv=p=0",p]).decode().strip()=="h264"): continue
                t = os.path.join(tmp, f"c.{ext}")
                try: run(["-ss", f"{s:.2f}", "-t", "8", "-i", p] + args + [t])
                except Exception as e: print("fail", n, tag, e); continue
                put(f"{n.split('.')[0]}_{i}_{tag}.{ext}", t, sid, tag)
        # 1-second MPEG-TS segments in native form
    print("video_derived", len(m.seen))

def audio_derived():
    """Derived lossy audio (AAC/M4A, Opus, MP3, Vorbis) and lossless WAV/FLAC from the Commons FLAC/WAV/MP3 sources."""
    m = Manifest("audio_derived")
    tmp = os.path.join(OUT, "_tmp"); os.makedirs(tmp, exist_ok=True)
    srcs = []
    for g in ("audio_flac", "audio_wav", "audio_mp3"):
        d = os.path.join(OUT, g)
        if os.path.isdir(d):
            for f in sorted(os.listdir(d)):
                srcs.append((os.path.join(d, f), "g:" + g + "/" + f))
    for p, sid in srcs[:45]:
        for tag, args, ext in (("aac", ["-c:a", "aac", "-b:a", "96k", "-t", "40"], "m4a"), ("opus", ["-c:a", "libopus", "-b:a", "64k", "-t", "40"], "opus"),
                               ("mp3", ["-c:a", "libmp3lame", "-b:a", "128k", "-t", "40"], "mp3"), ("flac", ["-c:a", "flac", "-t", "40"], "flac"), ("wav", ["-c:a", "pcm_s16le", "-ac", "1", "-ar", "22050", "-t", "20"], "wav"), ("aac-hq", ["-c:a", "aac", "-b:a", "192k", "-t", "40"], "m4a")):
            t = os.path.join(tmp, "a." + ext)
            try: subprocess.check_call(["ffmpeg", "-v", "error", "-y", "-i", p] + args + [t])
            except Exception: continue
            m.add(os.path.basename(p) + "." + tag + "." + ext, open(t, "rb").read(), "u:" + sid, "derived", "source licence, see Commons file; transcoded", "derived://" + tag + "/" + sid); os.remove(t)
    print("audio_derived", len(m.seen))

def image_derived():
    """HEIC/AVIF/BMP/TIFF(uncompressed) and extra WebP made from Kodak + Commons JPEGs (encoder defaults)."""
    tmp = os.path.join(OUT, "_tmp"); os.makedirs(tmp, exist_ok=True)
    srcs = []
    kd = os.path.join(OUT, "kodak")
    if os.path.isdir(kd): srcs += [(os.path.join(kd, f), "k:" + f) for f in sorted(os.listdir(kd))]
    jd = os.path.join(OUT, "jpeg_photo")
    if os.path.isdir(jd):
        fs = sorted(os.listdir(jd)); rnd.shuffle(fs)
        srcs += [(os.path.join(jd, f), "j:" + f) for f in fs[:60]]
    for tag, ext in (("heic", "heic"), ("avif", "avif"), ("bmp", "bmp"), ("tiff", "tif")):
        m = Manifest("img_" + tag)
        for p, sid in srcs:
            t = os.path.join(tmp, "i." + ext)
            try:
                if tag == "heic": subprocess.check_call(["convert", p + "[0]", "-resize", "1280x1280>", os.path.join(tmp, "i.png")]); subprocess.check_call(["heif-enc", "-q", "50", os.path.join(tmp, "i.png"), "-o", t], stderr=subprocess.DEVNULL)
                elif tag == "avif": subprocess.check_call(["convert", p + "[0]", "-resize", "1280x1280>", os.path.join(tmp, "i.png")]); subprocess.check_call(["avifenc", "-s", "8", "-q", "50", os.path.join(tmp, "i.png"), t], stderr=subprocess.DEVNULL, stdout=subprocess.DEVNULL)
                else: subprocess.check_call(["convert", p + "[0]", "-resize", "1024x1024>", t])
            except Exception as e: continue
            m.add(os.path.basename(p) + "." + ext, open(t, "rb").read(), "u:" + sid, "derived", "source licence; encoded by " + tag, "derived://" + tag + "/" + sid); os.remove(t)
        print(tag, len(m.seen))

if __name__ == "__main__":
    g = sys.argv[1]
    os.makedirs(OUT, exist_ok=True)
    fn = GROUPS.get(g) or globals().get(g)
    fn()
