//! P0.2 throwaway spike: one ublk target with three data-transfer modes
//! (`copy`, `user-copy`, `batch`) and three work levels (`none`, `hash`,
//! `hash+lz4`) so that only the transfer mechanism differs. Spike shortcuts:
//! raw buffer pointers are handed to worker threads (the buffer of a tag is
//! stable until its completion is committed), statistics are global atomics,
//! READ returns a deterministic LBA pattern when work != none.
//! `--workers N` moves hash/lz4 (and, in user-copy mode, the pread/pwrite on
//! /dev/ublkcN) to a bounded OS-thread pool; completions return to the queue
//! thread through a mutexed vec plus an eventfd (queue-thread-only commit, the
//! only model libublk 0.4.8 exposes).
use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use io_uring::{opcode, types};
use libublk::ctrl::{UblkCtrl, UblkCtrlBuilder, UblkQueueAffinity};
use libublk::io::{BufDescList, UblkDev, UblkIOCtx, UblkQueue};
use libublk::{sys, BufDesc, UblkBatchBuffers, UblkBatchCompletion, UblkBatchConfig, UblkBatchQueue, UblkFlags, UblkIORes};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::{Arc, Mutex};

static WR_BYTES: AtomicU64 = AtomicU64::new(0);
static LZ_OUT: AtomicU64 = AtomicU64::new(0);
static LZ_IN: AtomicU64 = AtomicU64::new(0);
const EVT_TAG: u16 = 0xfffe;

#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    Add {
        /// copy | user-copy | batch
        #[arg(long, default_value = "copy")]
        mode: String,
        /// none | hash | hash+lz4
        #[arg(long, default_value = "none")]
        work: String,
        #[arg(long, default_value_t = 0)]
        workers: usize,
        #[arg(long, default_value_t = 16)]
        queues: u16,
        #[arg(long, default_value_t = 256)]
        depth: u16,
        #[arg(long)]
        cpus: Option<String>,
        #[arg(long, default_value_t = 64)]
        size_gib: u64,
        #[arg(long, default_value_t = 1 << 20)]
        buf_bytes: u32,
        /// tags per batch fetch buffer (batch mode)
        #[arg(long, default_value_t = 64)]
        batch_size: u16,
    },
    Del {
        #[arg(long)]
        id: i32,
    },
}

#[derive(Clone, Copy, PartialEq)]
enum Work {
    None,
    Hash,
    HashLz4,
}

#[derive(Clone, Copy)]
struct SendPtr(usize);

struct CompQ {
    done: Mutex<Vec<(u16, i32)>>,
    efd: i32,
}

struct Job {
    tag: u16,
    qid: u16,
    op: u32,
    off_sectors: u64,
    len: usize,
    buf: SendPtr,
    comp: Arc<CompQ>,
}

#[derive(Clone)]
struct Cfg {
    work: Work,
    user_copy: bool,
    cdev: i32,
    tx: Option<SyncSender<Job>>,
}

fn parse_cpus(s: &str) -> Result<Vec<usize>> {
    let mut v = vec![];
    for part in s.split(',') {
        if let Some((a, b)) = part.split_once('-') {
            v.extend(a.parse::<usize>()?..=b.parse::<usize>()?);
        } else {
            v.push(part.parse()?);
        }
    }
    Ok(v)
}

fn pin(tid: i32, cpu: usize) {
    unsafe {
        let mut set: libc::cpu_set_t = std::mem::zeroed();
        libc::CPU_SET(cpu, &mut set);
        libc::sched_setaffinity(tid, std::mem::size_of::<libc::cpu_set_t>(), &set);
    }
}

fn fill_pattern(dst: &mut [u8], sector: u64) {
    let base = sector << 6; // 8-byte words from the LBA
    for (i, c) in dst.chunks_exact_mut(8).enumerate() {
        c.copy_from_slice(&(base + i as u64).to_le_bytes());
    }
}

fn hash_and_lz4(data: &[u8], work: Work, lz: &mut Vec<u8>) {
    std::hint::black_box(blake3::hash(data));
    WR_BYTES.fetch_add(data.len() as u64, Relaxed);
    if work == Work::HashLz4 {
        let (mut i, mut o) = (0u64, 0u64);
        for ch in data.chunks(64 << 10) {
            if lz.len() < lz4_flex::block::get_maximum_output_size(ch.len()) {
                lz.resize(lz4_flex::block::get_maximum_output_size(64 << 10), 0);
            }
            o += lz4_flex::block::compress_into(ch, lz).unwrap_or(0) as u64;
            i += ch.len() as u64;
        }
        LZ_IN.fetch_add(i, Relaxed);
        LZ_OUT.fetch_add(o, Relaxed);
    }
}

