//! The dirty overlay: an in-memory, LBA-sharded interval map of the data the
//! journal holds and the map does not yet (DESIGN sections 4.2 and 8.1).
//!
//! [`ShardedOverlay`] implements [`lunix_iface::overlay::Overlay`]; read the
//! trait for the contract. This page says how it is built.
//!
//! # Shape
//!
//! * The LBA space is cut into **stripes** of `stripe_chunks` whole chunks
//!   (default: at least 4096 blocks, 16 MiB). Stripe `n` belongs to shard
//!   `n % shards` (default 64 shards). A stripe is a multiple of the chunk, so
//!   a chunk never spans two shards, and an interval never spans two stripes.
//!   (The plan says "shard by `lba / chunk`"; a stripe of several chunks keeps
//!   that property and lets a long sequential write stay one interval per
//!   stripe instead of one per chunk. See `OverlayOptions::stripe_chunks`.)
//! * A shard is a `BTreeMap<start_lba, Entry>` of non-overlapping intervals.
//!   An entry is `{blocks, journal location | hole, write_seq}` in 32 bytes;
//!   with the key and the B-tree's fill that is about 60 to 85 bytes per
//!   unmergeable entry (the memory gate is 96).
//! * One version per block: the entry with the newest `write_seq` wins, and an
//!   equal `write_seq` replaces (a later insert of the same record's range). A
//!   late insert with an older `write_seq` only fills the blocks that no newer
//!   entry holds, so arrival order never matters.
//! * Neighbouring entries merge when they have the same `write_seq` and the
//!   same kind and, for journal data, contiguous locations. Records have
//!   distinct `write_seq`, so only one record's own range (or a repeated
//!   insert of it) merges; that is by design, because the `write_seq` of each
//!   block is needed by [`cut`](Overlay::cut) and [`release`](Overlay::release).
//!
//! # Locking
//!
//! Each shard is a `std::sync::RwLock`. `insert_*` takes the write lock of one
//! shard at a time (a range that crosses stripes is applied stripe by stripe,
//! never holding two locks), for a time proportional to the entries it
//! overlaps. `lookup` takes the read lock of one shard at a time, so readers
//! run in parallel. `cut` takes read locks only, one shard at a time, and
//! copies; it never blocks `lookup`. `release` takes each shard's write lock
//! once and scans it (`O(entries in the shard)`), on the destage thread. No
//! lock is held across I/O or a wait, and no call takes a lock while holding
//! another, so there is no lock order and no deadlock. A poisoned lock is
//! recovered (the map is consistent after every single `BTreeMap` call).
//!
//! # MVCC choice
//!
//! Entries are **single-version**; a cut is a copy taken under the shard read
//! locks. The returned [`OverlayCut`] is owned data, so later inserts cannot
//! change it. The alternative (versioned entries) would keep an overwritten
//! older version alive until release; the trait never asks for it, and
//! `FlatOverlay` (the test oracle) does not keep it either. The
//! consequence, shared with the oracle: a block whose newest entry has
//! `write_seq > c` is **absent** from `cut(c)`, even if an older entry
//! `<= c` once covered it. Its newer record covers the block in a later cut.
//!
//! `release(cut)` removes every entry with `write_seq <= cut.cut_seq`, as the
//! oracle does. That equals "the entries of the cut that are still visible"
//! provided that no insert with `write_seq <= c` arrives after the cut; the
//! cut protocol guarantees it by waiting for the in-flight writes (DESIGN
//! section 8.1 step 2) before `cut(c)`.
//!
//! # Dirty-memory cap
//!
//! [`dirty_bytes`](Overlay::dirty_bytes) is exact. [`OverlayOptions::dirty_cap_bytes`]
//! (default 1 GiB, the `B` trigger of DESIGN section 8.2) and
//! [`ShardedOverlay::over_cap`] let the engine ask "must I seal now?" after each
//! insert without any extra state: the cap itself never blocks an insert.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};

