//! Design-review measurements (fsync-scope and background-staging notes):
//! conditional `CompleteMultipartUpload`, PUT latency by object size with a fitted
//! fixed cost and per-MiB slope, lane isolation under a bulk upload, and two serial PUTs.

use super::*;

const MIB: usize = 1 << 20;

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1e3
}

fn row(section: &str, size: u64, conc: usize, v: &[f64], detail: String) -> Lat {
    let mut l = lat_row(section, size, v.len(), v.to_vec(), vec![]);
    l.conc = conc;
    l.detail = detail;
    l
}

/// Create-only or plain PUT on an explicit store (so a second client/pool can be used).
async fn put_on(
    c: &Ctx,
    store: &Arc<dyn ObjectStore>,
    key: &Path,
    body: Bytes,
    mode: PutMode,
) -> Result<(), object_store::Error> {
    c.meter.reqs.fetch_add(1, Ordering::Relaxed);
    c.meter.up.fetch_add(body.len() as u64, Ordering::Relaxed);
    store
        .put_opts(key, PutPayload::from(body), PutOptions::from(mode))
        .await
        .map(|_| ())
}

/// Least squares y = a + b x; returns (a, b).
pub fn fit(pts: &[(f64, f64)]) -> (f64, f64) {
    let n = pts.len() as f64;
    let (sx, sy) = pts.iter().fold((0.0, 0.0), |a, p| (a.0 + p.0, a.1 + p.1));
    let (sxx, sxy) = pts
        .iter()
        .fold((0.0, 0.0), |a, p| (a.0 + p.0 * p.0, a.1 + p.0 * p.1));
    let b = (n * sxy - sx * sy) / (n * sxx - sx * sx);
    ((sy - b * sx) / n, b)
}

// ------------------------------------------------- PUT latency by object size

pub async fn put_sizes(c: &Arc<Ctx>, lat: &mut Vec<Lat>) {
    let sizes: [usize; 8] = [
        4096,
        65536,
        256 << 10,
        MIB,
        2 * MIB,
        4 * MIB,
        8 * MIB,
        16 * MIB,
    ];
    let n_small = env_n("LUNIX_PROBE_SIZES_N_SMALL", 40);
    let n_big = env_n("LUNIX_PROBE_SIZES_N_BIG", 12);
    let n_of = |s: usize| if s <= MIB { n_small } else { n_big };
    let est: usize = sizes.iter().map(|s| s * n_of(*s)).sum();
    if !c.allow(sizes.iter().map(|s| n_of(*s) as u64).sum(), est as u64) {
        lat.push(Lat {
            section: "put_size_skipped_cap".into(),
            ..Default::default()
        });
        return;
    }
    let bodies: Vec<Bytes> = sizes.iter().map(|s| payload(*s, *s as u64 + 1)).collect();
    // warm the connection, then interleave sizes round by round so slow drift hits all sizes alike
    let _ = c
        .put(&c.key("sizes/warm"), uniform(64, 1), PutMode::Overwrite)
        .await;
    let mut per: Vec<Vec<f64>> = vec![vec![]; sizes.len()];
    let mut errs = 0;
    for i in 0..n_small.max(n_big) {
        for (j, s) in sizes.iter().enumerate() {
            if i >= n_of(*s) {
                continue;
            }
            let k = c.key(&format!("sizes/{s}/{i}"));
            let t = Instant::now();
            match c.put(&k, bodies[j].clone(), PutMode::Overwrite).await {
                Ok(_) => {
                    let m = ms(t);
                    per[j].push(m);
                    c.sample("put_size", *s as u64, m);
                }
                Err(_) => errs += 1,
            }
        }
    }
    let mut p50s = vec![];
    let mut p99s = vec![];
    for (j, s) in sizes.iter().enumerate() {
        let r = row(
            "put_size",
            *s as u64,
            1,
            &per[j],
            format!("sequential, conc 1, plain PUT, unique keys; errors={errs}"),
        );
        let mib = *s as f64 / MIB as f64;
        p50s.push((mib, r.p50));
        p99s.push((mib, r.p99));
        let mut r = r;
        r.mbs = mib * 1.048576 / (r.p50 / 1e3);
        lat.push(r);
    }
    for (name, pts) in [("p50", &p50s), ("p99", &p99s)] {
        let (a, b) = fit(pts);
        let big: Vec<_> = pts.iter().filter(|p| p.0 >= 1.0).copied().collect();
        let (a2, b2) = fit(&big);
        lat.push(Lat {
            section: format!("put_size_fit_{name}"),
            conc: 1,
            n: pts.len(),
            p50: a,
            detail: format!(
                "all sizes: fixed={a:.1} ms, slope={b:.2} ms/MiB ({:.0} MiB/s); 1-16 MiB only: fixed={a2:.1} ms, slope={b2:.2} ms/MiB ({:.0} MiB/s); fixed cost in the p50 column",
                1000.0 / b,
                1000.0 / b2
            ),
            ..Default::default()
        });
    }
}

