//! `s3probe`: provider-agnostic S3 probe for Lunix P0.10 (ADR-2, DESIGN §9 `doctor`).
//!
//! Throwaway spike code. It measures (a) whether an endpoint implements the
//! conditional-write protocol the commit chain and lease need, and (b) PUT/GET latency
//! and throughput. It never hardcodes or prints credentials: everything comes from the
//! environment. A provider whose variables are unset is reported as
//! "not run: no credentials".
//!
//! Per provider `<NAME>` (AWS, R2, OVH, MINIO, GCS, B2, ... or any name found as
//! `LUNIX_PROBE_<NAME>_BUCKET`):
//!   LUNIX_PROBE_<NAME>_BUCKET            required
//!   LUNIX_PROBE_<NAME>_ENDPOINT          optional (omit for AWS)
//!   LUNIX_PROBE_<NAME>_REGION            default us-east-1
//!   LUNIX_PROBE_<NAME>_ACCESS_KEY_ID / _SECRET_ACCESS_KEY / _SESSION_TOKEN
//!                                        else the standard AWS_* variables
//!   LUNIX_PROBE_<NAME>_PATH_STYLE=1      path-style addressing (MinIO, many others)
//!   LUNIX_PROBE_<NAME>_ALLOW_HTTP=1      plain http endpoint (local MinIO only)
//!   LUNIX_PROBE_<NAME>_LABEL             coarse location label for the report
//! AWS shared-config profiles are not resolved by this spike: export the credentials.
//!
//! Everything is written under `lunix-p0-probe/<run-id>/` and only that prefix is listed
//! and deleted. Defaults are sized to stay under about 5,000 requests and 2 GB
//! transferred per provider; phases that would exceed the cap are skipped and reported.

use bytes::Bytes;
use futures::{stream, StreamExt};
use object_store::aws::{AmazonS3Builder, S3ConditionalPut};
use object_store::path::Path;
use object_store::{
    ClientOptions, GetOptions, GetRange, MultipartUpload, ObjectStore, ObjectStoreExt, PutMode,
    PutOptions, PutPayload, PutResult, RetryConfig, UpdateVersion,
};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::Barrier;

const KNOWN: &[&str] = &[
    "AWS", "R2", "OVH", "MINIO", "GCS", "B2", "SCALEWAY", "WASABI", "HETZNER", "TIGRIS", "SPACES",
];
const RACERS: usize = 8;
const CAP_REQS: u64 = 5000;
const CAP_BYTES: u64 = 2_000_000_000;

// ---------------------------------------------------------------- environment

fn env_opt(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|s| !s.is_empty())
}

fn env_n(k: &str, default: usize) -> usize {
    env_opt(k).and_then(|v| v.parse().ok()).unwrap_or(default)
}

struct Prov {
    name: String,
    endpoint: Option<String>,
    region: String,
    bucket: String,
    path_style: bool,
    allow_http: bool,
    ak: String,
    sk: String,
    token: Option<String>,
    label: String,
}

fn load(name: &str) -> Result<Prov, String> {
    let v = |s: &str| env_opt(&format!("LUNIX_PROBE_{name}_{s}"));
    let bucket = v("BUCKET");
    let ak = v("ACCESS_KEY_ID").or_else(|| env_opt("AWS_ACCESS_KEY_ID"));
    let sk = v("SECRET_ACCESS_KEY").or_else(|| env_opt("AWS_SECRET_ACCESS_KEY"));
    match (bucket, ak, sk) {
        (Some(bucket), Some(ak), Some(sk)) => Ok(Prov {
            name: name.to_string(),
            endpoint: v("ENDPOINT"),
            region: v("REGION").unwrap_or_else(|| "us-east-1".into()),
            bucket,
            path_style: v("PATH_STYLE").is_some_and(|s| s == "1"),
            allow_http: v("ALLOW_HTTP").is_some_and(|s| s == "1"),
            ak,
            sk,
            token: v("SESSION_TOKEN").or_else(|| env_opt("AWS_SESSION_TOKEN")),
            label: v("LABEL").unwrap_or_else(|| "unlabelled".into()),
        }),
        _ => Err(format!(
            "not run: no credentials (LUNIX_PROBE_{name}_BUCKET and access keys unset)"
        )),
    }
}

fn discover() -> Vec<String> {
    let mut names: Vec<String> = KNOWN.iter().map(|s| s.to_string()).collect();
    for (k, _) in std::env::vars() {
        if let Some(rest) = k.strip_prefix("LUNIX_PROBE_") {
            if let Some(n) = rest.strip_suffix("_BUCKET") {
                if !names.iter().any(|x| x == n) {
                    names.push(n.to_string());
                }
            }
        }
    }
    names
}

fn build(p: &Prov) -> Result<Arc<dyn ObjectStore>, String> {
    let mut b = AmazonS3Builder::new()
        .with_bucket_name(&p.bucket)
        .with_region(&p.region)
        .with_access_key_id(&p.ak)
        .with_secret_access_key(&p.sk)
        .with_conditional_put(S3ConditionalPut::ETagMatch)
        // no hidden retries: the probe wants to see every status the provider returns
        .with_retry(RetryConfig {
            max_retries: 0,
            retry_timeout: Duration::from_secs(60),
            ..Default::default()
        })
        .with_client_options(
            ClientOptions::new()
                .with_timeout(Duration::from_secs(120))
                .with_connect_timeout(Duration::from_secs(10)),
        );
    if let Some(t) = &p.token {
        b = b.with_token(t);
    }
    if let Some(e) = &p.endpoint {
        b = b.with_endpoint(e);
    }
    if p.allow_http {
        b = b.with_allow_http(true);
    }
    if p.path_style {
        b = b.with_virtual_hosted_style_request(false);
    }
    let s = b.build().map_err(|e| format!("client build failed: {e}"))?;
    Ok(Arc::new(s))
}

// ------------------------------------------------------------------- results

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum V {
    Pass,
    Fail,
    Unsupported,
    Warn,
    Info,
    Skipped,
}

