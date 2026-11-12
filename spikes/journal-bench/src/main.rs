//! P0.5 throwaway spike: journal append microbenchmark. One thread, io_uring with a
//! registered file and one registered buffer, O_DIRECT appends (R padded to whole
//! 4 KiB sectors) followed by a durability primitive:
//!   group   : B WRITE_FIXED, wait, one FSYNC(DATASYNC) for the group (strategy A)
//!   linked  : chain of B WRITE_FIXED + FSYNC(DATASYNC) linked with IO_LINK, up to
//!             `--chains` chains in flight (strategy B)
//!   dsync   : each WRITE_FIXED carries RWF_DSYNC (strategy C)
//!   odsync  : file opened O_DSYNC, plain WRITE_FIXED (reference)
//! Open-loop offered load (`--rate`, records/s, deterministic arrivals; latency is
//! measured from the scheduled arrival to the durability CQE) or closed loop
//! (`--rate 0`: saturation, latency measured from the start of each group).
//! Prints one JSON line. Spike shortcuts: panics on I/O errors, no recovery logic.
use anyhow::{bail, Result};
use clap::Parser;
use hdrhistogram::Histogram;
use io_uring::{opcode, squeue, types, IoUring};
use std::collections::VecDeque;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;
use std::time::{Duration, Instant};

#[derive(Parser, Clone)]
struct Args {
    /// journal file (created if missing)
    #[arg(long)]
    file: String,
    #[arg(long, default_value = "group")]
    strategy: String,
    /// record size in bytes (rounded up to a multiple of 4096 for the I/O)
    #[arg(long, default_value_t = 4096)]
    rec: usize,
    #[arg(long, default_value_t = 16)]
    batch: usize,
    /// collect window in microseconds (group strategy, open loop)
    #[arg(long, default_value_t = 0)]
    window_us: u64,
    /// offered records per second; 0 = closed-loop saturation
    #[arg(long, default_value_t = 0)]
    rate: u64,
    #[arg(long, default_value_t = 3.0)]
    secs: f64,
    /// preallocate and zero-fill GiB (0 = growing file, created empty)
    #[arg(long, default_value_t = 0)]
    prealloc_gib: u64,
    #[arg(long, default_value_t = 4)]
    chains: usize,
    /// concurrent 64 KiB sequential O_DIRECT reader on the same file
    #[arg(long)]
    bg_read: bool,
    #[arg(long)]
    cpu: Option<usize>,
    #[arg(long, default_value = "")]
    label: String,
}

fn now_ns(t0: &Instant) -> u64 {
    t0.elapsed().as_nanos() as u64
}

fn thread_cpu_ns() -> u64 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut ts) };
    ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
}

struct Aligned(*mut u8, usize);
impl Aligned {
    fn new(n: usize) -> Self {
        let mut p = std::ptr::null_mut();
        unsafe { libc::posix_memalign(&mut p, 4096, n) };
        unsafe { std::ptr::write_bytes(p as *mut u8, 0x5a, n) };
        Aligned(p as *mut u8, n)
    }
}

struct Bench {
    a: Args,
    ring: IoUring,
    io_len: usize,
    file_size: u64,
    next_off: u64,
    buf: Aligned,
    t0: Instant,
    lat: Histogram<u64>,
    flat: Histogram<u64>,
    records: u64,
    flushes: u64,
    bytes: u64,
}

impl Bench {
    fn write_sqe(&mut self, slot: usize, dsync: bool, link: bool, ud: u64) -> squeue::Entry {
        let off = self.next_off;
        self.next_off += self.io_len as u64;
        if self.file_size != 0 && self.next_off + self.io_len as u64 > self.file_size {
            self.next_off = 0;
        }
        let addr = unsafe { self.buf.0.add(slot * self.io_len) };
        let mut w = opcode::WriteFixed::new(types::Fixed(0), addr, self.io_len as u32, 0).offset(off);
        if dsync {
            w = w.rw_flags(libc::RWF_DSYNC);
        }
        let mut e = w.build().user_data(ud);
        if link {
            e = e.flags(squeue::Flags::IO_LINK);
        }
        e
    }

