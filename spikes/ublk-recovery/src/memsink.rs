//! P0.12 RAM write buffer ("--sink mem"). Writes are appended to a ring in a memfd and
//! acknowledged from RAM; FLUSH (and FUA) drain the ring into the O_DIRECT backing file and
//! fdatasync it (stand-in for the S3 DELTA commit). The memfd outlives the daemon because a
//! keeper process holds a duplicate descriptor (SCM_RIGHTS). Single queue only.
//! Record: 32-byte header {magic u32, kind u32 (1 data, 2 pad), len u32, _ u32, off u64, seq u64}
//! followed by `len` payload bytes (a multiple of 4096). Positions are virtual u64 byte counters;
//! a record never straddles the physical end of the ring (a pad record fills the gap).
//! The ring header (first page) holds {magic, dev_id, generation, size, head, tail, seq}.
//! The backing file is opened WITHOUT O_DIRECT for this sink (see main.rs), so a drain is buffered writes plus one fdatasync.
//! Spike shortcuts: no crc, no torn-record handling beyond "head is published after the payload".
use anyhow::{bail, Result};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

const HDR_MAGIC: u64 = 0x4c554e49585f4d45; // "LUNIX_ME"
const REC_MAGIC: u32 = 0x4c52_4543;
const RHDR: u64 = 32;
const DATA_OFF: u64 = 4096;

#[repr(C)]
struct Hdr {
    magic: u64,
    dev_id: u64,
    generation: AtomicU64,
    size: u64,
    head: AtomicU64,
    tail: AtomicU64,
    seq: AtomicU64,
}

pub struct MemRing {
    pub fd: i32,
    hdr: *mut Hdr,
    data: *mut u8,
    size: u64,
    backing: i32,
    scratch: *mut u8,
    overlay: HashMap<u64, u64>,
}
// The ring is used from one queue thread only; the Mutex in the caller serialises access.
unsafe impl Send for MemRing {}

fn aligned(n: usize) -> *mut u8 {
    let mut p = std::ptr::null_mut();
    unsafe { libc::posix_memalign(&mut p, 4096, n) };
    p as *mut u8
}

impl MemRing {
    fn map(fd: i32, total: usize, backing: i32) -> Result<Self> {
        let p = unsafe { libc::mmap(std::ptr::null_mut(), total, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_SHARED, fd, 0) };
        if p == libc::MAP_FAILED {
            bail!("mmap memfd: {}", std::io::Error::last_os_error());
        }
        let hdr = p as *mut Hdr;
        let size = (total as u64) - DATA_OFF;
        Ok(Self { fd, hdr, data: unsafe { (p as *mut u8).add(DATA_OFF as usize) }, size, backing, scratch: aligned(2 << 20), overlay: HashMap::new() })
    }

    pub fn create(dev_id: u32, mib: u64, backing: i32) -> Result<Self> {
        let fd = unsafe { libc::memfd_create(b"lunix-wbuf\0".as_ptr() as *const _, 0) };
        if fd < 0 {
            bail!("memfd_create: {}", std::io::Error::last_os_error());
        }
        let total = (mib << 20) + DATA_OFF;
        if unsafe { libc::ftruncate(fd, total as i64) } != 0 {
            bail!("ftruncate memfd");
        }
        let r = Self::map(fd, total as usize, backing)?;
        unsafe {
            (*r.hdr).magic = HDR_MAGIC;
            (*r.hdr).dev_id = dev_id as u64;
            (*r.hdr).size = r.size;
            (*r.hdr).generation.store(1, Ordering::Release);
        }
        Ok(r)
    }

    /// Attach to a memfd received from the keeper; validates magic, device id and generation.
    pub fn attach(fd: i32, dev_id: u32, expect_gen: u64, backing: i32) -> Result<Self> {
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        unsafe { libc::fstat(fd, &mut st) };
        if (st.st_size as u64) < DATA_OFF * 2 {
            bail!("memfd too small");
        }
        let r = Self::map(fd, st.st_size as usize, backing)?;
        let h = unsafe { &*r.hdr };
        if h.magic != HDR_MAGIC {
            bail!("bad magic");
        }
        if h.dev_id != dev_id as u64 {
            bail!("device id mismatch");
        }
        if h.generation.load(Ordering::Acquire) != expect_gen {
            bail!("generation mismatch: buffer {} keeper {}", h.generation.load(Ordering::Acquire), expect_gen);
        }
        Ok(r)
    }

