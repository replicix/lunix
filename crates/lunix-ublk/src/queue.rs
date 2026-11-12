//! One queue thread: libublk's `UblkQueue` driven by the layer's own loop.
//!
//! The thread builds its engine [`QueueHandler`] and its `UblkQueue`, waits at
//! a gate until every queue is ready, submits the `FETCH` commands, arms a
//! `POLL_ADD` on the handler's wake fd and runs its own loop over libublk's
//! `flush_and_wake_io_tasks`. A request becomes an [`IoRequest`] for `on_request`; the wake fd's
//! completion calls `poll`. Both paths complete through [`Comp`], so a tag is
//! committed only from this thread (libublk 0.4.8, `lunix_iface::block`
//! threading rule). There is no `run_target()`.
//!
//! # Rules kept here
//!
//! * **W2.** A started request that is never completed is a bug the layer
//!   turns into `abort()`: a tag completed twice, an unknown tag, a commit
//!   libublk refuses, a panic, a queue loop that ends while the device is
//!   not stopping, and a loop that ends with started requests left all call
//!   [`fatal`], which is `std::process::abort()` (equivalent to `kill -9`,
//!   which phase 0 proved recoverable with `USER_RECOVERY_REISSUE`). Nothing on
//!   this thread exits gracefully with work in flight.
//! * Errors before the gate are reported to the attaching thread and are not
//!   fatal: nothing was fetched, so no request can be started.
//! * The wake race: the `POLL_ADD` is level triggered and re-armed after
//!   `poll` has drained the eventfd, so a signal that lands while the thread
//!   is blocked in `io_uring_enter`, or between the drain and the re-arm,
//!   completes the poll at once.

use std::os::fd::{AsRawFd, RawFd};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use io_uring::{cqueue, opcode, types};
use libublk::helpers::IoBuf;
use libublk::io::{UblkDev, UblkIOCtx, UblkQueue};
use libublk::{sys, BufDesc, BufDescList, UblkIORes, UblkUringData};
use lunix_iface::block::{
    BlockEngine, Completer, IoOp, IoRequest, IoStatus, QueueConfig, QueueHandler, QueueId, Tag,
};

const EINVAL: i32 = 22;
const EIO: i32 = 5;

/// `tag` of the target-I/O user data that marks the wake-fd poll. Real tags
/// are below the queue depth (at most 4096).
const WAKE_TAG: u16 = u16::MAX;

/// Counters of one queue, readable from any thread.
#[derive(Debug, Default)]
pub struct QueueCounters {
    started: AtomicU64,
    completed: AtomicU64,
    reads: AtomicU64,
    writes: AtomicU64,
    fua_writes: AtomicU64,
    flushes: AtomicU64,
    discards: AtomicU64,
    write_zeroes: AtomicU64,
    rejected: AtomicU64,
}

/// A copy of [`QueueCounters`].
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct QueueSnapshot {
    /// Requests taken from the driver.
    pub started: u64,
    /// Requests committed back.
    pub completed: u64,
    /// READ requests.
    pub reads: u64,
    /// WRITE requests (FUA ones included).
    pub writes: u64,
    /// WRITE requests with FUA.
    pub fua_writes: u64,
    /// FLUSH requests.
    pub flushes: u64,
    /// DISCARD requests.
    pub discards: u64,
    /// WRITE_ZEROES requests.
    pub write_zeroes: u64,
    /// Requests the layer itself failed with `EINVAL` (unknown operation,
    /// length above the tag buffer).
    pub rejected: u64,
}

impl QueueCounters {
    /// Started and not yet completed requests.
    pub fn in_flight(&self) -> u64 {
        let c = self.completed.load(Ordering::Acquire);
        self.started.load(Ordering::Acquire).saturating_sub(c)
    }

    /// A consistent-enough copy for reporting.
    pub fn snapshot(&self) -> QueueSnapshot {
        let l = |a: &AtomicU64| a.load(Ordering::Relaxed);
        QueueSnapshot {
            started: l(&self.started),
            completed: l(&self.completed),
            reads: l(&self.reads),
            writes: l(&self.writes),
            fua_writes: l(&self.fua_writes),
            flushes: l(&self.flushes),
            discards: l(&self.discards),
            write_zeroes: l(&self.write_zeroes),
            rejected: l(&self.rejected),
        }
    }
}

