//! `s3probe mpu`: multipart upload (parts in parallel) versus one PUT, for 16/32 MiB objects.
//!
//! `s3probe mpu --out DIR --providers AWS [--phases lat,gp,check,opt64] [--verify|--sweep]`
//! All requests go through the hand-signed client in `raw.rs` (one reqwest pool, bodies sent as
//! `UNSIGNED-PAYLOAD`, like object_store does over https), so single PUT and multipart use the
//! same stack. Outputs `mpu-<provider>-{samples,goodput}.csv` and `mpu-<provider>-meta.txt`.
//! Env (besides the usual `LUNIX_PROBE_<NAME>_*`): `LUNIX_PROBE_MPU_N` (latency samples per cell,
//! 10), `_GP16`/`_GP32` (objects per goodput cell, 24/12), `_OPT_N` (64 MiB samples, 5),
//! `_CAP_REQS` (5500), `_CAP_BYTES` (3.95e9). Everything lives under `lunix-p0-probe/mpu-<id>/`.

use super::*;
use crate::raw::Raw;
use futures::future::join_all;

const MIB: usize = 1 << 20;
const CONC: usize = 8;

#[derive(Clone, Copy)]
struct Cell {
    size: usize,
    /// 0 = one PUT, else number of parts
    parts: usize,
    par: bool,
}

impl Cell {
    fn label(&self) -> String {
        match (self.parts, self.par) {
            (0, _) => "single".into(),
            (n, true) => format!("par{n}"),
            (n, false) => format!("seq{n}"),
        }
    }
}

#[derive(Default)]
struct M {
    reqs: AtomicU64,
    up: AtomicU64,
    created: AtomicU64,
    aborted: AtomicU64,
    completed: AtomicU64,
}

impl M {
    fn req(&self, bytes: usize) {
        self.reqs.fetch_add(1, Ordering::Relaxed);
        self.up.fetch_add(bytes as u64, Ordering::Relaxed);
    }
}

#[derive(Default, Clone)]
struct T {
    create: f64,
    parts_wall: f64,
    max_part: f64,
    complete: f64,
    total: f64,
    ok: bool,
    err: String,
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1e3
}

/// Equal parts of ceil(size/n) bytes, the last one the remainder.
fn split(size: usize, n: usize) -> Vec<(usize, usize)> {
    let each = size.div_ceil(n);
    (0..n).map(|k| (k * each, ((k + 1) * each).min(size))).collect()
}

fn short(e: &str) -> String {
    e.chars().take(80).collect::<String>().replace([',', '\n'], " ")
}

async fn abort(raw: &Raw, key: &str, id: &str, m: &M) {
    m.req(0);
    if matches!(raw.abort(key, id).await, Ok(200 | 204)) {
        m.aborted.fetch_add(1, Ordering::Relaxed);
    }
}

async fn delete(raw: &Raw, key: &str, m: &M) {
    m.req(0);
    let _ = raw.send("DELETE", key, &[], &[], vec![]).await;
}

/// Upload one object in the given form and time its phases.
async fn run_one(raw: &Raw, key: &str, data: &Bytes, cell: Cell, m: &M) -> T {
    let mut t = T::default();
    let t0 = Instant::now();
    if cell.parts == 0 {
        m.req(data.len());
        match raw.put_bytes(key, data.clone()).await {
            Ok(r) if r.status == 200 => t.ok = true,
            Ok(r) => t.err = format!("put status {} {}", r.status, r.code()),
            Err(e) => t.err = short(&e),
        }
        t.total = ms(t0);
        return t;
    }
    m.req(0);
    let id = match raw.create(key).await {
        Ok((id, cms)) => {
            m.created.fetch_add(1, Ordering::Relaxed);
            t.create = cms;
            id
        }
        Err(e) => {
            t.err = short(&e);
            t.total = ms(t0);
            return t;
        }
    };
    let ranges = split(data.len(), cell.parts);
    let tp = Instant::now();
    let results: Vec<Result<(String, f64), String>> = if cell.par {
        join_all(ranges.iter().enumerate().map(|(k, &(a, b))| {
            m.req(b - a);
            raw.part_b(key, &id, (k + 1) as u32, data.slice(a..b))
        }))
        .await
    } else {
        let mut v = vec![];
        for (k, &(a, b)) in ranges.iter().enumerate() {
            m.req(b - a);
            let r = raw.part_b(key, &id, (k + 1) as u32, data.slice(a..b)).await;
            let bad = r.is_err();
            v.push(r);
            if bad {
                break;
            }
        }
        v
    };
    t.parts_wall = ms(tp);
    let mut etags = vec![];
    for r in results {
        match r {
            Ok((e, pm)) => {
                t.max_part = t.max_part.max(pm);
                etags.push(e);
            }
            Err(e) => {
                if t.err.is_empty() {
                    t.err = short(&e);
                }
            }
        }
    }
    if !t.err.is_empty() || etags.len() != ranges.len() {
        abort(raw, key, &id, m).await;
        t.total = ms(t0);
        return t;
    }
    m.req(0);
    match raw.complete(key, &id, &etags, false).await {
        Ok(r) if r.effective_status() == 200 => {
            t.complete = r.ms;
            m.completed.fetch_add(1, Ordering::Relaxed);
            t.ok = true;
        }
        Ok(r) => {
            t.err = format!("complete status {} {}", r.effective_status(), r.code());
            abort(raw, key, &id, m).await;
        }
        Err(e) => {
            t.err = short(&e);
            abort(raw, key, &id, m).await;
        }
    }
    t.total = ms(t0);
    t
}

