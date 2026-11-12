//! Store reads: the R1 check, the stale-hint fallback, the open-segment wait
//! and the read helper pool.
//!
//! **R1** (cross-cutting rule 13): every data read passes the map entry's
//! `id_prefix` (or the full id) and the volume's chunk size to
//! `ExtentCodec::open`; a mismatch is an `extent_auth_failures` event and an
//! error, never data. [`ExtentReader::open_at`] is the engine's only caller
//! of `SegmentStore::read_extent`, and it takes an [`Expectation`], which has
//! no "none" case: a lean volume (`id_len = 0`) reads as a scan
//! (`verify_id`), as R1 says.

use std::collections::{HashSet, VecDeque};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::Duration;

use lunix_format::node::DataEntry;
use lunix_format::segment::FooterBody;
use lunix_format::{ChunkId, ExtentKind, Hint, IdLen, IdPrefix, SegId};
use lunix_iface::block::Tag;
use lunix_iface::crypto::{ExtentCodec, OpenExpect, OpenOutput};
use lunix_iface::index::ChunkIndex;
use lunix_iface::map::{entry_hint, BlockMap, ChunkNo};
use lunix_iface::store::{ExtentLocation, SegmentInfo, SegmentStore, SegmentWriter};
use lunix_iface::{Error, ErrorKind, Result};