/// The target logic; identical for every mode. `scratch` is used only in
/// user-copy mode. Returns the request result.
fn do_io(cfg: &Cfg, qid: u16, tag: u16, op: u32, off_sectors: u64, len: usize, buf: *mut u8,
         scratch: &mut Vec<u8>, lz: &mut Vec<u8>) -> i32 {
    match op {
        sys::UBLK_IO_OP_FLUSH => 0,
        sys::UBLK_IO_OP_READ | sys::UBLK_IO_OP_WRITE => {
            if cfg.work == Work::None {
                return len as i32;
            }
            let pos = UblkIOCtx::ublk_user_copy_pos(qid, tag, 0) as libc::off_t;
            let is_wr = op == sys::UBLK_IO_OP_WRITE;
            if cfg.user_copy {
                if scratch.len() < len {
                    scratch.resize(len, 0);
                }
                if is_wr {
                    let n = unsafe { libc::pread(cfg.cdev, scratch.as_mut_ptr() as *mut _, len, pos) };
                    if n != len as isize {
                        return -libc::EIO;
                    }
                    hash_and_lz4(&scratch[..len], cfg.work, lz);
                } else {
                    fill_pattern(&mut scratch[..len], off_sectors);
                    let n = unsafe { libc::pwrite(cfg.cdev, scratch.as_ptr() as *const _, len, pos) };
                    if n != len as isize {
                        return -libc::EIO;
                    }
                }
            } else if is_wr {
                hash_and_lz4(unsafe { std::slice::from_raw_parts(buf, len) }, cfg.work, lz);
            } else {
                fill_pattern(unsafe { std::slice::from_raw_parts_mut(buf, len) }, off_sectors);
            }
            len as i32
        }
        _ => -libc::EOPNOTSUPP,
    }
}

fn spawn_workers(n: usize, cfg: &Cfg, rx: Receiver<Job>) {
    let rx = Arc::new(Mutex::new(rx));
    for _ in 0..n {
        let rx = rx.clone();
        let cfg = cfg.clone();
        std::thread::spawn(move || {
            let (mut scratch, mut lz) = (Vec::new(), Vec::new());
            loop {
                let job = match rx.lock().unwrap().recv() {
                    Ok(j) => j,
                    Err(_) => return,
                };
                let res = do_io(&cfg, job.qid, job.tag, job.op, job.off_sectors, job.len, job.buf.0 as *mut u8,
                                &mut scratch, &mut lz);
                job.comp.done.lock().unwrap().push((job.tag, res));
                let one = 1u64;
                unsafe { libc::write(job.comp.efd, &one as *const u64 as *const _, 8) };
            }
        });
    }
}

fn arm_eventfd(q: &UblkQueue, efd: i32, buf: *mut u64) {
    let sqe = opcode::Read::new(types::Fd(efd), buf as *mut u8, 8)
        .build()
        .user_data(UblkIOCtx::build_user_data(EVT_TAG, 0, 0, true));
    q.ublk_submit_sqe_sync(sqe).unwrap();
}

fn new_comp() -> (Arc<CompQ>, *mut u64) {
    let efd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC) };
    let c = Arc::new(CompQ { done: Mutex::new(Vec::new()), efd });
    (c, Box::leak(Box::new(0u64)) as *mut u64)
}

fn queue_copy(qid: u16, dev: &UblkDev, cfg: Cfg) {
    let bufs_rc = std::rc::Rc::new(dev.alloc_queue_io_bufs());
    let bufs = bufs_rc.clone();
    let (comp, evbuf) = new_comp();
    let (mut scratch, mut lz) = (Vec::new(), Vec::new());
    let comp2 = comp.clone();
    let uc = cfg.user_copy;
    let handler = move |q: &UblkQueue, tag: u16, io: &UblkIOCtx| {
        let slice_for = |t: u16| -> BufDesc { if uc { BufDesc::Slice(&[]) } else { BufDesc::Slice(bufs[t as usize].as_slice()) } };
        if io.is_tgt_io() {
            // eventfd wakeup: commit everything the workers finished
            let done: Vec<_> = std::mem::take(&mut *comp2.done.lock().unwrap());
            for (t, res) in done {
                q.complete_io_cmd_unified(t, slice_for(t), Ok(UblkIORes::Result(res))).unwrap();
            }
            arm_eventfd(q, comp2.efd, evbuf);
            return;
        }
        let iod = q.get_iod(tag);
        let (op, off, len) = (iod.op_flags & 0xff, iod.start_sector, (iod.nr_sectors << 9) as usize);
        let bptr = bufs[tag as usize].as_ptr() as *mut u8;
        match &cfg.tx {
            Some(tx) if cfg.work != Work::None && (op == sys::UBLK_IO_OP_READ || op == sys::UBLK_IO_OP_WRITE) => {
                tx.send(Job { tag, qid, op, off_sectors: off, len, buf: SendPtr(bptr as usize), comp: comp2.clone() }).unwrap();
            }
            _ => {
                let res = do_io(&cfg, qid, tag, op, off, len, bptr, &mut scratch, &mut lz);
                q.complete_io_cmd_unified(tag, slice_for(tag), Ok(UblkIORes::Result(res))).unwrap();
            }
        }
    };
    let queue = UblkQueue::new(qid, dev)
        .unwrap()
        .submit_fetch_commands_unified(if uc { BufDescList::Slices(None) } else { BufDescList::Slices(Some(&bufs_rc)) })
        .unwrap();
    arm_eventfd(&queue, comp.efd, evbuf);
    queue.wait_and_handle_io(handler);
}