struct Row {
    phase: &'static str,
    size_mib: usize,
    cell: String,
    i: usize,
    t: T,
}

fn rows_csv(rows: &[Row]) -> String {
    let mut s = String::from(
        "phase,size_mib,cell,i,create_ms,parts_wall_ms,max_part_ms,complete_ms,total_ms,ok,err\n",
    );
    for r in rows {
        s.push_str(&format!(
            "{},{},{},{},{:.2},{:.2},{:.2},{:.2},{:.2},{},{}\n",
            r.phase,
            r.size_mib,
            r.cell,
            r.i,
            r.t.create,
            r.t.parts_wall,
            r.t.max_part,
            r.t.complete,
            r.t.total,
            r.t.ok as u8,
            r.t.err
        ));
    }
    s
}

/// Warm `n` pooled connections with parallel tiny PUTs.
async fn warm(raw: &Raw, prefix: &str, n: usize, m: &M) {
    let body = Bytes::from(vec![7u8; 1024]);
    let futs: Vec<_> = (0..n)
        .map(|i| {
            m.req(1024);
            let (k, b) = (format!("{prefix}/warm/{i}"), body.clone());
            async move { raw.put_bytes(&k, b).await }
        })
        .collect();
    join_all(futs).await;
}

struct Gp {
    size_mib: usize,
    cell: String,
    objects: usize,
    ok: usize,
    bytes: u64,
    wall_ms: f64,
    reqs: u64,
}

async fn goodput(
    raw: &Arc<Raw>,
    prefix: &str,
    m: &Arc<M>,
    cell: Cell,
    n_obj: usize,
    data: &Bytes,
    rows: &mut Vec<Row>,
) -> Gp {
    warm(raw, prefix, CONC * cell.parts.max(1), m).await;
    let label = cell.label();
    let next = Arc::new(AtomicUsize::new(0));
    let r0 = m.reqs.load(Ordering::Relaxed);
    let t0 = Instant::now();
    let mut hs = vec![];
    for _ in 0..CONC {
        let (raw, m, next, data) = (raw.clone(), m.clone(), next.clone(), data.clone());
        let (prefix, label) = (prefix.to_string(), label.clone());
        hs.push(tokio::spawn(async move {
            let mut out = vec![];
            loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                if i >= n_obj {
                    break;
                }
                let key = format!("{prefix}/gp/{}/{label}/{i}", data.len() / MIB);
                let t = run_one(&raw, &key, &data, cell, &m).await;
                out.push((i, key, t));
            }
            out
        }));
    }
    let mut done = vec![];
    for h in hs {
        done.extend(h.await.unwrap_or_default());
    }
    let wall_ms = ms(t0);
    let reqs = m.reqs.load(Ordering::Relaxed) - r0;
    let mut ok = 0;
    for (i, key, t) in done {
        if t.ok {
            ok += 1;
            delete(raw, &key, m).await;
        }
        rows.push(Row { phase: "gp", size_mib: cell.size / MIB, cell: label.clone(), i, t });
    }
    Gp {
        size_mib: cell.size / MIB,
        cell: label,
        objects: n_obj,
        ok,
        bytes: (ok * cell.size) as u64,
        wall_ms,
        reqs,
    }
}

/// Latency: one object at a time, cells interleaved round by round (rotated order).
async fn latency(
    raw: &Raw,
    prefix: &str,
    m: &M,
    cells: &[Cell],
    n: usize,
    datas: &[(usize, Bytes)],
    rows: &mut Vec<Row>,
    caps: (u64, u64),
) {
    for i in 0..n {
        for j in 0..cells.len() {
            let c = cells[(j + i) % cells.len()];
            if m.reqs.load(Ordering::Relaxed) > caps.0 || m.up.load(Ordering::Relaxed) > caps.1 {
                println!("cap reached; skipping the rest of the latency phase");
                return;
            }
            let data = &datas.iter().find(|d| d.0 == c.size).unwrap().1;
            let key = format!("{prefix}/lat/{}/{}/{i}", c.size / MIB, c.label());
            let t = run_one(raw, &key, data, c, m).await;
            if t.ok {
                delete(raw, &key, m).await;
            }
            rows.push(Row { phase: "lat", size_mib: c.size / MIB, cell: c.label(), i, t });
        }
        println!("latency round {}/{} done", i + 1, n);
    }
}

