//! tracesim: P0.6 trace-driven simulators. Throwaway measurement code.
mod cachesim;
mod chunksim;
mod ds;
mod gate;
mod policies;
mod trace;

use std::fmt::Write as _;

fn usage() -> ! {
    eprintln!(
        "usage:\n  tracesim chunk --set NAME --fmt msr|alibaba|tencent --out DIR [--min-written-gib N] FILES...\n  tracesim cache --set NAME --fmt F --out DIR [--chunk 65536] [--scan-vols N] [--min-written-gib N] FILES...\n  tracesim gate DIR"
    );
    std::process::exit(2)
}

fn write_csv(dir: &str, name: &str, hdr: &str, rows: &[String]) {
    let mut s = String::new();
    let _ = writeln!(s, "{hdr}");
    for r in rows {
        let _ = writeln!(s, "{r}");
    }
    std::fs::write(format!("{dir}/{name}"), s).expect("write csv");
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        usage();
    }
    let cmd = args[0].as_str();
    if cmd == "gate" {
        if args.len() != 2 {
            usage();
        }
        print!("{}", gate::gate_chunk(&args[1]));
        println!();
        print!("{}", gate::gate_cache(&args[1]));
        return;
    }
    let (mut set, mut fmt, mut out, mut min_gib, mut chunk, mut scan_vols) = (String::new(), None, String::new(), 1.0f64, 65536u64, 10usize);
    let mut files = Vec::new();
    let mut it = args.iter().skip(1);
    while let Some(a) = it.next() {
        let mut val = || it.next().cloned().unwrap_or_else(|| usage());
        match a.as_str() {
            "--set" => set = val(),
            "--fmt" => fmt = trace::Fmt::parse(&val()),
            "--out" => out = val(),
            "--min-written-gib" => min_gib = val().parse().unwrap_or_else(|_| usage()),
            "--chunk" => chunk = val().parse().unwrap_or_else(|_| usage()),
            "--scan-vols" => scan_vols = val().parse().unwrap_or_else(|_| usage()),
            f => files.push(f.to_string()),
        }
    }
    let fmt = fmt.unwrap_or_else(|| usage());
    if set.is_empty() || out.is_empty() || files.is_empty() {
        usage();
    }
    std::fs::create_dir_all(&out).expect("mkdir out");
    let (vols, bad) = trace::load(&files, fmt).expect("read trace");
    let kept: Vec<_> = vols.iter().filter(|(_, r)| trace::written_bytes(r) as f64 >= min_gib * (1u64 << 30) as f64).collect();
    eprintln!("{} volumes loaded, {} kept (>= {} GiB written), {} unparsable lines skipped", vols.len(), kept.len(), min_gib, bad);
    match cmd {
        "chunk" => {
            let mut o = chunksim::Out::default();
            for (v, r) in &kept {
                chunksim::sim_volume(&set, v, r, &mut o);
            }
            write_csv(&out, &format!("chunk_rmw.{set}.csv"), chunksim::H_RMW, &o.rmw);
            write_csv(&out, &format!("chunk_map.{set}.csv"), chunksim::H_MAP, &o.map);
            write_csv(&out, &format!("chunk_commit.{set}.csv"), chunksim::H_COMMIT, &o.commit);
        }
        "cache" => {
            // Scan test volumes: the N kept volumes with the most requests (deterministic).
            let mut by_n: Vec<_> = kept.iter().map(|(v, r)| (r.len(), v.to_string())).collect();
            by_n.sort_by(|a, b| b.cmp(a));
            let scan: std::collections::BTreeSet<String> = by_n.into_iter().take(scan_vols).map(|x| x.1).collect();
            let (mut rows, mut srows) = (Vec::new(), Vec::new());
            for (v, r) in &kept {
                cachesim::sim_volume(&set, v, r, chunk, scan.contains(*v), &mut rows, &mut srows);
            }
            write_csv(&out, &format!("cache_results.{set}.csv"), cachesim::H_CACHE, &rows);
            write_csv(&out, &format!("cache_scan.{set}.csv"), cachesim::H_SCAN, &srows);
        }
        _ => usage(),
    }
}