fn queue_batch(qid: u16, dev: &UblkDev, cfg: Cfg, batch_size: u16) {
    let queue = UblkQueue::new(qid, dev).unwrap();
    let buffers = dev.alloc_queue_io_bufs();
    let config = UblkBatchConfig::new()
        .with_fetch_buffer_count(8)
        .with_fetch_command_count(2)
        .with_max_inflight_commits(4)
        .with_tags_per_fetch_buffer(batch_size);
    let mut batch = UblkBatchQueue::new(&queue, UblkBatchBuffers::IoBufs(buffers), config).unwrap();
    assert!(batch.try_submit_completions(&[]).unwrap());
    let mut pending: Vec<UblkBatchCompletion> = Vec::new();
    let (comp, evbuf) = new_comp();
    let (mut scratch, mut lz) = (Vec::new(), Vec::new());
    arm_eventfd(&queue, comp.efd, evbuf);
    loop {
        let mut err = None;
        queue
            .flush_and_wake_io_tasks(
                |_ud, cqe, _last| {
                    let r = batch.handle_cqe(cqe, |b, tags| {
                        for &tag in tags {
                            let iod = queue.get_iod(tag);
                            let (op, off, len) = (iod.op_flags & 0xff, iod.start_sector, (iod.nr_sectors << 9) as usize);
                            let bptr = b.io_buf_mut(tag).unwrap().as_mut_ptr();
                            match &cfg.tx {
                                Some(tx) if cfg.work != Work::None && (op == sys::UBLK_IO_OP_READ || op == sys::UBLK_IO_OP_WRITE) => {
                                    tx.send(Job { tag, qid, op, off_sectors: off, len, buf: SendPtr(bptr as usize), comp: comp.clone() }).unwrap();
                                }
                                _ => {
                                    let res = do_io(&cfg, qid, tag, op, off, len, bptr, &mut scratch, &mut lz);
                                    pending.push(UblkBatchCompletion::new(tag, res));
                                }
                            }
                        }
                        Ok(())
                    });
                    match r {
                        Ok(true) => {}
                        Ok(false) => {
                            // our eventfd CQE: move worker results into the pending commit list
                            for (t, res) in std::mem::take(&mut *comp.done.lock().unwrap()) {
                                pending.push(UblkBatchCompletion::new(t, res));
                            }
                            arm_eventfd(&queue, comp.efd, evbuf);
                        }
                        Err(e) => err = Some(e),
                    }
                },
                1,
            )
            .unwrap();
        if let Some(e) = err {
            eprintln!("batch queue {qid} error: {e}");
            break;
        }
        if !pending.is_empty() {
            match batch.try_submit_completions(&pending) {
                Ok(true) => pending.clear(),
                Ok(false) => {}
                Err(e) => {
                    eprintln!("batch commit error: {e}");
                    break;
                }
            }
        }
        if batch.try_begin_shutdown().unwrap() && batch.is_shutdown_complete() {
            break;
        }
    }
}

fn status_kb(key: &str) -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .unwrap_or_default()
        .lines()
        .find(|l| l.starts_with(key))
        .and_then(|l| l.split_whitespace().nth(1)?.parse().ok())
        .unwrap_or(0)
}

