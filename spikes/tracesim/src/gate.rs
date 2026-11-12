//! Applies the plan's fixed decision rules (ADR-5, ADR-14) to the CSVs
//! written by the `chunk` and `cache` commands. Thresholds are the plan's.
use crate::ds::*;
use std::collections::BTreeMap;
use std::fmt::Write;

type Row = FxMap<String, String>;

fn read_all(dir: &str, prefix: &str) -> Vec<Row> {
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .map(|d| d.filter_map(|e| e.ok()).map(|e| e.path()).collect())
        .unwrap_or_default();
    files.sort();
    let mut out = Vec::new();
    for p in files {
        let n = p.file_name().unwrap().to_string_lossy().to_string();
        if !n.starts_with(prefix) || !n.ends_with(".csv") {
            continue;
        }
        let txt = std::fs::read_to_string(&p).unwrap_or_default();
        let mut it = txt.lines();
        let hdr: Vec<String> = match it.next() {
            Some(h) => h.split(',').map(String::from).collect(),
            None => continue,
        };
        for l in it {
            let f: Vec<&str> = l.split(',').collect();
            if f.len() == hdr.len() {
                out.push(hdr.iter().cloned().zip(f.iter().map(|x| x.to_string())).collect());
            }
        }
    }
    out
}
fn num(r: &Row, k: &str) -> f64 {
    r.get(k).and_then(|v| v.parse().ok()).unwrap_or(0.0)
}
fn key(r: &Row) -> String {
    format!("{}/{}", r["set"], r["vol"])
}

pub fn gate_chunk(dir: &str) -> String {
    let rmw = read_all(dir, "chunk_rmw.");
    let map = read_all(dir, "chunk_map.");
    let com = read_all(dir, "chunk_commit.");
    let mut o = String::new();
    if rmw.is_empty() || com.is_empty() {
        return "ADR-5 gate: NOT RUN (no chunk CSVs in the directory)\n".into();
    }
    let sizes = [16384u64, 65536, 262144, 1 << 20];
    let _ = writeln!(o, "volumes: {}", rmw.iter().map(key).collect::<std::collections::BTreeSet<_>>().len());
    let _ = writeln!(o, "\n| T(s) | C(KiB) | S3 p50 | S3 p90 | S3 mean | RMW/1000w p50 | RMW p90 | MAP MiB/TiB touched p50 | mapWA p90-of-cuts: p50 vols | p90 vols |");
    let _ = writeln!(o, "|---|---|---|---|---|---|---|---|---|---|");
    // verdict inputs per T: [C] -> (S3 p50,p90, rmw p50,p90, mapwa p90 over vols)
    let mut verdict: BTreeMap<u64, String> = BTreeMap::new();
    for t in [10u64, 60, 300] {
        let mut st: BTreeMap<u64, [f64; 6]> = BTreeMap::new();
        for &c in &sizes {
            let mut s3 = Vec::new();
            let mut rm = Vec::new();
            let mut mp = Vec::new();
            let mut wa = Vec::new();
            for r in com.iter().filter(|r| num(r, "chunk") as u64 == c && num(r, "interval_s") as u64 == t && num(r, "fanout") as u64 == 512) {
                let lg = num(r, "logical_bytes") / (1u64 << 30) as f64;
                if lg > 0.0 {
                    s3.push((num(r, "data_r05") + num(r, "map_r05")) / (1u64 << 30) as f64 / lg);
                    wa.push(num(r, "mapwa_r05_p90"));
                }
            }
            for r in rmw.iter().filter(|r| num(r, "chunk") as u64 == c) {
                let w = num(r, "writes");
                if w > 0.0 {
                    rm.push(num(r, "rmw_lru10") * 1000.0 / w);
                }
            }
            for r in map.iter().filter(|r| num(r, "chunk") as u64 == c && num(r, "fanout") as u64 == 512) {
                mp.push(num(r, "mib_per_tib_touched_half"));
            }
            let v = [pct(&s3, 0.5), pct(&s3, 0.9), pct(&rm, 0.5), pct(&rm, 0.9), pct(&wa, 0.5), pct(&wa, 0.9)];
            let _ = writeln!(o, "| {t} | {} | {:.4} | {:.4} | {:.4} | {:.3} | {:.3} | {:.0} | {:.4} | {:.4} |", c >> 10, v[0], v[1], mean(&s3), v[2], v[3], pct(&mp, 0.5), v[4], v[5]);
            st.insert(c, v);
        }
        let a = |c: u64| st[&c];
        let min50 = [16384u64, 65536, 262144].iter().map(|&c| a(c)[0]).fold(f64::MAX, f64::min);
        let min90 = [16384u64, 65536, 262144].iter().map(|&c| a(c)[1]).fold(f64::MAX, f64::min);
        let s3ok = a(65536)[0] <= 1.25 * min50 && a(65536)[1] <= 1.25 * min90;
        let rmwok = a(65536)[2] <= 2.0 * a(16384)[2] && a(65536)[3] <= 2.0 * a(16384)[3];
        let waok = a(65536)[5] <= 0.25;
        let to16 = a(16384)[0] <= 0.75 * a(65536)[0] && a(16384)[1] <= 0.75 * a(65536)[1];
        let to256 = a(262144)[0] <= 0.75 * a(65536)[0] && a(262144)[1] <= 0.75 * a(65536)[1] && a(262144)[2] <= 1.5 * a(65536)[2] && a(262144)[3] <= 1.5 * a(65536)[3];
        let v = if s3ok && rmwok && waok {
            "64 KiB confirmed"
        } else if to16 {
            "amend to 16 KiB"
        } else if to256 {
            "amend to 256 KiB"
        } else {
            "keep 64 KiB (no amendment margin met, confirmation clauses not all met)"
        };
        verdict.insert(t, format!("{v} [S3<=1.25*min: {s3ok}, RMW<=2*RMW16: {rmwok}, mapWA p90<=25%: {waok}]"));
    }
    let _ = writeln!(o, "\nInformational only (NOT the plan's rule): S3(C) at T=60 s with the map bytes scaled by 0.46 (DESIGN's run-encoded leaves, 352/768 MiB per TiB):");
    for &c in &sizes {
        let v: Vec<f64> = com.iter().filter(|r| num(r, "chunk") as u64 == c && num(r, "interval_s") as u64 == 60 && num(r, "fanout") as u64 == 512 && num(r, "logical_bytes") > 0.0)
            .map(|r| (num(r, "data_r05") + 0.46 * num(r, "map_r05")) / num(r, "logical_bytes")).collect();
        let _ = writeln!(o, "  C={} KiB: S3 p50 {:.4} p90 {:.4}", c >> 10, pct(&v, 0.5) * (1u64 << 30) as f64 / (1u64 << 30) as f64, pct(&v, 0.9));
    }
    let _ = writeln!(o, "\nADR-5 verdict at T=60 s (primary): {}", verdict[&60]);
    let _ = writeln!(o, "sign check T=10 s: {}", verdict[&10]);
    let _ = writeln!(o, "sign check T=300 s: {}", verdict[&300]);
    o
}