impl V {
    fn s(self) -> &'static str {
        match self {
            V::Pass => "PASS",
            V::Fail => "FAIL",
            V::Unsupported => "UNSUPPORTED",
            V::Warn => "WARN",
            V::Info => "INFO",
            V::Skipped => "SKIPPED",
        }
    }
}

struct Sem {
    test: String,
    v: V,
    detail: String,
}

#[derive(Default)]
struct Lat {
    section: String,
    size: u64,
    conc: usize,
    n: usize,
    p50: f64,
    p90: f64,
    p99: f64,
    max: f64,
    ttfb50: f64,
    ttfb99: f64,
    mbs: f64,
    detail: String,
}

fn pct(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    let i = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[i]
}

fn lat_row(section: &str, size: u64, n: usize, mut ms: Vec<f64>, mut ttfb: Vec<f64>) -> Lat {
    ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
    ttfb.sort_by(|a, b| a.partial_cmp(b).unwrap());
    Lat {
        section: section.into(),
        size,
        conc: 1,
        n,
        p50: pct(&ms, 0.5),
        p90: pct(&ms, 0.9),
        p99: pct(&ms, 0.99),
        max: ms.last().copied().unwrap_or(f64::NAN),
        ttfb50: pct(&ttfb, 0.5),
        ttfb99: pct(&ttfb, 0.99),
        ..Default::default()
    }
}

// --------------------------------------------------------------- error status

/// HTTP status carried by an `object_store` error, when its text says (the raw error type
/// is crate-private, so this reads the `status code: NNN` phrasing, as Constellation's
/// `cas::http_status` does). Variants that imply a status are mapped directly.
fn status_in(text: &str) -> Option<u16> {
    let at = text.find("status code: ")? + "status code: ".len();
    let d: String = text[at..].chars().take(3).collect();
    if d.len() == 3 && d.chars().all(|c| c.is_ascii_digit()) {
        d.parse().ok()
    } else {
        None
    }
}

