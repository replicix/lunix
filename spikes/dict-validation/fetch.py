#!/usr/bin/env python3
"""Phase 1: download corpora from official sources, verify against the publisher's checksum file, write sources.lock.tsv.
Usage: fetch.py <dl-dir> [name ...]   (all when no names)."""
import sys, os, re, hashlib, subprocess, urllib.request, json
UB = 'https://cloud-images.ubuntu.com/releases/26.04/release-20261016/'
DEB = 'https://cloud.debian.org/images/cloud/trixie/latest/'
FED = 'https://dl.fedoraproject.org/pub/fedora/linux/releases/45/Cloud/x86_64/images/'
ALP = 'https://dl-cdn.alpinelinux.org/alpine/latest-stable/releases/cloud/'
ROC = 'https://dl.rockylinux.org/pub/rocky/10/images/x86_64/'
ARC = 'https://geo.mirror.pkgbuild.com/images/latest/'
SUS = 'https://download.opensuse.org/distribution/openSUSE-current/appliances/'
SE = 'https://archive.org/download/stackexchange/'
# name, url, checksum kind, checksum source, licence/note
ITEMS = [
 ('vm-ubuntu', UB+'ubuntu-26.04-server-cloudimg-amd64.img', 'sha256-list', UB+'SHA256SUMS', 'distribution licences (mixed, GPL/MIT/etc.)'),
 ('vm-debian', DEB+'debian-13-genericcloud-amd64.qcow2', 'sha512-list', DEB+'SHA512SUMS', 'distribution licences'),
 ('vm-fedora', FED+'Fedora-Cloud-Base-Generic-45-1.4.x86_64.qcow2', 'sha256-bsd', FED+'Fedora-Cloud-45-1.4-x86_64-CHECKSUM', 'distribution licences'),
 ('pool-alpine', ALP+'alpine-3.24.4-x86_64-cloudinit-r0.qcow2', 'sha512-file', ALP+'alpine-3.24.4-x86_64-cloudinit-r0.qcow2.sha512', 'distribution licences'),
 ('pool-rocky', ROC+'Rocky-10-GenericCloud-Base-10.2-20260525.0.x86_64.qcow2', 'sha256-bsd', ROC+'CHECKSUM', 'distribution licences'),
 ('pool-suse', SUS+'Leap-16.1-Minimal-VM.x86_64-Cloud.qcow2', 'sha256-file', SUS+'Leap-16.1-Minimal-VM.x86_64-Cloud.qcow2.sha256', 'distribution licences'),
 ('pool-arch', ARC+'Arch-Linux-x86_64-cloudimg-20261101.620452.qcow2', 'sha256-file', ARC+'Arch-Linux-x86_64-cloudimg-20261101.620452.qcow2.SHA256', 'distribution licences'),
 ('se-askubuntu', SE+'askubuntu.com.7z', 'ia-sha1', 'askubuntu.com.7z', 'CC BY-SA 4.0'),
 ('se-superuser', SE+'superuser.com.7z', 'ia-sha1', 'superuser.com.7z', 'CC BY-SA 4.0'),
 ('se-serverfault', SE+'serverfault.com.7z', 'ia-sha1', 'serverfault.com.7z', 'CC BY-SA 4.0'),
 ('geonames', 'https://download.geonames.org/export/dump/allCountries.zip', 'none', '', 'CC BY 4.0'),
 ('tlc-2024-01', 'https://d37ci6vzurychx.cloudfront.net/trip-data/yellow_tripdata_2024-01.parquet', 'none', '', 'NYC TLC terms; licence UNVERIFIED, used locally only'),
 ('tlc-2024-02', 'https://d37ci6vzurychx.cloudfront.net/trip-data/yellow_tripdata_2024-02.parquet', 'none', '', 'NYC TLC terms; licence UNVERIFIED, used locally only'),
 ('tlc-2024-03', 'https://d37ci6vzurychx.cloudfront.net/trip-data/yellow_tripdata_2024-03.parquet', 'none', '', 'NYC TLC terms; licence UNVERIFIED, used locally only'),
]
def get(url): return urllib.request.urlopen(url, timeout=60).read().decode('utf-8', 'replace')
def hexof(path, algo):
    h = hashlib.new(algo)
    with open(path, 'rb') as f:
        for b in iter(lambda: f.read(1 << 22), b''): h.update(b)
    return h.hexdigest()
def expected(kind, src, fname):
    if kind == 'none': return None, None
    if kind == 'ia-sha1':
        d = json.loads(get('https://archive.org/metadata/stackexchange'))
        for f in d['files']:
            if f['name'] == src: return 'sha1', f['sha1']
    t = get(src)
    algo = 'sha256' if 'sha256' in kind else 'sha512'
    if kind.endswith('-list'):
        for l in t.splitlines():
            p = l.split()
            if len(p) == 2 and p[1].lstrip('*') == fname: return algo, p[0]
    elif kind.endswith('-bsd'):
        m = re.search(r'%s \(%s\) = ([0-9a-f]+)' % (algo.upper(), re.escape(fname)), t)
        if m: return algo, m.group(1)
    else:
        m = re.search(r'\b([0-9a-f]{%d})\b' % (64 if algo == 'sha256' else 128), t)
        if m: return algo, m.group(1)
    raise SystemExit('no checksum for ' + fname)
def main():
    dl = sys.argv[1]; only = set(sys.argv[2:])
    lock = os.path.join(dl, 'sources.lock.tsv')
    done = {}
    if os.path.exists(lock):
        for l in open(lock): done[l.split('\t')[0]] = l
    for name, url, kind, src, lic in ITEMS:
        if only and name not in only: continue
        if name in done: continue
        fname = url.rsplit('/', 1)[1]; path = os.path.join(dl, fname)
        st = os.statvfs(dl)
        if st.f_bavail * st.f_frsize < 60 << 30: raise SystemExit('free-space guard')
        subprocess.run(['curl', '-sSL', '--fail', '-C', '-', '--retry', '5', '-o', path, url], check=True)
        algo, exp = expected(kind, src, fname)
        got = hexof(path, algo) if algo else None
        status = 'verified' if algo and got == exp else ('UNVERIFIED-no-publisher-checksum' if not algo else 'MISMATCH')
        if status == 'MISMATCH': raise SystemExit('checksum mismatch ' + fname)
        row = '\t'.join([name, url, fname, str(os.path.getsize(path)), hexof(path, 'sha256'), (algo or '-') + ':' + status, lic])
        open(lock, 'a').write(row + '\n'); print(row, flush=True)
main()
