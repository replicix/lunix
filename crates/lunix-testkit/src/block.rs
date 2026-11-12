//! [`MemBlockEngine`]: a [`BlockEngine`] over a sparse in-memory device, for
//! testing the ublk layer (M10) and as the worked example of the
//! [`QueueHandler`] contract.
//!
//! It implements the engine's I/O semantics and nothing else: requests with a
//! bad offset or length complete with `EINVAL`, reads of unwritten blocks are
//! zeros, discard and write-zeroes zero the range, FLUSH and FUA complete
//! after everything earlier has been applied (this engine applies a request
//! when it is handed over, so "durable" is immediate). What differs from a
//! trivial engine is **when completions happen**: [`CompletionMode`] chooses
//! between completing inside `on_request`, completing only from `poll` after
//! the queue's wake fd was signalled, or a seeded mix, so a ublk layer's
//! deferred-completion path gets exercised.
//!
//! [`TestCompleter`] and [`run_request`] drive a handler the way the ublk
//! layer does, from tests.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::os::fd::{AsFd, BorrowedFd};
use std::sync::{Arc, Mutex, MutexGuard};

use lunix_iface::block::{
    BlockEngine, Completer, IoOp, IoRequest, IoStatus, QueueConfig, QueueHandler, ResizeHook, Tag,
};
use lunix_iface::stats::EngineStats;
use lunix_iface::wake::WakeFd;
use lunix_iface::{Error, Result};

const BLOCK: u64 = 4096;
const ZEROS: [u8; BLOCK as usize] = [0; BLOCK as usize];
const EINVAL: i32 = 22;
const EIO: i32 = 5;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// When a request completes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CompletionMode {
    /// Inside `on_request`, always.
    Immediate,
    /// Never inside `on_request`; the next `poll` completes it and the wake fd
    /// is signalled.
    Deferred,
    /// Each request is deferred or immediate by a seeded coin.
    Mixed(u64),
}

/// Settings of a [`MemBlockEngine`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct MemEngineConfig {
    /// Device size in bytes (a multiple of 4096).
    pub size_bytes: u64,
    /// Queues the engine serves.
    pub nr_queues: u16,
    /// When requests complete.
    pub mode: CompletionMode,
}

impl MemEngineConfig {
    /// Immediate completions, one queue.
    pub fn new(size_bytes: u64) -> Self {
        Self {
            size_bytes,
            nr_queues: 1,
            mode: CompletionMode::Immediate,
        }
    }
}

/// Counters beyond [`EngineStats`].
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct MemEngineCounters {
    /// Requests completed `EINVAL`.
    pub einval: u64,
    /// FUA writes.
    pub fua_writes: u64,
    /// Requests completed from `poll`.
    pub deferred: u64,
    /// `quiesce` calls.
    pub quiesces: u64,
}

struct EngineState {
    size: u64,
    blocks: HashMap<u64, Box<[u8]>>,
    stats: EngineStats,
    counters: MemEngineCounters,
    hook: Option<Arc<dyn ResizeHook>>,
    taken: HashSet<u16>,
    rng: u64,
    mode: CompletionMode,
    read_only: bool,
    fail_next: BTreeMap<u8, i32>,
    quiesced: bool,
}

struct EngineShared {
    cfg: MemEngineConfig,
    st: Mutex<EngineState>,
}

/// The engine. Cloning shares the device.
#[derive(Clone)]
pub struct MemBlockEngine(Arc<EngineShared>);

fn op_key(op: IoOp) -> u8 {
    match op {
        IoOp::Read => 0,
        IoOp::Write => 1,
        IoOp::Flush => 2,
        IoOp::Discard => 3,
        IoOp::WriteZeroes => 4,
    }
}

