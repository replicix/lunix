//! The test daemon's file-backed [`BlockEngine`].
//!
//! The backing file is opened `O_DIRECT`. WRITE is `pwrite`, READ is `pread`,
//! FLUSH is `fdatasync`, FUA is `pwritev2` with `RWF_DSYNC`, DISCARD and
//! WRITE_ZEROES punch a hole (as in the phase 0 spike). The queue thread
//! never blocks: `on_request` copies a write's data and hands the job to a
//! small worker pool of its queue; the worker completes by pushing onto the
//! queue's done list and signalling the wake fd, and `poll` commits. All state
//! of the device lives in the backing file, so a killed daemon is replaced by
//! a new one over the same file.
//!
//! Fault toggle for the negative control: while `drop_writes` is on, WRITEs
//! are acknowledged and dropped.

use std::alloc::{alloc_zeroed, dealloc, Layout};
use std::collections::HashSet;
use std::fs::File;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard};

use lunix_iface::block::{
    BlockEngine, Completer, IoOp, IoRequest, IoStatus, QueueConfig, QueueHandler, ResizeHook, Tag,
};
use lunix_iface::stats::EngineStats;
use lunix_iface::wake::WakeFd;
use lunix_iface::{Error, Result};

const BLOCK: u64 = 4096;
const EINVAL: i32 = 22;
const EIO: i32 = 5;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// A zeroed, 4096-aligned buffer for `O_DIRECT`.
struct AlignedBuf {
    ptr: *mut u8,
    len: usize,
    layout: Layout,
}

// SAFETY: the buffer is plain bytes owned by this value.
unsafe impl Send for AlignedBuf {}

impl AlignedBuf {
    fn zeroed(len: usize) -> Self {
        let layout = Layout::from_size_align(len.max(BLOCK as usize), BLOCK as usize)
            .expect("aligned layout");
        // SAFETY: the layout has a non-zero size.
        let ptr = unsafe { alloc_zeroed(layout) };
        assert!(!ptr.is_null(), "out of memory");
        Self { ptr, len, layout }
    }

    fn from_slice(data: &[u8]) -> Self {
        let mut b = Self::zeroed(data.len());
        b.as_mut_slice().copy_from_slice(data);
        b
    }

    fn as_slice(&self) -> &[u8] {
        // SAFETY: `ptr` is valid for `len` bytes (allocated with at least that).
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }

    fn as_mut_slice(&mut self) -> &mut [u8] {
        // SAFETY: as above, and `&mut self` is unique.
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }
    }
}

impl Drop for AlignedBuf {
    fn drop(&mut self) {
        // SAFETY: allocated with `layout` in `zeroed`.
        unsafe { dealloc(self.ptr, self.layout) }
    }
}

struct Job {
    tag: Tag,
    op: IoOp,
    fua: bool,
    offset: u64,
    len: u32,
    data: Option<AlignedBuf>,
}

struct Done {
    tag: Tag,
    status: IoStatus,
    read: Option<AlignedBuf>,
}

/// Counters the harness reads.
#[derive(Default)]
struct Counts {
    reads: AtomicU64,
    writes: AtomicU64,
    flushes: AtomicU64,
    discards: AtomicU64,
    zeroes: AtomicU64,
    bytes_read: AtomicU64,
    bytes_written: AtomicU64,
    dropped_writes: AtomicU64,
}

struct Inner {
    file: File,
    size: AtomicU64,
    hook: Mutex<Option<Arc<dyn ResizeHook>>>,
    taken: Mutex<HashSet<u16>>,
    drop_writes: AtomicBool,
    /// One-shot fault: 0 off, 1 drop the next write, 2 drop the next FUA write.
    drop_once: AtomicU8,
    /// Worker threads per queue; 0 completes inline on the queue thread.
    workers: usize,
    counts: Counts,
}

/// The engine. Cloning shares the device.
#[derive(Clone)]
pub struct FileEngine(Arc<Inner>);

