//! Minimal raw ublk control channel (io_uring URING_CMD on /dev/ublk-control).
//! libublk 0.4.8 rejects ctrl flags outside its own list (NO_AUTO_PART_SCAN,
//! SAFE_STOP_DEV, USER_RECOVERY_FAIL_IO...) and has no TRY_STOP_DEV, so the
//! feature probes and a few commands go through this.
use anyhow::{bail, Result};
use io_uring::{cqueue, opcode, squeue, types, IoUring};
use std::os::fd::AsRawFd;

pub const DIR_R: u32 = 2;
pub const DIR_RW: u32 = 3;
pub const CMD_ADD_DEV: (u8, u32) = (0x04, DIR_RW);
pub const CMD_DEL_DEV: (u8, u32) = (0x05, DIR_RW);
pub const CMD_STOP_DEV: (u8, u32) = (0x07, DIR_RW);
pub const CMD_GET_PARAMS: (u8, u32) = (0x09, DIR_R);
pub const CMD_GET_FEATURES: (u8, u32) = (0x13, DIR_R);
pub const CMD_UPDATE_SIZE: (u8, u32) = (0x15, DIR_RW);
pub const CMD_QUIESCE_DEV: (u8, u32) = (0x16, DIR_RW);
pub const CMD_TRY_STOP_DEV: (u8, u32) = (0x17, DIR_RW);

#[repr(C)]
#[derive(Default, Clone, Copy)]
pub struct DevInfo {
    pub nr_hw_queues: u16,
    pub queue_depth: u16,
    pub state: u16,
    pub pad0: u16,
    pub max_io_buf_bytes: u32,
    pub dev_id: u32,
    pub ublksrv_pid: i32,
    pub pad1: u32,
    pub flags: u64,
    pub ublksrv_flags: u64,
    pub owner_uid: u32,
    pub owner_gid: u32,
    pub reserved1: u64,
    pub reserved2: u64,
}

pub struct Ctl {
    ring: IoUring<squeue::Entry128, cqueue::Entry>,
    file: std::fs::File,
}

impl Ctl {
    pub fn open() -> Result<Self> {
        let file = std::fs::OpenOptions::new().read(true).write(true).open("/dev/ublk-control")?;
        let ring = IoUring::<squeue::Entry128, cqueue::Entry>::builder().build(4)?;
        Ok(Self { ring, file })
    }

    /// Returns the (negative errno | non-negative) result of the command.
    pub fn cmd(&mut self, c: (u8, u32), dev_id: u32, addr: u64, len: u16, data0: u64) -> Result<i32> {
        let op = (c.1 << 30) | (32 << 16) | ((b'u' as u32) << 8) | c.0 as u32;
        let mut raw = [0u8; 80];
        raw[0..4].copy_from_slice(&dev_id.to_le_bytes());
        raw[4..6].copy_from_slice(&0xffffu16.to_le_bytes());
        raw[6..8].copy_from_slice(&len.to_le_bytes());
        raw[8..16].copy_from_slice(&addr.to_le_bytes());
        raw[16..24].copy_from_slice(&data0.to_le_bytes());
        let sqe = opcode::UringCmd80::new(types::Fd(self.file.as_raw_fd()), op).cmd(raw).build().user_data(1);
        // SAFETY: addr/len point at memory owned by the caller for the duration of the call.
        unsafe { self.ring.submission().push(&sqe).map_err(|e| anyhow::anyhow!("sq full: {e}"))? };
        self.ring.submit_and_wait(1)?;
        match self.ring.completion().next() {
            Some(c) => Ok(c.result()),
            None => bail!("no cqe"),
        }
    }

    pub fn features(&mut self) -> Result<u64> {
        let mut f = 0u64;
        let r = self.cmd(CMD_GET_FEATURES, u32::MAX, &mut f as *mut u64 as u64, 8, 0)?;
        if r < 0 {
            bail!("GET_FEATURES failed: {}", std::io::Error::from_raw_os_error(-r));
        }
        Ok(f)
    }

    /// ADD_DEV with explicit flags; returns the device id or a negative errno.
    pub fn add_dev(&mut self, flags: u64, queues: u16, depth: u16, buf: u32) -> Result<i32> {
        let mut info = DevInfo { nr_hw_queues: queues, queue_depth: depth, max_io_buf_bytes: buf, dev_id: u32::MAX,
                                 ublksrv_pid: std::process::id() as i32, flags, ..Default::default() };
        let r = self.cmd(CMD_ADD_DEV, u32::MAX, &mut info as *mut DevInfo as u64, 64, 0)?;
        Ok(if r < 0 { r } else { info.dev_id as i32 })
    }

    pub fn simple(&mut self, c: (u8, u32), dev_id: u32, data0: u64) -> Result<i32> {
        self.cmd(c, dev_id, 0, 0, data0)
    }
}
