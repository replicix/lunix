//! One worker's per-chunk pipeline: assemble, zero scan, id, dedup, compress,
//! seal (steps 1 to 6). Packing and the map update are the caller's, in order.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use lunix_format::node::DataEntry;
use lunix_format::{ChunkId, Codec, ExtentKind, Hint, IdPrefix, LineageIdx, WriteSeq};
use lunix_iface::crypto::EncodingDesc;
use lunix_iface::index::Confirm;
use lunix_iface::overlay::CutChunk;
use lunix_iface::{Error, Result};

use crate::assemble::{assemble, check_pieces, Assembled};
use crate::compress::{Compressor, Pressure};
use crate::deps::DestageDeps;
use crate::read::confirm_hint;
use crate::scan::is_all_zero;
use crate::stats::{ns, DestageStats};

/// A failure and the chunk it happened at, where there is one.
pub(crate) struct Failure {
    pub error: Error,
    pub chunk: Option<u64>,
}

impl Failure {
    pub(crate) fn at(error: Error, chunk: u64) -> Self {
        Self {
            error,
            chunk: Some(chunk),
        }
    }

    pub(crate) fn general(error: Error) -> Self {
        Self { error, chunk: None }
    }
}

pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// A sealed extent waiting to be appended.
pub(crate) struct Sealed {
    pub extent: Vec<u8>,
    pub codec: Codec,
    pub ulen: u32,
    pub clen: u32,
}

enum SlotState {
    Pending,
    Ready(Sealed),
    Taken,
}

/// The place where the first worker to reach a chunk id puts its sealed extent.
/// Workers that reach the same id later (the same data written twice in one
/// cut) do not compress it again; the in-order packer takes the extent from
/// the slot when the first chunk with that id comes up in LBA order, whichever
/// worker made it, so the layout does not depend on thread timing.
pub(crate) struct SealSlot {
    st: Mutex<SlotState>,
    cv: Condvar,
}

impl SealSlot {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            st: Mutex::new(SlotState::Pending),
            cv: Condvar::new(),
        })
    }

    fn fill(&self, s: Sealed) {
        *lock(&self.st) = SlotState::Ready(s);
        self.cv.notify_all();
    }

    /// Waits for the producer and takes the extent. `stop` is polled while
    /// waiting (a failed or cancelled cut never fills the slot).
    pub(crate) fn take(&self, stop: &dyn Fn() -> bool) -> Result<Sealed> {
        let mut g = lock(&self.st);
        loop {
            match &*g {
                SlotState::Ready(_) => {
                    return match std::mem::replace(&mut *g, SlotState::Taken) {
                        SlotState::Ready(s) => Ok(s),
                        _ => Err(Error::invalid("sealed extent slot changed under its lock")),
                    };
                }
                SlotState::Taken => return Err(Error::invalid("sealed extent taken twice")),
                SlotState::Pending => {}
            }
            if stop() {
                return Err(Error::Closed);
            }
            g = self
                .cv
                .wait_timeout(g, Duration::from_millis(20))
                .unwrap_or_else(|p| p.into_inner())
                .0;
        }
    }
}

/// What the packer must do for one chunk.
pub(crate) enum Action {
    /// The map entry becomes a hole.
    Hole,
    /// The chunk is already stored (index hit, verified or confirmed).
    Adopt { id: ChunkId, hint: Hint },
    /// Store the sealed extent of `id`, unless an earlier chunk of the cut
    /// with the same id already did.
    Store { id: ChunkId, slot: Arc<SealSlot> },
}

pub(crate) struct ChunkResult {
    pub chunk: u64,
    pub action: Action,
    /// `write_seq` range of the chunk's pieces.
    pub seq: (WriteSeq, WriteSeq),
}

pub(crate) struct JobResult {
    pub chunks: Vec<ChunkResult>,
    pub stats: DestageStats,
}

/// State shared by the workers of one cut.
pub(crate) struct Claims(Mutex<HashMap<ChunkId, Arc<SealSlot>>>);

impl Claims {
    pub(crate) fn new() -> Self {
        Self(Mutex::new(HashMap::new()))
    }

    /// The slot of `id` and whether the caller made it (and so must fill it).
    fn claim(&self, id: ChunkId) -> (Arc<SealSlot>, bool) {
        let mut m = lock(&self.0);
        match m.get(&id) {
            Some(s) => (Arc::clone(s), false),
            None => {
                let s = SealSlot::new();
                m.insert(id, Arc::clone(&s));
                (s, true)
            }
        }
    }
}

pub(crate) fn entry_for(id: &ChunkId, hint: &Hint, id_len: lunix_format::IdLen) -> DataEntry {
    DataEntry {
        lineage: LineageIdx(0),
        seg: hint.seg,
        off: hint.off,
        clen: hint.clen,
        prefix: IdPrefix::of(id, id_len),
    }
}

/// One worker thread's buffers and compression contexts.
pub(crate) struct Worker<'a> {
    deps: &'a DestageDeps<'a>,
    claims: &'a Claims,
    cancel: &'a (dyn Fn() -> bool + Sync),
    compressor: Compressor,
    buf: Vec<u8>,
    raw: Vec<u8>,
    scratch: Vec<u8>,
    payload: Vec<u8>,
}