/// The 5 MiB minimum for non-final parts: two 1 MiB parts, Complete, record the answer.
async fn min_part_check(raw: &Raw, prefix: &str, m: &M) -> String {
    let key = format!("{prefix}/check/min-part");
    m.req(0);
    let Ok((id, _)) = raw.create(&key).await else { return "create failed".into() };
    m.created.fetch_add(1, Ordering::Relaxed);
    let body = Bytes::from(vec![1u8; MIB]);
    let mut etags = vec![];
    for k in 1..=2u32 {
        m.req(MIB);
        match raw.part_b(&key, &id, k, body.clone()).await {
            Ok((e, _)) => etags.push(e),
            Err(e) => {
                abort(raw, &key, &id, m).await;
                return format!("part {k} refused: {}", short(&e));
            }
        }
    }
    m.req(0);
    match raw.complete(&key, &id, &etags, false).await {
        Ok(r) if r.effective_status() == 200 => {
            m.completed.fetch_add(1, Ordering::Relaxed);
            delete(raw, &key, m).await;
            "Complete of 1 MiB + 1 MiB accepted (no 5 MiB minimum enforced)".into()
        }
        Ok(r) => {
            abort(raw, &key, &id, m).await;
            format!("Complete of 1 MiB + 1 MiB refused: status {} {}", r.effective_status(), r.code())
        }
        Err(e) => {
            abort(raw, &key, &id, m).await;
            short(&e)
        }
    }
}

fn flag(args: &[String], f: &str) -> Option<String> {
    args.iter().position(|a| a == f).and_then(|i| args.get(i + 1)).cloned()
}