// ------------------------------------------------------------ serial PUT pairs

pub async fn serial_pairs(c: &Arc<Ctx>, lat: &mut Vec<Lat>) {
    // (first size, second size, n): the FLUSH-then-FUA pattern is two create-only PUTs in series
    for (a, b, n) in [(4096usize, 4096usize, 100usize), (MIB, 4096, 40)] {
        let n = env_n("LUNIX_PROBE_PAIR_N", n);
        if !c.allow(2 * n as u64, (n * (a + b)) as u64) {
            lat.push(Lat {
                section: "serial_pair_skipped_cap".into(),
                ..Default::default()
            });
            continue;
        }
        let (ba, bb) = (payload(a, 11), payload(b, 12));
        let (mut tot, mut f, mut s) = (vec![], vec![], vec![]);
        for i in 0..n {
            let t0 = Instant::now();
            let r1 = c
                .put(&c.key(&format!("pair/{a}-{b}/{i}/a")), ba.clone(), PutMode::Create)
                .await;
            let m1 = ms(t0);
            let t1 = Instant::now();
            let r2 = c
                .put(&c.key(&format!("pair/{a}-{b}/{i}/b")), bb.clone(), PutMode::Create)
                .await;
            let m2 = ms(t1);
            if r1.is_ok() && r2.is_ok() {
                tot.push(m1 + m2);
                f.push(m1);
                s.push(m2);
                c.sample(&format!("pair_{a}_{b}_total"), (a + b) as u64, m1 + m2);
            }
        }
        let (mut fs, mut ss) = (f.clone(), s.clone());
        fs.sort_by(|x, y| x.partial_cmp(y).unwrap());
        ss.sort_by(|x, y| x.partial_cmp(y).unwrap());
        let detail = format!(
            "create-only PUT of {a} B then, once complete, create-only PUT of {b} B; first p50={:.1} p99={:.1}, second p50={:.1} p99={:.1}",
            pct(&fs, 0.5),
            pct(&fs, 0.99),
            pct(&ss, 0.5),
            pct(&ss, 0.99)
        );
        lat.push(row(&format!("serial_pair_{a}_{b}"), (a + b) as u64, 1, &tot, detail));
    }
}

// ------------------------------------------------------------- lane isolation

struct LaneOut {
    small: Vec<(usize, f64)>, // (size, ms) for samples fully inside the bulk window
    attempted: usize,
    objs: usize,
    wall: f64,
    errs: usize,
}