impl<'a> Worker<'a> {
    pub(crate) fn new(
        deps: &'a DestageDeps<'a>,
        claims: &'a Claims,
        cancel: &'a (dyn Fn() -> bool + Sync),
    ) -> Self {
        let p = &deps.params;
        let c = &deps.config;
        Self {
            deps,
            claims,
            cancel,
            compressor: Compressor::new(p.policy, p.level, c.probe_len, c.probe_min_saving_percent),
            buf: Vec::new(),
            raw: Vec::new(),
            scratch: Vec::new(),
            payload: Vec::new(),
        }
    }

    /// Runs one job. `stats` accumulates even when the job fails, so that a
    /// failed cut still reports what it did.
    pub(crate) fn job(
        &mut self,
        chunks: &[CutChunk],
        cut_seq: WriteSeq,
        pressure: Pressure,
        stats: &mut DestageStats,
    ) -> std::result::Result<Vec<ChunkResult>, Failure> {
        let mut out = Vec::with_capacity(chunks.len());
        for cc in chunks {
            if (self.cancel)() {
                return Err(Failure::general(Error::Closed));
            }
            out.push(
                self.chunk(cc, cut_seq, pressure, stats)
                    .map_err(|e| Failure::at(e, cc.chunk))?,
            );
        }
        Ok(out)
    }

    fn chunk(
        &mut self,
        cc: &CutChunk,
        cut_seq: WriteSeq,
        pressure: Pressure,
        stats: &mut DestageStats,
    ) -> Result<ChunkResult> {
        let deps = self.deps;
        let size = deps.params.chunk_size;
        let t = Instant::now();
        let facts = check_pieces(cc, deps.params.chunk_blocks(), cut_seq)?;
        if cc.chunk >= deps.map.chunk_count() {
            return Err(Error::invalid("cut chunk beyond the map"));
        }
        stats.chunks_total += 1;
        stats.bytes_in += facts.covered * crate::config::BLOCK;
        let result = |action| ChunkResult {
            chunk: cc.chunk,
            action,
            seq: facts.seq,
        };

        // 1. Assemble.
        let assembled = assemble(deps, cc, &facts, &mut self.buf, &mut self.raw, stats)?;
        stats.assemble_ns += ns(t.elapsed());
        lunix_iface::crashpoint!("destage.after_assemble");
        if matches!(assembled, Assembled::Hole) {
            stats.zero_holes += 1;
            stats.hole_covered += 1;
            return Ok(result(Action::Hole));
        }

        // 2. Zero scan.
        let t = Instant::now();
        let zero = is_all_zero(&self.buf);
        stats.scan_ns += ns(t.elapsed());
        if zero {
            stats.zero_holes += 1;
            return Ok(result(Action::Hole));
        }

        // 3. Chunk id.
        let t = Instant::now();
        let id = deps.codec.chunk_id(ExtentKind::Data, &self.buf);
        stats.hash_ns += ns(t.elapsed());

        // 4. Dedup against the index; a hit from a persisted run is confirmed first.
        let t = Instant::now();
        let hit = deps.index.get_for_dedup(&id)?;
        let adopted = match hit {
            None => None,
            // A verified hint is trusted, but the map entry's invariant is kept
            // locally: a payload is 1 ..= chunk size bytes.
            Some(h) if h.confirm == Confirm::Verified => {
                if h.hint.clen == 0 || h.hint.clen > size {
                    stats.dedup_rejected += 1;
                    None
                } else {
                    Some(h.hint)
                }
            }
            Some(h) => {
                if confirm_hint(deps, &id, &h.hint, &mut self.raw, &mut self.scratch, stats)? {
                    stats.dedup_confirmed += 1;
                    Some(h.hint)
                } else {
                    stats.dedup_rejected += 1;
                    None
                }
            }
        };
        stats.dedup_ns += ns(t.elapsed());
        if let Some(hint) = adopted {
            stats.dedup_hits += 1;
            return Ok(result(Action::Adopt { id, hint }));
        }

        // The same data elsewhere in this cut: the first worker seals it.
        let (slot, producer) = self.claims.claim(id);
        if !producer {
            return Ok(result(Action::Store { id, slot }));
        }

        // 5. Compress.
        let t = Instant::now();
        let choice = self
            .compressor
            .compress(&self.buf, pressure, &mut self.payload, stats)?;
        stats.compress_ns += ns(t.elapsed());
        let compressed: &[u8] = if choice.codec == Codec::Raw {
            &self.buf
        } else {
            &self.payload
        };
        let clen = u32::try_from(compressed.len()).map_err(|_| Error::invalid("clen"))?;

        // 6. Seal (a fresh salt is drawn inside `seal`).
        let t = Instant::now();
        let desc = EncodingDesc {
            codec: choice.codec,
            level: choice.level,
            ulen: size,
            dict_id: None,
        };
        let mut extent = Vec::with_capacity(compressed.len() + 128);
        deps.codec
            .seal(ExtentKind::Data, &id, &desc, compressed, &mut extent)?;
        stats.seal_ns += ns(t.elapsed());
        slot.fill(Sealed {
            extent,
            codec: choice.codec,
            ulen: size,
            clen,
        });
        Ok(result(Action::Store { id, slot }))
    }
}

/// The cancel flag a worker polls: its own and the caller's.
pub(crate) fn stopped(own: &AtomicBool, external: Option<&AtomicBool>) -> bool {
    own.load(Ordering::Acquire) || external.is_some_and(|e| e.load(Ordering::Acquire))
}