impl FileEngine {
    /// Wraps an `O_DIRECT` backing file of `size` bytes.
    pub fn new(file: File, size: u64, workers: usize) -> Self {
        Self(Arc::new(Inner {
            file,
            size: AtomicU64::new(size),
            hook: Mutex::new(None),
            taken: Mutex::new(HashSet::new()),
            drop_writes: AtomicBool::new(false),
            drop_once: AtomicU8::new(0),
            workers,
            counts: Counts::default(),
        }))
    }

    /// Turns the acknowledge-but-drop fault on or off.
    pub fn set_drop_writes(&self, on: bool) {
        self.0.drop_writes.store(on, Ordering::SeqCst);
    }

    /// Arms a one-shot fault: `1` drops the next write, `2` the next FUA
    /// write (acknowledged, never written), `0` disarms.
    pub fn set_drop_once(&self, mode: u8) {
        self.0.drop_once.store(mode, Ordering::SeqCst);
    }

    /// Whether the fault is on.
    pub fn drop_writes(&self) -> bool {
        self.0.drop_writes.load(Ordering::SeqCst)
    }

    /// Writes the dropped by the fault so far.
    pub fn dropped_writes(&self) -> u64 {
        self.0.counts.dropped_writes.load(Ordering::SeqCst)
    }
}

fn pread_full(fd: i32, buf: &mut [u8], offset: u64) -> std::io::Result<()> {
    let mut done = 0usize;
    while done < buf.len() {
        // SAFETY: the pointer and length describe the unread tail of `buf`.
        let n = unsafe {
            libc::pread(
                fd,
                buf[done..].as_mut_ptr().cast::<libc::c_void>(),
                buf.len() - done,
                (offset + done as u64) as libc::off_t,
            )
        };
        if n < 0 {
            return Err(std::io::Error::last_os_error());
        }
        if n == 0 {
            // Past the end of the file: reads as zeros.
            buf[done..].fill(0);
            return Ok(());
        }
        done += n as usize;
    }
    Ok(())
}

fn pwrite_full(fd: i32, buf: &[u8], offset: u64, dsync: bool) -> std::io::Result<()> {
    let mut done = 0usize;
    while done < buf.len() {
        let iov = libc::iovec {
            iov_base: buf[done..].as_ptr().cast_mut().cast::<libc::c_void>(),
            iov_len: buf.len() - done,
        };
        let flags = if dsync { libc::RWF_DSYNC } else { 0 };
        // SAFETY: one iovec over the unwritten tail of `buf`.
        let n =
            unsafe { libc::pwritev2(fd, &iov, 1, (offset + done as u64) as libc::off_t, flags) };
        if n < 0 {
            return Err(std::io::Error::last_os_error());
        }
        if n == 0 {
            return Err(std::io::Error::from_raw_os_error(EIO));
        }
        done += n as usize;
    }
    Ok(())
}

