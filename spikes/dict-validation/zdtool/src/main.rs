//! zdtool: helper for the dict-validation study.
//!
//! * `zdtool dump <dir>` writes every shipped dictionary as `<index>.dict` (index = position in `zdicts::all()`, 1-based)
//!   and `manifest.tsv` (index, id, name, length).
//! * `zdtool sel <recs.bin> <image> <out.u8>` applies `select_by_content` to the first 4 KiB of every chunk record and writes
//!   one byte per record: 0 = no dictionary, else the 1-based dictionary index.
//!
//! * `zdtool seltime <lunix|obj> <recs> <sample> <passes> <mask|-> <names|-> <out.u8>` times the selector in memory (CPU-s per GB of
//!   input, one warm-up pass plus `passes` timed passes, median/min/max) and writes the selection of the first pass like `sel`.
//!   `lunix`: `select_by_content` on the first 4 KiB. `obj`: Constellation's `select(Some(name), head, len)` with the 64 KiB size
//!   rule, one object name per line in `names`. `mask` (u8 per record, 1 = skip, the record was stored raw by the probe) or `-`.
//!
//! Record layout (packed, 16 bytes, little endian): u32 file id, u32 length, u64 offset. Only file id 0 is supported.

use std::error::Error;
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::os::unix::fs::FileExt;
use std::path::Path;

type Res<T> = Result<T, Box<dyn Error>>;

const HEAD: usize = 4096;
const REC: usize = 16;

fn dump(dir: &Path) -> Res<()> {
    fs::create_dir_all(dir)?;
    let mut manifest = BufWriter::new(File::create(dir.join("manifest.tsv"))?);
    for (i, d) in zdicts::all().enumerate() {
        let idx = i + 1;
        fs::write(dir.join(format!("{idx}.dict")), d.bytes())?;
        writeln!(manifest, "{idx}\t{:#010x}\t{}\t{}", d.id(), d.name(), d.len())?;
    }
    manifest.flush()?;
    Ok(())
}

fn sel(recs_path: &Path, image: &Path, out: &Path) -> Res<()> {
    let ids: Vec<u32> = zdicts::all().map(|d| d.id()).collect();
    if ids.len() >= 255 {
        return Err("more than 254 dictionaries".into());
    }
    let recs = fs::read(recs_path)?;
    if recs.len() % REC != 0 {
        return Err("recs file length is not a multiple of 16".into());
    }
    let img = File::open(image)?;
    let mut buf = vec![0u8; HEAD];
    let mut res = Vec::with_capacity(recs.len() / REC);
    for r in recs.chunks_exact(REC) {
        let file_id = u32::from_le_bytes([r[0], r[1], r[2], r[3]]);
        let len = u32::from_le_bytes([r[4], r[5], r[6], r[7]]) as usize;
        let mut off = [0u8; 8];
        off.copy_from_slice(&r[8..16]);
        let off = u64::from_le_bytes(off);
        if file_id != 0 {
            return Err("only file id 0 is supported".into());
        }
        let n = len.min(HEAD);
        img.read_exact_at(&mut buf[..n], off)?;
        let code = match zdicts::select_by_content(&buf[..n]) {
            None => 0u8,
            Some(d) => {
                let pos = ids.iter().position(|&x| x == d.id()).ok_or("selected id not in registry")?;
                u8::try_from(pos + 1)?
            }
        };
        res.push(code);
    }
    fs::write(out, res)?;
    Ok(())
}

