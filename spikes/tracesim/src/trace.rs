//! Defensive trace readers. Input files are untrusted data: lines that do not
//! parse are counted and skipped, nothing is executed, arithmetic is checked.
use crate::ds::FxMap;
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};

pub const BLK: u64 = 4096;
const MAX_LEN: u64 = 1 << 30;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Req {
    pub t: u64,   // trace clock, microseconds
    pub off: u64, // 4 KiB aligned (rounded outwards)
    pub len: u64, // multiple of 4 KiB, > 0
    pub w: bool,
}

#[derive(Clone, Copy, Debug)]
pub enum Fmt {
    Msr,
    Alibaba,
    Tencent,
}
impl Fmt {
    pub fn parse(s: &str) -> Option<Fmt> {
        match s {
            "msr" => Some(Fmt::Msr),
            "alibaba" => Some(Fmt::Alibaba),
            "tencent" => Some(Fmt::Tencent),
            _ => None,
        }
    }
}

pub fn align(off: u64, len: u64) -> Option<(u64, u64)> {
    if len == 0 || len > MAX_LEN {
        return None;
    }
    let end = off.checked_add(len)?;
    let s = off & !(BLK - 1);
    let e = end.checked_add(BLK - 1)? & !(BLK - 1);
    Some((s, e - s))
}

/// MSR Cambridge: Timestamp(100ns filetime),Hostname,DiskNumber,Type,Offset,Size,ResponseTime
/// Alibaba: device_id,opcode(R|W),offset,length,timestamp(us)
/// Tencent CBS (IOTTA): Timestamp(s),Offset(sectors),Size(sectors),IOType(0 R/1 W),VolumeID
///   (Tencent layout is from memory of the IOTTA description: UNVERIFIED, never exercised here).
pub fn parse_line(fmt: Fmt, line: &str) -> Option<(String, Req)> {
    let f: Vec<&str> = line.trim().split(',').map(|x| x.trim()).collect();
    match fmt {
        Fmt::Msr => {
            if f.len() < 6 {
                return None;
            }
            let t = f[0].parse::<u64>().ok()? / 10;
            let w = match f[3] {
                "Write" | "write" | "W" => true,
                "Read" | "read" | "R" => false,
                _ => return None,
            };
            let (o, l) = align(f[4].parse().ok()?, f[5].parse().ok()?)?;
            Some((format!("{}_{}", f[1], f[2]), Req { t, off: o, len: l, w }))
        }
        Fmt::Alibaba => {
            if f.len() < 5 {
                return None;
            }
            let w = match f[1] {
                "W" => true,
                "R" => false,
                _ => return None,
            };
            let (o, l) = align(f[2].parse().ok()?, f[3].parse().ok()?)?;
            Some((f[0].to_string(), Req { t: f[4].parse().ok()?, off: o, len: l, w }))
        }
        Fmt::Tencent => {
            if f.len() < 5 {
                return None;
            }
            let ts: f64 = f[0].parse().ok()?;
            if !(0.0..1e13).contains(&ts) {
                return None;
            }
            let w = match f[3] {
                "1" => true,
                "0" => false,
                _ => return None,
            };
            let off = f[1].parse::<u64>().ok()?.checked_mul(512)?;
            let len = f[2].parse::<u64>().ok()?.checked_mul(512)?;
            let (o, l) = align(off, len)?;
            Some((f[4].to_string(), Req { t: (ts * 1e6) as u64, off: o, len: l, w }))
        }
    }
}

/// Stream a file, decompressing on the fly with an external tool (no shell).
pub fn for_each_line(path: &str, mut f: impl FnMut(&str)) -> std::io::Result<()> {
    let tool = if path.ends_with(".gz") {
        Some("gzip")
    } else if path.ends_with(".xz") {
        Some("xz")
    } else if path.ends_with(".zst") {
        Some("zstd")
    } else {
        None
    };
    if let Some(t) = tool {
        let mut ch = Command::new(t).arg("-dc").arg("--").arg(path).stdout(Stdio::piped()).spawn()?;
        let rd = BufReader::with_capacity(1 << 20, ch.stdout.take().unwrap());
        for l in rd.split(b'\n') {
            f(&String::from_utf8_lossy(&l?));
        }
        ch.wait()?;
    } else {
        let rd = BufReader::with_capacity(1 << 20, std::fs::File::open(path)?);
        for l in rd.split(b'\n') {
            f(&String::from_utf8_lossy(&l?));
        }
    }
    Ok(())
}

/// Load all files, merged per volume, stably sorted by trace clock.
pub fn load(paths: &[String], fmt: Fmt) -> std::io::Result<(BTreeMap<String, Vec<Req>>, u64)> {
    let mut by: FxMap<String, Vec<Req>> = FxMap::default();
    let mut bad = 0u64;
    for p in paths {
        for_each_line(p, |l| {
            if l.is_empty() {
                return;
            }
            match parse_line(fmt, l) {
                Some((v, r)) => by.entry(v).or_default().push(r),
                None => bad += 1,
            }
        })?;
    }
    let mut out = BTreeMap::new();
    for (k, mut v) in by {
        v.sort_by_key(|r| r.t);
        out.insert(k, v);
    }
    Ok((out, bad))
}

pub fn written_bytes(reqs: &[Req]) -> u64 {
    reqs.iter().filter(|r| r.w).map(|r| r.len).sum()
}
