//! P0.3/P0.4 throwaway spike: a recoverable file-backed ublk target built on
//! libublk 0.4.8 (copy mode, sync handlers, or BATCH_IO with --batch). All state
//! lives in the O_DIRECT backing file, so a killed daemon can be replaced by a
//! new one that recovers the device. Counters (reads/writes/FUA/FLUSH) are
//! printed on SIGUSR1; SIGHUP toggles a fault ("ack but drop writes", the
//! negative control); SIGUSR2 quiesces the device and exits (planned
//! handover); SIGTERM/SIGINT stop and delete the device.
//! Spike shortcuts: global atomics, panics on unexpected ring errors.
mod memsink;
mod rawctl;
use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use io_uring::{opcode, squeue, types};
use libublk::ctrl::{UblkCtrl, UblkCtrlBuilder, UblkQueueAffinity};
use libublk::io::{BufDescList, UblkDev, UblkIOCtx, UblkQueue};
use libublk::{sys, BufDesc, UblkBatchBuffers, UblkBatchCompletion, UblkBatchConfig, UblkBatchQueue, UblkFlags, UblkIORes};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::IntoRawFd;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::time::Instant;

static READS: AtomicU64 = AtomicU64::new(0);
static WRITES: AtomicU64 = AtomicU64::new(0);
static FUA_WRITES: AtomicU64 = AtomicU64::new(0);
static FLUSHES: AtomicU64 = AtomicU64::new(0);
static OTHER: AtomicU64 = AtomicU64::new(0);
static FAULT: AtomicBool = AtomicBool::new(false);
static MEM: std::sync::OnceLock<std::sync::Mutex<memsink::MemRing>> = std::sync::OnceLock::new();