fn punch(fd: i32, offset: u64, len: u64) -> std::io::Result<()> {
    // SAFETY: plain integer arguments.
    let r = unsafe {
        libc::fallocate(
            fd,
            libc::FALLOC_FL_PUNCH_HOLE | libc::FALLOC_FL_KEEP_SIZE,
            offset as libc::off_t,
            len as libc::off_t,
        )
    };
    if r == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

fn errno_of(e: &std::io::Error) -> i32 {
    e.raw_os_error().unwrap_or(EIO)
}

impl Inner {
    fn run(&self, job: Job) -> Done {
        let fd = self.file.as_raw_fd();
        let c = &self.counts;
        let err = |e: std::io::Error| IoStatus::err(errno_of(&e));
        match job.op {
            IoOp::Write => {
                c.writes.fetch_add(1, Ordering::Relaxed);
                c.bytes_written
                    .fetch_add(u64::from(job.len), Ordering::Relaxed);
                let cur = self.drop_once.load(Ordering::SeqCst);
                let once = (cur == 1 || (cur == 2 && job.fua))
                    && self
                        .drop_once
                        .compare_exchange(cur, 0, Ordering::SeqCst, Ordering::SeqCst)
                        .is_ok();
                let status = if once || self.drop_writes.load(Ordering::SeqCst) {
                    c.dropped_writes.fetch_add(1, Ordering::Relaxed);
                    IoStatus::ok(job.len)
                } else {
                    let data = job.data.as_ref().map_or(&[][..], AlignedBuf::as_slice);
                    match pwrite_full(fd, data, job.offset, job.fua) {
                        Ok(()) => IoStatus::ok(job.len),
                        Err(e) => err(e),
                    }
                };
                Done {
                    tag: job.tag,
                    status,
                    read: None,
                }
            }
            IoOp::Read => {
                c.reads.fetch_add(1, Ordering::Relaxed);
                c.bytes_read
                    .fetch_add(u64::from(job.len), Ordering::Relaxed);
                let mut buf = AlignedBuf::zeroed(job.len as usize);
                match pread_full(fd, buf.as_mut_slice(), job.offset) {
                    Ok(()) => Done {
                        tag: job.tag,
                        status: IoStatus::ok(job.len),
                        read: Some(buf),
                    },
                    Err(e) => Done {
                        tag: job.tag,
                        status: err(e),
                        read: None,
                    },
                }
            }
            IoOp::Flush => {
                c.flushes.fetch_add(1, Ordering::Relaxed);
                let status = match self.file.sync_data() {
                    Ok(()) => IoStatus::ok(0),
                    Err(e) => err(e),
                };
                Done {
                    tag: job.tag,
                    status,
                    read: None,
                }
            }
            IoOp::Discard | IoOp::WriteZeroes => {
                if job.op == IoOp::Discard {
                    c.discards.fetch_add(1, Ordering::Relaxed);
                } else {
                    c.zeroes.fetch_add(1, Ordering::Relaxed);
                }
                let status = match punch(fd, job.offset, u64::from(job.len)) {
                    Ok(()) => IoStatus::ok(job.len),
                    Err(e) => err(e),
                };
                Done {
                    tag: job.tag,
                    status,
                    read: None,
                }
            }
        }
    }
}

struct FileQueue {
    inner: Arc<Inner>,
    queue: u16,
    max_io: u32,
    wake: WakeFd,
    tx: Option<Sender<Job>>,
    inline: bool,
    done: Arc<Mutex<Vec<Done>>>,
    workers: Vec<std::thread::JoinHandle<()>>,
}

impl BlockEngine for FileEngine {
    fn size_bytes(&self) -> u64 {
        self.0.size.load(Ordering::SeqCst)
    }

    fn queue_handler(&self, cfg: &QueueConfig) -> Result<Box<dyn QueueHandler>> {
        if !lock(&self.0.taken).insert(cfg.queue.0) {
            return Err(Error::invalid("queue already has a handler"));
        }
        let wake = WakeFd::new()?;
        let (tx, rx) = channel::<Job>();
        let rx: Arc<Mutex<Receiver<Job>>> = Arc::new(Mutex::new(rx));
        let done: Arc<Mutex<Vec<Done>>> = Arc::new(Mutex::new(Vec::new()));
        let mut workers = Vec::new();
        for i in 0..self.0.workers {
            let (rx, done, inner, wake) = (
                Arc::clone(&rx),
                Arc::clone(&done),
                Arc::clone(&self.0),
                wake.clone(),
            );
            let h = std::thread::Builder::new()
                .name(format!("testd-q{}w{i}", cfg.queue.0))
                .spawn(move || loop {
                    let job = match lock(&rx).recv() {
                        Ok(j) => j,
                        Err(_) => return,
                    };
                    let d = inner.run(job);
                    lock(&done).push(d);
                    wake.signal();
                })?;
            workers.push(h);
        }
        Ok(Box::new(FileQueue {
            inner: Arc::clone(&self.0),
            queue: cfg.queue.0,
            max_io: cfg.max_io_buf_bytes,
            wake,
            tx: Some(tx),
            inline: self.0.workers == 0,
            done,
            workers,
        }))
    }

    fn grow(&self, new_size_bytes: u64) -> Result<()> {
        let cur = self.0.size.load(Ordering::SeqCst);
        if !new_size_bytes.is_multiple_of(BLOCK) {
            return Err(Error::invalid("size is not a multiple of 4096"));
        }
        if new_size_bytes < cur {
            return Err(Error::Unsupported {
                what: "shrinking a device",
            });
        }
        if new_size_bytes == cur {
            return Ok(());
        }
        if self.0.file.metadata()?.len() < new_size_bytes {
            self.0.file.set_len(new_size_bytes)?; // sparse
        }
        self.0.size.store(new_size_bytes, Ordering::SeqCst);
        let hook = lock(&self.0.hook).clone();
        match hook {
            Some(h) => h.resized(new_size_bytes),
            None => Ok(()),
        }
    }

    fn set_on_resize(&self, hook: Arc<dyn ResizeHook>) {
        *lock(&self.0.hook) = Some(hook);
    }

    fn quiesce(&self) -> Result<()> {
        // Nothing is buffered: every acknowledged write is already in the
        // file (FUA and FLUSH made it durable), so a final commit is a sync.
        self.0.file.sync_all()?;
        Ok(())
    }

    fn stats(&self) -> EngineStats {
        let c = &self.0.counts;
        let l = |a: &AtomicU64| a.load(Ordering::Relaxed);
        EngineStats {
            bytes_written: l(&c.bytes_written),
            bytes_read: l(&c.bytes_read),
            ops_read: l(&c.reads),
            ops_write: l(&c.writes),
            ops_flush: l(&c.flushes),
            ops_discard: l(&c.discards),
            ops_write_zeroes: l(&c.zeroes),
            flush_groups: l(&c.flushes),
            ..EngineStats::default()
        }
    }
}

impl FileQueue {
    fn reject(&self, req: &IoRequest, done: &mut dyn Completer) {
        done.complete(req.tag, IoStatus::err(EINVAL));
    }
}

impl QueueHandler for FileQueue {
    fn on_request(&mut self, req: IoRequest, buf: &mut [u8], done: &mut dyn Completer) {
        let size = self.inner.size.load(Ordering::SeqCst);
        let len = u64::from(req.len);
        if req.op != IoOp::Flush {
            let ok = req.offset_bytes.is_multiple_of(BLOCK)
                && len.is_multiple_of(BLOCK)
                && len > 0
                && req.offset_bytes.checked_add(len).is_some_and(|e| e <= size)
                && (!matches!(req.op, IoOp::Read | IoOp::Write)
                    || (req.len <= self.max_io && buf.len() == req.len as usize));
            if !ok {
                return self.reject(&req, done);
            }
        }
        let data = (req.op == IoOp::Write).then(|| AlignedBuf::from_slice(buf));
        if self.inline {
            // Completes inside `on_request`, like the M11 engine does.
            let job = Job {
                tag: req.tag,
                op: req.op,
                fua: req.fua,
                offset: req.offset_bytes,
                len: req.len,
                data,
            };
            let d = self.inner.run(job);
            if let (true, Some(read)) = (d.status.is_ok(), &d.read) {
                buf[..read.len].copy_from_slice(read.as_slice());
            }
            done.complete(d.tag, d.status);
            return;
        }
        let job = Job {
            tag: req.tag,
            op: req.op,
            fua: req.fua,
            offset: req.offset_bytes,
            len: req.len,
            data,
        };
        let sent = self.tx.as_ref().is_some_and(|tx| tx.send(job).is_ok());
        if !sent {
            done.complete(req.tag, IoStatus::err(EIO));
        }
    }

    fn poll(&mut self, done: &mut dyn Completer) {
        self.wake.drain();
        let ready = std::mem::take(&mut *lock(&self.done));
        for d in ready {
            if let (true, Some(data)) = (d.status.is_ok(), &d.read) {
                done.tag_buf(d.tag)[..data.len].copy_from_slice(data.as_slice());
            }
            done.complete(d.tag, d.status);
        }
    }

    fn wake_fd(&self) -> BorrowedFd<'_> {
        self.wake.as_fd()
    }
}

impl Drop for FileQueue {
    fn drop(&mut self) {
        self.tx = None; // workers see the closed channel and end
        for h in self.workers.drain(..) {
            let _ = h.join();
        }
        lock(&self.inner.taken).remove(&self.queue);
    }
}