impl MemBlockEngine {
    /// A zeroed device.
    ///
    /// # Panics
    /// If the size is zero or not a multiple of 4096, or there are no queues.
    pub fn new(cfg: MemEngineConfig) -> Self {
        assert!(cfg.size_bytes > 0 && cfg.size_bytes.is_multiple_of(BLOCK));
        assert!(cfg.nr_queues > 0);
        let seed = match cfg.mode {
            CompletionMode::Mixed(s) => s,
            _ => 0,
        };
        Self(Arc::new(EngineShared {
            cfg,
            st: Mutex::new(EngineState {
                size: cfg.size_bytes,
                blocks: HashMap::new(),
                stats: EngineStats::default(),
                counters: MemEngineCounters::default(),
                hook: None,
                taken: HashSet::new(),
                rng: seed,
                mode: cfg.mode,
                read_only: false,
                fail_next: BTreeMap::new(),
                quiesced: false,
            }),
        }))
    }

    /// Changes when requests complete (for requests handed over from now on).
    pub fn set_mode(&self, mode: CompletionMode) {
        lock(&self.0.st).mode = mode;
    }

    /// While set, writes, discards and write-zeroes complete `EIO` and change
    /// nothing; reads continue (the volume went read-only).
    pub fn set_read_only(&self, on: bool) {
        lock(&self.0.st).read_only = on;
    }

    /// Makes the next request of kind `op` complete with `-errno`.
    pub fn fail_next(&self, op: IoOp, errno: i32) {
        lock(&self.0.st).fail_next.insert(op_key(op), errno);
    }

    /// The device as one flat vector.
    pub fn snapshot(&self) -> Vec<u8> {
        let st = lock(&self.0.st);
        let mut out = vec![0u8; usize::try_from(st.size).expect("size fits memory")];
        for (&b, data) in &st.blocks {
            let at = (b * BLOCK) as usize;
            out[at..at + BLOCK as usize].copy_from_slice(data);
        }
        out
    }

    /// The extra counters.
    pub fn counters(&self) -> MemEngineCounters {
        lock(&self.0.st).counters
    }

    /// True once `quiesce` was called.
    pub fn is_quiesced(&self) -> bool {
        lock(&self.0.st).quiesced
    }
}

impl EngineState {
    fn coin(&mut self) -> bool {
        self.rng = self.rng.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.rng;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        (z ^ (z >> 31)) & 1 == 1
    }

    fn read_into(&self, offset: u64, out: &mut [u8]) {
        let mut done = 0usize;
        while done < out.len() {
            let pos = offset + done as u64;
            let (block, within) = (pos / BLOCK, (pos % BLOCK) as usize);
            let n = (BLOCK as usize - within).min(out.len() - done);
            match self.blocks.get(&block) {
                Some(b) => out[done..done + n].copy_from_slice(&b[within..within + n]),
                None => out[done..done + n].fill(0),
            }
            done += n;
        }
    }

    fn write_from(&mut self, offset: u64, data: &[u8]) {
        for (i, chunk) in data.chunks(BLOCK as usize).enumerate() {
            let block = offset / BLOCK + i as u64;
            if chunk == &ZEROS[..chunk.len()] {
                self.blocks.remove(&block);
            } else {
                self.blocks.insert(block, chunk.into());
            }
        }
    }

    fn zero_range(&mut self, offset: u64, len: u64) {
        let (first, last) = (offset / BLOCK, (offset + len) / BLOCK);
        if last - first > self.blocks.len() as u64 {
            self.blocks.retain(|b, _| *b < first || *b >= last);
        } else {
            for b in first..last {
                self.blocks.remove(&b);
            }
        }
    }
}

impl BlockEngine for MemBlockEngine {
    fn size_bytes(&self) -> u64 {
        lock(&self.0.st).size
    }

    fn queue_handler(&self, cfg: &QueueConfig) -> Result<Box<dyn QueueHandler>> {
        if cfg.queue.0 >= self.0.cfg.nr_queues || cfg.nr_queues != self.0.cfg.nr_queues {
            return Err(Error::invalid("bad queue number"));
        }
        if !lock(&self.0.st).taken.insert(cfg.queue.0) {
            return Err(Error::invalid("queue already has a handler"));
        }
        let wake = WakeFd::new()?;
        Ok(Box::new(MemQueue {
            engine: Arc::clone(&self.0),
            max_io: cfg.max_io_buf_bytes,
            wake,
            ready: Vec::new(),
        }))
    }