/// Aborts the process (W2): the equivalent of `kill -9`, which recovery with
/// `REISSUE` handles. Never returns.
pub fn fatal(queue: u16, why: &str) -> ! {
    eprintln!(
        "lunix-ublk: fatal error in queue {queue}: {why}; aborting (recoverable like kill -9)"
    );
    std::process::abort()
}

/// [`fatal`] for the control side of the device.
pub(crate) fn fatal_device(why: &str) -> ! {
    eprintln!("lunix-ublk: fatal error: {why}; aborting (recoverable like kill -9)");
    std::process::abort()
}

/// The decision all queue threads wait for before they fetch.
#[derive(Debug)]
pub(crate) struct StartGate {
    state: Mutex<Option<bool>>,
    cv: Condvar,
}

impl StartGate {
    pub(crate) fn new() -> Self {
        Self {
            state: Mutex::new(None),
            cv: Condvar::new(),
        }
    }

    /// Lets the queues fetch (`true`) or tells them to give up (`false`).
    pub(crate) fn decide(&self, go: bool) {
        let mut s = self.state.lock().unwrap_or_else(|p| p.into_inner());
        *s = Some(go);
        self.cv.notify_all();
    }

    fn wait(&self) -> bool {
        let mut s = self.state.lock().unwrap_or_else(|p| p.into_inner());
        loop {
            if let Some(go) = *s {
                return go;
            }
            s = self.cv.wait(s).unwrap_or_else(|p| p.into_inner());
        }
    }
}

/// Counts queue threads that have ended.
#[derive(Debug, Default)]
pub(crate) struct Ended {
    n: Mutex<usize>,
    cv: Condvar,
}

impl Ended {
    #[cfg(test)]
    pub(crate) fn count(&self) -> usize {
        *self.n.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Waits until `want` threads have ended; false on timeout.
    pub(crate) fn wait_for(&self, want: usize, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut n = self.n.lock().unwrap_or_else(|p| p.into_inner());
        while *n < want {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            let (g, _) = self
                .cv
                .wait_timeout(n, left)
                .unwrap_or_else(|p| p.into_inner());
            n = g;
        }
        true
    }
}

struct EndGuard(Arc<Ended>);

impl Drop for EndGuard {
    fn drop(&mut self) {
        let mut n = self.0.n.lock().unwrap_or_else(|p| p.into_inner());
        *n += 1;
        self.0.cv.notify_all();
    }
}

/// What the attaching thread needs to know about one queue.
pub(crate) type Ready = (u16, Result<(), String>);

/// Everything a queue thread is started with.
pub(crate) struct QueueArgs {
    pub qid: u16,
    pub dev: Arc<UblkDev>,
    pub engine: Arc<dyn BlockEngine>,
    pub nr_queues: u16,
    pub depth: u16,
    pub buf_bytes: u32,
    pub user_copy: bool,
    pub cpu: Option<usize>,
    pub counters: Arc<QueueCounters>,
    pub gate: Arc<StartGate>,
    pub ready: Sender<Ready>,
    pub ended: Arc<Ended>,
}

/// The entry point of a queue thread. A panic aborts the process.
pub(crate) fn queue_thread(args: QueueArgs) {
    let qid = args.qid;
    let r = catch_unwind(AssertUnwindSafe(|| run_queue(args)));
    if r.is_err() {
        fatal(qid, "panic in a queue thread");
    }
}

fn block_all_signals() {
    // SAFETY: `sigfillset` and `pthread_sigmask` get a valid, initialised set
    // and a null old-set pointer.
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigfillset(&mut set);
        libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
    }
}

