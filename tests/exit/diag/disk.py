import sys
rows=[];cur=None
for l in open(sys.argv[1]+"/samples.txt",errors="replace"):
    if l.startswith("T "):
        try: cur=float(l[2:])
        except: cur=None
    elif l.startswith("D ") and cur:
        p=l.split()
        if len(p)>=14 and p[3]=="nvme0n1":
            rows.append((cur,[int(x) for x in p[4:15]]))
def win(a,b,label):
    w=[(t,v) for t,v in rows if a<=t<=b]
    (t0,v0),(t1,v1)=w[0],w[-1]; dt=t1-t0
    rd=(v1[2]-v0[2])*512/dt/1e6; wr=(v1[6]-v0[6])*512/dt/1e6
    wio=(v1[4]-v0[4])/dt; await_w=(v1[7]-v0[7])/max(1,(v1[4]-v0[4]))
    util=(v1[9]-v0[9])/dt/10; q=(v1[10]-v0[10])/dt/1000
    print("%s: root disk read %.0f MB/s write %.0f MB/s, %.0f write IOs/s, write await %.1f ms, util %.0f %%, aqu-sz %.1f"%(label,rd,wr,wio,await_w,util,q))
for a in sys.argv[2:]:
    lo,hi,lab=a.split(",")
    win(float(lo),float(hi),lab)
