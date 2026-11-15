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
//! * **W2.** A fatal error in a queue thread (a tag completed twice, an
//!   unknown tag, a commit that cannot be queued, a panic, an unexpected
//!   answer of the driver) calls [`fatal`], which is `std::process::abort()`
//!   (equivalent to `kill -9`, which phase 0 proved recoverable with
//!   `USER_RECOVERY_REISSUE`). The only way out of the loop is the layer's
//!   own exit condition: the queue is **down** (the driver answered a command
//!   with `ABORT`, or a stop was requested) **and** every started request has
//!   been committed.
//! * **libublk is not trusted for that.** After the first `ABORT` libublk
//!   0.4.8 queues no commit at all and ends its loop although the engine still
//!   holds requests, and a completion in the same batch as the first `ABORT`
//!   keeps its own in-flight count above zero for ever. So the layer keeps its
//!   own `down` flag (set mid-batch, on the first `ABORT`), commits held
//!   requests itself after that with a `COMMIT_AND_FETCH_REQ` of its own (the
//!   driver accepts the commit; its re-fetch part is cancelled when the ring is
//!   torn down), and never blocks in libublk's wait once down. The kernel
//!   therefore sees every commit and the successor has nothing to replay.
//! * Errors before the gate are reported to the attaching thread and are not
//!   fatal: nothing was fetched, so no request can be started.
//! * The wake race: the `POLL_ADD` is level triggered and re-armed after
//!   `poll` has drained the eventfd, so a signal that lands while the thread
//!   is blocked in `io_uring_enter`, or between the drain and the re-arm,
//!   completes the poll at once.
//!
//! # Fault hook (test only)
//!
//! In a `crashpoints` build, when `LUNIX_UBLK_FAULT_HOLD` names a file, every
//! request that arrives while that file (or `<file>.<dev_id>`, for one device
//! only) exists is **held**: started (it counts
//! as in flight, so `QUIESCE_DEV` waits for it) but not handed to the engine.
//! Only removing the file releases them, in arrival order: they stay held
//! when the queue goes down (the first `ABORT` of a quiesce), so the drain
//! waits for them and the quiesce times out while the test keeps the file. A watcher thread signals the wake fd every
//! 10 ms while requests are held, so the release is noticed. This lets the
//! root harness make a quiesce time out on a real device (a second
//! `QUIESCE_DEV`, the `QuiesceAll` retry). Production builds compile it out.

use std::os::fd::{AsRawFd, RawFd};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use io_uring::{cqueue, opcode, squeue, types};
use libublk::helpers::IoBuf;
use libublk::io::{UblkDev, UblkIOCtx, UblkQueue};
use libublk::{sys, BufDesc, BufDescList, UblkIORes, UblkUringData};
use lunix_iface::block::{
    BlockEngine, Completer, IoOp, IoRequest, IoStatus, QueueConfig, QueueHandler, QueueId, Tag,
};
use lunix_iface::wake::WakeFd;

const EINVAL: i32 = 22;
const EIO: i32 = 5;

/// `tag` of the target-I/O user data that marks the engine wake-fd poll. Real
/// tags are below the queue depth (at most 4096).
const WAKE_TAG: u16 = u16::MAX;
/// `tag` of the poll on the layer's own stop fd.
const STOP_TAG: u16 = u16::MAX - 1;
/// Operation byte of the poll user data (`WAKE_TAG`, `STOP_TAG`).
const POLL_OP: u32 = 0xfe;
/// Operation byte of the layer's own commit commands.
const DRAIN_OP: u32 = 0xfd;

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
    drain_commits: AtomicU64,
    drain_cqes: AtomicU64,
    /// Bytes this queue locked: its I/O buffers and its thread stack.
    locked: AtomicU64,
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
    /// Requests committed by the layer's own command after the queue went
    /// down (the held requests of a quiesce or stop).
    pub drain_commits: u64,
    /// Completions the driver returned for those commits.
    pub drain_cqes: u64,
    /// Bytes locked by the queue: its I/O buffers (populated) and its thread
    /// stack (locked as it is touched).
    pub locked_bytes: u64,
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
            drain_commits: l(&self.drain_commits),
            locked_bytes: l(&self.locked),
            drain_cqes: l(&self.drain_cqes),
        }
    }
}

