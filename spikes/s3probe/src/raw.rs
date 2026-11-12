//! Minimal hand-signed (SigV4) S3 client for the requests `object_store` cannot express:
//! a conditional `CompleteMultipartUpload` (`If-None-Match: *`), and `ListMultipartUploads`
//! under the probe prefix (to prove that no upload is left open). Credentials stay in memory.

use hmac::{Hmac, Mac};
use bytes::Bytes;
use sha2::{Digest, Sha256};
use std::time::{Duration, Instant};

type HmacSha256 = Hmac<Sha256>;

pub struct Raw {
    http: reqwest::Client,
    base: String, // scheme://host[:port], no trailing slash
    path_prefix: String, // "/bucket" for path style, "" for virtual-hosted
    region: String,
    ak: String,
    sk: String,
    token: Option<String>,
    /// send `UNSIGNED-PAYLOAD` instead of hashing bodies over 4 KiB (what object_store does over https)
    pub unsigned: bool,
}

pub struct Resp {
    pub status: u16,
    pub etag: Option<String>,
    pub body: String,
    pub ms: f64,
}

impl Resp {
    /// An S3 error can hide in a 200 body of CompleteMultipartUpload.
    pub fn effective_status(&self) -> u16 {
        if self.status == 200 && self.body.contains("<Error>") {
            return 500;
        }
        self.status
    }
    pub fn code(&self) -> String {
        tag(&self.body, "Code").unwrap_or_default()
    }
}

pub fn tag(body: &str, name: &str) -> Option<String> {
    let open = format!("<{name}>");
    let a = body.find(&open)? + open.len();
    let b = body[a..].find(&format!("</{name}>"))? + a;
    Some(body[a..b].to_string())
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn sha_hex(b: &[u8]) -> String {
    hex(&Sha256::digest(b))
}

fn hmac(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut m = HmacSha256::new_from_slice(key).expect("hmac key");
    m.update(data);
    m.finalize().into_bytes().to_vec()
}

fn enc(s: &str, keep_slash: bool) -> String {
    let mut o = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => o.push(b as char),
            b'/' if keep_slash => o.push('/'),
            _ => o.push_str(&format!("%{b:02X}")),
        }
    }
    o
}