fn seltime(mode: &str, recs_path: &Path, sample: &Path, passes: usize, mask: Option<&Path>, names: Option<&Path>, out: &Path) -> Res<()> {
    let ids: Vec<u32> = zdicts::all().map(|d| d.id()).collect();
    for (i, id) in ids.iter().enumerate() {
        if *id != 0x8000_0001 + u32::try_from(i)? {
            return Err("dictionary ids are not contiguous".into());
        }
    }
    let recs = fs::read(recs_path)?;
    let data = fs::read(sample)?;
    let n = recs.len() / REC;
    let skip: Vec<u8> = match mask {
        Some(p) => fs::read(p)?,
        None => vec![0u8; n],
    };
    let obj_names: Vec<String> = match names {
        Some(p) => fs::read_to_string(p)?.lines().map(str::to_owned).collect(),
        None => Vec::new(),
    };
    let obj = mode == "obj";
    if skip.len() != n || (obj && obj_names.len() != n) {
        return Err("mask or names length differs from the record count".into());
    }
    let mut parsed = Vec::with_capacity(n);
    let mut total = 0usize;
    for r in recs.chunks_exact(REC) {
        let len = u32::from_le_bytes([r[4], r[5], r[6], r[7]]) as usize;
        let mut o = [0u8; 8];
        o.copy_from_slice(&r[8..16]);
        let off = usize::try_from(u64::from_le_bytes(o))?;
        if off + len > data.len() {
            return Err("record outside the sample".into());
        }
        total += len;
        parsed.push((off, len));
    }
    let mut res = vec![0u8; n];
    let mut times = Vec::with_capacity(passes);
    for pass in 0..=passes {
        let t0 = std::time::Instant::now();
        for (i, &(off, len)) in parsed.iter().enumerate() {
            if skip[i] != 0 {
                continue;
            }
            let head = &data[off..off + len.min(HEAD)];
            let d = if obj {
                zdicts::select(Some(Path::new(&obj_names[i])), head, len)
            } else {
                zdicts::select_by_content(head)
            };
            let code = match d {
                None => 0u8,
                Some(d) => u8::try_from(d.id() - 0x8000_0000)?,
            };
            res[i] = std::hint::black_box(code);
        }
        let dt = t0.elapsed().as_secs_f64();
        if pass > 0 {
            times.push(dt);
        }
    }
    fs::write(out, &res)?;
    times.sort_by(f64::total_cmp);
    let gb = total as f64 / 1e9;
    println!("{:.4}\t{:.4}\t{:.4}\t{}", times[times.len() / 2] / gb, times[0] / gb, times[times.len() - 1] / gb, total);
    Ok(())
}

/// `objsel <recs> <files> <names> <out>`: Constellation `select(Some(name), head, len)` per object record. `files` is one path per line
/// (the record's file id is the line number), `names` is NUL-separated, one name per record; `head` is the first 4 KiB of the object.
fn objsel(recs_path: &Path, files_path: &Path, names_path: &Path, out: &Path) -> Res<()> {
    let recs = fs::read(recs_path)?;
    let files: Vec<String> = fs::read_to_string(files_path)?.lines().map(str::to_owned).collect();
    let raw_names = fs::read(names_path)?;
    let names: Vec<&[u8]> = raw_names.split(|&b| b == 0).collect();
    let n = recs.len() / REC;
    if recs.len() % REC != 0 || names.len() < n {
        return Err("recs length or names count mismatch".into());
    }
    let mut res = Vec::with_capacity(n);
    let mut buf = vec![0u8; HEAD];
    let mut cur: Option<(u32, File)> = None;
    for (i, r) in recs.chunks_exact(REC).enumerate() {
        let id = u32::from_le_bytes([r[0], r[1], r[2], r[3]]);
        let len = u32::from_le_bytes([r[4], r[5], r[6], r[7]]) as usize;
        let mut o = [0u8; 8];
        o.copy_from_slice(&r[8..16]);
        let off = u64::from_le_bytes(o);
        if cur.as_ref().is_none_or(|(c, _)| *c != id) {
            let p = files.get(usize::try_from(id)?).ok_or("file id out of range")?;
            cur = Some((id, File::open(p)?));
        }
        let (_, f) = cur.as_ref().ok_or("no open file")?;
        let k = len.min(HEAD);
        f.read_exact_at(&mut buf[..k], off)?;
        let name = String::from_utf8_lossy(names[i]).into_owned();
        let code = match zdicts::select(Some(Path::new(&name)), &buf[..k], len) {
            None => 0u8,
            Some(d) => u8::try_from(d.id() - 0x8000_0000)?,
        };
        res.push(code);
    }
    fs::write(out, res)?;
    Ok(())
}

fn main() -> Res<()> {
    let a: Vec<String> = std::env::args().collect();
    match a.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        [_, "dump", dir] => dump(Path::new(dir)),
        [_, "sel", recs, image, out] => sel(Path::new(recs), Path::new(image), Path::new(out)),
        [_, "objsel", recs, files, names, out] => objsel(Path::new(recs), Path::new(files), Path::new(names), Path::new(out)),
        [_, "seltime", mode, recs, sample, passes, mask, names, out] => seltime(
            mode,
            Path::new(recs),
            Path::new(sample),
            passes.parse()?,
            (*mask != "-").then(|| Path::new(mask)),
            (*names != "-").then(|| Path::new(names)),
            Path::new(out),
        ),
        _ => Err("usage: zdtool dump <dir> | zdtool sel <recs> <image> <out> | zdtool seltime ...".into()),
    }
}