#[allow(clippy::too_many_arguments)]
fn add(mode: String, work: String, workers: usize, queues: u16, depth: u16, cpus: Option<String>, size_gib: u64,
       buf_bytes: u32, batch_size: u16) -> Result<()> {
    let cpus = cpus.map(|c| parse_cpus(&c)).transpose()?;
    let wk = match work.as_str() {
        "none" => Work::None,
        "hash" => Work::Hash,
        "hash+lz4" => Work::HashLz4,
        o => bail!("bad --work {o}"),
    };
    let mut cflags = sys::UBLK_F_CMD_IOCTL_ENCODE as u64;
    match mode.as_str() {
        "copy" => {}
        "user-copy" => cflags |= sys::UBLK_F_USER_COPY as u64,
        "batch" => cflags |= sys::UBLK_F_BATCH_IO as u64,
        o => bail!("bad --mode {o}"),
    }
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, libc::SIGINT);
        libc::sigaddset(&mut set, libc::SIGTERM);
        libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
    }
    let ctrl = UblkCtrlBuilder::default()
        .name("lunix_dp")
        .depth(depth)
        .nr_queues(queues)
        .io_buf_bytes(buf_bytes)
        .ctrl_flags(cflags)
        .dev_flags(UblkFlags::UBLK_DEV_F_ADD_DEV)
        .build()
        .context("build ctrl")?;
    let size = size_gib << 30;
    let (tx, rx) = sync_channel::<Job>(65536);
    let cfg = Cfg { work: wk, user_copy: mode == "user-copy", cdev: -1, tx: if workers > 0 { Some(tx) } else { None } };
    let tgt_init = move |dev: &mut UblkDev| {
        dev.tgt.dev_size = size;
        dev.tgt.params = sys::ublk_params {
            types: sys::UBLK_PARAM_TYPE_BASIC,
            basic: sys::ublk_param_basic {
                attrs: sys::UBLK_ATTR_VOLATILE_CACHE | sys::UBLK_ATTR_FUA,
                logical_bs_shift: 12,
                physical_bs_shift: 12,
                io_min_shift: 12,
                io_opt_shift: 12,
                max_sectors: dev.dev_info.max_io_buf_bytes >> 9,
                dev_sectors: size >> 9,
                ..Default::default()
            },
            ..Default::default()
        };
        Ok(())
    };
    let is_batch = mode == "batch";
    let _ = SHARED.set(Shared { cfg_base: cfg, rx: Mutex::new(Some(rx)), workers, once: std::sync::Once::new() });
    let q_fn = move |qid: u16, dev: &UblkDev| {
        let sh = SHARED.get().unwrap();
        let mut c = sh.cfg_base.clone();
        c.cdev = dev.tgt.fds[0];
        // The worker pool needs the cdev fd, known only once the device exists.
        sh.once.call_once(|| {
            if sh.workers > 0 {
                spawn_workers(sh.workers, &c, sh.rx.lock().unwrap().take().unwrap());
            }
        });
        if is_batch {
            queue_batch(qid, dev, c, batch_size)
        } else {
            queue_copy(qid, dev, c)
        }
    };
    let wh = move |c: &UblkCtrl| {
        println!(
            "READY id={} bdev={} mode={mode} work={work} workers={workers} buf_total_bytes={}",
            c.dev_info().dev_id,
            c.get_bdev_path(),
            buf_bytes as u64 * depth as u64 * queues as u64
        );
        for q in 0..queues {
            let mut aff = UblkQueueAffinity::new();
            if c.get_queue_affinity(q as u32, &mut aff).is_ok() {
                if let (Some(cl), Ok(tid)) = (&cpus, c.get_queue_tid(q as u32)) {
                    pin(tid, cl[aff.get_first_cpu().unwrap_or(q as usize) % cl.len()]);
                }
            }
        }
        use std::io::Write;
        std::io::stdout().flush().ok();
        unsafe {
            let mut set: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut set);
            libc::sigaddset(&mut set, libc::SIGINT);
            libc::sigaddset(&mut set, libc::SIGTERM);
            let mut sig = 0;
            libc::sigwait(&set, &mut sig);
        }
        println!(
            "STATS write_bytes={} lz4_in={} lz4_out={} vm_rss_kb={} vm_hwm_kb={}",
            WR_BYTES.load(Relaxed), LZ_IN.load(Relaxed), LZ_OUT.load(Relaxed), status_kb("VmRSS:"), status_kb("VmHWM:")
        );
        std::io::stdout().flush().ok();
        c.kill_dev().ok();
    };
    ctrl.run_target(tgt_init, q_fn, wh)?;
    Ok(())
}

struct Shared {
    cfg_base: Cfg,
    rx: Mutex<Option<Receiver<Job>>>,
    workers: usize,
    once: std::sync::Once,
}
static SHARED: std::sync::OnceLock<Shared> = std::sync::OnceLock::new();

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Add { mode, work, workers, queues, depth, cpus, size_gib, buf_bytes, batch_size } => {
            add(mode, work, workers, queues, depth, cpus, size_gib, buf_bytes, batch_size)
        }
        Cmd::Del { id } => {
            UblkCtrl::new_simple(id)?.del_dev()?;
            Ok(())
        }
    }
}