use crate::cache::ChunkCache;
use crate::queue::{Deferred, QueueShared};
use crate::stats::{add, Counters};

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// What a store read must match (R1). There is deliberately no variant for
/// "nothing".
#[derive(Clone, Copy, Debug)]
pub enum Expectation<'a> {
    /// The map entry's id prefix (empty on a lean volume, which then reads
    /// as a scan with `verify_id`).
    Prefix(&'a IdPrefix),
    /// The full 32-byte id (dedup confirmation, stale-hint fallback).
    Full(&'a ChunkId),
}

/// Verified chunk reads, as the destage pipeline needs them (partial-chunk
/// base reads and dedup confirmation go through the same R1 path as guest
/// reads).
pub trait ChunkReader: Send + Sync {
    /// Reads, verifies and decompresses the chunk `entry` names, falling
    /// back to the index on a stale hint. `out` is replaced by `chunk_size`
    /// bytes.
    ///
    /// # Errors
    /// `Auth` when no copy verifies, `NotFound` when none exists, `Io`.
    fn read_entry(&self, entry: &DataEntry, out: &mut Vec<u8>) -> Result<()>;

    /// True if the extent at `hint` is a valid extent of exactly `id`
    /// (D17 dedup confirmation). A missing or mismatching extent is `false`.
    ///
    /// # Errors
    /// `Io` and other store failures.
    fn confirm(&self, hint: &Hint, id: &ChunkId) -> Result<bool>;
}

/// Segments being written by the current cut. Their map entries exist from
/// C3, but `*.open` files are not readable, so a reader of such an entry
/// waits until C4 seals them (or the cut abandons them).
#[derive(Default)]
pub(crate) struct OpenSegs {
    set: Mutex<HashSet<SegId>>,
    cv: Condvar,
}

impl OpenSegs {
    pub(crate) fn add(&self, seg: SegId) {
        lock(&self.set).insert(seg);
    }

    pub(crate) fn remove(&self, seg: SegId) {
        lock(&self.set).remove(&seg);
        self.cv.notify_all();
    }

    /// Forgets every segment (a failed destage dropped its writers).
    pub(crate) fn clear(&self) {
        lock(&self.set).clear();
        self.cv.notify_all();
    }

    /// Blocks (worker threads only) until `seg` is no longer being written.
    pub(crate) fn wait_sealed(&self, seg: SegId) -> Result<()> {
        let mut g = lock(&self.set);
        let mut waited = Duration::ZERO;
        while g.contains(&seg) {
            if waited > Duration::from_secs(300) {
                return Err(Error::Busy {
                    what: "segment being sealed",
                });
            }
            let (ng, _) = self
                .cv
                .wait_timeout(g, Duration::from_millis(50))
                .unwrap_or_else(|p| p.into_inner());
            g = ng;
            waited += Duration::from_millis(50);
        }
        Ok(())
    }
}

/// The segment store as the destage pipeline sees it: opening a segment
/// registers it in [`OpenSegs`].
pub(crate) struct TrackedSegments {
    pub(crate) inner: Arc<dyn SegmentStore>,
    pub(crate) open: Arc<OpenSegs>,
}

impl SegmentStore for TrackedSegments {
    fn open_segment(&self) -> Result<Box<dyn SegmentWriter>> {
        let w = self.inner.open_segment()?;
        self.open.add(w.id());
        Ok(w)
    }

    fn read_extent(&self, hint: &Hint, out: &mut Vec<u8>) -> Result<()> {
        self.inner.read_extent(hint, out)
    }

    fn locate(&self, hint: &Hint) -> Result<ExtentLocation> {
        self.inner.locate(hint)
    }

    fn footer(&self, seg: SegId) -> Result<FooterBody> {
        self.inner.footer(seg)
    }

    fn list(&self) -> Result<Vec<SegmentInfo>> {
        self.inner.list()
    }

    fn delete(&self, seg: SegId) -> Result<u64> {
        self.inner.delete(seg)
    }
}

/// The R1 reader.
pub(crate) struct ExtentReader {
    pub(crate) segments: Arc<dyn SegmentStore>,
    pub(crate) index: Arc<dyn ChunkIndex>,
    pub(crate) codec: Arc<dyn ExtentCodec>,
    pub(crate) chunk_size: u32,
    pub(crate) verify_reads: bool,
    pub(crate) open: Arc<OpenSegs>,
    pub(crate) stats: Arc<Counters>,
}

fn stale(e: &Error) -> bool {
    matches!(e.kind(), ErrorKind::NotFound | ErrorKind::Corrupt)
}

impl ExtentReader {
    /// Reads the extent at `hint` and opens it against `exp`. The only call
    /// of `read_extent` in the engine.
    fn open_at(&self, hint: &Hint, exp: Expectation<'_>, out: &mut Vec<u8>) -> Result<()> {
        let mut raw = Vec::with_capacity(self.chunk_size as usize + 128);
        self.segments.read_extent(hint, &mut raw)?;
        let (chunk_id, id_prefix) = match exp {
            Expectation::Prefix(p) if p.id_len() == IdLen::Zero => (None, None),
            Expectation::Prefix(p) => (None, Some(p)),
            Expectation::Full(id) => (Some(id), None),
        };
        // A read with nothing to compare is a scan and recomputes the id (R1).
        let scan = chunk_id.is_none() && id_prefix.is_none();
        let expect = OpenExpect {
            chunk_size: self.chunk_size,
            chunk_id,
            id_prefix,
            verify_id: self.verify_reads || scan,
            output: OpenOutput::Plaintext,
        };
        out.clear();
        self.codec.open(ExtentKind::Data, &raw, &expect, out)?;
        if out.len() != self.chunk_size as usize {
            out.clear();
            return Err(Error::Corrupt {
                what: "extent",
                detail: "plaintext length is not the chunk size",
            });
        }
        Ok(())
    }

    fn auth_failed(&self) {
        add(&self.stats.extent_auth_failures, 1);
    }
}

impl ChunkReader for ExtentReader {
    fn read_entry(&self, entry: &DataEntry, out: &mut Vec<u8>) -> Result<()> {
        let hint = entry_hint(entry);
        self.open.wait_sealed(hint.seg)?;
        let first = match self.open_at(&hint, Expectation::Prefix(&entry.prefix), out) {
            Ok(()) => return Ok(()),
            Err(e) => e,
        };
        if first.kind() == ErrorKind::Auth {
            self.auth_failed();
        } else if !stale(&first) {
            return Err(first);
        }
        if entry.prefix.id_len() == IdLen::Zero {
            return Err(first);
        }
        // Stale hint (DESIGN §7): the index names the current copy; every
        // candidate is confirmed against its full id.
        for cand in self.index.get_by_prefix(&entry.prefix)? {
            if cand.hint == hint || !entry.prefix.matches(&cand.id) {
                continue;
            }
            match self.open_at(&cand.hint, Expectation::Full(&cand.id), out) {
                Ok(()) => return Ok(()),
                Err(e) if e.kind() == ErrorKind::Auth => self.auth_failed(),
                Err(e) if stale(&e) => {}
                Err(e) => return Err(e),
            }
        }
        out.clear();
        Err(first)
    }

    fn confirm(&self, hint: &Hint, id: &ChunkId) -> Result<bool> {
        self.open.wait_sealed(hint.seg)?;
        let mut out = Vec::new();
        match self.open_at(hint, Expectation::Full(id), &mut out) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == ErrorKind::Auth => {
                self.auth_failed();
                Ok(false)
            }
            Err(e) if stale(&e) => Ok(false),
            Err(e) => Err(e),
        }
    }
}

