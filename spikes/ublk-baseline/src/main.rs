//! P0.1 throwaway spike: libublk `null` and `loop` targets, sync (non-async)
//! queue handlers, one pinned thread per queue, no worker pool. Logic is taken
//! from libublk 0.4.8 `examples/null.rs` and `examples/loop.rs` (sync paths)
//! with these changes: logical/physical block size 4096, `attrs =
//! VOLATILE_CACHE | FUA`, O_DIRECT backing file, FLUSH = fdatasync (the
//! example uses sync_file_range), FUA write = RWF_DSYNC, explicit queue-thread
//! pinning. Shortcut allowed for a spike: the null target returns whatever is
//! in the buffer on READ (throughput probe only), the backing File is leaked.
use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use io_uring::{opcode, squeue, types};
use libublk::ctrl::{UblkCtrl, UblkCtrlBuilder, UblkQueueAffinity};
use libublk::io::{BufDescList, UblkDev, UblkIOCtx, UblkQueue};
use libublk::{sys, BufDesc, UblkFlags, UblkIORes};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::IntoRawFd;
use std::rc::Rc;

#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Create a device and serve it in the foreground until SIGINT/SIGTERM.
    Add {
        #[arg(long, default_value = "null")]
        target: String,
        #[arg(long)]
        backing: Option<String>,
        #[arg(long, default_value_t = 8)]
        queues: u16,
        #[arg(long, default_value_t = 256)]
        depth: u16,
        /// CPU list for queue threads, e.g. 0-3 or 0,2,4 (queue q -> cpus[q % n])
        #[arg(long)]
        cpus: Option<String>,
        #[arg(long, default_value_t = 64)]
        size_gib: u64,
        #[arg(long, default_value_t = 1 << 20)]
        buf_bytes: u32,
        /// extra ctrl flags, comma list: user_copy,update_size,quiesce
        #[arg(long, default_value = "")]
        flags: String,
        #[arg(long, default_value_t = -1)]
        id: i32,
    },
    Del {
        #[arg(long)]
        id: i32,
    },
    List,
    /// Print the kernel's GET_FEATURES mask decoded.
    Features,
}

