"""Synthetic log generators (3 families x 5 profiles). Seeded; the profile is the 'project' for train/test splits.
Synthetic: structure is mine, so gains on these are an upper-ish bound; independent real logs come from loghub (extras.py)."""
import random, math, datetime, json, uuid

def ip(r): return f'{r.choice([10,172,192,203,198,45,91,185])}.{r.randrange(256)}.{r.randrange(256)}.{r.randrange(1,255)}'
WORDS = 'order user cart item search product account invoice report session token image css js api v1 v2 static media admin login logout checkout profile settings feed list view edit delete upload download health metrics'.split()
UAS = ['Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/%d.0.%d.%d Safari/537.36',
 'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/%d.1 Safari/605.1.15',
 'Mozilla/5.0 (X11; Linux x86_64; rv:%d.0) Gecko/20100101 Firefox/%d.0', 'curl/8.%d.1', 'python-requests/2.%d.0', 'Go-http-client/2.0',
 'Mozilla/5.0 (iPhone; CPU iPhone OS 17_%d like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Mobile/15E148', 'Googlebot/2.1 (+http://www.google.com/bot.html)']

def syslog(r, prof, nbytes):
    host = f'{r.choice(["web","db","app","edge","cache"])}{prof}-{r.randrange(1,40):02d}'
    progs = r.sample(['sshd','cron','systemd','kernel','sudo','NetworkManager','dhclient','postfix/smtpd','rsyslogd','dockerd','CRON','systemd-logind','named','nginx'], 7)
    t = datetime.datetime(2026, r.randrange(1,12), r.randrange(1,28), r.randrange(24), r.randrange(60)); out = []; n = 0
    iso = prof == 4
    while n < nbytes:
        t += datetime.timedelta(seconds=r.expovariate(0.5)); p = r.choice(progs); pid = r.randrange(300, 40000)
        m = r.choice([f'Accepted publickey for {r.choice(["deploy","admin","devops","bob"])} from {ip(r)} port {r.randrange(1024,65535)} ssh2: RSA SHA256:{uuid.UUID(int=r.getrandbits(128)).hex[:43]}',
          f'pam_unix(sshd:session): session opened for user {r.choice(["deploy","root","admin"])}(uid={r.choice([0,1000,1001])}) by (uid=0)',
          f'Failed password for invalid user {r.choice(["test","oracle","git","pi","admin"])} from {ip(r)} port {r.randrange(1024,65535)} ssh2',
          f'(root) CMD (/usr/local/bin/{r.choice(["backup","rotate","sync","cleanup"])}.sh >/dev/null 2>&1)',
          f'Started {r.choice(["Session","Daily apt download activities","Journal Service","Network Time Synchronization"])} {r.randrange(1,9999)}.',
          f'eth0: link becomes ready', f'IPv4: martian source {ip(r)} from {ip(r)}, on dev eth0',
          f'{r.choice(["connect","disconnect","lost connection after AUTH"])} from unknown[{ip(r)}]',
          f'[UFW BLOCK] IN=eth0 OUT= MAC={uuid.UUID(int=r.getrandbits(128)).hex} SRC={ip(r)} DST={ip(r)} LEN={r.randrange(40,1500)} TTL={r.randrange(30,255)} PROTO=TCP SPT={r.randrange(1024,65535)} DPT={r.choice([22,80,443,3306,8080])}',
          f'Out of memory: Killed process {r.randrange(300,40000)} ({r.choice(["java","python3","node","mysqld"])}) total-vm:{r.randrange(10**5,10**7)}kB'])
        ts = t.strftime('%Y-%m-%dT%H:%M:%S.%f+00:00') if iso else t.strftime('%b %e %H:%M:%S')
        line = f'{ts} {host} {p}[{pid}]: {m}\n'; out.append(line); n += len(line)
    return ''.join(out).encode()