/// Aborts the process (W2): the equivalent of `kill -9`, which recovery with
/// `REISSUE` handles. Never returns.
pub fn fatal(queue: u16, why: &str) -> ! {
    eprintln!(
        "lunix-ublk: fatal error in queue {queue}: {why}; aborting (recoverable like kill -9)"
    );
    abort_without_core()
}

/// `abort()` with core dumps off: a dump of a process with locked buffers
/// keeps `/dev/ublkcN` open for long and the successor's recovery wait could
/// expire.
fn abort_without_core() -> ! {
    let zero = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: setrlimit reads one initialised `rlimit`.
    unsafe { libc::setrlimit(libc::RLIMIT_CORE, &zero) };
    std::process::abort()
}

/// [`fatal`] for the control side of the device.
pub(crate) fn fatal_device(why: &str) -> ! {
    eprintln!("lunix-ublk: fatal error: {why}; aborting (recoverable like kill -9)");
    abort_without_core()
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
    /// Signalled to take a queue that was never started down (release).
    pub stop: WakeFd,
}

/// The entry point of a queue thread. A panic aborts the process.
pub(crate) fn queue_thread(args: QueueArgs) {
    let qid = args.qid;
    let r = catch_unwind(AssertUnwindSafe(|| run_queue(args)));
    if r.is_err() {
        fatal(qid, "panic in a queue thread");
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

/// A flush in drain mode may find the queue already down (`QueueIsDown`);
/// any other error means a commit may not have been submitted.
fn drain_flush(qid: u16, r: std::result::Result<i32, libublk::UblkError>) {
    match r {
        Ok(_) | Err(libublk::UblkError::QueueIsDown) => {}
        Err(e) => fatal(qid, &format!("submitting the drain commits: {e}")),
    }
}

/// A request as the kernel describes it, after validation.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Decoded {
    /// Hand it to the engine.
    Request {
        op: IoOp,
        fua: bool,
        offset: u64,
        len: u32,
    },
    /// Fail it with `EINVAL` without involving the engine: an operation the
    /// layer does not know, a length that does not fit `u32`, or more data
    /// than a tag buffer holds.
    Reject,
}

/// Decodes `ublksrv_io_desc` fields. FUA counts for WRITE only.
pub(crate) fn decode_request(
    op_flags: u32,
    start_sector: u64,
    nr_sectors: u32,
    buf_bytes: usize,
) -> Decoded {
    let op = match op_flags & 0xff {
        sys::UBLK_IO_OP_READ => IoOp::Read,
        sys::UBLK_IO_OP_WRITE => IoOp::Write,
        sys::UBLK_IO_OP_FLUSH => IoOp::Flush,
        sys::UBLK_IO_OP_DISCARD => IoOp::Discard,
        sys::UBLK_IO_OP_WRITE_ZEROES => IoOp::WriteZeroes,
        _ => return Decoded::Reject,
    };
    let len64 = u64::from(nr_sectors) << 9;
    let Ok(len) = u32::try_from(len64) else {
        return Decoded::Reject;
    };
    if matches!(op, IoOp::Read | IoOp::Write) && len64 > buf_bytes as u64 {
        return Decoded::Reject;
    }
    Decoded::Request {
        op,
        fua: op == IoOp::Write && op_flags & sys::UBLK_IO_F_FUA != 0,
        offset: start_sector.saturating_mul(512),
        len,
    }
}

fn run_queue(a: QueueArgs) {
    let _end = EndGuard(Arc::clone(&a.ended));
    let qid = a.qid;
    crate::watchdog::block_all_signals();
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
        // Pre-fault every page now.
        b.zero_buf();
    }
    // Lock the I/O path's memory (DESIGN §3): every tag buffer, populated,
    // and this thread's stack as it is touched. A failure fails the attach.
    // Declared after `bufs`, so an early return unlocks before the buffers
    // are freed; the normal path drops it explicitly below.
    let mut locks = crate::harden::LockedMemory::default();
    for b in &bufs {
        // SAFETY: the buffer is a live allocation of `buf_bytes` bytes that
        // outlives `locks`.
        if let Err(e) = unsafe { locks.lock_region(b.as_mut_ptr(), a.buf_bytes as usize) } {
            return report(Err(format!("queue {qid} buffers: {e}")));
        }
    }
    if let Err(e) = locks.lock_own_stack() {
        return report(Err(format!("queue {qid}: {e}")));
    }
    a.counters.locked.store(locks.bytes(), Ordering::Release);
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
    let stop_fd = a.stop.raw_fd();
    let cdev_fd = a.dev.tgt.fds[0];
    let mut lp = Loop {
        hold: Hold::from_env(wake_fd, a.dev.dev_info.dev_id),
        handler,
        c: CompState {
            qid,
            ptrs: bufs.iter().map(IoBuf::as_mut_ptr).collect(),
            buf_bytes: a.buf_bytes as usize,
            user_copy: a.user_copy,
            cdev_fd,
            meta: vec![Meta::Idle; usize::from(a.depth)],
            counters: Arc::clone(&a.counters),
            down: false,
        },
        wake_fd,
    };
    lp.arm(&queue, WAKE_TAG, wake_fd);
    lp.arm(&queue, STOP_TAG, stop_fd);
    loop {
        if lp.c.down {
            lp.release_held(&queue);
            // Drain: no blocking wait in libublk any more. Reap what is there
            // and submit our commits; leave when nothing is in flight.
            drain_flush(
                qid,
                queue.flush_and_wake_io_tasks(|ud, cqe, _| lp.on_cqe(&queue, ud, cqe), 0),
            );
            if a.counters.in_flight() == 0 {
                // One more submit for the commits of the last completions.
                drain_flush(
                    qid,
                    queue.flush_and_wake_io_tasks(|ud, cqe, _| lp.on_cqe(&queue, ud, cqe), 0),
                );
                break;
            }
            lp.wait_for_engine(&queue);
            continue;
        }
        let r = queue.flush_and_wake_io_tasks(|ud, cqe, _last| lp.on_cqe(&queue, ud, cqe), 1);
        match r {
            Ok(_) => {}
            // libublk saw every command answered with ABORT.
            Err(libublk::UblkError::QueueIsDown) => lp.c.down = true,
            Err(e) => fatal(qid, &format!("queue loop error: {e}")),
        }
    }

    let left = a.counters.in_flight();
    if left != 0 {
        fatal(
            qid,
            &format!("the queue ended with {left} started request(s) uncommitted"),
        );
    }
    drop(lp);
    drop(queue);
    a.counters.locked.store(0, Ordering::Release);
    drop(locks);
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
    /// The queue is down: the driver answered a command with `ABORT`, or a
    /// stop was requested. From here on commits are the layer's own.
    down: bool,
}