pub async fn run(args: &[String]) {
    let out_dir = flag(args, "--out").unwrap_or_else(|| "out".into());
    let only: Option<Vec<String>> = flag(args, "--providers")
        .map(|s| s.split(',').map(|x| x.trim().to_uppercase()).collect());
    let phases: Vec<String> = flag(args, "--phases")
        .unwrap_or_else(|| "check,lat,gp".into())
        .split(',')
        .map(|s| s.trim().to_string())
        .collect();
    let on = |p: &str| phases.iter().any(|x| x == p);
    let verify = args.iter().any(|a| a == "--verify");
    let sweep = args.iter().any(|a| a == "--sweep");
    std::fs::create_dir_all(&out_dir).expect("create out dir");
    for name in discover() {
        if only.as_ref().is_some_and(|o| !o.contains(&name)) {
            continue;
        }
        let Ok(p) = load(&name) else { continue };
        let mut raw = Raw::new(p.endpoint.as_deref(), &p.bucket, &p.region, &p.ak, &p.sk, p.token.as_deref())
            .expect("raw client");
        raw.unsigned = true;
        let raw = Arc::new(raw);
        let store = build(&p).expect("store");
        let all = Path::from("lunix-p0-probe");
        if verify || sweep {
            let mut open = raw.list_uploads("lunix-p0-probe/").await.unwrap_or_default();
            if sweep {
                for (k, id) in &open {
                    let _ = raw.abort(k, id).await;
                }
                open = raw.list_uploads("lunix-p0-probe/").await.unwrap_or_default();
            }
            let objs = store.list(Some(&all)).filter_map(|m| async { m.ok() }).count().await;
            println!("{name}: objects under lunix-p0-probe/: {objs}; open multipart uploads: {}", open.len());
            continue;
        }
        println!("{name}: mpu probe ({})", p.label);
        let prefix = format!("lunix-p0-probe/mpu-{}", run_id());
        let m = Arc::new(M::default());
        let caps = (
            env_n("LUNIX_PROBE_MPU_CAP_REQS", 5500) as u64,
            env_n("LUNIX_PROBE_MPU_CAP_BYTES", 3_950_000_000) as u64,
        );
        let n = env_n("LUNIX_PROBE_MPU_N", 10);
        let (d16, d32, d64) = (payload(16 * MIB, 1), payload(32 * MIB, 2), payload(64 * MIB, 3));
        let c = |size: usize, parts: usize, par: bool| Cell { size: size * MIB, parts, par };
        let mut rows = vec![];
        let mut gps = vec![];
        let mut meta = format!("provider label: {}\nstarted: {}\n", p.label, utc_stamp());
        warm(&raw, &prefix, CONC, &m).await;

        if on("check") {
            let r = min_part_check(&raw, &prefix, &m).await;
            meta.push_str(&format!("min-part check: {r}\n"));
            println!("min-part check: {r}");
        }
        if on("lat") {
            // 16 MiB: 8 x 2 and 4 x 4 MiB parts are below S3's 5 MiB minimum, so 2 x 8 MiB and
            // 3 x ~5.33 MiB; 32 MiB: 2 x 16, 4 x 8, 6 x ~5.33 MiB (8 x 4 MiB is not allowed)
            let cells = [
                c(16, 0, false), c(16, 2, true), c(16, 3, true), c(16, 2, false),
                c(32, 0, false), c(32, 2, true), c(32, 4, true), c(32, 6, true), c(32, 4, false),
            ];
            let datas = vec![(16 * MIB, d16.clone()), (32 * MIB, d32.clone())];
            latency(&raw, &prefix, &m, &cells, n, &datas, &mut rows, caps).await;
        }
        if on("gp") {
            let (g16, g32) = (env_n("LUNIX_PROBE_MPU_GP16", 24), env_n("LUNIX_PROBE_MPU_GP32", 12));
            for (cell, objs, data) in [
                (c(16, 0, false), g16, &d16),
                (c(32, 0, false), g32, &d32),
                (c(32, 4, true), g32, &d32),
            ] {
                if m.reqs.load(Ordering::Relaxed) > caps.0
                    || m.up.load(Ordering::Relaxed) + (objs * cell.size) as u64 > caps.1
                {
                    println!("cap reached; goodput cell {} {} MiB skipped", cell.label(), cell.size / MIB);
                    continue;
                }
                let g = goodput(&raw, &prefix, &m, cell, objs, data, &mut rows).await;
                println!(
                    "goodput {} MiB {}: {}/{} ok, {:.1} MB/s",
                    g.size_mib, g.cell, g.ok, g.objects, g.bytes as f64 / 1e6 / (g.wall_ms / 1e3)
                );
                gps.push(g);
            }
        }
        if on("opt64") {
            let cells = [c(64, 0, false), c(64, 8, true)];
            let datas = vec![(64 * MIB, d64.clone())];
            latency(&raw, &prefix, &m, &cells, env_n("LUNIX_PROBE_MPU_OPT_N", 5), &datas, &mut rows, caps).await;
        }

        // cleanup: abort anything open under our prefix, delete only our prefix, verify
        let mine = format!("{prefix}/");
        let open = raw.list_uploads(&mine).await.unwrap_or_default();
        for (k, id) in &open {
            abort(&raw, k, id, &m).await;
        }
        let pp = Path::from(prefix.clone());
        let metas: Vec<_> = store.list(Some(&pp)).filter_map(|x| async { x.ok() }).collect().await;
        for x in &metas {
            m.req(0);
            let _ = store.delete(&x.location).await;
        }
        let leftover = store.list(Some(&pp)).filter_map(|x| async { x.ok() }).count().await;
        let still_open = raw.list_uploads(&mine).await.map(|v| v.len() as i64).unwrap_or(-1);
        let tag = p.name.to_lowercase();
        let mut g = String::from("size_mib,cell,objects,ok,bytes,wall_ms,mb_s,reqs,reqs_per_gb\n");
        for x in &gps {
            let mbs = x.bytes as f64 / 1e6 / (x.wall_ms / 1e3);
            g.push_str(&format!(
                "{},{},{},{},{},{:.1},{:.2},{},{:.1}\n",
                x.size_mib, x.cell, x.objects, x.ok, x.bytes, x.wall_ms, mbs, x.reqs,
                x.reqs as f64 / (x.bytes as f64 / 1e9)
            ));
        }
        meta.push_str(&format!(
            "requests (counter, includes deletes and cleanup): {}\nbytes uploaded: {}\nmultipart uploads created/completed/aborted: {}/{}/{}\nopen at the end (before sweep): {}\nleftover objects: {leftover}\nopen uploads after sweep: {still_open}\nfailed samples: {}\n",
            m.reqs.load(Ordering::Relaxed) + 3,
            m.up.load(Ordering::Relaxed),
            m.created.load(Ordering::Relaxed),
            m.completed.load(Ordering::Relaxed),
            m.aborted.load(Ordering::Relaxed),
            open.len(),
            rows.iter().filter(|r| !r.t.ok).count(),
        ));
        std::fs::write(format!("{out_dir}/mpu-{tag}-samples.csv"), rows_csv(&rows)).expect("write");
        std::fs::write(format!("{out_dir}/mpu-{tag}-goodput.csv"), g).expect("write");
        std::fs::write(format!("{out_dir}/mpu-{tag}-meta.txt"), &meta).expect("write");
        print!("{meta}");
    }
}