fn chain_status(e: &(dyn std::error::Error + 'static)) -> Option<u16> {
    let mut cur: Option<&(dyn std::error::Error + 'static)> = Some(e);
    while let Some(x) = cur {
        if let Some(c) = status_in(&x.to_string()) {
            return Some(c);
        }
        cur = x.source();
    }
    None
}

fn status(e: &object_store::Error) -> Option<u16> {
    use object_store::Error as E;
    if let Some(c) = chain_status(e) {
        return Some(c);
    }
    match e {
        E::Precondition { .. } => Some(412),
        E::NotModified { .. } => Some(304),
        E::NotFound { .. } => Some(404),
        E::NotImplemented { .. } => Some(501),
        E::PermissionDenied { .. } => Some(403),
        E::Unauthenticated { .. } => Some(401),
        E::AlreadyExists { source, .. } => match source.downcast_ref::<object_store::Error>() {
            Some(inner) => status(inner),
            // A 409 from the HTTP layer already matched `chain_status` above. An
            // `AlreadyExists` with no status text comes from a store without the HTTP
            // layer (in-memory, file system): the key exists, i.e. a lost create race.
            None => Some(412),
        },
        _ => None,
    }
}

fn variant(e: &object_store::Error) -> &'static str {
    use object_store::Error as E;
    match e {
        E::AlreadyExists { .. } => "AlreadyExists",
        E::Precondition { .. } => "Precondition",
        E::NotModified { .. } => "NotModified",
        E::NotFound { .. } => "NotFound",
        E::NotImplemented { .. } => "NotImplemented",
        E::PermissionDenied { .. } => "PermissionDenied",
        E::Unauthenticated { .. } => "Unauthenticated",
        E::Generic { .. } => "Generic",
        _ => "other",
    }
}

fn desc(e: &object_store::Error) -> String {
    format!(
        "{} {}",
        status(e)
            .map(|s| s.to_string())
            .unwrap_or_else(|| "no-status".into()),
        variant(e)
    )
}

// ------------------------------------------------------------------- context

#[derive(Default)]
struct Meter {
    reqs: AtomicU64,
    up: AtomicU64,
    down: AtomicU64,
}

struct Ctx {
    store: Arc<dyn ObjectStore>,
    prefix: String,
    meter: Meter,
    stop: AtomicBool,
}

impl Ctx {
    fn key(&self, rel: &str) -> Path {
        Path::from(format!("{}/{}", self.prefix, rel))
    }

    fn allow(&self, est_reqs: u64, est_bytes: u64) -> bool {
        self.meter.reqs.load(Ordering::Relaxed) + est_reqs <= CAP_REQS
            && self.meter.up.load(Ordering::Relaxed)
                + self.meter.down.load(Ordering::Relaxed)
                + est_bytes
                <= CAP_BYTES
    }

    async fn put(
        &self,
        key: &Path,
        body: Bytes,
        mode: PutMode,
    ) -> Result<PutResult, object_store::Error> {
        self.meter.reqs.fetch_add(1, Ordering::Relaxed);
        self.meter
            .up
            .fetch_add(body.len() as u64, Ordering::Relaxed);
        self.store
            .put_opts(key, PutPayload::from(body), PutOptions::from(mode))
            .await
    }

    async fn get(&self, key: &Path) -> Result<Bytes, object_store::Error> {
        self.meter.reqs.fetch_add(1, Ordering::Relaxed);
        let b = self.store.get(key).await?.bytes().await?;
        self.meter.down.fetch_add(b.len() as u64, Ordering::Relaxed);
        Ok(b)
    }

    async fn delete(&self, key: &Path) -> Result<(), object_store::Error> {
        self.meter.reqs.fetch_add(1, Ordering::Relaxed);
        self.store.delete(key).await
    }

    async fn head(&self, key: &Path) -> Result<object_store::ObjectMeta, object_store::Error> {
        self.meter.reqs.fetch_add(1, Ordering::Relaxed);
        self.store.head(key).await
    }

    async fn list_count(&self, rel: &str) -> Result<usize, object_store::Error> {
        self.meter.reqs.fetch_add(1, Ordering::Relaxed);
        let p = self.key(rel);
        let mut s = self.store.list(Some(&p));
        let mut n = 0;
        while let Some(m) = s.next().await {
            m?;
            n += 1;
        }
        Ok(n)
    }
}

fn payload(len: usize, seed: u64) -> Bytes {
    let mut s = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let mut v = Vec::with_capacity(len);
    while v.len() < len {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        v.extend_from_slice(&s.to_le_bytes());
    }
    v.truncate(len);
    Bytes::from(v)
}

fn uniform(len: usize, b: u8) -> Bytes {
    Bytes::from(vec![b; len])
}

// --------------------------------------------------------- semantics: basics

async fn sem_basic(c: &Ctx, out: &mut Vec<Sem>) {
    let k = c.key("sem/basic");
    let mut push = |t: &str, v: V, d: String| {
        out.push(Sem {
            test: t.into(),
            v,
            detail: d,
        })
    };

    // create-if-absent
    let first = c.put(&k, Bytes::from_static(b"one"), PutMode::Create).await;
    let first = match first {
        Ok(r) => {
            push(
                "create_if_absent_first",
                V::Pass,
                format!(
                    "etag={:?} version={:?}",
                    r.e_tag.as_deref().map(|e| e.starts_with('"')),
                    r.version
                ),
            );
            r
        }
        Err(e) => {
            let st = status(&e);
            let v = if st == Some(501) || st == Some(400) {
                V::Unsupported
            } else {
                V::Fail
            };
            push("create_if_absent_first", v, desc(&e));
            push(
                "create_if_absent_second_412",
                V::Skipped,
                "first create failed".into(),
            );
            return;
        }
    };
    push(
        "versioning_state",
        V::Info,
        match first.version.as_deref() {
            Some("null") => "suspended (version id null)".to_string(),
            Some(_) => "enabled (version id returned)".to_string(),
            None => "off or not reported".to_string(),
        },
    );
    push(
        "etag_quoted",
        V::Info,
        format!(
            "{}",
            first.e_tag.as_deref().is_some_and(|e| e.starts_with('"'))
        ),
    );
    match c.put(&k, Bytes::from_static(b"two"), PutMode::Create).await {
        Ok(_) => push(
            "create_if_absent_second_412",
            V::Fail,
            "second create succeeded: If-None-Match: * ignored".into(),
        ),
        Err(e) => match status(&e) {
            Some(412) => push("create_if_absent_second_412", V::Pass, desc(&e)),
            Some(501) => push("create_if_absent_second_412", V::Unsupported, desc(&e)),
            _ => push(
                "create_if_absent_second_412",
                V::Warn,
                format!("unexpected: {}", desc(&e)),
            ),
        },
    }

    // If-Match swap
    let v1 = UpdateVersion {
        e_tag: first.e_tag.clone(),
        version: first.version.clone(),
    };
    let second = c
        .put(
            &k,
            Bytes::from_static(b"three"),
            PutMode::Update(v1.clone()),
        )
        .await;
    match &second {
        Ok(r) => push(
            "if_match_swap_ok",
            if r.e_tag != first.e_tag {
                V::Pass
            } else {
                V::Warn
            },
            format!("etag changed={}", r.e_tag != first.e_tag),
        ),
        Err(e) => push(
            "if_match_swap_ok",
            if status(e) == Some(501) {
                V::Unsupported
            } else {
                V::Fail
            },
            desc(e),
        ),
    }
    match c
        .put(&k, Bytes::from_static(b"four"), PutMode::Update(v1))
        .await
    {
        Ok(_) => push(
            "if_match_stale_412",
            V::Fail,
            "stale ETag accepted: If-Match ignored".into(),
        ),
        Err(e) => match status(&e) {
            Some(412) => push("if_match_stale_412", V::Pass, desc(&e)),
            Some(501) => push("if_match_stale_412", V::Unsupported, desc(&e)),
            _ => push(
                "if_match_stale_412",
                V::Warn,
                format!("unexpected: {}", desc(&e)),
            ),
        },
    }

    // If-Match on a deleted key
    let cur = c.head(&k).await.ok();
    let _ = c.delete(&k).await;
    let ver = UpdateVersion {
        e_tag: cur.and_then(|m| m.e_tag),
        version: None,
    };
    match c
        .put(&k, Bytes::from_static(b"five"), PutMode::Update(ver))
        .await
    {
        Ok(_) => push(
            "if_match_deleted_key",
            V::Fail,
            "If-Match on a deleted key created the object".into(),
        ),
        Err(e) => match status(&e) {
            Some(404) | Some(412) => push(
                "if_match_deleted_key",
                V::Pass,
                format!("{} (both 404 and 412 are acceptable)", desc(&e)),
            ),
            _ => push(
                "if_match_deleted_key",
                V::Warn,
                format!("unexpected: {}", desc(&e)),
            ),
        },
    }
    let _ = c.delete(&k).await;
}

// ------------------------------------------------- semantics: concurrent races

#[derive(Default, Debug)]
struct Round {
    winners: usize,
    lost: usize,
    busy: usize,
    other: Vec<String>,
}

async fn racer(c: Arc<Ctx>, key: Path, body: Bytes, mode: PutMode, gate: Arc<Barrier>) -> Round {
    let mut r = Round::default();
    gate.wait().await;
    for attempt in 0..6 {
        match c.put(&key, body.clone(), mode.clone()).await {
            Ok(_) => {
                r.winners += 1;
                break;
            }
            Err(e) => match status(&e) {
                Some(412) | Some(304) => {
                    r.lost += 1;
                    break;
                }
                Some(409) => {
                    r.busy += 1;
                    tokio::time::sleep(Duration::from_millis(20 * (attempt + 1))).await;
                }
                _ => {
                    r.other.push(desc(&e));
                    break;
                }
            },
        }
    }
    r
}

async fn sem_create_race(c: &Arc<Ctx>, rounds: usize, out: &mut Vec<Sem>) {
    let (mut ok_rounds, mut lost, mut busy) = (0usize, 0usize, 0usize);
    let mut bad: Vec<String> = vec![];
    for r in 0..rounds {
        let k = c.key(&format!("sem/create-race/{r}"));
        let gate = Arc::new(Barrier::new(RACERS));
        let hs: Vec<_> = (0..RACERS)
            .map(|i| {
                tokio::spawn(racer(
                    c.clone(),
                    k.clone(),
                    uniform(1024, i as u8),
                    PutMode::Create,
                    gate.clone(),
                ))
            })
            .collect();
        let mut tot = Round::default();
        for h in hs {
            let x = h.await.unwrap();
            tot.winners += x.winners;
            tot.lost += x.lost;
            tot.busy += x.busy;
            tot.other.extend(x.other);
        }
        let final_ok = match c.get(&k).await {
            Ok(b) => b.len() == 1024 && b.iter().all(|x| *x == b[0]),
            Err(_) => false,
        };
        if tot.winners == 1 && tot.other.is_empty() && final_ok {
            ok_rounds += 1;
        } else {
            bad.push(format!(
                "round {r}: winners={} other={:?} intact={final_ok}",
                tot.winners, tot.other
            ));
        }
        lost += tot.lost;
        busy += tot.busy;
    }
    let v = if ok_rounds == rounds {
        V::Pass
    } else {
        V::Fail
    };
    let mut d = format!("{ok_rounds}/{rounds} rounds with exactly one winner; losers 412={lost}, 409 retries={busy}");
    if !bad.is_empty() {
        d.push_str(&format!("; first failures: {:?}", &bad[..bad.len().min(3)]));
    }
    out.push(Sem {
        test: "concurrent_create_x8".into(),
        v,
        detail: d,
    });
}

fn torn(b: &Bytes, len: usize, allowed: &[u8]) -> bool {
    b.len() != len || !b.iter().all(|x| *x == b[0]) || !allowed.contains(&b[0])
}

async fn sem_swap_race(c: &Arc<Ctx>, rounds: usize, out: &mut Vec<Sem>) {
    const LEN: usize = 64 * 1024;
    let k = c.key("sem/swap-race");
    let (mut ok_rounds, mut lost, mut busy, mut reads, mut torn_reads) =
        (0usize, 0usize, 0usize, 0usize, 0usize);
    let mut bad: Vec<String> = vec![];
    let allowed: Vec<u8> = (0..RACERS as u8).chain([0xB0]).collect();
    for r in 0..rounds {
        let base = match c.put(&k, uniform(LEN, 0xB0), PutMode::Overwrite).await {
            Ok(b) => b,
            Err(e) => {
                out.push(Sem {
                    test: "concurrent_swap_x8".into(),
                    v: V::Fail,
                    detail: format!("base put failed: {}", desc(&e)),
                });
                return;
            }
        };
        let mode = PutMode::Update(UpdateVersion {
            e_tag: base.e_tag.clone(),
            version: base.version.clone(),
        });
        let gate = Arc::new(Barrier::new(RACERS));
        let done = Arc::new(AtomicBool::new(false));
        let readers: Vec<_> = (0..2)
            .map(|_| {
                let (c2, k2, d2, al) = (c.clone(), k.clone(), done.clone(), allowed.clone());
                tokio::spawn(async move {
                    let (mut n, mut t) = (0usize, 0usize);
                    while !d2.load(Ordering::Relaxed) && n < 6 {
                        if let Ok(b) = c2.get(&k2).await {
                            n += 1;
                            if torn(&b, LEN, &al) {
                                t += 1;
                            }
                        }
                    }
                    (n, t)
                })
            })
            .collect();
        let hs: Vec<_> = (0..RACERS)
            .map(|i| {
                tokio::spawn(racer(
                    c.clone(),
                    k.clone(),
                    uniform(LEN, i as u8),
                    mode.clone(),
                    gate.clone(),
                ))
            })
            .collect();
        let mut tot = Round::default();
        for h in hs {
            let x = h.await.unwrap();
            tot.winners += x.winners;
            tot.lost += x.lost;
            tot.busy += x.busy;
            tot.other.extend(x.other);
        }
        done.store(true, Ordering::Relaxed);
        for h in readers {
            let (n, t) = h.await.unwrap();
            reads += n;
            torn_reads += t;
        }
        let end = c.get(&k).await.ok();
        let end_ok = end
            .as_ref()
            .is_some_and(|b| !torn(b, LEN, &allowed) && b[0] != 0xB0);
        if tot.winners == 1 && tot.other.is_empty() && end_ok {
            ok_rounds += 1;
        } else {
            bad.push(format!(
                "round {r}: winners={} other={:?} end_ok={end_ok}",
                tot.winners, tot.other
            ));
        }
        lost += tot.lost;
        busy += tot.busy;
    }
    let v = if ok_rounds == rounds && torn_reads == 0 {
        V::Pass
    } else {
        V::Fail
    };
    let mut d =
        format!("{ok_rounds}/{rounds} rounds with exactly one winner; losers 412={lost}, 409 retries={busy}; {reads} concurrent reads, {torn_reads} torn");
    if !bad.is_empty() {
        d.push_str(&format!("; first failures: {:?}", &bad[..bad.len().min(3)]));
    }
    out.push(Sem {
        test: "concurrent_swap_x8".into(),
        v,
        detail: d,
    });
}

// --------------------------------------- semantics: ranges, consistency, misc

async fn sem_misc(c: &Ctx, out: &mut Vec<Sem>) {
    // ranged GET
    let k = c.key("sem/range");
    let body = payload(1 << 20, 7);
    let mut push = |t: &str, v: V, d: String| {
        out.push(Sem {
            test: t.into(),
            v,
            detail: d,
        })
    };
    if c.put(&k, body.clone(), PutMode::Overwrite).await.is_ok() {
        c.meter.reqs.fetch_add(3, Ordering::Relaxed);
        let b = c
            .store
            .get_opts(
                &k,
                GetOptions {
                    range: Some(GetRange::Bounded(100..4196)),
                    ..Default::default()
                },
            )
            .await;
        let b = match b {
            Ok(r) => r.bytes().await.ok(),
            Err(_) => None,
        };
        push(
            "range_bounded",
            if b.as_deref() == Some(&body[100..4196]) {
                V::Pass
            } else {
                V::Fail
            },
            "bytes=100-4195".into(),
        );
        let s = c
            .store
            .get_opts(
                &k,
                GetOptions {
                    range: Some(GetRange::Suffix(777)),
                    ..Default::default()
                },
            )
            .await;
        let s = match s {
            Ok(r) => r.bytes().await.ok(),
            Err(_) => None,
        };
        push(
            "range_suffix",
            if s.as_deref() == Some(&body[body.len() - 777..]) {
                V::Pass
            } else {
                V::Fail
            },
            "bytes=-777".into(),
        );
        let o = c
            .store
            .get_opts(
                &k,
                GetOptions {
                    range: Some(GetRange::Offset(1_000_000)),
                    ..Default::default()
                },
            )
            .await;
        let o = match o {
            Ok(r) => r.bytes().await.ok(),
            Err(_) => None,
        };
        push(
            "range_offset",
            if o.as_deref() == Some(&body[1_000_000..]) {
                V::Pass
            } else {
                V::Fail
            },
            "bytes=1000000-".into(),
        );
    } else {
        push("range_bounded", V::Fail, "setup put failed".into());
    }

    // read-your-write: PUT then GET, with an overwrite in between
    let n = 50;
    let (mut stale, mut err) = (0, 0);
    for i in 0..n {
        let k = c.key(&format!("sem/ryw-get/{i}"));
        let (a, b) = (uniform(2048, 1), uniform(2048, 2));
        if c.put(&k, a, PutMode::Overwrite).await.is_err()
            || c.put(&k, b.clone(), PutMode::Overwrite).await.is_err()
        {
            err += 1;
            continue;
        }
        match c.get(&k).await {
            Ok(g) if g == b => {}
            Ok(_) => stale += 1,
            Err(_) => err += 1,
        }
    }
    push(
        "read_your_write_get",
        if stale == 0 && err == 0 {
            V::Pass
        } else {
            V::Fail
        },
        format!("{n} PUT,PUT,GET sequences: stale={stale} errors={err}"),
    );

    // read-your-write: PUT then LIST, DELETE then LIST
    let (mut missing, mut lingering) = (0, 0);
    for i in 0..n {
        let dir = format!("sem/ryw-list/{i}");
        let k = c.key(&format!("{dir}/obj"));
        if c.put(&k, uniform(512, 3), PutMode::Overwrite)
            .await
            .is_err()
        {
            missing += 1;
            continue;
        }
        if c.list_count(&dir).await.unwrap_or(0) != 1 {
            missing += 1;
        }
        let _ = c.delete(&k).await;
        if i < 20 && c.list_count(&dir).await.unwrap_or(1) != 0 {
            lingering += 1;
        }
    }
    push(
        "read_your_write_list",
        if missing == 0 && lingering == 0 {
            V::Pass
        } else {
            V::Fail
        },
        format!(
            "{n} PUT-then-LIST: missing={missing}; 20 DELETE-then-LIST: still listed={lingering}"
        ),
    );

    // multipart create/abort (a probe, not a transfer)
    let k = c.key("sem/multipart");
    c.meter.reqs.fetch_add(3, Ordering::Relaxed);
    match c.store.put_multipart(&k).await {
        Ok(mut up) => match up.abort().await {
            Ok(()) => {
                let gone = matches!(c.head(&k).await, Err(object_store::Error::NotFound { .. }));
                push(
                    "multipart_create_abort",
                    if gone { V::Pass } else { V::Warn },
                    format!("abort ok; object absent={gone}"),
                );
            }
            Err(e) => push(
                "multipart_create_abort",
                V::Fail,
                format!("abort failed: {}", desc(&e)),
            ),
        },
        Err(e) => push(
            "multipart_create_abort",
            if status(&e) == Some(501) {
                V::Unsupported
            } else {
                V::Fail
            },
            desc(&e),
        ),
    }

    // clock skew: Last-Modified of a fresh object against the local clock.
    let k = c.key("sem/skew");
    let before = now_unix();
    if c.put(&k, uniform(16, 4), PutMode::Overwrite).await.is_ok() {
        let after = now_unix();
        if let Ok(m) = c.head(&k).await {
            let lm = m.last_modified.timestamp();
            let skew = if lm < before {
                lm - before
            } else if lm > after {
                lm - after
            } else {
                0
            };
            push(
                "clock_skew",
                if skew.abs() > 2 { V::Warn } else { V::Pass },
                format!("Last-Modified minus local clock window: {skew} s (flag > 2 s; 1 s resolution; Date header not exposed by object_store)"),
            );
        }
    }
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

// ------------------------------------------------------------------- latency

async fn lat_seq(c: &Ctx, size: usize, n_put: usize, n_get: usize, tag: &str, lat: &mut Vec<Lat>) {
    let est_bytes = ((n_put + n_get) * size) as u64;
    if !c.allow((n_put + n_get) as u64, est_bytes) {
        lat.push(Lat {
            section: format!("{tag}_skipped_cap"),
            size: size as u64,
            ..Default::default()
        });
        return;
    }
    let body = payload(size, size as u64);
    let mut put_ms = vec![];
    for i in 0..n_put {
        let k = c.key(&format!("lat/{size}/{i}"));
        let t = Instant::now();
        if let Err(e) = c.put(&k, body.clone(), PutMode::Overwrite).await {
            lat.push(Lat {
                section: format!("seq_put_{tag}_error"),
                size: size as u64,
                detail: desc(&e),
                ..Default::default()
            });
            return;
        }
        put_ms.push(t.elapsed().as_secs_f64() * 1e3);
    }
    let mut r = lat_row(
        &format!("seq_put_{tag}"),
        size as u64,
        n_put,
        put_ms,
        vec![],
    );
    r.mbs = size as f64 / 1e6 / (r.p50 / 1e3);
    lat.push(r);
    let (mut get_ms, mut ttfb) = (vec![], vec![]);
    for i in 0..n_get {
        let k = c.key(&format!("lat/{size}/{i}"));
        c.meter.reqs.fetch_add(1, Ordering::Relaxed);
        let t = Instant::now();
        match c.store.get(&k).await {
            Ok(res) => {
                let h = t.elapsed().as_secs_f64() * 1e3;
                match res.bytes().await {
                    Ok(b) => {
                        c.meter.down.fetch_add(b.len() as u64, Ordering::Relaxed);
                        ttfb.push(h);
                        get_ms.push(t.elapsed().as_secs_f64() * 1e3);
                    }
                    Err(e) => {
                        lat.push(Lat {
                            section: format!("seq_get_{tag}_error"),
                            size: size as u64,
                            detail: desc(&e),
                            ..Default::default()
                        });
                        return;
                    }
                }
            }
            Err(e) => {
                lat.push(Lat {
                    section: format!("seq_get_{tag}_error"),
                    size: size as u64,
                    detail: desc(&e),
                    ..Default::default()
                });
                return;
            }
        }
    }
    let mut r = lat_row(&format!("seq_get_{tag}"), size as u64, n_get, get_ms, ttfb);
    r.mbs = size as f64 / 1e6 / (r.p50 / 1e3);
    lat.push(r);
}

async fn lat_parallel(c: &Arc<Ctx>, conc: usize, n: usize, lat: &mut Vec<Lat>) {
    const SZ: usize = 16 << 20;
    if !c.allow((2 * n) as u64, (2 * n * SZ) as u64) {
        lat.push(Lat {
            section: "par_skipped_cap".into(),
            size: SZ as u64,
            conc,
            ..Default::default()
        });
        return;
    }
    let body = payload(SZ, 99);
    let t = Instant::now();
    let res: Vec<bool> = stream::iter(0..n)
        .map(|i| {
            let (c2, b) = (c.clone(), body.clone());
            async move {
                c2.put(
                    &c2.key(&format!("lat/par/{conc}/{i}")),
                    b,
                    PutMode::Overwrite,
                )
                .await
                .is_ok()
            }
        })
        .buffer_unordered(conc)
        .collect()
        .await;
    let wall = t.elapsed().as_secs_f64();
    let okn = res.iter().filter(|x| **x).count();
    lat.push(Lat {
        section: "par_put".into(),
        size: SZ as u64,
        conc,
        n: okn,
        mbs: (okn * SZ) as f64 / 1e6 / wall,
        p50: wall * 1e3,
        detail: format!("wall_ms in p50 column; {okn}/{n} ok"),
        ..Default::default()
    });
    let t = Instant::now();
    let res: Vec<usize> = stream::iter(0..n)
        .map(|i| {
            let c2 = c.clone();
            async move {
                c2.get(&c2.key(&format!("lat/par/{conc}/{i}")))
                    .await
                    .map(|b| b.len())
                    .unwrap_or(0)
            }
        })
        .buffer_unordered(conc)
        .collect()
        .await;
    let wall = t.elapsed().as_secs_f64();
    let bytes: usize = res.iter().sum();
    lat.push(Lat {
        section: "par_get".into(),
        size: SZ as u64,
        conc,
        n: res.iter().filter(|x| **x == SZ).count(),
        mbs: bytes as f64 / 1e6 / wall,
        p50: wall * 1e3,
        detail: format!(
            "wall_ms in p50 column; {}/{n} ok",
            res.iter().filter(|x| **x == SZ).count()
        ),
        ..Default::default()
    });
    for i in 0..n {
        let _ = c.delete(&c.key(&format!("lat/par/{conc}/{i}"))).await;
    }
}

async fn lat_head_list(c: &Ctx, lat: &mut Vec<Lat>) {
    let k = c.key("lat/head-obj");
    let _ = c.put(&k, uniform(1024, 5), PutMode::Overwrite).await;
    let mut ms = vec![];
    for _ in 0..100 {
        let t = Instant::now();
        if c.head(&k).await.is_ok() {
            ms.push(t.elapsed().as_secs_f64() * 1e3);
        }
    }
    lat.push(lat_row("head", 1024, ms.len(), ms, vec![]));
    for i in 0..10 {
        let _ = c
            .put(
                &c.key(&format!("lat/list10/{i}")),
                uniform(64, 6),
                PutMode::Overwrite,
            )
            .await;
    }
    let mut ms = vec![];
    for _ in 0..50 {
        let t = Instant::now();
        if c.list_count("lat/list10").await.ok() == Some(10) {
            ms.push(t.elapsed().as_secs_f64() * 1e3);
        }
    }
    lat.push(lat_row("list_10_keys", 0, ms.len(), ms, vec![]));
}

fn endpoint_host(p: &Prov) -> (String, u16, bool) {
    let (url, https) = match &p.endpoint {
        Some(e) => (e.clone(), e.starts_with("https://")),
        None => (format!("https://s3.{}.amazonaws.com", p.region), true),
    };
    let rest = url
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    let hostport = rest.split('/').next().unwrap_or(rest);
    match hostport.rsplit_once(':') {
        Some((h, port)) if port.parse::<u16>().is_ok() => {
            (h.to_string(), port.parse().unwrap(), https)
        }
        _ => (hostport.to_string(), if https { 443 } else { 80 }, https),
    }
}

async fn net_rtt(p: &Prov, c: &Ctx) -> String {
    use std::net::ToSocketAddrs;
    let (host, port, https) = endpoint_host(p);
    let addr = match (host.as_str(), port)
        .to_socket_addrs()
        .ok()
        .and_then(|mut a| a.next())
    {
        Some(a) => a,
        None => return "tcp connect: unresolved".into(),
    };
    let mut tcp = vec![];
    for _ in 0..10 {
        let t = Instant::now();
        if std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(5)).is_ok() {
            tcp.push(t.elapsed().as_secs_f64() * 1e3);
        }
    }
    tcp.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let tcp50 = pct(&tcp, 0.5);
    // Warm HEAD (existing connection) vs cold HEAD on a fresh client: the cold-minus-warm
    // difference is connect + TLS + one RTT, so TLS ~ cold - warm - tcp. Approximate.
    let warm = {
        let k = c.key("lat/head-obj");
        let t = Instant::now();
        let _ = c.head(&k).await;
        t.elapsed().as_secs_f64() * 1e3
    };
    let cold = match build(p) {
        Ok(s2) => {
            let t = Instant::now();
            let _ = s2.head(&c.key("lat/head-obj")).await;
            c.meter.reqs.fetch_add(1, Ordering::Relaxed);
            t.elapsed().as_secs_f64() * 1e3
        }
        Err(_) => f64::NAN,
    };
    if https {
        format!("tcp connect p50 {tcp50:.2} ms (n={}); cold-HEAD {cold:.1} ms, warm-HEAD {warm:.1} ms, TLS+setup approx {:.1} ms", tcp.len(), cold - warm - tcp50)
    } else {
        format!("tcp connect p50 {tcp50:.2} ms (n={}); cold-HEAD {cold:.1} ms, warm-HEAD {warm:.1} ms; plain http, no TLS", tcp.len())
    }
}

// ------------------------------------------------------------------- driver

fn utc_stamp() -> String {
    let s = now_unix();
    let (days, rem) = (s.div_euclid(86400), s.rem_euclid(86400));
    // civil-from-days (Howard Hinnant)
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60
    )
}