use lunix_format::WriteSeq;
use lunix_iface::journal::{JournalLoc, LaneId};
use lunix_iface::overlay::{BlockRange, CutChunk, Overlay, OverlayCut, Piece, PieceSource};
use lunix_iface::{Error, Result};

const BLOCK: u64 = 4096;

/// Default dirty cap: 1 GiB of data, the `B` commit trigger.
pub const DEFAULT_DIRTY_CAP_BYTES: u64 = 1 << 30;

/// Construction parameters of a [`ShardedOverlay`].
#[derive(Clone, Copy, Debug)]
pub struct OverlayOptions {
    /// Blocks per chunk (cuts group by chunk). At least 1.
    pub chunk_blocks: u64,
    /// Number of shards (default 64). At least 1.
    pub shards: usize,
    /// Chunks per stripe, the unit that is assigned to a shard (default: enough
    /// chunks for at least 4096 blocks). A stripe must not exceed `u32::MAX`
    /// blocks. Small values make a range cross shards often; tests use them.
    pub stripe_chunks: u64,
    /// The dirty-data cap in bytes for [`ShardedOverlay::over_cap`].
    pub dirty_cap_bytes: u64,
}

impl OverlayOptions {
    /// The defaults for `chunk_blocks`.
    pub fn new(chunk_blocks: u64) -> Self {
        Self {
            chunk_blocks,
            shards: 64,
            stripe_chunks: 4096u64.div_ceil(chunk_blocks.max(1)).max(1),
            dirty_cap_bytes: DEFAULT_DIRTY_CAP_BYTES,
        }
    }
}

/// One interval: 32 bytes.
#[derive(Clone, Copy, Debug)]
struct Entry {
    seq: u64,
    /// Byte offset of the first block (unused for a hole).
    offset: u64,
    ordinal: u32,
    blocks: u32,
    lane: u16,
    hole: bool,
}

/// What an insert writes.
#[derive(Clone, Copy)]
enum Src {
    Hole,
    Journal(JournalLoc),
}

impl Entry {
    fn new(src: Src, skip_blocks: u64, blocks: u64, seq: u64) -> Self {
        let blocks = u32::try_from(blocks).expect("an interval stays inside a stripe");
        match src {
            Src::Hole => Self {
                seq,
                offset: 0,
                ordinal: 0,
                blocks,
                lane: 0,
                hole: true,
            },
            Src::Journal(l) => Self {
                seq,
                offset: l.offset + skip_blocks * BLOCK,
                ordinal: l.ordinal,
                blocks,
                lane: l.lane.0,
                hole: false,
            },
        }
    }

    fn len(&self) -> u64 {
        u64::from(self.blocks)
    }

    fn source(&self, skip_blocks: u64) -> PieceSource {
        if self.hole {
            PieceSource::Hole
        } else {
            PieceSource::Journal(JournalLoc {
                lane: LaneId(self.lane),
                ordinal: self.ordinal,
                offset: self.offset + skip_blocks * BLOCK,
            })
        }
    }

    /// The part from `skip` blocks on, with `blocks` blocks.
    fn part(&self, skip: u64, blocks: u64) -> Self {
        Self {
            offset: if self.hole {
                0
            } else {
                self.offset + skip * BLOCK
            },
            blocks: u32::try_from(blocks).expect("part of an interval"),
            ..*self
        }
    }

    /// `next`, which starts where `self` ends, can be one entry with `self`.
    fn continues(&self, next: &Self) -> bool {
        self.seq == next.seq
            && self.hole == next.hole
            && (self.hole
                || (self.lane == next.lane
                    && self.ordinal == next.ordinal
                    && self.offset + self.len() * BLOCK == next.offset))
            && self.len() + next.len() <= u64::from(u32::MAX)
    }
}

type Shard = BTreeMap<u64, Entry>;

fn read(s: &RwLock<Shard>) -> RwLockReadGuard<'_, Shard> {
    s.read().unwrap_or_else(|p| p.into_inner())
}