    fn fsync_sqe(&self, ud: u64) -> squeue::Entry {
        opcode::Fsync::new(types::Fixed(0)).flags(types::FsyncFlags::DATASYNC).build().user_data(ud)
    }

    fn push(&mut self, e: squeue::Entry) {
        unsafe { self.ring.submission().push(&e).expect("sq full") };
    }

    fn record(&mut self, arrivals: &[u64], done: u64, flush_lat: u64) {
        for a in arrivals {
            self.lat.record(done.saturating_sub(*a).max(1)).ok();
        }
        self.flat.record(flush_lat.max(1)).ok();
        self.records += arrivals.len() as u64;
        self.flushes += 1;
        self.bytes += (arrivals.len() * self.io_len) as u64;
    }

    /// open-loop arrivals: push scheduled arrivals up to `now`
    fn gen_arrivals(&self, pending: &mut VecDeque<u64>, next_arr: &mut u64, now: u64) {
        let step = 1_000_000_000 / self.a.rate;
        while *next_arr <= now {
            pending.push_back(*next_arr);
            *next_arr += step;
            if pending.len() > 2_000_000 {
                break;
            }
        }
    }

    fn wait_until(&self, t: u64) {
        loop {
            let n = now_ns(&self.t0);
            if n >= t {
                return;
            }
            let d = t - n;
            if d > 60_000 {
                std::thread::sleep(Duration::from_nanos(d - 40_000));
            } else {
                std::hint::spin_loop();
            }
        }
    }

    fn run_group(&mut self, mode: &str) {
        let end = (self.a.secs * 1e9) as u64;
        let (b, w) = (if mode == "group" { self.a.batch } else { 1 }, self.a.window_us * 1000);
        let open = self.a.rate > 0;
        let mut pending: VecDeque<u64> = VecDeque::new();
        let mut next_arr = 0u64;
        loop {
            let mut now = now_ns(&self.t0);
            if now >= end {
                break;
            }
            let arrivals: Vec<u64>;
            if open {
                self.gen_arrivals(&mut pending, &mut next_arr, now);
                if pending.is_empty() {
                    self.wait_until(next_arr.min(end));
                    continue;
                }
                // collect window or fill to B
                let first = *pending.front().unwrap();
                while pending.len() < b && now_ns(&self.t0) < first + w {
                    now = now_ns(&self.t0);
                    self.gen_arrivals(&mut pending, &mut next_arr, now);
                    std::hint::spin_loop();
                }
                let n = b.min(pending.len());
                arrivals = pending.drain(..n).collect();
                if pending.len() > 1_000_000 {
                    break;
                }
            } else {
                arrivals = vec![now; b];
            }
            let n = arrivals.len();
            let t_sub = now_ns(&self.t0);
            let dsync = mode == "dsync";
            for i in 0..n {
                let e = self.write_sqe(i, dsync, false, i as u64);
                self.push(e);
            }
            self.ring.submit_and_wait(n).expect("submit");
            for c in self.ring.completion().take(n) {
                assert!(c.result() == self.io_len as i32, "write res {}", c.result());
            }
            let (flush_start, done);
            if mode == "group" {
                flush_start = now_ns(&self.t0);
                let e = self.fsync_sqe(1 << 40);
                self.push(e);
                self.ring.submit_and_wait(1).expect("fsync");
                let c = self.ring.completion().next().expect("cqe");
                assert!(c.result() >= 0, "fsync res {}", c.result());
                done = now_ns(&self.t0);
            } else {
                // per-write durability: the write CQE is the durable point
                done = now_ns(&self.t0);
                flush_start = t_sub;
            }
            self.record(&arrivals, done, done - flush_start);
        }
    }

