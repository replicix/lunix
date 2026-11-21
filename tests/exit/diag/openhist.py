import json,sys
rows=[];cur=None
for l in open(sys.argv[1]+"/samples.txt",errors="replace"):
    if l.startswith("T "):
        try: cur=float(l[2:])
        except: cur=None
    elif l.startswith("S ") and cur:
        try: s=json.loads(l[2:])
        except: continue
        if s.get("lat"): rows.append((cur,s))
t0=rows[0][0]; a,b=float(sys.argv[2]),float(sys.argv[3])
w=[(t,s) for t,s in rows if a<=t-t0<=b]
states=set(s["state"] for t,s in w)
h=[x-y for x,y in zip(w[-1][1]["lat"],w[0][1]["lat"])]
tot=sum(h); acc=0; out={}
for i,v in enumerate(h):
    acc+=v
    for q in (0.5,0.99,0.999,1.0):
        if q not in out and tot and acc>=q*tot-1e-9: out[q]=i
print("window %d-%d s states %s: %d writes; bucket (2^b us) at p50 %s p99 %s p99.9 %s max %s ; max = %.2f s"%(a,b,states,tot,out.get(.5),out.get(.99),out.get(.999),out.get(1.0),2**out.get(1.0,0)/1e6))
