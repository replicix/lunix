mod codec;
mod gen;
use codec::*;
use gen::*;
use std::hint::black_box;
use std::io::Write;
use std::time::Instant;

fn pct(v: &mut Vec<u32>, p: f64) -> u32 {
    v.sort_unstable();
    v[((v.len() as f64 - 1.0) * p) as usize]
}

fn check_leaf(var: &Variant, e: &[E], d: &[DE]) {
    let t = var.raw.t();
    for i in 0..FAN {
        let (a, b) = (&e[i], &d[i]);
        if a.hole() {
            assert_eq!(b.seg, 0, "{} hole {}", var.name(), i);
            continue;
        }
        assert_eq!(a.seg, b.seg, "{} seg {}", var.name(), i);
        if var.raw.slot() {
            assert_eq!(a.slot, b.slot, "{} slot {}", var.name(), i);
        } else {
            assert_eq!((a.off, a.clen), (b.off, b.clen), "{} off/clen {}", var.name(), i);
        }
        assert_eq!(&idbytes(a.idseed)[..t], &b.id[..t], "{} id {}", var.name(), i);
    }
}
fn check_lk(var: &Variant, e: &E, l: &Lk) {
    if e.hole() {
        assert_eq!(l.seg, 0, "{}", var.name());
        return;
    }
    assert_eq!(e.seg, l.seg, "{} lk seg", var.name());
    if var.raw.slot() {
        assert_eq!(e.slot, l.slot, "{} lk slot", var.name());
    } else {
        assert_eq!((e.off, e.clen), (l.off, l.clen), "{} lk off/clen", var.name());
    }
    let t = var.raw.t();
    let mut x = [0u8; 8];
    let k = t.min(8);
    x[..k].copy_from_slice(&idbytes(e.idseed)[..k]);
    assert_eq!(u64::from_le_bytes(x), l.idp, "{} lk id", var.name());
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let arg = |k: &str, d: &str| -> String {
        args.iter().position(|a| a == k).map(|i| args[i + 1].clone()).unwrap_or(d.to_string())
    };
    let chunk: u32 = arg("--chunk", "65536").parse().unwrap();
    let works: Vec<char> = arg("--workloads", "abcde").chars().collect();
    let stride: usize = arg("--stride", "1").parse().unwrap();
    let tib_chunks: usize = ((1u64 << 40) / chunk as u64) as usize;
    let nsample: usize = arg("--sample", "4096").parse().unwrap();
    let nops: usize = arg("--ops", "200000").parse().unwrap();
    let check_only = args.iter().any(|a| a == "--check");
    let only: String = arg("--only", "");
    let tib_chunks = if check_only { tib_chunks / 64 } else { tib_chunks };
    let nleaves_total = tib_chunks / FAN;
    let scale = (1u64 << 40) as f64 / (tib_chunks as f64 * chunk as f64); // volume bytes -> per-TiB

    // timer overhead
    let mut o: Vec<u32> = (0..200000)
        .map(|_| {
            let t = Instant::now();
            black_box(());
            t.elapsed().as_nanos() as u32
        })
        .collect();
    let ovh = pct(&mut o, 0.5);
    println!("<!-- timer overhead p50 = {ovh} ns; chunk={chunk}; leaves={nleaves_total}; stride={stride}; sample={nsample}; ops={nops} -->");

    for &w in &works {
        let t0 = Instant::now();
        let vol = gen(w, tib_chunks, chunk, 0xC0FFEE + w as u64);
        eprintln!("generated {} in {:?}", vol.name, t0.elapsed());
        // workload stats
        let (mut nonhole, mut runs, mut segs, mut nel) = (0usize, 0usize, 0usize, 0usize);
        let mut live: Vec<usize> = Vec::new();
        for l in (0..nleaves_total).step_by(stride) {
            let le = &vol.ents[l * FAN..(l + 1) * FAN];
            let nh = le.iter().filter(|e| !e.hole()).count();
            if nh == 0 {
                continue;
            }
            live.push(l);
            nonhole += nh;
            nel += 1;
            let mut s: Vec<u64> = le.iter().filter(|e| !e.hole()).map(|e| e.seg).collect();
            s.sort_unstable();
            s.dedup();
            segs += s.len();
            let mut r = 0;
            for i in 0..FAN {
                let cont = i > 0 && {
                    let (p, e) = (&le[i - 1], &le[i]);
                    (p.hole() && e.hole()) || (!p.hole() && !e.hole() && p.seg == e.seg && e.off == p.off + HDR + p.clen)
                };
                if !cont {
                    r += 1
                }
            }
            runs += r;
        }
        let nseg = vol.footers.len();
        let next: usize = vol.footers.iter().map(|f| f.len()).sum();
        println!("\n### Workload {} (chunk {} B)\n", vol.name, chunk);
        println!(
            "non-empty leaves {} of {}; non-hole entries/leaf {:.1}; mean runs/leaf {:.1}; mean distinct segments/leaf {:.1}; segments written {}; extents written {}\n",
            live.len() * stride,
            nleaves_total,
            nonhole as f64 / nel as f64,
            runs as f64 / nel as f64,
            segs as f64 / nel as f64,
            nseg,
            next
        );
        println!("| variant | MiB/TiB | B/entry | enc us/leaf | dec us/leaf | lookup(enc) mean ns | p50 | p99 | lookup(decoded) ns |");
        println!("|---|---:|---:|---:|---:|---:|---:|---:|---:|");

        // sample
        let step = (live.len() / nsample).max(1);
        let sample: Vec<usize> = live.iter().step_by(step).take(nsample).copied().collect();
        let mut rng = Rng(7);
        let ops: Vec<(u32, u16)> = (0..nops).map(|_| (rng.below(sample.len() as u64) as u32, rng.below(FAN as u64) as u16)).collect();
        // decoded-leaf lookup baseline (one set per workload)
        let dec_sample: Vec<DE> = {
            let rv = Variant::new(Box::new(Row { t: 32, slot: false }), Wrap::None);
            let mut v = vec![DE::default(); sample.len() * FAN];
            let mut buf = Vec::new();
            for (si, &l) in sample.iter().enumerate() {
                rv.enc(&vol.ents[l * FAN..(l + 1) * FAN], &mut buf);
                rv.dec(&buf, &mut v[si * FAN..(si + 1) * FAN]);
            }
            v
        };
        let dec_ns = {
            let mut best = f64::MAX;
            for _ in 0..5 {
                let t = Instant::now();
                let mut s = 0u64;
                for &(l, i) in &ops {
                    let d = &dec_sample[l as usize * FAN + i as usize];
                    s = s.wrapping_add(d.seg ^ d.off as u64 ^ d.clen as u64);
                }
                black_box(s);
                best = best.min(t.elapsed().as_nanos() as f64 / nops as f64);
            }
            best
        };

        for var in all_variants() {
            if !only.is_empty() && !var.name().contains(&only) {
                continue;
            }
            let slotv = var.raw.slot();
            // size over all (strided) live leaves
            let mut buf = Vec::new();
            let mut total = 0usize;
            let mut dbuf = vec![DE::default(); FAN];
            for (n, &l) in live.iter().enumerate() {
                let le = &vol.ents[l * FAN..(l + 1) * FAN];
                var.enc(le, &mut buf);
                total += buf.len();
                if n % 97 == 0 || check_only {
                    var.dec(&buf, &mut dbuf);
                    check_leaf(&var, le, &dbuf);
                    for i in (0..FAN).step_by(if check_only { 1 } else { 7 }) {
                        check_lk(&var, &le[i], &var.lk(&buf, i));
                    }
                }
            }
            if check_only {
                continue;
            }
            let mib_tib = total as f64 * stride as f64 * scale / (1 << 20) as f64;
            let b_entry = total as f64 / nonhole as f64;
            // encoded sample
            let enc: Vec<Vec<u8>> = sample
                .iter()
                .map(|&l| {
                    let mut b = Vec::new();
                    var.enc(&vol.ents[l * FAN..(l + 1) * FAN], &mut b);
                    b
                })
                .collect();
            // encode timing
            let mut enc_us = f64::MAX;
            for rep in 0..4 {
                let t = Instant::now();
                for &l in &sample {
                    var.enc(&vol.ents[l * FAN..(l + 1) * FAN], &mut buf);
                    black_box(&buf);
                }
                if rep > 0 {
                    enc_us = enc_us.min(t.elapsed().as_nanos() as f64 / sample.len() as f64 / 1e3);
                }
            }
            let mut dec_us = f64::MAX;
            for rep in 0..4 {
                let t = Instant::now();
                for b in &enc {
                    var.dec(b, &mut dbuf);
                    black_box(&dbuf);
                }
                if rep > 0 {
                    dec_us = dec_us.min(t.elapsed().as_nanos() as f64 / enc.len() as f64 / 1e3);
                }
            }
            let resolve = |l: &Lk| -> u64 {
                if slotv {
                    let f = &vol.footers[(l.seg & 0xFF_FFFF_FFFF) as usize - 1];
                    let (o, c) = f[l.slot as usize];
                    l.seg ^ o as u64 ^ c as u64
                } else {
                    l.seg ^ l.off as u64 ^ l.clen as u64 ^ l.idp
                }
            };
            // mean lookup
            let heavy = matches!(var.w, Wrap::Z(_));
            let nuse = if heavy { nops / 4 } else { nops };
            let mut mean = f64::MAX;
            for rep in 0..4 {
                let t = Instant::now();
                let mut s = 0u64;
                for &(l, i) in &ops[..nuse] {
                    let lk = var.lk(&enc[l as usize], i as usize);
                    if lk.seg != 0 {
                        s = s.wrapping_add(resolve(&lk));
                    }
                }
                black_box(s);
                if rep > 0 {
                    mean = mean.min(t.elapsed().as_nanos() as f64 / nuse as f64);
                }
            }
            // per-op percentiles
            let mut lat: Vec<u32> = Vec::with_capacity(50000);
            for &(l, i) in ops[..50000.min(nops)].iter() {
                let t = Instant::now();
                let lk = var.lk(&enc[l as usize], i as usize);
                let r = if lk.seg != 0 { resolve(&lk) } else { 0 };
                black_box(r);
                lat.push((t.elapsed().as_nanos() as u32).saturating_sub(ovh));
            }
            let (p50, p99) = (pct(&mut lat, 0.5), pct(&mut lat, 0.99));
            println!(
                "| {} | {:.1} | {:.2} | {:.1} | {:.1} | {:.0} | {} | {} | {:.1} |",
                var.name(),
                mib_tib,
                b_entry,
                enc_us,
                dec_us,
                mean,
                p50,
                p99,
                dec_ns
            );
            std::io::stdout().flush().ok();
        }
        // footer sizing (informational)
        let tot_ext = next as f64;
        println!(
            "\nFooter sizing for this workload: {} segments, {} extents written (incl. dead): full footer @ ~44 B/extent = {:.0} MiB/TiB; offset+clen sidecar @ 5.1 B/extent = {:.0} MiB/TiB; avg extents/segment {:.0}.\n",
            nseg,
            next,
            tot_ext * 44.0 * scale / (1 << 20) as f64,
            tot_ext * 5.125 * scale / (1 << 20) as f64,
            tot_ext / nseg as f64
        );
    }
}