struct Loop {
    /// Declared first: it drops (and joins its watcher, which writes to the
    /// handler's wake fd) before the handler.
    hold: Option<Hold>,
    handler: Box<dyn QueueHandler>,
    c: CompState,
    wake_fd: RawFd,
}

/// The fault hook's state (see the module docs).
struct Hold {
    path: std::path::PathBuf,
    /// `<path>.<dev_id>`: holds this device only.
    dev_path: std::path::PathBuf,
    held: Vec<u16>,
    count: Arc<std::sync::atomic::AtomicUsize>,
    stop: Arc<std::sync::atomic::AtomicBool>,
    watcher: Option<std::thread::JoinHandle<()>>,
}

impl Hold {
    /// The hook, when this build has it and `LUNIX_UBLK_FAULT_HOLD` is set.
    fn from_env(wake_fd: RawFd, dev_id: u32) -> Option<Self> {
        if !cfg!(feature = "crashpoints") {
            return None;
        }
        let path = std::path::PathBuf::from(std::env::var_os("LUNIX_UBLK_FAULT_HOLD")?);
        let mut dev_path = path.clone().into_os_string();
        dev_path.push(format!(".{dev_id}"));
        let dev_path = std::path::PathBuf::from(dev_path);
        let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (c, st) = (Arc::clone(&count), Arc::clone(&stop));
        let (p, dp) = (path.clone(), dev_path.clone());
        let watcher = std::thread::Builder::new()
            .name("ublk-fault-hold".into())
            .spawn(move || {
                while !st.load(Ordering::Acquire) {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                    if c.load(Ordering::Acquire) > 0 && !p.exists() && !dp.exists() {
                        let one = 1u64.to_ne_bytes();
                        // SAFETY: an 8-byte write to the handler's eventfd,
                        // which outlives this thread (joined in `drop`).
                        unsafe { libc::write(wake_fd, one.as_ptr().cast(), one.len()) };
                    }
                }
            })
            .ok()?;
        Some(Self {
            path,
            dev_path,
            held: Vec::new(),
            count,
            stop,
            watcher: Some(watcher),
        })
    }

