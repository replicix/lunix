import json,sys,collections
rows=[];cur=None
for l in open(sys.argv[1]+"/samples.txt",errors="replace"):
    if l.startswith("T "):
        try: cur=float(l[2:])
        except: cur=None
    elif l.startswith("S ") and cur:
        try: s=json.loads(l[2:])
        except: continue
        if s.get("bw") is None: continue
        rows.append((cur,s))
agg=collections.defaultdict(lambda:[0.0,0,0.0,0])
for (t0,a),(t1,b) in zip(rows,rows[1:]):
    dt=t1-t0
    if dt<=0 or dt>3: continue
    g=agg[b["state"]]; g[0]+=dt; g[1]+=b["bw"]-a["bw"]; g[2]+=(b["rate"] or 0)*dt; g[3]+=b["wr"]-a["wr"]
for st,g in agg.items():
    print(st,"secs",round(g[0]),"written MB/s",round(g[1]/g[0]/1e6,1),"gate rate MB/s",round(g[2]/g[0]/1e6,1),"writes/s",round(g[3]/g[0]),"avg KB",round(g[1]/max(1,g[3])/1024))