async fn lane_phase(
    c: &Arc<Ctx>,
    mode: &str,
    bulk: Arc<dyn ObjectStore>,
    small: Option<Arc<dyn ObjectStore>>,
    nobj: usize,
) -> LaneOut {
    const BULK: usize = 16 * MIB;
    const WORKERS: usize = 8;
    let body = payload(BULK, 77);
    let active = Arc::new(AtomicBool::new(true));
    let next = Arc::new(AtomicUsize::new(0));
    let done = Arc::new(AtomicUsize::new(0));
    let errs = Arc::new(AtomicUsize::new(0));
    let t0 = Instant::now();
    let mut ws = vec![];
    for _ in 0..WORKERS {
        let (c2, st, b2) = (c.clone(), bulk.clone(), body.clone());
        let (a2, n2, d2, e2) = (active.clone(), next.clone(), done.clone(), errs.clone());
        let mode = mode.to_string();
        ws.push(tokio::spawn(async move {
            loop {
                let i = n2.fetch_add(1, Ordering::SeqCst);
                if i >= nobj {
                    // the first worker to run dry ends the window: fewer than 8 uploads in flight
                    a2.store(false, Ordering::SeqCst);
                    break;
                }
                let k = c2.key(&format!("lane/{mode}/bulk/{i}"));
                match put_on(&c2, &st, &k, b2.clone(), PutMode::Overwrite).await {
                    Ok(()) => {
                        d2.fetch_add(1, Ordering::SeqCst);
                    }
                    Err(_) => {
                        e2.fetch_add(1, Ordering::SeqCst);
                    }
                }
            }
        }));
    }
    let sampler = small.map(|st| {
        let (c2, a2) = (c.clone(), active.clone());
        let mode = mode.to_string();
        tokio::spawn(async move {
            let mut out = vec![];
            let mut attempted = 0;
            let mut i = 0usize;
            while a2.load(Ordering::SeqCst) {
                let size = if i % 2 == 0 { 4096 } else { 65536 };
                let b = payload(size, 5 + i as u64);
                let k = c2.key(&format!("lane/{mode}/small/{i}"));
                let was = a2.load(Ordering::SeqCst);
                let t = Instant::now();
                let r = put_on(&c2, &st, &k, b, PutMode::Create).await;
                let m = ms(t);
                let is = a2.load(Ordering::SeqCst);
                attempted += 1;
                if r.is_ok() && was && is {
                    out.push((size, m));
                }
                i += 1;
            }
            (out, attempted)
        })
    });
    for w in ws {
        let _ = w.await;
    }
    let wall = t0.elapsed().as_secs_f64();
    let (small, attempted) = match sampler {
        Some(h) => h.await.unwrap_or((vec![], 0)),
        None => (vec![], 0),
    };
    LaneOut {
        small,
        attempted,
        objs: done.load(Ordering::SeqCst),
        wall,
        errs: errs.load(Ordering::SeqCst),
    }
}