pub const FLAG_NAMES: [&str; 19] = [
    "SUPPORT_ZERO_COPY", "URING_CMD_COMP_IN_TASK", "NEED_GET_DATA", "USER_RECOVERY",
    "USER_RECOVERY_REISSUE", "UNPRIVILEGED_DEV", "CMD_IOCTL_ENCODE", "USER_COPY", "ZONED",
    "USER_RECOVERY_FAIL_IO", "UPDATE_SIZE", "AUTO_BUF_REG", "QUIESCE", "PER_IO_DAEMON",
    "BUF_REG_OFF_DAEMON", "BATCH_IO", "INTEGRITY", "SAFE_STOP_DEV", "NO_AUTO_PART_SCAN",
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

fn pin(tid: i32, cpu: usize) {
    unsafe {
        let mut set: libc::cpu_set_t = std::mem::zeroed();
        libc::CPU_SET(cpu, &mut set);
        libc::sched_setaffinity(tid, std::mem::size_of::<libc::cpu_set_t>(), &set);
    }
}

fn sync_flags(is_flush: bool) -> u32 {
    let _ = is_flush;
    0
}

fn null_queue(qid: u16, dev: &UblkDev) {
    let bufs_rc = Rc::new(dev.alloc_queue_io_bufs());
    let bufs = bufs_rc.clone();
    let handler = move |q: &UblkQueue, tag: u16, _io: &UblkIOCtx| {
        let bytes = (q.get_iod(tag).nr_sectors << 9) as i32;
        q.complete_io_cmd_unified(tag, BufDesc::Slice(bufs[tag as usize].as_slice()), Ok(UblkIORes::Result(bytes)))
            .unwrap();
    };
    let queue = UblkQueue::new(qid, dev)
        .unwrap()
        .submit_fetch_commands_unified(BufDescList::Slices(Some(&bufs_rc)))
        .unwrap();
    queue.wait_and_handle_io(handler);
}

fn loop_make_sqe(iod: &sys::ublksrv_io_desc, buf: *mut u8) -> Option<squeue::Entry> {
    let op = iod.op_flags & 0xff;
    let off = iod.start_sector << 9;
    let bytes = iod.nr_sectors << 9;
    let fixed = types::Fixed(1);
    Some(match op {
        sys::UBLK_IO_OP_FLUSH => opcode::Fsync::new(fixed)
            .flags(types::FsyncFlags::DATASYNC)
            .build()
            .flags(squeue::Flags::FIXED_FILE),
        sys::UBLK_IO_OP_READ => opcode::Read::new(fixed, buf, bytes).offset(off).build().flags(squeue::Flags::FIXED_FILE),
        sys::UBLK_IO_OP_WRITE => {
            let mut w = opcode::Write::new(fixed, buf, bytes).offset(off);
            if iod.op_flags & sys::UBLK_IO_F_FUA != 0 {
                w = w.rw_flags(libc::RWF_DSYNC);
            }
            w.build().flags(squeue::Flags::FIXED_FILE)
        }
        _ => return None,
    })
}

fn loop_queue(qid: u16, dev: &UblkDev) {
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
        match loop_make_sqe(iod, slice.as_ptr() as *mut u8) {
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

fn add(target: String, backing: Option<String>, queues: u16, depth: u16, cpus: Option<String>, size_gib: u64,
       buf_bytes: u32, flags: String, id: i32) -> Result<()> {
    let cpus = cpus.map(|c| parse_cpus(&c)).transpose()?;
    let mut cflags: u64 = sys::UBLK_F_CMD_IOCTL_ENCODE as u64;
    for f in flags.split(',').filter(|s| !s.is_empty()) {
        cflags |= match f {
            "user_copy" => sys::UBLK_F_USER_COPY as u64,
            "update_size" => sys::UBLK_F_UPDATE_SIZE as u64,
            "quiesce" => sys::UBLK_F_QUIESCE as u64,
            other => bail!("unknown flag {other}"),
        };
    }
    // Block SIGINT/SIGTERM before any thread exists so only sigwait sees them.
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, libc::SIGINT);
        libc::sigaddset(&mut set, libc::SIGTERM);
        libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
    }
    let ctrl = UblkCtrlBuilder::default()
        .name("lunix_spike")
        .id(id)
        .depth(depth)
        .nr_queues(queues)
        .io_buf_bytes(buf_bytes)
        .ctrl_flags(cflags)
        .dev_flags(UblkFlags::UBLK_DEV_F_ADD_DEV)
        .build()
        .context("build ctrl")?;
    let is_loop = target == "loop";
    let backing_fd = if is_loop {
        let p = backing.context("--backing required for loop")?;
        let f = std::fs::OpenOptions::new().read(true).write(true).custom_flags(libc::O_DIRECT).open(&p)?;
        Some((f.metadata()?.len(), f.into_raw_fd()))
    } else {
        None
    };
    let size = backing_fd.map(|b| b.0).unwrap_or(size_gib << 30);
    let tgt_init = move |dev: &mut UblkDev| {
        if let Some((_, fd)) = backing_fd {
            let n = dev.tgt.nr_fds as usize;
            dev.tgt.fds[n] = fd;
            dev.tgt.nr_fds += 1;
        }
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
    let wh = move |c: &UblkCtrl| {
        for q in 0..queues {
            let mut aff = UblkQueueAffinity::new();
            if c.get_queue_affinity(q as u32, &mut aff).is_ok() {
                eprintln!("queue {q} kernel affinity {:?}", aff.to_bits_vec());
            }
            // Pin to cpus[c % n] where c is the CPU the kernel maps this queue to, so
            // that queues serving distinct submit CPUs get distinct daemon CPUs.
            if let (Some(cl), Ok(tid)) = (&cpus, c.get_queue_tid(q as u32)) {
                let kc = aff.get_first_cpu().unwrap_or(q as usize);
                pin(tid, cl[kc % cl.len()]);
            }
        }
        println!("READY id={} bdev={} features={:#x}", c.dev_info().dev_id, c.get_bdev_path(), c.dev_info().flags);
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
        c.kill_dev().ok();
    };
    if is_loop {
        ctrl.run_target(tgt_init, loop_queue, wh)?;
    } else {
        ctrl.run_target(tgt_init, null_queue, wh)?;
    }
    let _ = sync_flags(false);
    Ok(())
}

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Add { target, backing, queues, depth, cpus, size_gib, buf_bytes, flags, id } => {
            add(target, backing, queues, depth, cpus, size_gib, buf_bytes, flags, id)
        }
        Cmd::Del { id } => {
            UblkCtrl::new_simple(id)?.del_dev()?;
            Ok(())
        }
        Cmd::List => {
            UblkCtrl::for_each_dev_id(|id| UblkCtrl::new_simple(id as i32).unwrap().dump());
            Ok(())
        }
        Cmd::Features => {
            match UblkCtrl::get_features() {
                None => println!("GET_FEATURES not supported"),
                Some(m) => {
                    println!("GET_FEATURES mask = {m:#x}\n\n| flag | bit | set |\n|---|---|---|");
                    for (b, n) in FLAG_NAMES.iter().enumerate() {
                        println!("| UBLK_F_{n} | {b} | {} |", if m >> b & 1 == 1 { "yes" } else { "no" });
                    }
                }
            }
            Ok(())
        }
    }
}