fn write(s: &RwLock<Shard>) -> RwLockWriteGuard<'_, Shard> {
    s.write().unwrap_or_else(|p| p.into_inner())
}

/// Puts `[a, b)` (inside one stripe) as new data into `map`, merging with its
/// neighbours. `s` is the first block of the whole stripe piece that `src`
/// describes.
fn fill(map: &mut Shard, stripe: u64, s: u64, a: u64, b: u64, seq: u64, src: Src) {
    if a >= b {
        return;
    }
    let ent = Entry::new(src, a - s, b - a, seq);
    // Never merge across a stripe boundary: an interval stays inside its stripe.
    let prev = if a.is_multiple_of(stripe) {
        None
    } else {
        map.range_mut(..a).next_back()
    };
    let key = match prev {
        Some((&pk, p)) if pk + p.len() == a && p.continues(&ent) => {
            p.blocks += ent.blocks;
            pk
        }
        _ => {
            map.insert(a, ent);
            a
        }
    };
    let cur = map[&key];
    let next = if b.is_multiple_of(stripe) {
        None
    } else {
        map.get(&b)
    };
    if let Some(&n) = next {
        if cur.continues(&n) {
            map.remove(&b);
            if let Some(c) = map.get_mut(&key) {
                c.blocks += n.blocks;
            }
        }
    }
}

/// Applies one insert of `[s, e)` to a shard; returns blocks `(removed, added)`.
fn put(map: &mut Shard, stripe: u64, s: u64, e: u64, seq: u64, src: Src) -> (u64, u64) {
    let first = match map.range(..=s).next_back() {
        Some((&k, en)) if k + en.len() > s => k,
        _ => s,
    };
    let (mut removed, mut added) = (0u64, 0u64);
    let mut cursor = s;
    let mut from = first;
    while let Some((&k, &o)) = map.range(from..e).next() {
        let o_end = k + o.len();
        map.remove(&k);
        removed += o.len();
        if k < s {
            map.insert(k, o.part(0, s - k));
            added += s - k;
        }
        if o_end > e {
            map.insert(e, o.part(e - k, o_end - e));
            added += o_end - e;
        }
        let (ms, me) = (k.max(s), o_end.min(e));
        if o.seq > seq {
            // A newer entry stays; the new data fills only around it.
            fill(map, stripe, s, cursor, ms, seq, src);
            added += ms.saturating_sub(cursor);
            map.insert(ms, o.part(ms - k, me - ms));
            added += me - ms;
            cursor = me;
        }
        from = me;
    }
    fill(map, stripe, s, cursor, e, seq, src);
    added += e - cursor;
    (removed, added)
}

fn push_none(out: &mut Vec<Piece>, lba: u64, blocks: u64) {
    if let Some(last) = out.last_mut() {
        if matches!(last.source, PieceSource::None) && last.range.end() == Some(lba) {
            last.range.blocks += blocks;
            return;
        }
    }
    out.push(Piece {
        range: BlockRange { lba, blocks },
        source: PieceSource::None,
        write_seq: WriteSeq(0),
    });
}

/// The sharded interval-map overlay. See the [crate docs](crate).
#[derive(Debug)]
pub struct ShardedOverlay {
    opts: OverlayOptions,
    stripe_blocks: u64,
    shards: Box<[RwLock<Shard>]>,
    dirty_blocks: AtomicU64,
}

impl ShardedOverlay {
    /// An overlay with the default options for `chunk_blocks`.
    ///
    /// # Panics
    /// If `chunk_blocks` is zero or the default stripe does not fit in
    /// `u32` blocks (a chunk above 2^32 blocks).
    pub fn new(chunk_blocks: u64) -> Self {
        Self::with_options(OverlayOptions::new(chunk_blocks)).expect("valid default options")
    }