    fn active(&self) -> bool {
        self.path.exists() || self.dev_path.exists()
    }
}

impl Drop for Hold {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(w) = self.watcher.take() {
            let _ = w.join();
        }
    }
}

/// The [`Completer`] handed to the engine: commits only from the queue thread.
struct Comp<'a, 'q> {
    q: &'a UblkQueue<'q>,
    c: &'a mut CompState,
}

impl Loop {
    fn arm(&self, q: &UblkQueue<'_>, tag: u16, fd: RawFd) {
        let ud = UblkIOCtx::build_user_data(tag, POLL_OP, 0, true);
        let sqe = opcode::PollAdd::new(types::Fd(fd), libc::POLLIN as u32)
            .build()
            .user_data(ud);
        if let Err(e) = q.ublk_submit_sqe_sync(sqe) {
            fatal(self.c.qid, &format!("arming a poll: {e}"));
        }
    }

    fn on_cqe(&mut self, q: &UblkQueue<'_>, user_data: u64, cqe: &cqueue::Entry) {
        let Ok(tag) = u16::try_from(UblkIOCtx::user_data_to_tag(user_data)) else {
            fatal(self.c.qid, "a completion with a tag above 65535");
        };
        let res = cqe.result();
        if user_data & (UblkUringData::Target as u64) != 0 {
            return match UblkIOCtx::user_data_to_op(user_data) {
                DRAIN_OP => {
                    // The answer to one of our own commits. A successful commit
                    // makes no completion until its re-fetch half is cancelled
                    // (`ABORT`, or `ECANCELED` at the ring's teardown); any
                    // other answer means the driver rejected the commit and
                    // the request was not completed: a silent reissue, which
                    // the drain claims not to do.
                    if res != sys::UBLK_IO_RES_ABORT && res != -libc::ECANCELED {
                        fatal(
                            self.c.qid,
                            &format!("the driver rejected a drain commit of tag {tag}: {res}"),
                        );
                    }
                    self.c.counters.drain_cqes.fetch_add(1, Ordering::Relaxed);
                }
                POLL_OP => self.on_poll(q, tag, res),
                _ => fatal(self.c.qid, "unexpected target completion"),
            };
        }
        if res == sys::UBLK_IO_RES_OK as i32 {
            self.on_request(q, tag);
        } else if res == sys::UBLK_IO_RES_ABORT {
            // Set mid-batch: a completion later in this very batch must
            // already take the drain path.
            self.c.down = true;
        } else if !self.c.down {
            fatal(
                self.c.qid,
                &format!("the driver answered a command with {res}"),
            );
        }
    }

