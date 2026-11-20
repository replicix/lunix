//! `journal-zero-bench`: `FALLOC_FL_ZERO_RANGE` against rewriting zeros when a
//! freed ring file is re-initialised (milestone jstall, ADR-33 W7).
//!
//! Two journals of one lane and four 64 MiB files each (the production
//! geometry) are driven in turn, one with `zero_range` on and one off, on the
//! file system of `--dir`. Each starts with every file full, then per round:
//! reclaim the oldest file (timed: the work the coordinator waits for), append
//! 4 KiB records one at a time into the re-initialised file for their latency
//! ("after reuse"), fill the middle of the file with 64 KiB records, take the
//! same latency again ("mid file"), and fill the rest until the lane is full
//! again (throughput). Alternating the two journals every round keeps the
//! shared device's drift out of the comparison.
//!
//! ```text
//! journal-zero-bench --dir DIR [--rounds 12] [--probe 256] [--keep]
//! journal-zero-bench --dir DIR --nearly-full 8 [--inject-errno 28] [--rounds 3]
//! ```
//!
//! `--nearly-full MB` runs the `ZERO_RANGE` journal alone with a filler file
//! that leaves `MB` MiB free on the file system, and reports whether every
//! ring file is fully allocated afterwards (no hole), and how many files went
//! through `ZERO_RANGE` and through the rewrite; `--inject-errno` makes the
//! `ZERO_RANGE` calls fail with that errno (a file system that answers
//! `ENOSPC`).
//!
//! Prints one JSON line per mode.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Instant;

use lunix_format::journal::RecordKind;
use lunix_format::{VolumeUuid, WriteSeq};
use lunix_iface::journal::{Journal, JournalCompletion, JournalLane, LaneId, SlotOutcome};
use lunix_iface::wake::WakeFd;
use lunix_journal::{FileJournal, JournalConfig};

const FILE_MIB: u64 = 64;
const FILES: u64 = 4;

struct Args {
    dir: PathBuf,
    rounds: usize,
    probe: usize,
    keep: bool,
    nearly_full: Option<u64>,
    inject: Option<i32>,
}