/// One part of a guest read that needs the map or the store.
pub(crate) struct ReadJob {
    pub(crate) queue: Arc<QueueShared>,
    pub(crate) tag: Tag,
    /// Generation of the request on its tag (a tag is reused after completion).
    pub(crate) gen: u64,
    /// Byte offset of this part in the request.
    pub(crate) req_off: usize,
    /// The chunk.
    pub(crate) ci: u64,
    /// Byte offset of the part in the chunk.
    pub(crate) chunk_off: usize,
    /// Bytes.
    pub(crate) len: usize,
    /// The map entry seen on the queue thread, or `None` to ask the map.
    pub(crate) entry: Option<DataEntry>,
}

/// What the helpers share.
pub(crate) struct ReadCtx {
    pub(crate) map: Arc<dyn BlockMap>,
    pub(crate) reader: Arc<ExtentReader>,
    pub(crate) cache: Arc<ChunkCache>,
}

impl ReadCtx {
    fn run(&self, job: &ReadJob) -> Result<Vec<u8>> {
        let entry = match job.entry {
            Some(e) => Some(e),
            None => self.map.get(ChunkNo(job.ci))?,
        };
        let Some(e) = entry else {
            return Ok(vec![0; job.len]);
        };
        let key = (entry_hint(&e), e.prefix);
        let data = match self.cache.get(&key) {
            Some(d) => d,
            None => {
                let mut out = Vec::new();
                self.reader.read_entry(&e, &mut out)?;
                let d: Arc<[u8]> = Arc::from(out);
                self.cache.insert(key, Arc::clone(&d));
                d
            }
        };
        data.get(job.chunk_off..job.chunk_off + job.len)
            .map(<[u8]>::to_vec)
            .ok_or(Error::Corrupt {
                what: "chunk",
                detail: "read past the chunk",
            })
    }
}

struct PoolState {
    jobs: VecDeque<ReadJob>,
    closed: bool,
}

struct PoolShared {
    st: Mutex<PoolState>,
    cv: Condvar,
}

/// The read helper pool: blocking map and store reads off the queue threads.
pub(crate) struct ReadPool {
    shared: Arc<PoolShared>,
    threads: Mutex<Vec<JoinHandle<()>>>,
}

impl ReadPool {
    pub(crate) fn new(ctx: Arc<ReadCtx>, threads: usize) -> Self {
        let shared = Arc::new(PoolShared {
            st: Mutex::new(PoolState {
                jobs: VecDeque::new(),
                closed: false,
            }),
            cv: Condvar::new(),
        });
        let handles = (0..threads.max(1))
            .map(|i| {
                let shared = Arc::clone(&shared);
                let ctx = Arc::clone(&ctx);
                std::thread::Builder::new()
                    .name(format!("lunix-read-{i}"))
                    .spawn(move || helper(&shared, &ctx))
                    .expect("spawn a read helper")
            })
            .collect();
        Self {
            shared,
            threads: Mutex::new(handles),
        }
    }

    /// Queues a job. Non-blocking (a short lock): the number of jobs is
    /// bounded by the tags in flight.
    pub(crate) fn submit(&self, job: ReadJob) {
        lock(&self.shared.st).jobs.push_back(job);
        self.shared.cv.notify_one();
    }

    pub(crate) fn shutdown(&self) {
        lock(&self.shared.st).closed = true;
        self.shared.cv.notify_all();
        for h in lock(&self.threads).drain(..) {
            let _ = h.join();
        }
    }
}

fn helper(shared: &PoolShared, ctx: &ReadCtx) {
    loop {
        let job = {
            let mut st = lock(&shared.st);
            loop {
                if let Some(j) = st.jobs.pop_front() {
                    break j;
                }
                if st.closed {
                    return;
                }
                st = shared.cv.wait(st).unwrap_or_else(|p| p.into_inner());
            }
        };
        let result = ctx.run(&job).map_err(|e| e.errno());
        job.queue.post(Deferred::ReadPart {
            tag: job.tag,
            gen: job.gen,
            req_off: job.req_off,
            result,
        });
    }
}
