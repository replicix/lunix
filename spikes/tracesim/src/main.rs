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
        "usage:\n  tracesim chunk|all --set NAME --fmt msr|alibaba|tencent --out DIR [--min-written-gib N] FILES...\n  tracesim cache --set NAME --fmt F --out DIR [--chunk 65536,16384] [--scan-list a,b,..] [--tag T] [--min-written-gib N] FILES...\n  tracesim gate DIR"
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
    let (mut set, mut fmt, mut out, mut min_gib, mut chunks, mut scan_list, mut tag) = (String::new(), None, String::new(), 1.0f64, vec![65536u64], String::new(), String::new());
    let mut files = Vec::new();
    let mut it = args.iter().skip(1);
    while let Some(a) = it.next() {
        let mut val = || it.next().cloned().unwrap_or_else(|| usage());
        match a.as_str() {
            "--set" => set = val(),
            "--fmt" => fmt = trace::Fmt::parse(&val()),
            "--out" => out = val(),
            "--min-written-gib" => min_gib = val().parse().unwrap_or_else(|_| usage()),
            "--chunk" => chunks = val().split(',').map(|x| x.parse().unwrap_or_else(|_| usage())).collect(),
            "--scan-list" => scan_list = val(),
            "--tag" => tag = val(),
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
    let fname = |p: &str| if tag.is_empty() { format!("{p}.{set}.csv") } else { format!("{p}.{set}.{tag}.csv") };
    if cmd == "chunk" || cmd == "all" {
        let mut o = chunksim::Out::default();
        for (v, r) in &kept {
            chunksim::sim_volume(&set, v, r, &mut o);
        }
        write_csv(&out, &fname("chunk_rmw"), chunksim::H_RMW, &o.rmw);
        write_csv(&out, &fname("chunk_map"), chunksim::H_MAP, &o.map);
        write_csv(&out, &fname("chunk_commit"), chunksim::H_COMMIT, &o.commit);
    }
    if cmd == "cache" || cmd == "all" {
        // Scan-test volumes are named explicitly (the caller states the selection rule).
        let scan: std::collections::BTreeSet<&str> = scan_list.split(',').filter(|x| !x.is_empty()).collect();
        let (mut rows, mut srows) = (Vec::new(), Vec::new());
        for (v, r) in &kept {
            for &c in &chunks {
                cachesim::sim_volume(&set, v, r, c, scan.contains(v.as_str()), &mut rows, &mut srows);
            }
        }
        write_csv(&out, &fname("cache_results"), cachesim::H_CACHE, &rows);
        write_csv(&out, &fname("cache_scan"), cachesim::H_SCAN, &srows);
    }
    if !matches!(cmd, "chunk" | "cache" | "all") {
        usage();
    }
}