    /// An overlay with explicit options.
    ///
    /// # Errors
    /// `Invalid` if a count is zero or the stripe exceeds `u32::MAX` blocks.
    pub fn with_options(opts: OverlayOptions) -> Result<Self> {
        if opts.chunk_blocks == 0 || opts.shards == 0 || opts.stripe_chunks == 0 {
            return Err(Error::invalid("overlay options must be at least 1"));
        }
        let stripe_blocks = opts
            .chunk_blocks
            .checked_mul(opts.stripe_chunks)
            .filter(|&s| s <= u64::from(u32::MAX))
            .ok_or(Error::invalid("overlay stripe is too large"))?;
        Ok(Self {
            opts,
            stripe_blocks,
            shards: (0..opts.shards)
                .map(|_| RwLock::new(Shard::new()))
                .collect(),
            dirty_blocks: AtomicU64::new(0),
        })
    }

    /// True once [`dirty_bytes`](Overlay::dirty_bytes) reached the configured
    /// cap: the engine seals (cuts and destages) before admitting more.
    pub fn over_cap(&self) -> bool {
        self.dirty_bytes() >= self.opts.dirty_cap_bytes
    }

    /// The configured dirty cap in bytes.
    pub fn dirty_cap_bytes(&self) -> u64 {
        self.opts.dirty_cap_bytes
    }

    /// Number of intervals held (for tests and metrics; takes every read lock
    /// in turn).
    pub fn entries(&self) -> usize {
        self.shards.iter().map(|s| read(s).len()).sum()
    }

    /// Checks the structural invariants: intervals are non-empty, ordered,
    /// non-overlapping, inside one stripe of their own shard, and the dirty
    /// counter equals the blocks held. Meant for tests; quiesce writers first.
    ///
    /// # Errors
    /// A description of the first violation.
    pub fn validate(&self) -> std::result::Result<(), String> {
        let mut total = 0u64;
        for (i, sh) in self.shards.iter().enumerate() {
            let m = read(sh);
            let mut prev_end = 0u64;
            for (&k, e) in m.iter() {
                if e.blocks == 0 {
                    return Err(format!("empty interval at {k}"));
                }
                if k < prev_end {
                    return Err(format!("overlap at {k}"));
                }
                prev_end = k + e.len();
                if k / self.stripe_blocks != (prev_end - 1) / self.stripe_blocks {
                    return Err(format!("interval at {k} crosses a stripe"));
                }
                if self.shard_of(k) != i {
                    return Err(format!("interval at {k} is in the wrong shard"));
                }
                total += e.len();
            }
        }
        let counted = self.dirty_blocks.load(Ordering::Relaxed);
        if total == counted {
            Ok(())
        } else {
            Err(format!("dirty counter {counted} but {total} blocks held"))
        }
    }

    fn shard_of(&self, lba: u64) -> usize {
        // The remainder is below `shards`, a `usize`.
        usize::try_from((lba / self.stripe_blocks) % self.shards.len() as u64)
            .expect("below the shard count")
    }

    /// Calls `f(shard, a, b)` for each stripe piece `[a, b)` of the range.
    fn for_stripes(&self, lba: u64, end: u64, mut f: impl FnMut(&RwLock<Shard>, u64, u64)) {
        let mut a = lba;
        while a < end {
            let b = end.min((a - a % self.stripe_blocks).saturating_add(self.stripe_blocks));
            f(&self.shards[self.shard_of(a)], a, b);
            a = b;
        }
    }

    fn adjust(&self, removed: u64, added: u64) {
        if added >= removed {
            self.dirty_blocks
                .fetch_add(added - removed, Ordering::Relaxed);
        } else {
            self.dirty_blocks
                .fetch_sub(removed - added, Ordering::Relaxed);
        }
    }