    fn grow(&self, new_size_bytes: u64) -> Result<()> {
        let hook = {
            let mut st = lock(&self.0.st);
            if !new_size_bytes.is_multiple_of(BLOCK) {
                return Err(Error::invalid("size is not a multiple of 4096"));
            }
            if new_size_bytes < st.size {
                return Err(Error::Unsupported {
                    what: "shrinking a device",
                });
            }
            if new_size_bytes == st.size {
                return Ok(());
            }
            st.size = new_size_bytes;
            st.hook.clone()
        };
        match hook {
            Some(h) => h.resized(new_size_bytes),
            None => Ok(()),
        }
    }

    fn set_on_resize(&self, hook: Arc<dyn ResizeHook>) {
        lock(&self.0.st).hook = Some(hook);
    }

    fn quiesce(&self) -> Result<()> {
        let mut st = lock(&self.0.st);
        st.quiesced = true;
        st.counters.quiesces += 1;
        Ok(())
    }

    fn stats(&self) -> EngineStats {
        lock(&self.0.st).stats
    }
}

struct Ready {
    tag: Tag,
    status: IoStatus,
    read: Option<Vec<u8>>,
}

struct MemQueue {
    engine: Arc<EngineShared>,
    max_io: u32,
    wake: WakeFd,
    ready: Vec<Ready>,
}

impl MemQueue {
    /// Applies the request; returns its status and, for a read, the data.
    fn execute(&self, req: &IoRequest, buf: &mut [u8]) -> (IoStatus, Option<Vec<u8>>) {
        let mut st = lock(&self.engine.st);
        let einval = |st: &mut EngineState| {
            st.counters.einval += 1;
            (IoStatus::err(EINVAL), None)
        };
        if st.quiesced {
            return (IoStatus::err(EIO), None);
        }
        if let Some(errno) = st.fail_next.remove(&op_key(req.op)) {
            return (IoStatus::err(errno), None);
        }
        let len = u64::from(req.len);
        if req.op != IoOp::Flush {
            let ok = req.offset_bytes.is_multiple_of(BLOCK)
                && len.is_multiple_of(BLOCK)
                && len > 0
                && req
                    .offset_bytes
                    .checked_add(len)
                    .is_some_and(|end| end <= st.size);
            let io_ok = !matches!(req.op, IoOp::Read | IoOp::Write)
                || (req.len <= self.max_io && buf.len() == req.len as usize);
            if !ok || !io_ok {
                return einval(&mut st);
            }
        }
        match req.op {
            IoOp::Read => {
                st.stats.ops_read += 1;
                st.stats.bytes_read += len;
                let mut data = vec![0u8; req.len as usize];
                st.read_into(req.offset_bytes, &mut data);
                (IoStatus::ok(req.len), Some(data))
            }
            IoOp::Write => {
                if st.read_only {
                    return (IoStatus::err(EIO), None);
                }
                st.stats.ops_write += 1;
                st.stats.bytes_written += len;
                if req.fua {
                    st.counters.fua_writes += 1;
                }
                st.write_from(req.offset_bytes, buf);
                (IoStatus::ok(req.len), None)
            }
            IoOp::Flush => {
                st.stats.ops_flush += 1;
                st.stats.flush_groups += 1;
                (IoStatus::ok(0), None)
            }
            IoOp::Discard | IoOp::WriteZeroes => {
                if st.read_only {
                    return (IoStatus::err(EIO), None);
                }
                if req.op == IoOp::Discard {
                    st.stats.ops_discard += 1;
                } else {
                    st.stats.ops_write_zeroes += 1;
                }
                st.zero_range(req.offset_bytes, len);
                (IoStatus::ok(req.len), None)
            }
        }
    }