    fn on_poll(&mut self, q: &UblkQueue<'_>, tag: u16, res: i32) {
        if res < 0 {
            if self.c.down {
                return; // the ring is going away
            }
            fatal(self.c.qid, &format!("a poll failed with {res}"));
        }
        match tag {
            WAKE_TAG => {
                self.release_held(q);
                let Loop { handler, c, .. } = self;
                handler.poll(&mut Comp { q, c: &mut *c });
                self.arm(q, WAKE_TAG, self.wake_fd);
            }
            STOP_TAG => {
                // A stop was requested (a device that never started).
                self.c.down = true;
            }
            _ => fatal(self.c.qid, "unexpected poll completion"),
        }
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

    /// Hands held requests on once the hold file is gone (fault hook); the
    /// queue being down does not release them.
    fn release_held(&mut self, q: &UblkQueue<'_>) {
        let tags = match self.hold.as_mut() {
            Some(h) if !h.held.is_empty() && !h.active() => {
                h.count.store(0, Ordering::Release);
                std::mem::take(&mut h.held)
            }
            _ => return,
        };
        for tag in tags {
            self.dispatch(q, tag);
        }
    }

    fn on_request(&mut self, q: &UblkQueue<'_>, tag: u16) {
        let t = usize::from(tag);
        if t >= self.c.meta.len() || self.c.meta[t] != Meta::Idle {
            fatal(
                self.c.qid,
                "the driver delivered a tag that is already started",
            );
        }
        self.c.counters.started.fetch_add(1, Ordering::AcqRel);
        lunix_iface::crashpoint!("ublk.after_fetch");
        if let Some(h) = self.hold.as_mut() {
            if h.active() {
                h.held.push(tag);
                h.count.store(h.held.len(), Ordering::Release);
                return;
            }
        }
        self.dispatch(q, tag);
    }

    /// Decodes a started request and hands it to the engine.
    fn dispatch(&mut self, q: &UblkQueue<'_>, tag: u16) {
        let Loop { handler, c, .. } = self;
        let t = usize::from(tag);

        let iod = q.get_iod(tag);
        let decoded = decode_request(iod.op_flags, iod.start_sector, iod.nr_sectors, c.buf_bytes);
        let Decoded::Request {
            op,
            fua,
            offset,
            len,
        } = decoded
        else {
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

impl Comp<'_, '_> {
    /// Commits a request after the queue went down: libublk would drop it
    /// silently, so the layer queues `COMMIT_AND_FETCH_REQ` itself. The
    /// re-fetch half is never waited for (the ring's teardown cancels it).
    fn drain_commit(&mut self, tag: u16, res: i32) {
        let t = usize::from(tag);
        let addr = if self.c.user_copy {
            0
        } else {
            self.c.ptrs[t] as u64
        };
        let mut cmd = [0u8; 16]; // struct ublksrv_io_cmd
        cmd[0..2].copy_from_slice(&self.c.qid.to_le_bytes());
        cmd[2..4].copy_from_slice(&tag.to_le_bytes());
        cmd[4..8].copy_from_slice(&res.to_le_bytes());
        cmd[8..16].copy_from_slice(&addr.to_le_bytes());
        let sqe = opcode::UringCmd16::new(types::Fixed(0), sys::UBLK_U_IO_COMMIT_AND_FETCH_REQ)
            .cmd(cmd)
            .build()
            .flags(squeue::Flags::FIXED_FILE)
            .user_data(UblkIOCtx::build_user_data(tag, DRAIN_OP, 0, true));
        if let Err(e) = self.q.ublk_submit_sqe_sync(sqe) {
            fatal(
                self.c.qid,
                &format!("queueing the drain commit of tag {tag}: {e}"),
            );
        }
        self.c
            .counters
            .drain_commits
            .fetch_add(1, Ordering::Relaxed);
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
        if self.c.down {
            self.drain_commit(tag.0, res);
        } else {
            let committed = if self.c.user_copy {
                self.q.complete_io_cmd_unified(
                    tag.0,
                    BufDesc::Slice(&[]),
                    Ok(UblkIORes::Result(res)),
                )
            } else {
                // SAFETY: the buffer is `buf_bytes` long and owned by this thread.
                let slice: &[u8] =
                    unsafe { std::slice::from_raw_parts(self.c.ptrs[t], self.c.buf_bytes) };
                self.q.complete_io_cmd_unified(
                    tag.0,
                    BufDesc::Slice(slice),
                    Ok(UblkIORes::Result(res)),
                )
            };
            if let Err(e) = committed {
                fatal(qid, &format!("committing tag {}: {e}", tag.0));
            }
        }
        self.c.counters.completed.fetch_add(1, Ordering::AcqRel);
    }

    fn tag_buf(&mut self, tag: Tag) -> &mut [u8] {
        let t = usize::from(tag.0);
        let Some(ptr) = self.c.ptrs.get(t).copied() else {
            fatal(self.c.qid, "tag_buf of an unknown tag");
        };
        debug_assert!(
            matches!(self.c.meta.get(t), Some(Meta::Started { .. })),
            "tag_buf of a tag that is not started"
        );
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
    fn the_poll_user_data_is_a_target_io_with_a_reserved_tag() {
        for tag in [WAKE_TAG, STOP_TAG] {
            let ud = UblkIOCtx::build_user_data(tag, POLL_OP, 0, true);
            assert_eq!(UblkIOCtx::user_data_to_tag(ud), u32::from(tag));
            assert_eq!(UblkIOCtx::user_data_to_op(ud), POLL_OP);
            assert!(ud & (UblkUringData::Target as u64) != 0);
        }
        assert_ne!(WAKE_TAG, STOP_TAG);
        // The layer's own commits are target I/O too, so libublk does not
        // count them as driver commands.
        let ud = UblkIOCtx::build_user_data(7, DRAIN_OP, 0, true);
        assert_eq!(UblkIOCtx::user_data_to_op(ud), DRAIN_OP);
        assert!(ud & (UblkUringData::Target as u64) != 0);
        assert_ne!(DRAIN_OP, POLL_OP);
    }

    fn req(op: u32, flags: u32, start: u64, sectors: u32) -> Decoded {
        decode_request(op | flags, start, sectors, 1 << 20)
    }

    #[test]
    fn requests_decode_to_the_engine_operations() {
        let r = |op| match req(op, 0, 8, 16) {
            Decoded::Request {
                op, offset, len, ..
            } => (op, offset, len),
            Decoded::Reject => panic!("rejected"),
        };
        assert_eq!(r(sys::UBLK_IO_OP_READ), (IoOp::Read, 4096, 8192));
        assert_eq!(r(sys::UBLK_IO_OP_WRITE), (IoOp::Write, 4096, 8192));
        assert_eq!(r(sys::UBLK_IO_OP_FLUSH).0, IoOp::Flush);
        assert_eq!(r(sys::UBLK_IO_OP_DISCARD).0, IoOp::Discard);
        assert_eq!(r(sys::UBLK_IO_OP_WRITE_ZEROES).0, IoOp::WriteZeroes);
    }

    #[test]
    fn fua_counts_for_writes_only() {
        let fua = |op| match req(op, sys::UBLK_IO_F_FUA, 0, 8) {
            Decoded::Request { fua, .. } => fua,
            Decoded::Reject => panic!("rejected"),
        };
        assert!(fua(sys::UBLK_IO_OP_WRITE));
        assert!(!fua(sys::UBLK_IO_OP_READ));
        assert!(!fua(sys::UBLK_IO_OP_FLUSH));
        assert!(!fua(sys::UBLK_IO_OP_WRITE_ZEROES));
        assert!(matches!(
            req(sys::UBLK_IO_OP_WRITE, 0, 0, 8),
            Decoded::Request { fua: false, .. }
        ));
    }

    #[test]
    fn unknown_operations_and_oversize_data_are_rejected() {
        assert_eq!(req(sys::UBLK_IO_OP_WRITE_SAME, 0, 0, 8), Decoded::Reject);
        assert_eq!(req(sys::UBLK_IO_OP_ZONE_APPEND, 0, 0, 8), Decoded::Reject);
        // 1 MiB + 512 B of data does not fit a 1 MiB tag buffer.
        assert_eq!(
            req(sys::UBLK_IO_OP_READ, 0, 0, (1 << 11) + 1),
            Decoded::Reject
        );
        assert_eq!(
            req(sys::UBLK_IO_OP_WRITE, 0, 0, (1 << 11) + 1),
            Decoded::Reject
        );
        assert!(matches!(
            req(sys::UBLK_IO_OP_WRITE, 0, 0, 1 << 11),
            Decoded::Request { .. }
        ));
        // Discards are not limited by the tag buffer (they carry no data) but
        // by what fits `u32`.
        assert!(matches!(
            req(sys::UBLK_IO_OP_DISCARD, 0, 0, crate::config::MAX_DISCARD_SECTORS),
            Decoded::Request { len, .. } if i64::from(len) <= i64::from(i32::MAX)
        ));
        assert_eq!(
            req(sys::UBLK_IO_OP_DISCARD, 0, 0, u32::MAX),
            Decoded::Reject
        );
    }

    #[test]
    fn a_huge_start_sector_saturates_instead_of_wrapping() {
        match req(sys::UBLK_IO_OP_READ, 0, u64::MAX, 8) {
            Decoded::Request { offset, .. } => assert_eq!(offset, u64::MAX),
            Decoded::Reject => panic!("rejected"),
        }
    }
}