fn run_id() -> String {
    let mut b = [0u8; 6];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        use std::io::Read;
        let _ = f.read_exact(&mut b);
    }
    format!(
        "{}-{}",
        now_unix(),
        b.iter().map(|x| format!("{x:02x}")).collect::<String>()
    )
}

struct Report {
    sem: Vec<Sem>,
    lat: Vec<Lat>,
    net: String,
    reqs: u64,
    bytes: u64,
    leftover: usize,
    started: String,
}

async fn run_all(store: Arc<dyn ObjectStore>, prov: Option<&Prov>) -> Report {
    let prefix = format!("lunix-p0-probe/{}", run_id());
    let c = Arc::new(Ctx {
        store,
        prefix: prefix.clone(),
        meter: Meter::default(),
        stop: AtomicBool::new(false),
    });
    let started = utc_stamp();
    let mut sem = vec![];
    let mut lat = vec![];
    // warm the connection pool so the first race is not dominated by connection set-up
    let _ = c
        .put(&c.key("sem/warm"), uniform(8, 0), PutMode::Overwrite)
        .await;
    let wk = c.key("sem/warm");
    let warm: Vec<_> = (0..RACERS).map(|_| c.head(&wk)).collect();
    let _ = futures::future::join_all(warm).await;

    sem_basic(&c, &mut sem).await;
    sem_create_race(&c, env_n("LUNIX_PROBE_CREATE_ROUNDS", 20), &mut sem).await;
    sem_swap_race(&c, env_n("LUNIX_PROBE_SWAP_ROUNDS", 100), &mut sem).await;
    sem_misc(&c, &mut sem).await;

    let n_small = env_n("LUNIX_PROBE_SMALL_N", 200);
    lat_seq(&c, 4096, n_small, n_small, "4k", &mut lat).await;
    lat_seq(&c, 65536, n_small, n_small, "64k", &mut lat).await;
    let (put16, get16) = (
        env_n("LUNIX_PROBE_SEQ16_PUT_N", 40),
        env_n("LUNIX_PROBE_SEQ16_GET_N", 16),
    );
    lat_seq(&c, 16 << 20, put16, get16, "16m", &mut lat).await;
    for i in 0..put16 {
        let _ = c.delete(&c.key(&format!("lat/{}/{i}", 16 << 20))).await;
    }
    for (conc, n) in [
        (1usize, env_n("LUNIX_PROBE_PAR1_N", 2)),
        (8, env_n("LUNIX_PROBE_PAR8_N", 8)),
        (32, env_n("LUNIX_PROBE_PAR32_N", 16)),
    ] {
        lat_parallel(&c, conc, n, &mut lat).await;
    }
    lat_head_list(&c, &mut lat).await;
    let net = match prov {
        Some(p) => net_rtt(p, &c).await,
        None => "n/a (in-memory)".into(),
    };

    // cleanup: delete only our prefix, then verify it is empty
    let p = Path::from(prefix.clone());
    let metas: Vec<_> = c
        .store
        .list(Some(&p))
        .filter_map(|m| async { m.ok() })
        .collect()
        .await;
    for m in &metas {
        let _ = c.store.delete(&m.location).await;
    }
    let leftover = c
        .store
        .list(Some(&p))
        .filter_map(|m| async { m.ok() })
        .count()
        .await;
    c.stop.store(true, Ordering::Relaxed);
    Report {
        sem,
        lat,
        net,
        reqs: c.meter.reqs.load(Ordering::Relaxed),
        bytes: c.meter.up.load(Ordering::Relaxed) + c.meter.down.load(Ordering::Relaxed),
        leftover,
        started,
    }
}