/// `bulk` is the bulk client; `other` is a second, independent client (own connection pool).
pub async fn lane(
    c: &Arc<Ctx>,
    bulk: Arc<dyn ObjectStore>,
    other: Arc<dyn ObjectStore>,
    lat: &mut Vec<Lat>,
) {
    const BULK: usize = 16 * MIB;
    let nobj = env_n("LUNIX_PROBE_LANE_OBJS", 32);
    let n_idle = env_n("LUNIX_PROBE_LANE_IDLE_N", 60);
    let n_alone = env_n("LUNIX_PROBE_LANE_ALONE_OBJS", nobj);
    if !c.allow(
        (2 * nobj + n_alone + 2 * n_idle) as u64,
        ((2 * nobj + n_alone) * BULK + n_idle * 70_000) as u64,
    ) {
        lat.push(Lat {
            section: "lane_skipped_cap".into(),
            ..Default::default()
        });
        return;
    }
    // baseline: the same small PUTs with nothing else running, on the separate client
    let _ = put_on(c, &other, &c.key("lane/warm"), uniform(64, 1), PutMode::Overwrite).await;
    let mut idle: Vec<(usize, f64)> = vec![];
    for i in 0..n_idle {
        let size = if i % 2 == 0 { 4096 } else { 65536 };
        let k = c.key(&format!("lane/idle/{i}"));
        let t = Instant::now();
        if put_on(c, &other, &k, payload(size, i as u64), PutMode::Create)
            .await
            .is_ok()
        {
            idle.push((size, ms(t)));
        }
    }
    let _ = put_on(c, &bulk, &c.key("lane/warm2"), uniform(64, 1), PutMode::Overwrite).await;
    // order: bulk alone, separate pool, shared pool
    let alone = lane_phase(c, "alone", bulk.clone(), None, n_alone).await;
    let sep = lane_phase(c, "separate", bulk.clone(), Some(other.clone()), nobj).await;
    let shared = lane_phase(c, "shared", bulk.clone(), Some(bulk.clone()), nobj).await;
    let emit = |lat: &mut Vec<Lat>, name: &str, v: &[(usize, f64)], extra: &str| {
        for size in [4096usize, 65536] {
            let xs: Vec<f64> = v.iter().filter(|p| p.0 == size).map(|p| p.1).collect();
            for (sz, m) in v.iter().filter(|p| p.0 == size) {
                c.sample(&format!("lane_{name}"), *sz as u64, *m);
            }
            lat.push(row(
                &format!("lane_{name}_put"),
                size as u64,
                1,
                &xs,
                format!("create-only PUT, {extra}"),
            ));
        }
    };
    emit(lat, "idle", &idle, "no bulk upload running");
    emit(
        lat,
        "separate",
        &sep.small,
        &format!(
            "separate client while {} x 16 MiB bulk at conc 8 ran; {} attempted, {} fully inside the bulk window",
            sep.objs, sep.attempted, sep.small.len()
        ),
    );
    emit(
        lat,
        "shared",
        &shared.small,
        &format!(
            "same client as the bulk upload; {} attempted, {} fully inside the bulk window",
            shared.attempted, shared.small.len()
        ),
    );
    for (name, o) in [("alone", &alone), ("separate", &sep), ("shared", &shared)] {
        lat.push(Lat {
            section: format!("lane_{name}_bulk"),
            size: BULK as u64,
            conc: 8,
            n: o.objs,
            p50: o.wall * 1e3,
            mbs: (o.objs * BULK) as f64 / 1e6 / o.wall,
            detail: format!(
                "bulk 16 MiB PUT goodput over the whole phase (decimal MB/s), wall_ms in p50 column, errors={}",
                o.errs
            ),
            ..Default::default()
        });
    }
}

// --------------------------------------------- conditional CompleteMultipartUpload

fn upload_key(c: &Ctx, rel: &str) -> String {
    format!("{}/{}", c.prefix, rel)
}