fn set_io_flusher() {
    const PR_SET_IO_FLUSHER: libc::c_int = 57;
    // SAFETY: prctl with integer arguments only.
    let r = unsafe { libc::prctl(PR_SET_IO_FLUSHER, 1, 0, 0, 0) };
    if r != 0 {
        eprintln!(
            "lunix-ublk: PR_SET_IO_FLUSHER failed ({}); continuing without it",
            std::io::Error::last_os_error()
        );
    }
}

fn pin_to(cpu: usize) -> bool {
    let mut set = rustix::thread::CpuSet::new();
    if cpu >= rustix::thread::CpuSet::MAX_CPU {
        return false;
    }
    set.set(cpu);
    rustix::thread::sched_setaffinity(None, &set).is_ok()
}

fn run_queue(a: QueueArgs) {
    let _end = EndGuard(Arc::clone(&a.ended));
    let qid = a.qid;
    block_all_signals();
    if let Some(cpu) = a.cpu {
        if !pin_to(cpu) {
            eprintln!("lunix-ublk: queue {qid}: cannot pin to cpu {cpu}; running unpinned");
        }
    }
    set_io_flusher();
    let report = |r: Result<(), String>| {
        let _ = a.ready.send((qid, r));
    };

    // Build phase: failures are reported, nothing was fetched.
    let cfg = QueueConfig {
        queue: QueueId(qid),
        nr_queues: a.nr_queues,
        depth: a.depth,
        max_io_buf_bytes: a.buf_bytes,
    };
    let handler = match a.engine.queue_handler(&cfg) {
        Ok(h) => h,
        Err(e) => return report(Err(format!("engine queue handler: {e}"))),
    };
    let queue = match UblkQueue::new(qid, &a.dev) {
        Ok(q) => q,
        Err(e) => return report(Err(format!("UblkQueue::new: {e}"))),
    };
    let mut bufs: Vec<IoBuf<u8>> = (0..a.depth)
        .map(|_| IoBuf::<u8>::new(a.buf_bytes as usize))
        .collect();
    for b in &mut bufs {
        // Pre-fault (and, with mlockall, pin) every page now.
        b.zero_buf();
    }
    report(Ok(()));
    if !a.gate.wait() {
        return; // the attach was abandoned; `queue` and `handler` drop
    }

    // From here on a failure aborts (W2).
    let fetch = if a.user_copy {
        queue.submit_fetch_commands_unified(BufDescList::Slices(None))
    } else {
        queue.submit_fetch_commands_unified(BufDescList::Slices(Some(&bufs)))
    };
    let queue = fetch.unwrap_or_else(|e| fatal(qid, &format!("submitting FETCH: {e}")));
    let wake_fd = handler.wake_fd().as_raw_fd();
    let cdev_fd = a.dev.tgt.fds[0];
    let mut lp = Loop {
        handler,
        c: CompState {
            qid,
            ptrs: bufs.iter().map(IoBuf::as_mut_ptr).collect(),
            buf_bytes: a.buf_bytes as usize,
            user_copy: a.user_copy,
            cdev_fd,
            meta: vec![Meta::Idle; usize::from(a.depth)],
            counters: Arc::clone(&a.counters),
        },
        wake_fd,
    };
    lp.arm(&queue);
    // Our own loop instead of libublk's `wait_and_handle_io`, which ends as
    // soon as the kernel has aborted the idle FETCH commands, even while the
    // engine still holds started requests (it assumes handlers complete at
    // once). We end only when the queue is down AND nothing is in flight (W2).
    loop {
        let r = queue.flush_and_wake_io_tasks(|ud, cqe, _last| lp.on_cqe(&queue, ud, cqe), 1);
        match r {
            Ok(_) => {}
            Err(libublk::UblkError::QueueIsDown) => {
                if a.counters.in_flight() == 0 {
                    break;
                }
                lp.wait_for_engine(&queue);
            }
            Err(e) => fatal(qid, &format!("queue loop error: {e}")),
        }
    }

    // The queue is down and nothing is in flight.
    if !queue.is_stopping() {
        fatal(qid, "the queue loop ended while the device is not stopping");
    }
    let left = a.counters.in_flight();
    if left != 0 {
        fatal(
            qid,
            &format!("the queue ended with {left} started request(s) uncompleted"),
        );
    }
    drop(lp);
    drop(queue);
    drop(bufs);
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Meta {
    Idle,
    Started { op: IoOp, len: u32 },
}

/// The state completions need; split from the handler so both can be borrowed.
struct CompState {
    qid: u16,
    ptrs: Vec<*mut u8>,
    buf_bytes: usize,
    user_copy: bool,
    cdev_fd: RawFd,
    meta: Vec<Meta>,
    counters: Arc<QueueCounters>,
}

struct Loop {
    handler: Box<dyn QueueHandler>,
    c: CompState,
    wake_fd: RawFd,
}

/// The [`Completer`] handed to the engine: commits only from the queue thread.
struct Comp<'a, 'q> {
    q: &'a UblkQueue<'q>,
    c: &'a mut CompState,
}

impl Loop {
    fn arm(&self, q: &UblkQueue<'_>) {
        let ud = UblkIOCtx::build_user_data(WAKE_TAG, 0xfe, 0, true);
        let sqe = opcode::PollAdd::new(types::Fd(self.wake_fd), libc::POLLIN as u32)
            .build()
            .user_data(ud);
        if let Err(e) = q.ublk_submit_sqe_sync(sqe) {
            fatal(self.c.qid, &format!("arming the wake poll: {e}"));
        }
    }

    fn on_cqe(&mut self, q: &UblkQueue<'_>, user_data: u64, cqe: &cqueue::Entry) {
        let Ok(tag) = u16::try_from(UblkIOCtx::user_data_to_tag(user_data)) else {
            fatal(self.c.qid, "a completion with a tag above 65535");
        };
        let res = cqe.result();
        if user_data & (UblkUringData::Target as u64) != 0 {
            return self.on_wake(q, tag, res);
        }
        if res == sys::UBLK_IO_RES_OK as i32 {
            self.on_request(q, tag);
        } else if res != sys::UBLK_IO_RES_ABORT && !q.is_stopping() {
            fatal(
                self.c.qid,
                &format!("the driver answered a command with {res}"),
            );
        }
        // ABORT (or anything while stopping): the queue is winding down.
    }

    fn on_wake(&mut self, q: &UblkQueue<'_>, tag: u16, res: i32) {
        if tag != WAKE_TAG {
            fatal(self.c.qid, "unexpected target completion");
        }
        if res < 0 {
            if q.is_stopping() {
                return; // the ring is going away
            }
            fatal(self.c.qid, &format!("wake poll failed with {res}"));
        }
        let Loop { handler, c, .. } = self;
        handler.poll(&mut Comp { q, c: &mut *c });
        self.arm(q);
    }

    /// The queue is down but the engine still holds requests: wait for its
    /// wake fd (bounded, so the loop re-checks) and let it complete them.
    fn wait_for_engine(&mut self, q: &UblkQueue<'_>) {
        let mut p = libc::pollfd {
            fd: self.wake_fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one valid pollfd; the fd lives as long as the handler.
        unsafe { libc::poll(&mut p, 1, 50) };
        let Loop { handler, c, .. } = self;
        handler.poll(&mut Comp { q, c: &mut *c });
    }

    fn on_request(&mut self, q: &UblkQueue<'_>, tag: u16) {
        let Loop { handler, c, .. } = self;
        let t = usize::from(tag);
        if t >= c.meta.len() || c.meta[t] != Meta::Idle {
            fatal(c.qid, "the driver delivered a tag that is already started");
        }
        c.counters.started.fetch_add(1, Ordering::AcqRel);
        lunix_iface::crashpoint!("ublk.after_fetch");

        let iod = q.get_iod(tag);
        let op_raw = iod.op_flags & 0xff;
        let offset = iod.start_sector.saturating_mul(512);
        let len64 = u64::from(iod.nr_sectors) << 9;
        let op = match op_raw {
            sys::UBLK_IO_OP_READ => Some(IoOp::Read),
            sys::UBLK_IO_OP_WRITE => Some(IoOp::Write),
            sys::UBLK_IO_OP_FLUSH => Some(IoOp::Flush),
            sys::UBLK_IO_OP_DISCARD => Some(IoOp::Discard),
            sys::UBLK_IO_OP_WRITE_ZEROES => Some(IoOp::WriteZeroes),
            _ => None,
        };
        let fua = op == Some(IoOp::Write) && iod.op_flags & sys::UBLK_IO_F_FUA != 0;
        let decoded = match (op, u32::try_from(len64)) {
            (Some(op), Ok(len))
                if !matches!(op, IoOp::Read | IoOp::Write) || len64 <= c.buf_bytes as u64 =>
            {
                Some((op, len))
            }
            _ => None,
        };
        let Some((op, len)) = decoded else {
            // Unknown operation or a length above the tag buffer: reject
            // without involving the engine, and commit `EINVAL`.
            c.counters.rejected.fetch_add(1, Ordering::Relaxed);
            c.meta[t] = Meta::Started {
                op: IoOp::Flush,
                len: 0,
            };
            Comp { q, c: &mut *c }.complete(Tag(tag), IoStatus::err(EINVAL));
            return;
        };
        let moves_data = matches!(op, IoOp::Read | IoOp::Write);
        match op {
            IoOp::Read => c.counters.reads.fetch_add(1, Ordering::Relaxed),
            IoOp::Write => {
                if fua {
                    c.counters.fua_writes.fetch_add(1, Ordering::Relaxed);
                }
                c.counters.writes.fetch_add(1, Ordering::Relaxed)
            }
            IoOp::Flush => c.counters.flushes.fetch_add(1, Ordering::Relaxed),
            IoOp::Discard => c.counters.discards.fetch_add(1, Ordering::Relaxed),
            IoOp::WriteZeroes => c.counters.write_zeroes.fetch_add(1, Ordering::Relaxed),
        };
        c.meta[t] = Meta::Started { op, len };
        let len_usize = if moves_data { len as usize } else { 0 };
        if c.user_copy && op == IoOp::Write {
            let pos = UblkIOCtx::ublk_user_copy_pos(c.qid, tag, 0);
            if !copy_in(c.cdev_fd, c.ptrs[t], len_usize, pos) {
                Comp { q, c: &mut *c }.complete(Tag(tag), IoStatus::err(EIO));
                return;
            }
        }
        // SAFETY: `ptrs[t]` is the start of a buffer of `buf_bytes >= len`
        // bytes that this thread owns; the tag is started, so neither the
        // kernel nor another tag touches it until it is completed. The
        // engine's contract forbids `tag_buf(tag)` while this borrow is live.
        let buf: &mut [u8] = unsafe { std::slice::from_raw_parts_mut(c.ptrs[t], len_usize) };
        let req = IoRequest {
            tag: Tag(tag),
            op,
            fua,
            offset_bytes: offset,
            len,
        };
        handler.on_request(req, buf, &mut Comp { q, c: &mut *c });
    }
}

impl Completer for Comp<'_, '_> {
    fn complete(&mut self, tag: Tag, status: IoStatus) {
        let qid = self.c.qid;
        let t = usize::from(tag.0);
        let meta = match self.c.meta.get(t) {
            Some(m) => *m,
            None => fatal(qid, "completed an unknown tag"),
        };
        let Meta::Started { op, len } = meta else {
            fatal(
                qid,
                "completed a tag that is not started (twice, or never fetched)",
            );
        };
        self.c.meta[t] = Meta::Idle;
        lunix_iface::crashpoint!("ublk.before_commit");

        let mut res = status.raw();
        if self.c.user_copy && op == IoOp::Read && res >= 0 {
            let n = usize::try_from(res).unwrap_or(0).min(len as usize);
            let pos = UblkIOCtx::ublk_user_copy_pos(qid, tag.0, 0);
            if copy_out(self.c.cdev_fd, self.c.ptrs[t], n, pos) {
                res = i32::try_from(n).unwrap_or(i32::MAX);
            } else {
                res = IoStatus::err(EIO).raw();
            }
        }
        let committed = if self.c.user_copy {
            self.q
                .complete_io_cmd_unified(tag.0, BufDesc::Slice(&[]), Ok(UblkIORes::Result(res)))
        } else {
            // SAFETY: the buffer is `buf_bytes` long and owned by this thread.
            let slice: &[u8] =
                unsafe { std::slice::from_raw_parts(self.c.ptrs[t], self.c.buf_bytes) };
            self.q
                .complete_io_cmd_unified(tag.0, BufDesc::Slice(slice), Ok(UblkIORes::Result(res)))
        };
        if let Err(e) = committed {
            fatal(qid, &format!("committing tag {}: {e}", tag.0));
        }
        self.c.counters.completed.fetch_add(1, Ordering::AcqRel);
    }

    fn tag_buf(&mut self, tag: Tag) -> &mut [u8] {
        let Some(ptr) = self.c.ptrs.get(usize::from(tag.0)).copied() else {
            fatal(self.c.qid, "tag_buf of an unknown tag");
        };
        // SAFETY: a buffer of `buf_bytes` bytes owned by this thread; the
        // returned borrow is tied to `&mut self`, so one tag buffer is
        // borrowed at a time through the completer.
        unsafe { std::slice::from_raw_parts_mut(ptr, self.c.buf_bytes) }
    }
}

/// `pread`s `len` bytes of a write's data from the char device (USER_COPY).
fn copy_in(fd: RawFd, ptr: *mut u8, len: usize, pos: u64) -> bool {
    let mut done = 0usize;
    while done < len {
        // SAFETY: `ptr..ptr+len` is the tag buffer; `done < len`.
        let n = unsafe {
            libc::pread(
                fd,
                ptr.add(done).cast::<libc::c_void>(),
                len - done,
                (pos + done as u64) as libc::off_t,
            )
        };
        if n <= 0 {
            return false;
        }
        done += n as usize;
    }
    true
}

/// `pwrite`s `len` bytes of a read's data to the char device (USER_COPY).
fn copy_out(fd: RawFd, ptr: *mut u8, len: usize, pos: u64) -> bool {
    let mut done = 0usize;
    while done < len {
        // SAFETY: as in `copy_in`.
        let n = unsafe {
            libc::pwrite(
                fd,
                ptr.add(done).cast::<libc::c_void>().cast_const(),
                len - done,
                (pos + done as u64) as libc::off_t,
            )
        };
        if n <= 0 {
            return false;
        }
        done += n as usize;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_track_in_flight() {
        let c = QueueCounters::default();
        assert_eq!(c.in_flight(), 0);
        c.started.fetch_add(3, Ordering::AcqRel);
        c.completed.fetch_add(1, Ordering::AcqRel);
        assert_eq!(c.in_flight(), 2);
        let s = c.snapshot();
        assert_eq!((s.started, s.completed), (3, 1));
    }

    #[test]
    fn the_gate_releases_every_waiter_with_the_decision() {
        let gate = Arc::new(StartGate::new());
        let mut waiters = Vec::new();
        for _ in 0..3 {
            let g = Arc::clone(&gate);
            waiters.push(std::thread::spawn(move || g.wait()));
        }
        gate.decide(false);
        for w in waiters {
            assert!(!w.join().unwrap());
        }
        // A late waiter sees the same decision.
        assert!(!gate.wait());
    }

    #[test]
    fn ended_counts_and_times_out() {
        let ended = Arc::new(Ended::default());
        assert!(!ended.wait_for(1, Duration::from_millis(30)));
        let g = EndGuard(Arc::clone(&ended));
        drop(g);
        assert_eq!(ended.count(), 1);
        assert!(ended.wait_for(1, Duration::from_millis(30)));
    }

    #[test]
    fn the_wake_user_data_is_a_target_io_with_the_reserved_tag() {
        let ud = UblkIOCtx::build_user_data(WAKE_TAG, 0xfe, 0, true);
        assert_eq!(UblkIOCtx::user_data_to_tag(ud), u32::from(WAKE_TAG));
        assert!(ud & (1 << 63) != 0);
    }
}