pub fn gate_cache(dir: &str) -> String {
    let rows = read_all(dir, "cache_results.");
    let scan = read_all(dir, "cache_scan.");
    let mut o = String::new();
    if rows.is_empty() {
        return "ADR-14 gate: NOT RUN (no cache CSVs in the directory)\n".into();
    }
    let chunk = 65536.0;
    let chunk16 = 16384.0;
    let _ = writeln!(o, "| size (per mille of WS) | LRU med | S3-FIFO med | SIEVE med | W-TinyLFU med | S3-FIFO loses to LRU on |");
    let _ = writeln!(o, "|---|---|---|---|---|---|");
    let mut med: BTreeMap<(u32, &str), f64> = BTreeMap::new();
    let mut loses: BTreeMap<u32, f64> = BTreeMap::new();
    for pm in [5u32, 10, 20, 50, 100, 250] {
        let mut per: BTreeMap<&str, BTreeMap<String, f64>> = BTreeMap::new();
        for p in crate::policies::POLICIES {
            let m: BTreeMap<String, f64> = rows.iter().filter(|r| r["policy"] == p && num(r, "frac_pm") as u32 == pm && num(r, "chunk") == chunk).map(|r| (key(r), num(r, "miss_req"))).collect();
            per.insert(p, m);
        }
        let mut vals = vec![];
        for p in crate::policies::POLICIES {
            let v: Vec<f64> = per[p].values().copied().collect();
            med.insert((pm, p), pct(&v, 0.5));
            vals.push(pct(&v, 0.5));
        }
        let n = per["lru"].len().max(1);
        let l = per["s3fifo"].iter().filter(|(k, v)| per["lru"].get(*k).map_or(false, |x| **v > *x)).count();
        loses.insert(pm, l as f64 / n as f64);
        let _ = writeln!(o, "| {} | {:.4} | {:.4} | {:.4} | {:.4} | {:.0}% of {} vols |", pm, vals[0], vals[1], vals[2], vals[3], 100.0 * l as f64 / n as f64, n);
    }
    let _ = writeln!(o, "\nInformational: 16 KiB chunk cache check (median request miss ratio over volumes):");
    for pm in [10u32, 50, 100] {
        let line: Vec<String> = crate::policies::POLICIES.iter().map(|p| {
            let v: Vec<f64> = rows.iter().filter(|r| r["policy"] == *p && num(r, "chunk") == chunk16 && num(r, "frac_pm") as u32 == pm).map(|r| num(r, "miss_req")).collect();
            format!("{p} {:.4}", pct(&v, 0.5))
        }).collect();
        let _ = writeln!(o, "  {pm} per mille: {}", line.join(", "));
    }
    let sizes = [10u32, 50, 100];
    let acc = sizes.iter().all(|&pm| {
        let best = crate::policies::POLICIES.iter().map(|p| med[&(pm, *p)]).fold(f64::MAX, f64::min);
        med[&(pm, "s3fifo")] <= 1.05 * best && med[&(pm, "s3fifo")] <= med[&(pm, "lru")] && loses[&pm] < 0.25
    });
    let sieve = sizes.iter().filter(|&&pm| med[&(pm, "sieve")] <= 0.95 * med[&(pm, "s3fifo")]).count() >= 2;
    let wtl = sizes.iter().all(|&pm| med[&(pm, "wtinylfu")] <= 0.90 * med[&(pm, "s3fifo")]);
    // scan: mean over scan volumes of (S3-FIFO - LRU) miss ratio, per size
    let mut scan_fail = false;
    let _ = writeln!(o, "\nScan test (mean over scan volumes of S3-FIFO minus LRU miss ratio, absolute):");
    for pm in [5u32, 10, 20, 50, 100, 250] {
        let get = |p: &str| -> BTreeMap<String, f64> { scan.iter().filter(|r| r["policy"] == p && num(r, "chunk") == 65536.0 && num(r, "frac_pm") as u32 == pm).map(|r| (key(r), num(r, "miss_req"))).collect() };
        let (s, l) = (get("s3fifo"), get("lru"));
        let d: Vec<f64> = s.iter().filter_map(|(k, v)| l.get(k).map(|x| v - x)).collect();
        if !d.is_empty() {
            let m = mean(&d);
            if m > 0.02 {
                scan_fail = true;
            }
            let _ = writeln!(o, "  {pm} per mille: {:+.4} over {} volumes (worst {:+.4})", m, d.len(), d.iter().cloned().fold(f64::MIN, f64::max));
        }
    }
    // Per-test reading of the scan rule: any single (volume, size) where S3-FIFO is > 2 % abs worse than LRU.
    let mut cases = Vec::new();
    for r in scan.iter().filter(|r| r["policy"] == "s3fifo" && num(r, "chunk") == chunk) {
        if let Some(l) = scan.iter().find(|x| x["policy"] == "lru" && x["vol"] == r["vol"] && x["set"] == r["set"] && x["frac_pm"] == r["frac_pm"] && num(x, "chunk") == chunk) {
            let d = num(r, "miss_req") - num(l, "miss_req");
            if d > 0.02 {
                cases.push(format!("{} @{}pm (+{:.4})", r["vol"], r["frac_pm"], d));
            }
        }
    }
    let _ = writeln!(o, "Per-test reading: {} scan tests with S3-FIFO > 2% abs worse than LRU: {}", cases.len(), cases.join("; "));
    let _ = writeln!(o, "Informational: share of volumes where S3-FIFO is worse than LRU by more than a margin (miss ratio, abs):");
    for pm in [10u32, 50, 100] {
        let get = |p: &str| -> BTreeMap<String, f64> { rows.iter().filter(|r| r["policy"] == p && num(r, "chunk") == chunk && num(r, "frac_pm") as u32 == pm).map(|r| (key(r), num(r, "miss_req"))).collect() };
        let (s, l) = (get("s3fifo"), get("lru"));
        let n = l.len().max(1) as f64;
        let f = |m: f64| s.iter().filter(|(k, v)| l.get(*k).map_or(false, |x| **v - *x > m)).count() as f64 / n;
        let _ = writeln!(o, "  {pm} per mille: >0: {:.0}%  >0.001: {:.0}%  >0.005: {:.0}%  >0.01: {:.0}%", 100.0 * f(0.0), 100.0 * f(0.001), 100.0 * f(0.005), 100.0 * f(0.01));
    }
    scan_fail |= !cases.is_empty();
    let v = if scan_fail {
        "S3-FIFO FAILS the scan rule (any single scan test > 2% abs worse than LRU)"
    } else if acc {
        "S3-FIFO accepted"
    } else if sieve {
        "amend to SIEVE"
    } else if wtl {
        "amend to W-TinyLFU"
    } else {
        "inconclusive: S3-FIFO not accepted but no amendment margin met (report, keep proposed)"
    };
    let _ = writeln!(o, "\nADR-14 verdict: {v} [accept clauses: {acc}, sieve<=0.95 at >=2 sizes: {sieve}, wtinylfu<=0.90 at all: {wtl}, scan_fail: {scan_fail}]");
    o
}
