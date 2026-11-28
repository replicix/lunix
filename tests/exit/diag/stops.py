import json,sys,os,re,collections
d=sys.argv[1]
rows=[];cur=None
for l in open(d+"/samples.txt",errors="replace"):
    if l.startswith("T "):
        try: cur=float(l[2:])
        except: cur=None
    elif l.startswith("S ") and cur:
        try: s=json.loads(l[2:])
        except: continue
        if s.get("bw") is None: continue
        rows.append((cur,s))
t0=rows[0][0]
bpf0=float(os.popen("stat -c %W "+d+"/bpf.txt").read())
longs=[]
for l in open(d+"/bpf.txt",errors="replace"):
    m=re.match(r"(\d+) LONG dispatch_to_complete (\d+) ms",l)
    if m: longs.append((bpf0+int(m.group(1))/1000,int(m.group(2))))
eps=[];st=None
for t,s in rows:
    if s["state"]=="stop":
        if st is None: st=[t,t,s["sw"],s["used"]]
        st[1]=t; st[2]=max(st[2],s["sw"])
    elif st: eps.append(st); st=None
if st: eps.append(st)
for a,b,sw,used in eps:
    ln=[m for t,m in longs if a-1<=t<=b+8]
    print("stop episode t=%d..%d (%d s) wall %s max space_waiters %d used %d MiB; kernel long completions in it: %d max %d ms"%(a-t0,b-t0,b-a+1,os.popen("date -u -d @%d +%%T"%a).read().strip(),sw,used>>20,len(ln),max(ln) if ln else 0))