    pub fn generation(&self) -> u64 {
        unsafe { (*self.hdr).generation.load(Ordering::Acquire) }
    }
    pub fn bump_generation(&self) -> u64 {
        unsafe { (*self.hdr).generation.fetch_add(1, Ordering::AcqRel) + 1 }
    }
    pub fn pending_bytes(&self) -> u64 {
        unsafe { (*self.hdr).head.load(Ordering::Acquire) - (*self.hdr).tail.load(Ordering::Acquire) }
    }

    fn put_rec_hdr(&self, pos: u64, kind: u32, len: u32, off: u64, seq: u64) {
        let p = unsafe { self.data.add((pos % self.size) as usize) } as *mut u32;
        unsafe {
            p.write(REC_MAGIC);
            p.add(1).write(kind);
            p.add(2).write(len);
            p.add(3).write(0);
            (p.add(4) as *mut u64).write(off);
            (p.add(6) as *mut u64).write(seq);
        }
    }

    /// Append a data record and publish it; returns after the record is visible in the ring.
    pub fn append(&mut self, off: u64, data: &[u8]) -> Result<()> {
        let total = RHDR + data.len() as u64;
        let h = unsafe { &*self.hdr };
        loop {
            let (head, tail) = (h.head.load(Ordering::Acquire), h.tail.load(Ordering::Acquire));
            let to_end = self.size - head % self.size;
            let need = total + if to_end < total { to_end } else { 0 };
            if self.size - (head - tail) < need {
                if head == tail {
                    bail!("record larger than the ring");
                }
                self.drain()?;
                continue;
            }
            let mut head = head;
            if to_end < total {
                self.put_rec_hdr(head, 2, (to_end - RHDR) as u32, 0, 0);
                head += to_end;
            }
            let seq = h.seq.fetch_add(1, Ordering::AcqRel);
            self.put_rec_hdr(head, 1, data.len() as u32, off, seq);
            let pay = unsafe { self.data.add(((head + RHDR) % self.size) as usize) };
            unsafe { std::ptr::copy_nonoverlapping(data.as_ptr(), pay, data.len()) };
            let payload_pos = head + RHDR;
            for b in 0..(data.len() as u64 / 4096) {
                self.overlay.insert(off / 4096 + b, payload_pos + b * 4096);
            }
            h.head.store(head + total, Ordering::Release);
            return Ok(());
        }
    }

    /// Read: backing file first, then newer blocks from the ring.
    pub fn read(&self, off: u64, dst: &mut [u8]) -> i32 {
        let n = unsafe { libc::pread(self.backing, dst.as_mut_ptr() as *mut _, dst.len(), off as i64) };
        if n != dst.len() as isize {
            return -libc::EIO;
        }
        for b in 0..(dst.len() / 4096) {
            if let Some(pos) = self.overlay.get(&(off / 4096 + b as u64)) {
                unsafe {
                    let src = self.data.add((pos % self.size) as usize);
                    std::ptr::copy_nonoverlapping(src, dst.as_mut_ptr().add(b * 4096), 4096);
                }
            }
        }
        dst.len() as i32
    }

    /// Apply every pending record to the backing file and fdatasync; returns bytes applied.
    pub fn drain(&mut self) -> Result<u64> {
        let h = unsafe { &*self.hdr };
        let (mut pos, head) = (h.tail.load(Ordering::Acquire), h.head.load(Ordering::Acquire));
        let mut bytes = 0u64;
        while pos < head {
            let p = unsafe { self.data.add((pos % self.size) as usize) } as *const u32;
            let (magic, kind, len) = unsafe { (p.read(), p.add(1).read(), p.add(2).read() as u64) };
            if magic != REC_MAGIC {
                bail!("corrupt record at {pos}");
            }
            if kind == 2 {
                pos += RHDR + len;
                continue;
            }
            let off = unsafe { (p.add(4) as *const u64).read() };
            unsafe {
                std::ptr::copy_nonoverlapping(self.data.add(((pos + RHDR) % self.size) as usize), self.scratch, len as usize);
                let n = libc::pwrite(self.backing, self.scratch as *const _, len as usize, off as i64);
                if n != len as isize {
                    bail!("pwrite during drain: {}", std::io::Error::last_os_error());
                }
            }
            bytes += len;
            pos += RHDR + len;
        }
        if unsafe { libc::fdatasync(self.backing) } != 0 {
            bail!("fdatasync during drain");
        }
        h.tail.store(head, Ordering::Release);
        self.overlay.clear();
        Ok(bytes)
    }
}