def access(r, prof, nbytes):
    tree = [f'/{"/".join(r.sample(WORDS, r.randrange(1,4)))}' for _ in range(r.randrange(30,200))]
    ref = [f'https://{r.choice(["www","shop","blog"])}.{r.choice(["example","acme","foo","bar"])}.{r.choice(["com","org","net"])}/{r.choice(WORDS)}' for _ in range(10)]
    uas = [u % tuple(r.randrange(1,130) for _ in range(u.count('%d'))) for u in r.sample(UAS, 5) for _ in (0,)]
    t = datetime.datetime(2026, r.randrange(1,12), r.randrange(1,28), 0, 0, 0); out = []; n = 0
    ips = [ip(r) for _ in range(r.randrange(20,400))]
    while n < nbytes:
        t += datetime.timedelta(seconds=r.expovariate(2.0)); u = r.choice(tree)
        if r.random() < 0.3: u += f'?id={r.randrange(10**r.randrange(1,7))}&{r.choice(WORDS)}={r.choice(WORDS)}'
        st = r.choices([200,304,301,404,500,403,206],[70,12,4,8,2,2,2])[0]
        extra = f' {r.random()*2:.3f}' if prof == 4 else ''
        line = f'{r.choice(ips)} - {"-" if r.random()<.9 else r.choice(WORDS)} [{t.strftime("%d/%b/%Y:%H:%M:%S +0000")}] "{r.choice(["GET","GET","GET","POST","HEAD"])} {u} HTTP/{r.choice(["1.1","2.0"])}" {st} {r.randrange(100, 10**r.randrange(3,7))} "{r.choice(ref) if r.random()<.5 else "-"}" "{r.choice(uas)}"{extra}\n'
        out.append(line); n += len(line)
    return ''.join(out).encode()

def applog(r, prof, nbytes):
    pk = r.choice(['com.acme.order','org.example.billing','io.corp.auth','net.shop.catalog']); svc = r.sample(['OrderService','PaymentGateway','InventoryClient','UserRepository','CacheManager','Scheduler','KafkaConsumer'], 4)
    t = datetime.datetime(2026, r.randrange(1,12), r.randrange(1,28), r.randrange(24)); out = []; n = 0
    while n < nbytes:
        t += datetime.timedelta(milliseconds=r.expovariate(0.01)); lv = r.choices(['INFO','DEBUG','WARN','ERROR'],[60,25,10,5])[0]; s = r.choice(svc)
        msg = r.choice([f'Processing order {r.randrange(10**6)} for customer {r.randrange(10**4)}', f'Cache {r.choice(["hit","miss"])} for key user:{r.randrange(10**5)}',
           f'Request completed in {r.randrange(1,2000)} ms status={r.choice([200,200,201,400,500])}', f'Retrying connection to {ip(r)}:{r.choice([5432,6379,9092])} attempt {r.randrange(1,6)}',
           f'Consumed {r.randrange(1,500)} records from topic {r.choice(["orders","events","audit"])} partition {r.randrange(12)} offset {r.randrange(10**7)}'])
        if prof % 2 == 1 or prof == 4:
            line = json.dumps({'ts': t.isoformat()+'Z', 'level': lv, 'logger': f'{pk}.{s}', 'thread': f'worker-{r.randrange(16)}', 'msg': msg, 'trace_id': uuid.UUID(int=r.getrandbits(128)).hex}, separators=(',',':'))+'\n'
        else:
            line = f'{t.strftime("%Y-%m-%d %H:%M:%S,")}{t.microsecond//1000:03d} {lv:5s} [http-nio-8080-exec-{r.randrange(1,20)}] {pk}.{s} - {msg}\n'
            if lv == 'ERROR' and r.random() < .7:
                line += f'java.lang.{r.choice(["NullPointerException","IllegalStateException","IOException"])}: {msg[:40]}\n' + ''.join(f'\tat {pk}.{r.choice(svc)}.{r.choice(WORDS)}({r.choice(svc)}.java:{r.randrange(10,900)})\n' for _ in range(r.randrange(3,14)))
        out.append(line); n += len(line)
    return ''.join(out).encode()

GEN = {'syslog': syslog, 'access': access, 'applog': applog}
def sizes(r, n):  # log-uniform 2 KiB .. 8 MiB
    return [int(2**r.uniform(11, 23)) for _ in range(n)]
