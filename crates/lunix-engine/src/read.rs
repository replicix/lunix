//! Store reads: the R1 check, the stale-hint fallback, the revalidating read
//! (C7) and the read helper pool.
//!
//! **R1** (cross-cutting rule 13): every data read passes the map entry's
//! `id_prefix` (or the full id) and the volume's chunk size to
//! `ExtentCodec::open`; a mismatch is an error, never data, and an
//! `extent_auth_failures` event once it is known to concern the block's
//! current entry (see below). [`ExtentReader::open_at`] is the engine's only
//! caller of `SegmentStore::read_extent`, and it takes an [`Expectation`],
//! which has no "none" case: a lean volume (`id_len = 0`) reads as a scan
//! (`verify_id`), as R1 says.
//!
//! **C7** (revalidating read): a guest read part whose entry went stale (its
//! segment collected, its hint failing authentication) looks the block up in
//! the map again and reads the new entry; see [`ReadCtx`]. An `Auth` event is
//! counted only when the map still names the entry that failed, so a read
//! that raced a collection never raises a false integrity alarm. The
//! destage base read (`ChunkReader::read_entry`) does not revalidate and
//! counts at once: it runs inside a cut, and a GC step runs only between
//! cuts with the map at the tip. Out-of-process readers of a live volume
//! cannot revalidate and must accept `NotFound`.

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::Duration;

use lunix_format::node::DataEntry;
use lunix_format::{ChunkId, ExtentKind, Hint, IdLen, IdPrefix};
use lunix_iface::block::Tag;
use lunix_iface::clock::Clock;
use lunix_iface::crypto::{ExtentCodec, OpenExpect, OpenOutput};
use lunix_iface::index::ChunkIndex;
use lunix_iface::map::{entry_hint, BlockMap, ChunkNo};
use lunix_iface::store::SegmentStore;
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
    /// bytes. Every `Auth` failure is counted in `extent_auth_failures` at
    /// once: this reader does not re-resolve the block (guest reads do, in
    /// `ReadCtx`).
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

/// The R1 reader.
pub(crate) struct ExtentReader {
    pub(crate) segments: Arc<dyn SegmentStore>,
    pub(crate) index: Arc<dyn ChunkIndex>,
    pub(crate) codec: Arc<dyn ExtentCodec>,
    pub(crate) chunk_size: u32,
    pub(crate) verify_reads: bool,
    pub(crate) stats: Arc<Counters>,
}

fn stale(e: &Error) -> bool {
    matches!(e.kind(), ErrorKind::NotFound | ErrorKind::Corrupt)
}

/// Failures after which a guest read looks the block up again (C7): the
/// extent is gone, damaged or not the block's any more.
fn revalidates(e: &Error) -> bool {
    stale(e) || e.kind() == ErrorKind::Auth
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

    fn count_auth(&self, n: u64) {
        if n > 0 {
            add(&self.stats.extent_auth_failures, n);
        }
    }

    /// [`ChunkReader::read_entry`] with the `Auth` events of this attempt
    /// added to `auth` instead of the counter, so a caller that can
    /// re-resolve the block decides whether they count (C7: not while the
    /// hint was stale).
    fn read_entry_deferred(
        &self,
        entry: &DataEntry,
        out: &mut Vec<u8>,
        auth: &mut u64,
    ) -> Result<()> {
        let hint = entry_hint(entry);
        let first = match self.open_at(&hint, Expectation::Prefix(&entry.prefix), out) {
            Ok(()) => return Ok(()),
            Err(e) => e,
        };
        if first.kind() == ErrorKind::Auth {
            *auth += 1;
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
                Err(e) if e.kind() == ErrorKind::Auth => *auth += 1,
                Err(e) if stale(&e) => {}
                Err(e) => return Err(e),
            }
        }
        out.clear();
        Err(first)
    }
}

impl ChunkReader for ExtentReader {
    fn read_entry(&self, entry: &DataEntry, out: &mut Vec<u8>) -> Result<()> {
        let mut auth = 0;
        let r = self.read_entry_deferred(entry, out, &mut auth);
        self.count_auth(auth);
        r
    }