fn verdict(sem: &[Sem]) -> (&'static str, String) {
    let get = |t: &str| sem.iter().find(|s| s.test == t).map(|s| s.v);
    let need = [
        "create_if_absent_second_412",
        "if_match_swap_ok",
        "if_match_stale_412",
        "concurrent_create_x8",
        "concurrent_swap_x8",
        "read_your_write_get",
        "read_your_write_list",
    ];
    let failing: Vec<_> = need.iter().filter(|t| get(t) != Some(V::Pass)).collect();
    if failing.is_empty() {
        ("AUTHORITY-CAPABLE", "all authority-gate probes PASS".into())
    } else {
        (
            "MIRROR-ONLY",
            format!(
                "gate not met: {}",
                failing
                    .iter()
                    .map(|t| format!("{t}={}", get(t).map(|v| v.s()).unwrap_or("missing")))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        )
    }
}

fn csv_escape(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

fn render(name: &str, label: &str, r: &Report) -> (String, String) {
    let mut csv = String::from("section,test,size_bytes,concurrency,n,result,p50_ms,p90_ms,p99_ms,max_ms,ttfb_p50_ms,ttfb_p99_ms,agg_mb_s,detail\n");
    let mut txt = String::new();
    let (v, why) = verdict(&r.sem);
    writeln!(txt, "provider {name} ({label}), started {}", r.started).unwrap();
    writeln!(txt, "classification: {v} ({why})").unwrap();
    writeln!(
        txt,
        "requests: {}, bytes transferred: {} MB, leftover objects under probe prefix: {}",
        r.reqs,
        r.bytes / 1_000_000,
        r.leftover
    )
    .unwrap();
    writeln!(txt, "network: {}", r.net).unwrap();
    for s in &r.sem {
        writeln!(txt, "  [{:11}] {:30} {}", s.v.s(), s.test, s.detail).unwrap();
        writeln!(
            csv,
            "semantics,{},,,,{},,,,,,,,{}",
            s.test,
            s.v.s(),
            csv_escape(&s.detail)
        )
        .unwrap();
    }
    for l in &r.lat {
        writeln!(
            txt,
            "  {:22} size={:>9} conc={:>2} n={:>3} p50={:8.2} p90={:8.2} p99={:8.2} max={:8.2} ttfb50={:7.2} ttfb99={:7.2} MB/s={:8.1} {}",
            l.section, l.size, l.conc, l.n, l.p50, l.p90, l.p99, l.max, l.ttfb50, l.ttfb99, l.mbs, l.detail
        )
        .unwrap();
        writeln!(
            csv,
            "latency,{},{},{},{},,{:.3},{:.3},{:.3},{:.3},{:.3},{:.3},{:.2},{}",
            l.section,
            l.size,
            l.conc,
            l.n,
            l.p50,
            l.p90,
            l.p99,
            l.max,
            l.ttfb50,
            l.ttfb99,
            l.mbs,
            csv_escape(&l.detail)
        )
        .unwrap();
    }
    writeln!(
        csv,
        "meta,classification,,,,{v},,,,,,,,{}",
        csv_escape(&why)
    )
    .unwrap();
    writeln!(csv, "meta,network,,,,,,,,,,,,{}", csv_escape(&r.net)).unwrap();
    writeln!(
        csv,
        "meta,totals,,,,,,,,,,,,{}",
        csv_escape(&format!(
            "requests={} bytes={} leftover={} started={}",
            r.reqs, r.bytes, r.leftover, r.started
        ))
    )
    .unwrap();
    (csv, txt)
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let out_dir = args
        .iter()
        .position(|a| a == "--out")
        .and_then(|i| args.get(i + 1))
        .cloned()
        .unwrap_or_else(|| "out".into());
    let only: Option<Vec<String>> = args
        .iter()
        .position(|a| a == "--providers")
        .and_then(|i| args.get(i + 1))
        .map(|s| s.split(',').map(|x| x.trim().to_uppercase()).collect());
    let list_only = args.iter().any(|a| a == "--list");
    std::fs::create_dir_all(&out_dir).expect("create out dir");
    let mut status_lines = BTreeMap::new();
    for name in discover() {
        if only.as_ref().is_some_and(|o| !o.contains(&name)) {
            continue;
        }
        match load(&name) {
            Err(why) => {
                println!("{name}: {why}");
                status_lines.insert(name, why);
            }
            Ok(p) if list_only => println!("{name}: configured ({})", p.label),
            Ok(p) => {
                println!("{name}: running ({})", p.label);
                let store = match build(&p) {
                    Ok(s) => s,
                    Err(e) => {
                        println!("{name}: {e}");
                        continue;
                    }
                };
                let rep = run_all(store, Some(&p)).await;
                let (csv, txt) = render(&p.name, &p.label, &rep);
                print!("{txt}");
                std::fs::write(format!("{out_dir}/{}.csv", p.name.to_lowercase()), csv)
                    .expect("write csv");
                std::fs::write(format!("{out_dir}/{}.txt", p.name.to_lowercase()), txt)
                    .expect("write txt");
            }
        }
    }
    if !status_lines.is_empty() {
        let body: String = status_lines
            .iter()
            .map(|(k, v)| format!("{k}: {v}\n"))
            .collect();
        std::fs::write(format!("{out_dir}/not-run.txt"), body).expect("write not-run");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use object_store::memory::InMemory;

    /// The harness itself, against the in-memory store (which has exact conditional-write
    /// semantics): every authority-gate probe must PASS and nothing may be left behind.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn harness_passes_on_the_reference_store() {
        std::env::set_var("LUNIX_PROBE_SMALL_N", "5");
        std::env::set_var("LUNIX_PROBE_CREATE_ROUNDS", "3");
        std::env::set_var("LUNIX_PROBE_SWAP_ROUNDS", "5");
        std::env::set_var("LUNIX_PROBE_SEQ16_PUT_N", "2");
        std::env::set_var("LUNIX_PROBE_SEQ16_GET_N", "2");
        std::env::set_var("LUNIX_PROBE_PAR1_N", "1");
        std::env::set_var("LUNIX_PROBE_PAR8_N", "2");
        std::env::set_var("LUNIX_PROBE_PAR32_N", "2");
        let rep = run_all(Arc::new(InMemory::new()), None).await;
        let (v, why) = verdict(&rep.sem);
        assert_eq!(
            v,
            "AUTHORITY-CAPABLE",
            "{why}; {:?}",
            rep.sem
                .iter()
                .map(|s| (&s.test, s.v, &s.detail))
                .collect::<Vec<_>>()
        );
        assert_eq!(rep.leftover, 0);
    }

    #[test]
    fn missing_credentials_are_reported_not_run() {
        let e = load("NOSUCHPROVIDER").err().expect("must be skipped");
        assert!(e.starts_with("not run: no credentials"));
    }

    #[test]
    fn status_text_parsing() {
        assert_eq!(
            status_in("Generic error, status code: 409 Conflict, body"),
            Some(409)
        );
        assert_eq!(status_in("nothing here"), None);
    }
}