    fn run_linked(&mut self) {
        struct Chain {
            arrivals: Vec<u64>,
            writes_left: usize,
            last_write: u64,
        }
        let end = (self.a.secs * 1e9) as u64;
        let b = self.a.batch;
        let open = self.a.rate > 0;
        let mut pending: VecDeque<u64> = VecDeque::new();
        let mut next_arr = 0u64;
        let mut chains: Vec<Option<Chain>> = (0..self.a.chains).map(|_| None).collect();
        let mut inflight = 0usize;
        let mut slot_base = vec![0usize; self.a.chains];
        for (i, s) in slot_base.iter_mut().enumerate() {
            *s = i * b;
        }
        loop {
            let now = now_ns(&self.t0);
            let stop = now >= end;
            if open {
                self.gen_arrivals(&mut pending, &mut next_arr, now);
                if pending.len() > 1_000_000 {
                    break;
                }
            }
            // submit new chains
            if !stop {
                while inflight < self.a.chains {
                    let arrivals: Vec<u64> = if open {
                        if pending.is_empty() {
                            break;
                        }
                        let n = b.min(pending.len());
                        pending.drain(..n).collect()
                    } else {
                        vec![now; b]
                    };
                    let cid = chains.iter().position(|c| c.is_none()).unwrap();
                    let n = arrivals.len();
                    for i in 0..n {
                        let e = self.write_sqe(slot_base[cid] + i, false, true, ((cid as u64) << 8) | 1);
                        self.push(e);
                    }
                    let e = self.fsync_sqe(((cid as u64) << 8) | 2);
                    self.push(e);
                    chains[cid] = Some(Chain { arrivals, writes_left: n, last_write: 0 });
                    inflight += 1;
                }
            }
            if inflight == 0 {
                if stop {
                    break;
                }
                self.wait_until(next_arr.min(end));
                continue;
            }
            // wait for at least one CQE (bounded so that arrivals are picked up)
            if open && inflight < self.a.chains {
                let ts = types::Timespec::new().nsec(20_000);
                let args = types::SubmitArgs::new().timespec(&ts);
                let _ = self.ring.submitter().submit_with_args(1, &args);
            } else {
                self.ring.submit_and_wait(1).expect("wait");
            }
            let t = now_ns(&self.t0);
            let cqes: Vec<(u64, i32)> = self.ring.completion().map(|c| (c.user_data(), c.result())).collect();
            for (ud, res) in cqes {
                let cid = (ud >> 8) as usize;
                assert!(res >= 0, "cqe res {res} kind {}", ud & 0xff);
                let ch = chains[cid].as_mut().unwrap();
                if ud & 0xff == 1 {
                    ch.writes_left -= 1;
                    if ch.writes_left == 0 {
                        ch.last_write = t;
                    }
                } else {
                    let ch = chains[cid].take().unwrap();
                    self.record(&ch.arrivals, t, t.saturating_sub(ch.last_write));
                    inflight -= 1;
                }
            }
        }
    }
}