pub async fn cond_complete(c: &Arc<Ctx>, sem: &mut Vec<Sem>, lat: &mut Vec<Lat>) {
    let Some(raw) = c.raw.as_ref() else {
        sem.push(Sem {
            test: "cond_complete".into(),
            v: V::Skipped,
            detail: "no raw client (in-memory store)".into(),
        });
        return;
    };
    const PART: usize = 5 * MIB;
    let part5 = vec![0xA5u8; PART];
    let small = vec![0x5Au8; 4096];

    // 1. create-only Complete on an absent key: success, and the latency of each step.
    //    Also an unconditional Complete as the control. 1, 2, 4 parts (< 20 MiB total).
    for (label, parts, cond, n) in [
        ("1part_4k_plain", vec![4096usize], false, 30usize),
        ("1part_4k_cond", vec![4096], true, 30),
        ("2part_5m_4k_cond", vec![PART, 4096], true, 8),
        ("4part_15m_4k_cond", vec![PART, PART, PART, 4096], true, 6),
    ] {
        let (mut cr, mut up, mut co) = (vec![], vec![], vec![]);
        let (mut ok, mut other) = (0, vec![]);
        let tot: usize = parts.iter().sum();
        if !c.allow((n * (parts.len() + 2)) as u64, (n * tot) as u64) {
            continue;
        }
        for i in 0..n {
            let key = upload_key(c, &format!("mpu/{label}/{i}"));
            c.meter.reqs.fetch_add(2 + parts.len() as u64, Ordering::Relaxed);
            let (id, m) = match raw.create(&key).await {
                Ok(x) => x,
                Err(e) => {
                    other.push(e);
                    continue;
                }
            };
            c.mpu[0].fetch_add(1, Ordering::Relaxed);
            cr.push(m);
            let mut etags = vec![];
            let mut failed = false;
            for (pi, len) in parts.iter().enumerate() {
                let data = if *len == PART { part5.clone() } else { small.clone() };
                c.meter.up.fetch_add(*len as u64, Ordering::Relaxed);
                match raw.part(&key, &id, pi as u32 + 1, data).await {
                    Ok((e, m)) => {
                        etags.push(e);
                        if *len == 4096 {
                            up.push(m);
                        }
                    }
                    Err(e) => {
                        other.push(e);
                        failed = true;
                        break;
                    }
                }
            }
            if failed {
                if raw.abort(&key, &id).await.is_ok() {
                    c.mpu[1].fetch_add(1, Ordering::Relaxed);
                }
                continue;
            }
            match raw.complete(&key, &id, &etags, cond).await {
                Ok(r) if r.effective_status() == 200 => {
                    ok += 1;
                    co.push(r.ms);
                    c.mpu[2].fetch_add(1, Ordering::Relaxed);
                }
                Ok(r) => {
                    other.push(format!("complete status {} {}", r.effective_status(), r.code()));
                    if raw.abort(&key, &id).await.is_ok() {
                        c.mpu[1].fetch_add(1, Ordering::Relaxed);
                    }
                }
                Err(e) => other.push(e),
            }
        }
        lat.push(row(
            &format!("mpu_complete_{label}"),
            tot as u64,
            1,
            &co,
            format!("{ok}/{n} ok; other: {}", other.first().cloned().unwrap_or_default()),
        ));
        for (nm, v) in [("create", &cr), ("part4k", &up)] {
            if label.starts_with("1part") && !v.is_empty() {
                lat.push(row(
                    &format!("mpu_{nm}_{label}"),
                    if nm == "part4k" { 4096 } else { 0 },
                    1,
                    v,
                    "UploadPart / CreateMultipartUpload latency".into(),
                ));
            }
        }
        for m in &co {
            c.sample(&format!("mpu_complete_{label}"), tot as u64, *m);
        }
        if label == "1part_4k_cond" {
            sem.push(Sem {
                test: "cond_complete_create_absent".into(),
                v: if ok == n { V::Pass } else { V::Fail },
                detail: format!("If-None-Match: * on Complete of an absent key: {ok}/{n} succeeded"),
            });
        }
    }

    // 2. the key already exists: must be refused (412), not silently overwritten
    let (mut r412, mut r200, mut others) = (0, 0, vec![]);
    let rounds = 10;
    for i in 0..rounds {
        let key = upload_key(c, &format!("mpu/exists/{i}"));
        let p = Path::from(key.clone());
        let orig = uniform(512, 9);
        if c.put(&p, orig.clone(), PutMode::Overwrite).await.is_err() {
            others.push("setup put failed".to_string());
            continue;
        }
        c.meter.reqs.fetch_add(4, Ordering::Relaxed);
        let Ok((id, _)) = raw.create(&key).await else {
            others.push("create failed".into());
            continue;
        };
        c.mpu[0].fetch_add(1, Ordering::Relaxed);
        let Ok((etag, _)) = raw.part(&key, &id, 1, small.clone()).await else {
            let _ = raw.abort(&key, &id).await;
            c.mpu[1].fetch_add(1, Ordering::Relaxed);
            continue;
        };
        let mut completed = false;
        match raw.complete(&key, &id, &[etag], true).await {
            Ok(r) => match r.effective_status() {
                412 => r412 += 1,
                200 => {
                    r200 += 1;
                    completed = true;
                    c.mpu[2].fetch_add(1, Ordering::Relaxed);
                }
                s => others.push(format!("{s} {}", r.code())),
            },
            Err(e) => others.push(e),
        }
        if !completed && raw.abort(&key, &id).await.is_ok() {
            // an upload that did not complete is still open: abort it
            c.mpu[1].fetch_add(1, Ordering::Relaxed);
        }
    }
    let v = if r412 == rounds {
        V::Pass
    } else if r200 > 0 {
        V::Fail
    } else {
        V::Unsupported
    };
    sem.push(Sem {
        test: "cond_complete_existing_412".into(),
        v,
        detail: format!(
            "Complete with If-None-Match: * on an existing key, {rounds} rounds: 412={r412}, 200 (overwritten)={r200}, other={}{}",
            others.len(),
            others.first().map(|s| format!(" ({s})")).unwrap_or_default()
        ),
    });

    // 3. 8-way race: eight uploads of one key complete together; exactly one may win
    let rounds = env_n("LUNIX_PROBE_CCRACE_ROUNDS", 10);
    let (mut perfect, mut statuses): (usize, BTreeMap<String, usize>) = (0, BTreeMap::new());
    for r in 0..rounds {
        let key = upload_key(c, &format!("mpu/race/{r}"));
        let mut ups = vec![];
        for _ in 0..RACERS {
            c.meter.reqs.fetch_add(3, Ordering::Relaxed);
            if let Ok((id, _)) = raw.create(&key).await {
                c.mpu[0].fetch_add(1, Ordering::Relaxed);
                if let Ok((e, _)) = raw.part(&key, &id, 1, small.clone()).await {
                    ups.push((id, e));
                }
            }
        }
        let gate = Barrier::new(ups.len());
        let res = futures::future::join_all(ups.iter().map(|(id, e)| {
            let (gate, key) = (&gate, &key);
            async move {
                gate.wait().await;
                raw.complete(key, id, std::slice::from_ref(e), true).await
            }
        }))
        .await;
        let mut wins = 0;
        for ((id, _), r) in ups.iter().zip(res) {
            let s = r.as_ref().map(|r| r.effective_status()).unwrap_or(0);
            let label = match &r {
                Ok(r) if s != 200 => format!("{s} {}", r.code()),
                Ok(_) => "200".to_string(),
                Err(e) => format!("0 {}", e.chars().take(40).collect::<String>()),
            };
            *statuses.entry(label).or_default() += 1;
            if s == 200 {
                wins += 1;
                c.mpu[2].fetch_add(1, Ordering::Relaxed);
            } else if raw.abort(&key, id).await.is_ok() {
                c.mpu[1].fetch_add(1, Ordering::Relaxed);
            }
        }
        if wins == 1 && ups.len() == RACERS {
            perfect += 1;
        }
        c.meter.reqs.fetch_add(RACERS as u64 * 2, Ordering::Relaxed);
    }
    sem.push(Sem {
        test: "cond_complete_race_x8".into(),
        v: if perfect == rounds { V::Pass } else { V::Fail },
        detail: format!("{perfect}/{rounds} rounds with exactly one winner; Complete statuses over all {} attempts: {statuses:?}", rounds * RACERS),
    });
}