#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Serve a device in the foreground (see module comment for signals).
    Run {
        #[arg(long, default_value_t = -1)]
        dev_id: i32,
        #[arg(long)]
        backing: String,
        /// recover an existing quiesced device instead of adding one
        #[arg(long)]
        recover: bool,
        #[arg(long, default_value_t = 4)]
        queues: u16,
        #[arg(long, default_value_t = 128)]
        depth: u16,
        #[arg(long, default_value_t = 1 << 20)]
        buf_bytes: u32,
        /// create without USER_RECOVERY_REISSUE (negative control)
        #[arg(long)]
        no_reissue: bool,
        /// drop VOLATILE_CACHE from attrs (control for the write-cache sysfs test)
        #[arg(long)]
        no_volatile_cache: bool,
        #[arg(long)]
        batch: bool,
        /// extra raw ctrl flags (hex or dec), added with a raw ADD_DEV (libublk rejects some)
        #[arg(long)]
        extra_flags: Option<String>,
        /// use USER_RECOVERY_FAIL_IO instead of REISSUE (raw ADD_DEV)
        #[arg(long)]
        fail_io: bool,
        #[arg(long)]
        cpus: Option<String>,
        #[arg(long, default_value_t = 5000)]
        quiesce_timeout_ms: u64,
        /// write sink: file (O_DIRECT writes straight to the backing file) or mem (P0.12 RAM buffer)
        #[arg(long, default_value = "file")]
        sink: String,
        /// keeper socket (required with --sink mem)
        #[arg(long)]
        keeper: Option<String>,
        /// RAM buffer size in MiB (--sink mem)
        #[arg(long, default_value_t = 256)]
        mem_mib: u64,
    },
    Del { #[arg(long)] id: i32 },
    /// UPDATE_SIZE; with --backing the backing file is first extended (sparse) to the size.
    Resize {
        #[arg(long)]
        id: i32,
        #[arg(long)]
        bytes: u64,
        #[arg(long)]
        backing: Option<String>,
    },
    Quiesce { #[arg(long)] id: i32, #[arg(long, default_value_t = 5000)] timeout_ms: u64 },
    /// TRY_STOP_DEV (SAFE_STOP_DEV); prints the raw result.
    TryStop { #[arg(long)] id: i32 },
    /// plain STOP_DEV through the raw channel
    Stop { #[arg(long)] id: i32 },
    Info { #[arg(long)] id: i32 },
    /// Feature table (GET_FEATURES decoded). Live tests are driven by run-features.sh.
    Features,
    /// ADD_DEV only (no server) with raw flags; prints the id or errno; deletes it again unless --keep.
    ProbeAdd { #[arg(long)] flags: String, #[arg(long)] keep: bool },
}

pub const FLAGS: [(&str, &str, &str, &str, &str); 19] = [
    ("SUPPORT_ZERO_COPY", "0", "never", "6.15 functional (6.0 stub)", "Server cannot read the data (ADR-17: never)"),
    ("URING_CMD_COMP_IN_TASK", "1", "ignored", "6.0", "Set by the driver itself on every device"),
    ("NEED_GET_DATA", "2", "never", "6.0", "Extra round trip per write"),
    ("USER_RECOVERY", "3", "mandatory", "6.1", ""),
    ("USER_RECOVERY_REISSUE", "4", "mandatory", "6.1", ""),
    ("UNPRIVILEGED_DEV", "5", "not used", "6.3", "Forbids USER_COPY/ZC"),
    ("CMD_IOCTL_ENCODE", "6", "mandatory", "6.4", "Legacy opcodes are compiled out on this kernel"),
    ("USER_COPY", "7", "used if the spike wins", "6.5", "P0.2 decides"),
    ("ZONED", "8", "never", "6.6", ""),
    ("USER_RECOVERY_FAIL_IO", "9", "optional policy", "6.13", "fail instead of queue while the server is away"),
    ("UPDATE_SIZE", "10", "mandatory", "6.16", ""),
    ("AUTO_BUF_REG", "11", "never", "6.16", ""),
    ("QUIESCE", "12", "mandatory", "6.16", "needs USER_RECOVERY"),
    ("PER_IO_DAEMON", "13", "not used", "6.16", "Set by the driver itself on every device"),
    ("BUF_REG_OFF_DAEMON", "14", "never", "6.17", "Set by the driver itself on every device"),
    ("BATCH_IO", "15", "used when present", "7.0", "P0.2/P0.3 decide the default"),
    ("INTEGRITY", "16", "not used", "7.0", "Requires USER_COPY"),
    ("SAFE_STOP_DEV", "17", "used when present", "7.0", "Enables TRY_STOP_DEV; set by the driver itself"),
    ("NO_AUTO_PART_SCAN", "18", "used when present", "7.0", ""),
];

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

fn parse_num(s: &str) -> Result<u64> {
    Ok(match s.strip_prefix("0x") {
        Some(h) => u64::from_str_radix(h, 16)?,
        None => s.parse()?,
    })
}

fn pin(tid: i32, cpu: usize) {
    unsafe {
        let mut set: libc::cpu_set_t = std::mem::zeroed();
        libc::CPU_SET(cpu, &mut set);
        libc::sched_setaffinity(tid, std::mem::size_of::<libc::cpu_set_t>(), &set);
    }
}

fn file_sqe(iod: &sys::ublksrv_io_desc, buf: *mut u8) -> Option<squeue::Entry> {
    let op = iod.op_flags & 0xff;
    let off = iod.start_sector << 9;
    let bytes = iod.nr_sectors << 9;
    let fixed = types::Fixed(1);
    Some(match op {
        sys::UBLK_IO_OP_FLUSH => {
            FLUSHES.fetch_add(1, Relaxed);
            opcode::Fsync::new(fixed).flags(types::FsyncFlags::DATASYNC).build().flags(squeue::Flags::FIXED_FILE)
        }
        sys::UBLK_IO_OP_READ => {
            READS.fetch_add(1, Relaxed);
            opcode::Read::new(fixed, buf, bytes).offset(off).build().flags(squeue::Flags::FIXED_FILE)
        }
        sys::UBLK_IO_OP_WRITE => {
            let fua = iod.op_flags & sys::UBLK_IO_F_FUA != 0;
            WRITES.fetch_add(1, Relaxed);
            if fua {
                FUA_WRITES.fetch_add(1, Relaxed);
            }
            let mut w = opcode::Write::new(fixed, buf, bytes).offset(off);
            if fua {
                w = w.rw_flags(libc::RWF_DSYNC);
            }
            w.build().flags(squeue::Flags::FIXED_FILE)
        }
        _ => {
            OTHER.fetch_add(1, Relaxed);
            return None;
        }
    })
}

fn is_dropped_write(iod: &sys::ublksrv_io_desc) -> bool {
    FAULT.load(Relaxed) && (iod.op_flags & 0xff) == sys::UBLK_IO_OP_WRITE
}

fn copy_queue(qid: u16, dev: &UblkDev) {
    let bufs_rc = Rc::new(dev.alloc_queue_io_bufs());
    let bufs = bufs_rc.clone();
    let handler = move |q: &UblkQueue, tag: u16, io: &UblkIOCtx| {
        let slice = bufs[tag as usize].as_slice();
        let done = |res: i32| {
            q.complete_io_cmd_unified(tag, BufDesc::Slice(slice), Ok(UblkIORes::Result(res))).unwrap();
        };
        if io.is_tgt_io() {
            let res = io.result();
            if res != -libc::EAGAIN {
                return done(res);
            }
        }
        let iod = q.get_iod(tag);
        if is_dropped_write(iod) {
            return done((iod.nr_sectors << 9) as i32);
        }
        match file_sqe(iod, slice.as_ptr() as *mut u8) {
            None => done(-libc::EINVAL),
            Some(sqe) => {
                let ud = UblkIOCtx::build_user_data(tag, iod.op_flags & 0xff, 0, true);
                q.ublk_submit_sqe_sync(sqe.user_data(ud)).unwrap();
            }
        }
    };
    let queue = UblkQueue::new(qid, dev)
        .unwrap()
        .submit_fetch_commands_unified(BufDescList::Slices(Some(&bufs_rc)))
        .unwrap();
    queue.wait_and_handle_io(handler);
}

fn batch_queue(qid: u16, dev: &UblkDev) {
    let queue = UblkQueue::new(qid, dev).unwrap();
    let buffers = dev.alloc_queue_io_bufs();
    let config = UblkBatchConfig::new()
        .with_fetch_buffer_count(8)
        .with_fetch_command_count(2)
        .with_max_inflight_commits(4)
        .with_tags_per_fetch_buffer(32);
    let mut batch = UblkBatchQueue::new(&queue, UblkBatchBuffers::IoBufs(buffers), config).unwrap();
    assert!(batch.try_submit_completions(&[]).unwrap());
    let mut pending: Vec<UblkBatchCompletion> = Vec::new();
    loop {
        let mut err = None;
        queue
            .flush_and_wake_io_tasks(
                |ud, cqe, _last| {
                    let r = batch.handle_cqe(cqe, |b, tags| {
                        for &tag in tags {
                            let iod = queue.get_iod(tag);
                            if is_dropped_write(iod) {
                                pending.push(UblkBatchCompletion::new(tag, (iod.nr_sectors << 9) as i32));
                                continue;
                            }
                            let bptr = b.io_buf_mut(tag).unwrap().as_mut_ptr();
                            match file_sqe(iod, bptr) {
                                None => pending.push(UblkBatchCompletion::new(tag, -libc::EINVAL)),
                                Some(sqe) => {
                                    let ud = UblkIOCtx::build_user_data(tag, iod.op_flags & 0xff, 0, true);
                                    queue.ublk_submit_sqe_sync(sqe.user_data(ud)).unwrap();
                                }
                            }
                        }
                        Ok(())
                    });
                    match r {
                        Ok(true) => {}
                        Ok(false) => {
                            // completion of our file I/O
                            let tag = UblkIOCtx::user_data_to_tag(ud) as u16;
                            let res = cqe.result();
                            if res == -libc::EAGAIN {
                                let iod = queue.get_iod(tag);
                                let bptr = batch.io_buf_mut(tag).unwrap().as_mut_ptr();
                                if let Some(sqe) = file_sqe(iod, bptr) {
                                    queue.ublk_submit_sqe_sync(sqe.user_data(ud)).unwrap();
                                }
                            } else {
                                pending.push(UblkBatchCompletion::new(tag, res));
                            }
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

/// Lost or unusable RAM buffer: never resume. Delete the device so that I/O fails with EIO.
fn refuse(dev_id: i32, why: &str) -> ! {
    println!("REFUSED {why}");
    if let Ok(mut ctl) = rawctl::Ctl::open() {
        let r = ctl.simple(rawctl::CMD_DEL_DEV, dev_id as u32, 0);
        println!("REFUSED deleted device {dev_id}: {r:?}");
    }
    let _ = std::fs::remove_file(format!("/run/ublksrv/{:04}.json", dev_id));
    use std::io::Write;
    std::io::stdout().flush().ok();
    std::process::exit(3);
}

fn mem_queue(qid: u16, dev: &UblkDev) {
    let bufs_rc = Rc::new(dev.alloc_queue_io_bufs());
    let bufs = bufs_rc.clone();
    let handler = move |q: &UblkQueue, tag: u16, _io: &UblkIOCtx| {
        let iod = q.get_iod(tag);
        let (op, off, len) = (iod.op_flags & 0xff, iod.start_sector << 9, (iod.nr_sectors << 9) as usize);
        let slice = bufs[tag as usize].as_slice();
        let res = {
            let mut m = MEM.get().unwrap().lock().unwrap();
            match op {
                sys::UBLK_IO_OP_WRITE => {
                    WRITES.fetch_add(1, Relaxed);
                    let fua = iod.op_flags & sys::UBLK_IO_F_FUA != 0;
                    if FAULT.load(Relaxed) {
                        len as i32
                    } else if m.append(off, &slice[..len]).is_err() {
                        -libc::EIO
                    } else if fua {
                        FUA_WRITES.fetch_add(1, Relaxed);
                        if m.drain().is_err() { -libc::EIO } else { len as i32 }
                    } else {
                        len as i32
                    }
                }
                sys::UBLK_IO_OP_READ => {
                    READS.fetch_add(1, Relaxed);
                    // SAFETY: the buffer of a tag is owned by this request until it is completed.
                    let dst = unsafe { std::slice::from_raw_parts_mut(slice.as_ptr() as *mut u8, len) };
                    m.read(off, dst)
                }
                sys::UBLK_IO_OP_FLUSH => {
                    FLUSHES.fetch_add(1, Relaxed);
                    if m.drain().is_err() { -libc::EIO } else { 0 }
                }
                _ => -libc::EINVAL,
            }
        };
        q.complete_io_cmd_unified(tag, BufDesc::Slice(slice), Ok(UblkIORes::Result(res))).unwrap();
    };
    let queue = UblkQueue::new(qid, dev)
        .unwrap()
        .submit_fetch_commands_unified(BufDescList::Slices(Some(&bufs_rc)))
        .unwrap();
    queue.wait_and_handle_io(handler);
}

fn sigset(sigs: &[i32]) -> libc::sigset_t {
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        for s in sigs {
            libc::sigaddset(&mut set, *s);
        }
        set
    }
}

fn print_counters() {
    println!(
        "COUNTERS reads={} writes={} fua_writes={} flushes={} other={}",
        READS.load(Relaxed), WRITES.load(Relaxed), FUA_WRITES.load(Relaxed), FLUSHES.load(Relaxed), OTHER.load(Relaxed)
    );
    use std::io::Write;
    std::io::stdout().flush().ok();
}

fn dump_params(c: &UblkCtrl, tag: &str) {
    let mut p = sys::ublk_params { len: std::mem::size_of::<sys::ublk_params>() as u32, ..Default::default() };
    match c.get_params(&mut p) {
        Ok(_) => println!(
            "PARAMS {tag} types={:#x} attrs={:#x} lbs_shift={} pbs_shift={} io_min_shift={} io_opt_shift={} max_sectors={} dev_sectors={}",
            p.types, p.basic.attrs, p.basic.logical_bs_shift, p.basic.physical_bs_shift, p.basic.io_min_shift,
            p.basic.io_opt_shift, p.basic.max_sectors, p.basic.dev_sectors
        ),
        Err(e) => println!("PARAMS {tag} error {e:?}"),
    }
    let i = c.dev_info();
    println!(
        "DEVINFO {tag} id={} state={} queues={} depth={} max_io_buf={} flags={:#x}",
        i.dev_id, i.state, i.nr_hw_queues, i.queue_depth, i.max_io_buf_bytes, i.flags
    );
    for q in 0..i.nr_hw_queues {
        let mut aff = UblkQueueAffinity::new();
        if c.get_queue_affinity(q as u32, &mut aff).is_ok() {
            println!("AFFINITY {tag} q{q} {:?}", aff.to_bits_vec());
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn run(dev_id: i32, backing: String, recover: bool, queues: u16, depth: u16, buf_bytes: u32, no_reissue: bool,
       no_vc: bool, batch: bool, extra_flags: Option<String>, fail_io: bool, cpus: Option<String>, qto: u64, sink: String, keeper: Option<String>, mem_mib: u64) -> Result<()> {
    let t0 = Instant::now();
    let cpus = cpus.map(|c| parse_cpus(&c)).transpose()?;
    unsafe {
        libc::mlockall(libc::MCL_CURRENT | libc::MCL_FUTURE);
        let set = sigset(&[libc::SIGHUP, libc::SIGUSR1, libc::SIGUSR2, libc::SIGTERM, libc::SIGINT]);
        libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
    }
    std::thread::spawn(|| {
        let set = sigset(&[libc::SIGHUP, libc::SIGUSR1]);
        loop {
            let mut sig = 0;
            unsafe { libc::sigwait(&set, &mut sig) };
            if sig == libc::SIGUSR1 {
                print_counters();
            } else {
                let v = !FAULT.load(Relaxed);
                FAULT.store(v, Relaxed);
                println!("FAULT drop_writes={v}");
            }
        }
    });
    let f = std::fs::OpenOptions::new().read(true).write(true).custom_flags(libc::O_DIRECT).open(&backing)?;
    let size = f.metadata()?.len();
    let fd = f.into_raw_fd();
    let mut cflags: u64 = (sys::UBLK_F_CMD_IOCTL_ENCODE | sys::UBLK_F_USER_RECOVERY | sys::UBLK_F_QUIESCE | sys::UBLK_F_UPDATE_SIZE) as u64;
    if !no_reissue && !fail_io {
        cflags |= sys::UBLK_F_USER_RECOVERY_REISSUE as u64;
    }
    if fail_io {
        cflags |= sys::UBLK_F_USER_RECOVERY_FAIL_IO as u64;
    }
    if batch {
        cflags |= sys::UBLK_F_BATCH_IO as u64;
    }
    let mut id = dev_id;
    let mut raw_added = false;
    let mem_sink = sink == "mem";
    anyhow::ensure!(!mem_sink || (queues == 1 && !batch && keeper.is_some()), "--sink mem needs --queues 1, no --batch and --keeper");
    let mut mem_fd: Option<(i32, u64)> = None;
    if recover && mem_sink {
        // Decide before touching the device: no buffer or a mismatching one means no recovery.
        match memsink::keeper_get(keeper.as_deref().unwrap(), dev_id as u32) {
            Ok(Some(x)) => mem_fd = Some(x),
            Ok(None) => refuse(dev_id, "keeper holds no buffer for this device"),
            Err(e) => refuse(dev_id, &format!("keeper unreachable: {e}")),
        }
    }
    if recover {
        anyhow::ensure!(id >= 0, "--recover needs --dev-id");
        let c0 = UblkCtrl::new_simple(id).context("open existing device")?;
        println!("RECOVER_FOUND state={}", c0.dev_info().state);
        c0.start_user_recover().context("START_USER_RECOVERY")?;
        println!("RECOVER_STARTED ms={}", t0.elapsed().as_millis());
    } else if fail_io || extra_flags.is_some() {
        // libublk validates ctrl flags against its own list, so add the device
        // ourselves and attach libublk to it afterwards.
        let extra = extra_flags.as_deref().map(parse_num).transpose()?.unwrap_or(0);
        let mut ctl = rawctl::Ctl::open()?;
        let r = ctl.add_dev(cflags | extra, queues, depth, buf_bytes)?;
        anyhow::ensure!(r >= 0, "raw ADD_DEV failed: {}", std::io::Error::from_raw_os_error(-r));
        id = r;
        raw_added = true;
    }
    let mut b = UblkCtrlBuilder::default().name("lunix_rec").id(id).depth(depth).nr_queues(queues).io_buf_bytes(buf_bytes);
    if raw_added || (recover && fail_io) {
        // libublk rejects FAIL_IO in ctrl_flags; for an existing device the kernel already knows its flags
        b = b.dev_flags(if recover { UblkFlags::UBLK_DEV_F_RECOVER_DEV } else { UblkFlags::empty() });
    } else {
        b = b.ctrl_flags(cflags).dev_flags(if recover { UblkFlags::UBLK_DEV_F_RECOVER_DEV } else { UblkFlags::UBLK_DEV_F_ADD_DEV });
    }
    let ctrl = b.build().context("build ctrl")?;
    // The device must outlive this process (USER_RECOVERY); explicit del on SIGTERM.
    ctrl.disown();
    if mem_sink {
        let did = ctrl.dev_info().dev_id;
        let kp = keeper.as_deref().unwrap();
        let ring = if let Some((mfd, gen)) = mem_fd {
            let mut r = match memsink::MemRing::attach(mfd, did, gen, fd) {
                Ok(r) => r,
                Err(e) => refuse(did as i32, &format!("buffer rejected: {e}")),
            };
            let pending = r.pending_bytes();
            let t = Instant::now();
            let applied = r.drain().context("replay")?;
            println!("REPLAY pending_bytes={pending} applied_bytes={applied} ms={:.1} generation={}", t.elapsed().as_secs_f64() * 1e3, r.generation());
            let g = r.bump_generation();
            memsink::keeper_put(kp, did, g, r.fd)?;
            r
        } else {
            let r = memsink::MemRing::create(did, mem_mib, fd)?;
            memsink::keeper_put(kp, did, r.generation(), r.fd)?;
            println!("MEMBUF created mib={mem_mib} generation={}", r.generation());
            r
        };
        let _ = MEM.set(std::sync::Mutex::new(ring));
    }
    let attrs = if no_vc { 0 } else { sys::UBLK_ATTR_VOLATILE_CACHE | sys::UBLK_ATTR_FUA };
    let tgt_init = move |dev: &mut UblkDev| {
        let n = dev.tgt.nr_fds as usize;
        dev.tgt.fds[n] = fd;
        dev.tgt.nr_fds += 1;
        dev.tgt.dev_size = size;
        dev.tgt.params = sys::ublk_params {
            types: sys::UBLK_PARAM_TYPE_BASIC,
            basic: sys::ublk_param_basic {
                attrs,
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
    let wh = move |c: &UblkCtrl| {
        if let Some(cl) = &cpus {
            for q in 0..queues {
                if let Ok(tid) = c.get_queue_tid(q as u32) {
                    pin(tid, cl[q as usize % cl.len()]);
                }
            }
        }
        println!("READY id={} bdev={} recovered={recover} ms={}", c.dev_info().dev_id, c.get_bdev_path(), t0.elapsed().as_millis());
        dump_params(c, if recover { "after-recovery" } else { "after-add" });
        use std::io::Write;
        std::io::stdout().flush().ok();
        let set = sigset(&[libc::SIGTERM, libc::SIGINT, libc::SIGUSR2]);
        let mut sig = 0;
        unsafe { libc::sigwait(&set, &mut sig) };
        if sig == libc::SIGUSR2 {
            let r = c.quiesce_dev(qto);
            println!("QUIESCE result={r:?} ms={}", t0.elapsed().as_millis());
            std::io::stdout().flush().ok();
            // Do NOT return into run_target(): it would issue STOP_DEV on the quiesced device, which
            // deadlocked this daemon in the kernel on 2026-11-11 (see bench/ublk-recovery/RESULTS.md).
            // Exiting closes /dev/ublkcN, which is what moves the device to QUIESCED.
            std::process::exit(if r.is_ok() { 0 } else { 4 });
        } else {
            c.kill_dev().ok();
            println!("STOPPING");
        }
        std::io::stdout().flush().ok();
    };
    if mem_sink {
        ctrl.run_target(tgt_init, |q, d| mem_queue(q, d), wh)?;
    } else if batch {
        ctrl.run_target(tgt_init, |q, d| batch_queue(q, d), wh)?;
    } else {
        ctrl.run_target(tgt_init, |q, d| copy_queue(q, d), wh)?;
    }
    // Reached after SIGTERM (device stopped; delete it) or after quiesce (leave it).
    print_counters();
    Ok(())
}

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Run { dev_id, backing, recover, queues, depth, buf_bytes, no_reissue, no_volatile_cache, batch, extra_flags,
                   fail_io, cpus, quiesce_timeout_ms, sink, keeper, mem_mib } => run(dev_id, backing, recover, queues, depth,
                   buf_bytes, no_reissue, no_volatile_cache, batch, extra_flags, fail_io, cpus, quiesce_timeout_ms, sink, keeper, mem_mib),
        Cmd::Del { id } => {
            let mut ctl = rawctl::Ctl::open()?;
            let r = ctl.simple(rawctl::CMD_DEL_DEV, id as u32, 0)?;
            println!("DEL_DEV result={r}");
            let _ = std::fs::remove_file(format!("/run/ublksrv/{:04}.json", id));
            Ok(())
        }
        Cmd::Resize { id, bytes, backing } => {
            if let Some(p) = backing {
                let f = std::fs::OpenOptions::new().write(true).open(&p)?;
                if f.metadata()?.len() < bytes {
                    f.set_len(bytes)?;
                }
            }
            let mut ctl = rawctl::Ctl::open()?;
            let r = ctl.simple(rawctl::CMD_UPDATE_SIZE, id as u32, bytes >> 9)?;
            println!("UPDATE_SIZE bytes={bytes} sectors={} result={r} ({})", bytes >> 9,
                     if r < 0 { std::io::Error::from_raw_os_error(-r).to_string() } else { "ok".into() });
            Ok(())
        }
        Cmd::Quiesce { id, timeout_ms } => {
            let mut ctl = rawctl::Ctl::open()?;
            let r = ctl.simple(rawctl::CMD_QUIESCE_DEV, id as u32, timeout_ms)?;
            println!("QUIESCE_DEV result={r}");
            Ok(())
        }
        Cmd::TryStop { id } => {
            let mut ctl = rawctl::Ctl::open()?;
            let r = ctl.simple(rawctl::CMD_TRY_STOP_DEV, id as u32, 0)?;
            println!("TRY_STOP_DEV result={r} ({})", if r < 0 { std::io::Error::from_raw_os_error(-r).to_string() } else { "ok".into() });
            Ok(())
        }
        Cmd::Stop { id } => {
            let mut ctl = rawctl::Ctl::open()?;
            let r = ctl.simple(rawctl::CMD_STOP_DEV, id as u32, 0)?;
            println!("STOP_DEV result={r}");
            Ok(())
        }
        Cmd::Info { id } => {
            UblkCtrl::new_simple(id)?.dump();
            Ok(())
        }
        Cmd::ProbeAdd { flags, keep } => {
            let fl = parse_num(&flags)?;
            let mut ctl = rawctl::Ctl::open()?;
            let r = ctl.add_dev(fl, 1, 16, 1 << 20)?;
            if r < 0 {
                println!("ADD_DEV flags={fl:#x} rejected: {}", std::io::Error::from_raw_os_error(-r));
            } else {
                println!("ADD_DEV flags={fl:#x} accepted id={r}");
                if !keep {
                    ctl.simple(rawctl::CMD_DEL_DEV, r as u32, 0)?;
                }
            }
            Ok(())
        }
        Cmd::Features => {
            let m = rawctl::Ctl::open()?.features()?;
            println!("GET_FEATURES mask = {m:#x}\n");
            println!("| flag | bit | set on this kernel | ADR-17 class | first kernel (fact-check §E, not re-verified) | note |");
            println!("|---|---|---|---|---|---|");
            for (n, b, class, since, note) in FLAGS {
                let bit: u32 = b.parse().unwrap();
                println!("| UBLK_F_{n} | {b} | {} | {class} | {since} | {note} |", if m >> bit & 1 == 1 { "yes" } else { "no" });
            }
            Ok(())
        }
    }
}