    fn defer(&self) -> bool {
        let mut st = lock(&self.engine.st);
        match st.mode {
            CompletionMode::Immediate => false,
            CompletionMode::Deferred => true,
            CompletionMode::Mixed(_) => st.coin(),
        }
    }
}

impl QueueHandler for MemQueue {
    fn on_request(&mut self, req: IoRequest, buf: &mut [u8], done: &mut dyn Completer) {
        let (status, read) = self.execute(&req, buf);
        if self.defer() {
            self.ready.push(Ready {
                tag: req.tag,
                status,
                read,
            });
            self.wake.signal();
            return;
        }
        if let (true, Some(data)) = (status.is_ok(), &read) {
            buf[..data.len()].copy_from_slice(data);
        }
        done.complete(req.tag, status);
    }

    fn poll(&mut self, done: &mut dyn Completer) {
        self.wake.drain();
        let ready = std::mem::take(&mut self.ready);
        lock(&self.engine.st).counters.deferred += ready.len() as u64;
        for r in ready {
            if let (true, Some(data)) = (r.status.is_ok(), &r.read) {
                done.tag_buf(r.tag)[..data.len()].copy_from_slice(data);
            }
            done.complete(r.tag, r.status);
        }
    }

    fn wake_fd(&self) -> BorrowedFd<'_> {
        self.wake.as_fd()
    }
}

/// A [`Completer`] for tests: tag buffers and a log of completions.
#[derive(Debug)]
pub struct TestCompleter {
    buf_len: usize,
    bufs: HashMap<Tag, Vec<u8>>,
    completed: Vec<(Tag, IoStatus)>,
}

impl TestCompleter {
    /// A completer whose tag buffers are `max_io_buf_bytes` long.
    pub fn new(max_io_buf_bytes: u32) -> Self {
        Self {
            buf_len: max_io_buf_bytes as usize,
            bufs: HashMap::new(),
            completed: Vec::new(),
        }
    }

    /// Takes the completions recorded so far.
    pub fn take_completed(&mut self) -> Vec<(Tag, IoStatus)> {
        std::mem::take(&mut self.completed)
    }

    /// The first `len` bytes of a tag's buffer.
    pub fn buf_of(&mut self, tag: Tag, len: usize) -> Vec<u8> {
        self.tag_buf(tag)[..len].to_vec()
    }
}

impl Completer for TestCompleter {
    fn complete(&mut self, tag: Tag, status: IoStatus) {
        assert!(
            !self.completed.iter().any(|(t, _)| *t == tag),
            "tag {tag:?} completed twice"
        );
        self.completed.push((tag, status));
    }

    fn tag_buf(&mut self, tag: Tag) -> &mut [u8] {
        let n = self.buf_len;
        self.bufs.entry(tag).or_insert_with(|| vec![0; n])
    }
}

/// Hands one request to `handler` and waits (polling) until it completes, the
/// way a single-depth ublk queue would. `payload` is the data of a write.
/// Returns the status and, for a successful read, the data.
///
/// # Panics
/// If the handler never completes the request after a few polls, or completes
/// a tag it was not given.
pub fn run_request(
    handler: &mut dyn QueueHandler,
    completer: &mut TestCompleter,
    req: IoRequest,
    payload: &[u8],
) -> (IoStatus, Vec<u8>) {
    let mut buf = match req.op {
        IoOp::Write => payload.to_vec(),
        IoOp::Read => vec![0u8; req.len as usize],
        _ => Vec::new(),
    };
    completer.take_completed();
    handler.on_request(req, &mut buf, completer);
    let mut inline = true;
    for _ in 0..8 {
        let done = std::mem::take(&mut completer.completed);
        if let Some((tag, status)) = done.into_iter().find(|(t, _)| *t == req.tag) {
            let data = if req.op == IoOp::Read && status.is_ok() {
                if inline {
                    buf
                } else {
                    completer.buf_of(tag, req.len as usize)
                }
            } else {
                Vec::new()
            };
            return (status, data);
        }
        inline = false;
        handler.poll(completer);
    }
    panic!("request {req:?} never completed");
}
