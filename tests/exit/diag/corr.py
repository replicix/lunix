import json,sys,os,re,collections
d=sys.argv[1]; lo,hi=float(sys.argv[2]),float(sys.argv[3])
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
bpf0=float(os.popen("stat -c %W "+d+"/bpf.txt").read())
longs=collections.Counter(); mx={}
for l in open(d+"/bpf.txt",errors="replace"):
    m=re.match(r"(\d+) LONG dispatch_to_complete (\d+) ms",l)
    if m:
        t=int(bpf0+int(m.group(1))/1000); ms=int(m.group(2))
        if ms>=4000: longs[t]+=1; mx[t]=max(mx.get(t,0),ms)
w=[(t,s) for t,s in rows if lo<=t<=hi]
secs=len(w); sl=sum(1 for t,s in w if s["state"]!="open"); cutmax=max(s["cut_ms"] or 0 for t,s in w)
cuts_long=sum(1 for t,s in w if (s["cut_ms"] or 0)>1000)
ls=[t for t,s in w if longs.get(int(t),0)>0]
print("window %d s: %d s not open (%d %%), last_cut_ms max %d, seconds with last_cut_ms>1000: %d"%(secs,sl,100*sl//max(1,secs),cutmax,cuts_long))
print("seconds with a kernel completion >=4 s: %d; of those, gate state slowdown/stop: %d; with last_cut_ms>1000: %d; max completion %d ms"%(len(ls),sum(1 for t,s in w if longs.get(int(t),0)>0 and s["state"]!="open"),sum(1 for t,s in w if longs.get(int(t),0)>0 and (s["cut_ms"] or 0)>1000),max(mx.values() or [0])))
cm=[s["commits"] for t,s in w]; print("commits %d..%d (%.1f s per commit)"%(cm[0],cm[-1],secs/max(1,cm[-1]-cm[0])))