    fn confirm(&self, hint: &Hint, id: &ChunkId) -> Result<bool> {
        let mut out = Vec::new();
        match self.open_at(hint, Expectation::Full(id), &mut out) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == ErrorKind::Auth => {
                self.count_auth(1);
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
    /// `clock.monotonic()` when `entry` was taken from the map; the age of a
    /// hint that goes stale (`read_stale_age_*`). Unused when `entry` is
    /// `None`: the helper's own lookup stamps its time.
    pub(crate) born: Duration,
}

/// What the helpers share.
pub(crate) struct ReadCtx {
    pub(crate) map: Arc<dyn BlockMap>,
    pub(crate) reader: Arc<ExtentReader>,
    pub(crate) cache: Arc<ChunkCache>,
    /// The engine's clock, for the age of a stale hint.
    pub(crate) clock: Arc<dyn Clock>,
}

/// Retries of one read part after a failed read (C7). Each needs an observed
/// change of the block's entry (a cut, a re-hint) during the read, so the
/// bound costs nothing in normal operation; a part still failing after these
/// reports its error and counts `read_revalidate_exhausted`.
#[doc(hidden)]
pub const REVALIDATE_MAX: u32 = 16;

impl ReadCtx {
    /// The decoded chunk `e` names, from the cache or the store. `Auth`
    /// events are added to `auth`, not counted.
    fn chunk(&self, e: &DataEntry, auth: &mut u64) -> Result<Arc<[u8]>> {
        let key = (entry_hint(e), e.prefix);
        if let Some(d) = self.cache.get(&key) {
            return Ok(d);
        }
        let mut out = Vec::new();
        self.reader.read_entry_deferred(e, &mut out, auth)?;
        let d: Arc<[u8]> = Arc::from(out);
        self.cache.insert(key, Arc::clone(&d));
        Ok(d)
    }

    /// One read part. The entry the job carries (or the map gave) can be
    /// stale by the time the store is read: a cut re-hinted the block, or
    /// the overwritten chunk's segment was collected. When the read then
    /// fails with `NotFound`, `Corrupt` or `Auth`, the block is looked up
    /// again and a changed entry is read instead (C7). `Auth` events are
    /// counted only if the map still names the entry that failed, also when
    /// the failed hint was bridged by the index fallback and the read
    /// succeeded. Nothing is added to the success path but a local counter.
    ///
    /// A retried part reads the map as it is now, so the parts of one request
    /// may come from different snapshots (a part retried after a concurrent
    /// write is newer than its siblings). That is allowed for a read that
    /// overlaps a write; it is never older than the request's start.
    fn run(&self, job: &ReadJob) -> Result<Vec<u8>> {
        let (mut entry, mut held_since) = match job.entry {
            Some(e) => (Some(e), job.born),
            None => {
                let e = self.map.get(ChunkNo(job.ci))?;
                (e, self.clock.monotonic())
            }
        };
        let mut retries = 0;
        loop {
            let Some(e) = entry else {
                return Ok(vec![0; job.len]);
            };
            let mut auth = 0;
            let err = match self.chunk(&e, &mut auth) {
                Ok(data) => {
                    if auth > 0 {
                        self.settle_auth(job.ci, &e, auth);
                    }
                    return data
                        .get(job.chunk_off..job.chunk_off + job.len)
                        .map(<[u8]>::to_vec)
                        .ok_or(Error::Corrupt {
                            what: "chunk",
                            detail: "read past the chunk",
                        });
                }
                Err(err) => err,
            };
            match self.after_failure(job.ci, &e, &err, auth, retries, held_since) {
                Some(next) => {
                    entry = next;
                    held_since = self.clock.monotonic();
                    retries += 1;
                }
                None => return Err(err),
            }
        }
    }

    /// A read of `e` succeeded after `auth` failed authentications (the hint
    /// was bridged by the index): they are real only if the block still
    /// names `e`.
    #[cold]
    fn settle_auth(&self, ci: u64, e: &DataEntry, auth: u64) {
        match self.map.get(ChunkNo(ci)) {
            Ok(now) if now != Some(*e) => {}
            Ok(_) => self.reader.count_auth(auth),
            Err(err) => {
                tracing::debug!(chunk = ci, error = %err, "re-lookup failed; counting the authentication failure");
                self.reader.count_auth(auth);
            }
        }
    }

    /// The read of `e` failed with `err` after `auth` failed authentications.
    /// `Some(next)` retries with the block's new entry; `None` reports `err`
    /// (the events counted if they concern the current entry).
    #[cold]
    fn after_failure(
        &self,
        ci: u64,
        e: &DataEntry,
        err: &Error,
        auth: u64,
        retries: u32,
        held_since: Duration,
    ) -> Option<Option<DataEntry>> {
        if !revalidates(err) {
            self.reader.count_auth(auth);
            return None;
        }
        let now = match self.map.get(ChunkNo(ci)) {
            Ok(now) => now,
            Err(me) => {
                tracing::debug!(chunk = ci, error = %me, "re-lookup failed; reporting the read error");
                self.reader.count_auth(auth);
                return None;
            }
        };
        if now == Some(*e) {
            // Nothing changed: the error is real.
            self.reader.count_auth(auth);
            return None;
        }
        // The attempt's events concern an entry the block no longer names.
        if retries >= REVALIDATE_MAX {
            add(&self.reader.stats.read_revalidate_exhausted, 1);
            tracing::warn!(chunk = ci, retries, error = %err, "read part still failing after its entry changed; giving up");
            return None;
        }
        add(&self.reader.stats.read_revalidated, 1);
        if err.kind() == ErrorKind::NotFound {
            // The extent vanished under a hint taken `age` ago: a collected
            // segment must have waited out its grace longer than that.
            let age = self.clock.monotonic().saturating_sub(held_since);
            self.reader
                .stats
                .note_stale_age(u64::try_from(age.as_millis()).unwrap_or(u64::MAX));
        }
        tracing::debug!(chunk = ci, error = %err, "read part retried with the block's new entry");
        Some(now)
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