/// Abort anything still open under our prefix and report how many were found.
pub async fn sweep_uploads(c: &Arc<Ctx>) -> Result<usize, String> {
    let Some(raw) = c.raw.as_ref() else { return Ok(0) };
    let open = raw.list_uploads(&format!("{}/", c.prefix)).await?;
    for (k, id) in &open {
        if k.starts_with(&format!("{}/", c.prefix)) && raw.abort(k, id).await.is_ok() {
            c.mpu[1].fetch_add(1, Ordering::Relaxed);
        }
    }
    Ok(open.len())
}

/// Read-only: GetBucketVersioning. Never changes bucket configuration.
pub async fn versioning_state(c: &Arc<Ctx>, sem: &mut Vec<Sem>) {
    let Some(raw) = c.raw.as_ref() else { return };
    c.meter.reqs.fetch_add(1, Ordering::Relaxed);
    let (v, d) = match raw.send("GET", "", &[("versioning", "")], &[], vec![]).await {
        Ok(r) if r.status == 200 => (
            V::Info,
            match raw::tag(&r.body, "Status") {
                Some(s) => format!("GetBucketVersioning (read-only): {s}"),
                None => "GetBucketVersioning (read-only): never enabled (no Status element)".into(),
            },
        ),
        Ok(r) => (
            V::Info,
            format!("GetBucketVersioning not readable: HTTP {} {}", r.status, r.code()),
        ),
        Err(e) => (V::Info, format!("GetBucketVersioning failed: {e}")),
    };
    sem.push(Sem {
        test: "versioning_state_read".into(),
        v,
        detail: d,
    });
}