/// (YYYYMMDD, YYYYMMDDTHHMMSSZ) for unix seconds.
fn amz_stamp(secs: i64) -> (String, String) {
    let (days, rem) = (secs.div_euclid(86400), secs.rem_euclid(86400));
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
    let date = format!("{y:04}{m:02}{d:02}");
    let dt = format!(
        "{date}T{:02}{:02}{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    );
    (date, dt)
}

impl Raw {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        endpoint: Option<&str>,
        bucket: &str,
        region: &str,
        ak: &str,
        sk: &str,
        token: Option<&str>,
    ) -> Result<Raw, String> {
        let (base, path_prefix) = match endpoint {
            Some(e) => (e.trim_end_matches('/').to_string(), format!("/{bucket}")),
            None => (
                format!("https://{bucket}.s3.{region}.amazonaws.com"),
                String::new(),
            ),
        };
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .connect_timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| format!("raw client: {e}"))?;
        Ok(Raw {
            http,
            base,
            path_prefix,
            region: region.into(),
            ak: ak.into(),
            sk: sk.into(),
            token: token.map(String::from),
            unsigned: false,
        })
    }

    /// One signed request. `key` is the full object key ("" for bucket-level requests).
    pub async fn send(
        &self,
        method: &str,
        key: &str,
        query: &[(&str, &str)],
        extra: &[(&str, &str)],
        body: Vec<u8>,
    ) -> Result<Resp, String> {
        self.send_bytes(method, key, query, extra, Bytes::from(body)).await
    }

    pub async fn send_bytes(
        &self,
        method: &str,
        key: &str,
        query: &[(&str, &str)],
        extra: &[(&str, &str)],
        body: Bytes,
    ) -> Result<Resp, String> {
        let path = format!("{}/{}", self.path_prefix, key);
        let path = if path == "/" { "/".to_string() } else { path };
        let cpath = enc(&path, true);
        let mut q: Vec<(String, String)> = query
            .iter()
            .map(|(k, v)| (enc(k, false), enc(v, false)))
            .collect();
        q.sort();
        let cq = q
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("&");
        let url = format!("{}{}{}{}", self.base, cpath, if cq.is_empty() { "" } else { "?" }, cq);
        let host = self
            .base
            .trim_start_matches("https://")
            .trim_start_matches("http://")
            .to_string();
        let (date, dt) = amz_stamp(crate::now_unix());
        let phash = if self.unsigned && body.len() > 4096 {
            "UNSIGNED-PAYLOAD".to_string()
        } else {
            sha_hex(&body)
        };
        let mut hs: Vec<(String, String)> = vec![
            ("host".into(), host),
            ("x-amz-content-sha256".into(), phash.clone()),
            ("x-amz-date".into(), dt.clone()),
        ];
        if let Some(t) = &self.token {
            hs.push(("x-amz-security-token".into(), t.clone()));
        }
        for (k, v) in extra {
            hs.push((k.to_lowercase(), v.to_string()));
        }
        hs.sort();
        let chead: String = hs.iter().map(|(k, v)| format!("{k}:{}\n", v.trim())).collect();
        let signed = hs.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>().join(";");
        let creq = format!("{method}\n{cpath}\n{cq}\n{chead}\n{signed}\n{phash}");
        let scope = format!("{date}/{}/s3/aws4_request", self.region);
        let sts = format!("AWS4-HMAC-SHA256\n{dt}\n{scope}\n{}", sha_hex(creq.as_bytes()));
        let k = hmac(format!("AWS4{}", self.sk).as_bytes(), date.as_bytes());
        let k = hmac(&k, self.region.as_bytes());
        let k = hmac(&k, b"s3");
        let k = hmac(&k, b"aws4_request");
        let sig = hex(&hmac(&k, sts.as_bytes()));
        let auth = format!(
            "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed}, Signature={sig}",
            self.ak
        );
        let m = reqwest::Method::from_bytes(method.as_bytes()).map_err(|e| e.to_string())?;
        let mut rb = self.http.request(m, &url).header("authorization", auth);
        for (k, v) in &hs {
            if k != "host" {
                rb = rb.header(k.as_str(), v.as_str());
            }
        }
        let t = Instant::now();
        let r = rb.body(body).send().await.map_err(|e| format!("send: {}", e.without_url()))?;
        let status = r.status().as_u16();
        let etag = r
            .headers()
            .get("etag")
            .and_then(|v| v.to_str().ok())
            .map(String::from);
        let body = r.text().await.map_err(|e| format!("body: {}", e.without_url()))?;
        Ok(Resp {
            status,
            etag,
            body,
            ms: t.elapsed().as_secs_f64() * 1e3,
        })
    }

    pub async fn create(&self, key: &str) -> Result<(String, f64), String> {
        let r = self.send("POST", key, &[("uploads", "")], &[], vec![]).await?;
        match (r.status, tag(&r.body, "UploadId")) {
            (200, Some(id)) => Ok((id, r.ms)),
            _ => Err(format!("create status {} {}", r.status, r.code())),
        }
    }

    pub async fn part(
        &self,
        key: &str,
        id: &str,
        n: u32,
        data: Vec<u8>,
    ) -> Result<(String, f64), String> {
        self.part_b(key, id, n, Bytes::from(data)).await
    }

    pub async fn part_b(
        &self,
        key: &str,
        id: &str,
        n: u32,
        data: Bytes,
    ) -> Result<(String, f64), String> {
        let ns = n.to_string();
        let r = self
            .send_bytes("PUT", key, &[("partNumber", &ns), ("uploadId", id)], &[], data)
            .await?;
        match (r.status, r.etag.clone()) {
            (200, Some(e)) => Ok((e, r.ms)),
            _ => Err(format!("part status {} {}", r.status, r.code())),
        }
    }

    pub async fn complete(
        &self,
        key: &str,
        id: &str,
        etags: &[String],
        if_none_match: bool,
    ) -> Result<Resp, String> {
        let mut x = String::from("<CompleteMultipartUpload>");
        for (i, e) in etags.iter().enumerate() {
            x.push_str(&format!("<Part><PartNumber>{}</PartNumber><ETag>{}</ETag></Part>", i + 1, e.replace('"', "&quot;")));
        }
        x.push_str("</CompleteMultipartUpload>");
        let extra: &[(&str, &str)] = if if_none_match { &[("If-None-Match", "*")] } else { &[] };
        self.send("POST", key, &[("uploadId", id)], extra, x.into_bytes()).await
    }

    pub async fn put_bytes(&self, key: &str, data: Bytes) -> Result<Resp, String> {
        self.send_bytes("PUT", key, &[], &[], data).await
    }

    pub async fn abort(&self, key: &str, id: &str) -> Result<u16, String> {
        let r = self.send("DELETE", key, &[("uploadId", id)], &[], vec![]).await?;
        Ok(r.status)
    }

    /// Open multipart uploads whose key starts with `prefix` as (key, upload id).
    pub async fn list_uploads(&self, prefix: &str) -> Result<Vec<(String, String)>, String> {
        let r = self
            .send("GET", "", &[("uploads", ""), ("prefix", prefix)], &[], vec![])
            .await?;
        if r.status != 200 {
            return Err(format!("list uploads status {} {}", r.status, r.code()));
        }
        let mut out = vec![];
        let mut rest = r.body.as_str();
        while let Some(i) = rest.find("<Upload>") {
            let seg_end = rest[i..].find("</Upload>").map(|e| e + i).unwrap_or(rest.len());
            let seg = &rest[i..seg_end];
            if let (Some(k), Some(u)) = (tag(seg, "Key"), tag(seg, "UploadId")) {
                out.push((k, u));
            }
            rest = &rest[seg_end..];
        }
        if r.body.contains("<IsTruncated>true</IsTruncated>") {
            return Err("list uploads truncated (more than a page open)".into());
        }
        Ok(out)
    }
}