fn parse() -> Result<Args, String> {
    let mut a = Args {
        dir: PathBuf::new(),
        rounds: 12,
        probe: 256,
        keep: false,
        nearly_full: None,
        inject: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(k) = it.next() {
        let mut val = |name: &str| it.next().ok_or_else(|| format!("{name} needs a value"));
        match k.as_str() {
            "--dir" => a.dir = PathBuf::from(val("--dir")?),
            "--rounds" => a.rounds = val("--rounds")?.parse().map_err(|e| format!("{e}"))?,
            "--probe" => a.probe = val("--probe")?.parse().map_err(|e| format!("{e}"))?,
            "--keep" => a.keep = true,
            "--nearly-full" => {
                a.nearly_full = Some(val("--nearly-full")?.parse().map_err(|e| format!("{e}"))?);
            }
            "--inject-errno" => {
                a.inject = Some(val("--inject-errno")?.parse().map_err(|e| format!("{e}"))?);
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    if a.dir.as_os_str().is_empty() {
        return Err("--dir is required".into());
    }
    Ok(a)
}

fn wait_wake(wake: &WakeFd, ms: i32) {
    let mut p = libc::pollfd {
        fd: wake.raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: `p` is a live pollfd and the count is 1.
    unsafe { libc::poll(&mut p, 1, ms) };
    wake.drain();
}

struct Ring {
    name: &'static str,
    j: FileJournal,
    lane: Box<dyn JournalLane>,
    wake: WakeFd,
    /// Ordinal of a file -> its highest `write_seq`.
    files: BTreeMap<u32, u64>,
    outstanding: usize,
    reclaim_us: Vec<u64>,
    reuse_us: Vec<u64>,
    mid_us: Vec<u64>,
    fill_bytes: u64,
    fill_ns: u128,
}

impl Ring {
    fn open(name: &'static str, dir: &std::path::Path, zero_range: bool) -> Self {
        let mut cfg = JournalConfig::new(
            dir.join(name).join("journal"),
            VolumeUuid([5; 16]),
            1,
            (FILES * FILE_MIB) << 20,
        );
        cfg.zero_range = zero_range;
        cfg.mark_idle = None;
        let j = FileJournal::open(cfg).expect("open journal");
        j.recover(None, WriteSeq(0)).expect("recover");
        let wake = WakeFd::new().expect("eventfd");
        let lane = j.open_lane(LaneId(0), wake.clone()).expect("lane");
        Self {
            name,
            j,
            lane,
            wake,
            files: BTreeMap::new(),
            outstanding: 0,
            reclaim_us: Vec::new(),
            reuse_us: Vec::new(),
            mid_us: Vec::new(),
            fill_bytes: 0,
            fill_ns: 0,
        }
    }

    fn reap(&mut self) {
        let mut out = Vec::new();
        self.lane.poll(&mut out);
        for c in out {
            if let JournalCompletion::Append { status, .. } = c {
                assert!(status.is_ok(), "append failed");
                self.outstanding -= 1;
            }
        }
    }

    fn drain_to(&mut self, n: usize) {
        loop {
            self.reap();
            if self.outstanding <= n {
                return;
            }
            wait_wake(&self.wake, 2);
        }
    }

    /// Appends one record of `blocks` blocks; `None` when the ring is full.
    /// Returns the ordinal of the file it went to.
    fn append(&mut self, blocks: u32) -> Option<u32> {
        match self
            .lane
            .try_reserve(RecordKind::Write, blocks)
            .expect("reserve")
        {
            SlotOutcome::Reserved(slot) => {
                let data = vec![0xa5u8; blocks as usize * 4096];
                let seq = slot.write_seq.0;
                let a = self.lane.append(slot, 0, &data).expect("append");
                self.outstanding += 1;
                let e = self.files.entry(a.loc.ordinal).or_insert(0);
                *e = (*e).max(seq);
                Some(a.loc.ordinal)
            }
            SlotOutcome::Full | SlotOutcome::NoSpace => None,
        }
    }

    /// 4 KiB records at queue depth 1: the latency of each, microseconds.
    fn probe(&mut self, n: usize, newer_than: u32) -> Vec<u64> {
        let mut v = Vec::with_capacity(n);
        for _ in 0..n {
            let t = Instant::now();
            match self.append(1) {
                Some(ord) if ord > newer_than => {
                    self.drain_to(0);
                    v.push(t.elapsed().as_micros() as u64);
                }
                Some(_) => self.drain_to(0),
                None => break,
            }
        }
        v
    }

    /// 64 KiB records, eight in flight, until `stop` says so or the ring is
    /// full; returns true when it is full.
    fn fill(&mut self, stop: impl Fn(&Self) -> bool) -> bool {
        let t = Instant::now();
        let mut full = false;
        while !stop(self) {
            if self.append(16).is_none() {
                full = true;
                break;
            }
            self.fill_bytes += 16 * 4096;
            if self.outstanding >= 8 {
                self.drain_to(4);
            }
        }
        self.drain_to(0);
        self.fill_ns += t.elapsed().as_nanos();
        full
    }

    fn first_fill(&mut self) {
        let t = Instant::now();
        assert!(self.fill(|_| false), "the ring must fill");
        eprintln!("{}: initial fill {:?}", self.name, t.elapsed());
        self.fill_bytes = 0;
        self.fill_ns = 0;
    }

    fn round(&mut self, probe: usize) {
        let flush = {
            let (tx, rx) = std::sync::mpsc::channel();
            self.j
                .submit_group_flush(
                    self.j.last_issued(),
                    Box::new(move |r| {
                        let _ = tx.send(r.is_ok());
                    }),
                )
                .expect("submit flush");
            rx
        };
        assert!(flush.recv().expect("flush"), "flush failed");
        let (&oldest, &cut) = self.files.iter().next().expect("a file");
        let newest = *self.files.keys().next_back().expect("a file");
        let tail = self.j.tail_after(WriteSeq(cut)).expect("tail");
        let t = Instant::now();
        let r = self.j.reclaim(&tail).expect("reclaim");
        self.reclaim_us.push(t.elapsed().as_micros() as u64);
        assert_eq!(r.files, 1, "one file freed");
        self.files.remove(&oldest);
        // Into the re-initialised file: latency of the first records.
        let v = self.probe(probe, newest);
        self.reuse_us.extend(v);
        // The middle of the file, then the same probe.
        let mid = 32u64 << 20;
        let start = self.fill_bytes;
        self.fill(|s| s.fill_bytes - start >= mid);
        let newest = *self.files.keys().next_back().expect("a file");
        let v = self.probe(probe, newest - 1);
        self.mid_us.extend(v);
        // The rest, until the lane is full again.
        self.fill(|_| false);
    }
}

fn pct(v: &[u64], q: f64) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    let mut v = v.to_vec();
    v.sort_unstable();
    let i = ((v.len() as f64 - 1.0) * q).round() as usize;
    v[i] as f64
}

fn report(r: &Ring, zero_range: bool) {
    let (by_range, by_rewrite) = r.j.reinit_counts();
    println!(
        "{{\"bench\":\"journal-zero-bench\",\"mode\":\"{}\",\"zero_range\":{},\"files_by_range\":{},\"files_by_rewrite\":{},\"rounds\":{},\
\"reclaim_ms_p50\":{:.1},\"reclaim_ms_p99\":{:.1},\"reclaim_ms_max\":{:.1},\
\"after_reuse_us_p50\":{:.0},\"after_reuse_us_p99\":{:.0},\"after_reuse_us_max\":{:.0},\
\"mid_file_us_p50\":{:.0},\"mid_file_us_p99\":{:.0},\"mid_file_us_max\":{:.0},\
\"fill_mb_per_s\":{:.1}}}",
        r.name,
        zero_range,
        by_range,
        by_rewrite,
        r.reclaim_us.len(),
        pct(&r.reclaim_us, 0.5) / 1000.0,
        pct(&r.reclaim_us, 0.99) / 1000.0,
        pct(&r.reclaim_us, 1.0) / 1000.0,
        pct(&r.reuse_us, 0.5),
        pct(&r.reuse_us, 0.99),
        pct(&r.reuse_us, 1.0),
        pct(&r.mid_us, 0.5),
        pct(&r.mid_us, 0.99),
        pct(&r.mid_us, 1.0),
        r.fill_bytes as f64 / (r.fill_ns as f64 / 1e9) / 1e6,
    );
}

fn main() {
    let a = match parse() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("journal-zero-bench: {e}");
            std::process::exit(2);
        }
    };
    if let Some(mb) = a.nearly_full {
        nearly_full(&a, mb);
        return;
    }
    let mut range = Ring::open("range", &a.dir, true);
    let mut rewrite = Ring::open("rewrite", &a.dir, false);
    range.first_fill();
    rewrite.first_fill();
    for i in 0..a.rounds {
        // Alternate which goes first, so neither always follows the other.
        if i % 2 == 0 {
            range.round(a.probe);
            rewrite.round(a.probe);
        } else {
            rewrite.round(a.probe);
            range.round(a.probe);
        }
        eprintln!("round {} of {}", i + 1, a.rounds);
    }
    report(&range, true);
    report(&rewrite, false);
    drop(range);
    drop(rewrite);
    if !a.keep {
        let _ = std::fs::remove_dir_all(a.dir.join("range"));
        let _ = std::fs::remove_dir_all(a.dir.join("rewrite"));
    }
}

/// Free bytes of the file system holding `dir`.
fn free_bytes(dir: &std::path::Path) -> u64 {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(dir.as_os_str().as_bytes()).expect("path");
    // SAFETY: `c` is a NUL-terminated path, `st` a live out-parameter.
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: as above.
    let rc = unsafe { libc::statvfs(c.as_ptr(), &mut st) };
    assert_eq!(rc, 0, "statvfs");
    st.f_bavail as u64 * st.f_frsize as u64
}

/// The `ZERO_RANGE` journal on a nearly full file system.
fn nearly_full(a: &Args, mb: u64) {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::MetadataExt;
    std::fs::create_dir_all(&a.dir).expect("dir");
    let mut r = Ring::open("range", &a.dir, true);
    r.first_fill();
    if let Some(e) = a.inject {
        r.j.inject_zero_range_errno(Some(e));
    }
    // The filler takes everything but `mb` MiB.
    let filler = a.dir.join("filler");
    let f = std::fs::File::create(&filler).expect("filler");
    let mut want = free_bytes(&a.dir).saturating_sub(mb << 20);
    loop {
        // SAFETY: the fd is open; plain integers.
        let rc = unsafe { libc::posix_fallocate(f.as_raw_fd(), 0, want as libc::off_t) };
        if rc == 0 || want < (16 << 20) {
            assert_eq!(rc, 0, "filler of {want} bytes: errno {rc}");
            break;
        }
        want -= 8 << 20;
    }
    let free = free_bytes(&a.dir);
    let mut rounds = 0;
    for i in 0..a.rounds.min(6) {
        r.round(a.probe.min(64));
        rounds = i + 1;
    }
    let (by_range, by_rewrite) = r.j.reinit_counts();
    let mut sparse = 0u32;
    let mut files = 0u32;
    let jdir = a.dir.join("range").join("journal");
    for e in std::fs::read_dir(&jdir).expect("journal dir") {
        let m = e.expect("entry").metadata().expect("meta");
        if m.is_file() && m.len() >= (1 << 20) {
            files += 1;
            if m.blocks() * 512 < m.len() {
                sparse += 1;
            }
        }
    }
    println!(
        "{{\"bench\":\"journal-zero-bench\",\"mode\":\"nearly-full\",\"free_mib_after_filler\":{},\"inject_errno\":{},\"rounds\":{},\"zero_range_calls\":{},\"zero_range_errnos\":{:?},\"files_by_range\":{},\"files_by_rewrite\":{},\"ring_files\":{},\"sparse_ring_files\":{}}}",
        free >> 20,
        a.inject.unwrap_or(0),
        rounds,
        r.j.zero_range_attempts(),
        r.j.zero_range_errnos(),
        by_range,
        by_rewrite,
        files,
        sparse
    );
    drop(r);
    let _ = std::fs::remove_file(&filler);
    if !a.keep {
        let _ = std::fs::remove_dir_all(a.dir.join("range"));
    }
}