    fn insert(&self, range: BlockRange, src: Src, seq: WriteSeq) {
        if range.blocks == 0 {
            return;
        }
        let end = range
            .end()
            .expect("overlay range end overflows the block address space");
        if let Src::Journal(l) = src {
            let bytes = range
                .blocks
                .checked_mul(BLOCK)
                .expect("overlay range overflows the byte address space");
            l.advance(bytes).expect("journal offset overflow");
        }
        self.for_stripes(range.lba, end, |shard, a, b| {
            // The location of block `a` of the insert.
            let at = match src {
                Src::Hole => Src::Hole,
                Src::Journal(l) => {
                    Src::Journal(l.advance((a - range.lba) * BLOCK).expect("checked above"))
                }
            };
            let (removed, added) = put(&mut write(shard), self.stripe_blocks, a, b, seq.0, at);
            self.adjust(removed, added);
        });
    }
}

impl Overlay for ShardedOverlay {
    fn insert_write(&self, range: BlockRange, loc: JournalLoc, write_seq: WriteSeq) {
        self.insert(range, Src::Journal(loc), write_seq);
    }

    fn insert_hole(&self, range: BlockRange, write_seq: WriteSeq) {
        self.insert(range, Src::Hole, write_seq);
    }

    fn lookup(&self, range: BlockRange, out: &mut Vec<Piece>) {
        out.clear();
        if range.blocks == 0 {
            return;
        }
        let end = range
            .end()
            .expect("overlay range end overflows the block address space");
        self.for_stripes(range.lba, end, |shard, s, e| {
            let m = read(shard);
            let start = match m.range(..=s).next_back() {
                Some((&k, en)) if k + en.len() > s => k,
                _ => s,
            };
            let mut pos = s;
            for (&k, en) in m.range(start..e) {
                let (es, ee) = (k.max(s), (k + en.len()).min(e));
                if pos < es {
                    push_none(out, pos, es - pos);
                }
                out.push(Piece {
                    range: BlockRange {
                        lba: es,
                        blocks: ee - es,
                    },
                    source: en.source(es - k),
                    write_seq: WriteSeq(en.seq),
                });
                pos = ee;
            }
            if pos < e {
                push_none(out, pos, e - pos);
            }
        });
    }

    fn cut(&self, c: WriteSeq) -> OverlayCut {
        let mut pieces: Vec<Piece> = Vec::new();
        for sh in self.shards.iter() {
            let m = read(sh);
            for (&k, en) in m.iter().filter(|(_, en)| en.seq <= c.0) {
                // Split at chunk boundaries so that a piece stays in its chunk.
                let end = k + en.len();
                let mut a = k;
                while a < end {
                    let next_chunk = (a / self.opts.chunk_blocks)
                        .saturating_add(1)
                        .saturating_mul(self.opts.chunk_blocks);
                    let b = end.min(next_chunk);
                    pieces.push(Piece {
                        range: BlockRange {
                            lba: a,
                            blocks: b - a,
                        },
                        source: en.source(a - k),
                        write_seq: WriteSeq(en.seq),
                    });
                    a = b;
                }
            }
        }
        pieces.sort_unstable_by_key(|p| p.range.lba);
        let mut chunks: Vec<CutChunk> = Vec::new();
        for p in pieces {
            let chunk = p.range.lba / self.opts.chunk_blocks;
            match chunks.last_mut() {
                Some(last) if last.chunk == chunk => last.pieces.push(p),
                _ => chunks.push(CutChunk {
                    chunk,
                    pieces: vec![p],
                }),
            }
        }
        OverlayCut { cut_seq: c, chunks }
    }

    fn release(&self, cut: &OverlayCut) {
        for sh in self.shards.iter() {
            let mut removed = 0u64;
            write(sh).retain(|_, en| {
                let keep = en.seq > cut.cut_seq.0;
                if !keep {
                    removed += en.len();
                }
                keep
            });
            self.adjust(removed, 0);
        }
    }

    fn dirty_bytes(&self) -> u64 {
        self.dirty_blocks
            .load(Ordering::Relaxed)
            .saturating_mul(BLOCK)
    }
}