fn main() -> Result<()> {
    let a = Args::parse();
    if let Some(c) = a.cpu {
        unsafe {
            let mut set: libc::cpu_set_t = std::mem::zeroed();
            libc::CPU_SET(c, &mut set);
            libc::sched_setaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &set);
        }
    }
    let io_len = a.rec.div_ceil(4096) * 4096;
    let mut oflags = libc::O_DIRECT;
    if a.strategy == "odsync" {
        oflags |= libc::O_DSYNC;
    }
    let file = std::fs::OpenOptions::new().read(true).write(true).create(true).custom_flags(oflags).open(&a.file)?;
    let mut file_size = 0u64;
    if a.prealloc_gib > 0 {
        file_size = a.prealloc_gib << 30;
        if file.metadata()?.len() < file_size {
            bail!("file not preallocated/zero-filled to {} GiB (run prep first)", a.prealloc_gib);
        }
    } else {
        file.set_len(0)?;
    }
    let slots = match a.strategy.as_str() {
        "linked" => a.chains * a.batch,
        "group" => a.batch,
        _ => 1,
    };
    let buf = Aligned::new(slots * io_len);
    let ring = IoUring::new(512)?;
    ring.submitter().register_files(&[file.as_raw_fd()])?;
    let iov = libc::iovec { iov_base: buf.0 as *mut _, iov_len: buf.1 };
    unsafe { ring.submitter().register_buffers(&[iov])? };
    let bg = if a.bg_read {
        let path = a.file.clone();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let s2 = stop.clone();
        let cpu = a.cpu.map(|c| c + 1);
        let sz = file_size;
        Some((stop, std::thread::spawn(move || {
            if let Some(c) = cpu {
                unsafe {
                    let mut set: libc::cpu_set_t = std::mem::zeroed();
                    libc::CPU_SET(c, &mut set);
                    libc::sched_setaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &set);
                }
            }
            let f = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_DIRECT).open(&path).unwrap();
            let b = Aligned::new(64 << 10);
            let mut off = 0u64;
            let mut total = 0u64;
            while !s2.load(std::sync::atomic::Ordering::Relaxed) {
                let n = unsafe { libc::pread(f.as_raw_fd(), b.0 as *mut _, 64 << 10, off as i64) };
                if n <= 0 {
                    off = 0;
                    continue;
                }
                total += n as u64;
                off += n as u64;
                if sz != 0 && off + (64 << 10) > sz {
                    off = 0;
                }
            }
            total
        })))
    } else {
        None
    };
    let mut bench = Bench {
        io_len,
        file_size,
        next_off: 0,
        buf,
        t0: Instant::now(),
        lat: Histogram::new_with_bounds(1, 60_000_000_000, 3)?,
        flat: Histogram::new_with_bounds(1, 60_000_000_000, 3)?,
        records: 0,
        flushes: 0,
        bytes: 0,
        ring,
        a: a.clone(),
    };
    let cpu0 = thread_cpu_ns();
    bench.t0 = Instant::now();
    match a.strategy.as_str() {
        "group" | "dsync" | "odsync" => bench.run_group(&a.strategy),
        "linked" => bench.run_linked(),
        s => bail!("unknown strategy {s}"),
    }
    let wall = bench.t0.elapsed().as_secs_f64();
    let cpu = (thread_cpu_ns() - cpu0) as f64 / 1e9;
    let bgbytes = bg.map(|(stop, h)| {
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        h.join().unwrap()
    });
    let us = |h: &Histogram<u64>, q: f64| h.value_at_quantile(q) as f64 / 1000.0;
    println!(
        "{{\"label\":\"{}\",\"strategy\":\"{}\",\"rec\":{},\"io_len\":{},\"batch\":{},\"window_us\":{},\"rate\":{},\"prealloc_gib\":{},\"bg_read\":{},\"records\":{},\"secs\":{:.3},\"rec_per_s\":{:.0},\"mb_per_s\":{:.1},\"flushes_per_s\":{:.0},\"rec_per_flush\":{:.2},\"dur_p50_us\":{:.1},\"dur_p99_us\":{:.1},\"dur_p999_us\":{:.1},\"flush_p50_us\":{:.1},\"flush_p99_us\":{:.1},\"cpu_pct\":{:.1},\"bg_read_mb_s\":{:.1}}}",
        a.label, a.strategy, a.rec, io_len, a.batch, a.window_us, a.rate, a.prealloc_gib, a.bg_read, bench.records, wall,
        bench.records as f64 / wall, bench.bytes as f64 / wall / 1e6, bench.flushes as f64 / wall,
        bench.records as f64 / bench.flushes.max(1) as f64,
        us(&bench.lat, 0.5), us(&bench.lat, 0.99), us(&bench.lat, 0.999), us(&bench.flat, 0.5), us(&bench.flat, 0.99),
        cpu / wall * 100.0, bgbytes.map(|b| b as f64 / wall / 1e6).unwrap_or(0.0)
    );
    Ok(())
}