// ---- keeper protocol over a Unix socket with SCM_RIGHTS (see keeper/keeper.py) ----

fn connect(path: &str) -> Result<i32> {
    let s = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0) };
    let mut a: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    a.sun_family = libc::AF_UNIX as u16;
    for (i, b) in path.bytes().enumerate() {
        a.sun_path[i] = b as _;
    }
    let r = unsafe { libc::connect(s, &a as *const _ as *const libc::sockaddr, std::mem::size_of::<libc::sockaddr_un>() as u32) };
    if r != 0 {
        bail!("keeper connect {path}: {}", std::io::Error::last_os_error());
    }
    Ok(s)
}

fn send_msg(s: i32, text: &str, fd: Option<i32>) {
    let mut iov = libc::iovec { iov_base: text.as_ptr() as *mut _, iov_len: text.len() };
    let mut cbuf = [0u8; 64];
    let mut mh: libc::msghdr = unsafe { std::mem::zeroed() };
    mh.msg_iov = &mut iov;
    mh.msg_iovlen = 1;
    if let Some(fd) = fd {
        mh.msg_control = cbuf.as_mut_ptr() as *mut _;
        mh.msg_controllen = unsafe { libc::CMSG_SPACE(4) } as _;
        unsafe {
            let c = libc::CMSG_FIRSTHDR(&mh);
            (*c).cmsg_level = libc::SOL_SOCKET;
            (*c).cmsg_type = libc::SCM_RIGHTS;
            (*c).cmsg_len = libc::CMSG_LEN(4) as _;
            (libc::CMSG_DATA(c) as *mut i32).write(fd);
        }
    }
    unsafe { libc::sendmsg(s, &mh, 0) };
}

fn recv_msg(s: i32) -> (String, Option<i32>) {
    let mut buf = [0u8; 256];
    let mut iov = libc::iovec { iov_base: buf.as_mut_ptr() as *mut _, iov_len: buf.len() };
    let mut cbuf = [0u8; 64];
    let mut mh: libc::msghdr = unsafe { std::mem::zeroed() };
    mh.msg_iov = &mut iov;
    mh.msg_iovlen = 1;
    mh.msg_control = cbuf.as_mut_ptr() as *mut _;
    mh.msg_controllen = cbuf.len() as _;
    let n = unsafe { libc::recvmsg(s, &mut mh, libc::MSG_CMSG_CLOEXEC) };
    let text = String::from_utf8_lossy(&buf[..n.max(0) as usize]).to_string();
    let mut fd = None;
    unsafe {
        let c = libc::CMSG_FIRSTHDR(&mh);
        if !c.is_null() && (*c).cmsg_type == libc::SCM_RIGHTS {
            fd = Some((libc::CMSG_DATA(c) as *const i32).read());
        }
    }
    (text, fd)
}

pub fn keeper_put(path: &str, dev_id: u32, generation: u64, fd: i32) -> Result<()> {
    let s = connect(path)?;
    send_msg(s, &format!("PUT {dev_id} {generation}\n"), Some(fd));
    let (r, _) = recv_msg(s);
    unsafe { libc::close(s) };
    if !r.starts_with("OK") {
        bail!("keeper PUT failed: {r}");
    }
    Ok(())
}

/// Returns (memfd, generation) or None when the keeper has no buffer for the device.
pub fn keeper_get(path: &str, dev_id: u32) -> Result<Option<(i32, u64)>> {
    let s = connect(path)?;
    send_msg(s, &format!("GET {dev_id}\n"), None);
    let (r, fd) = recv_msg(s);
    unsafe { libc::close(s) };
    let mut it = r.split_whitespace();
    if it.next() == Some("OK") {
        let gen: u64 = it.next().and_then(|x| x.parse().ok()).unwrap_or(0);
        return Ok(fd.map(|f| (f, gen)));
    }
    Ok(None)
}
