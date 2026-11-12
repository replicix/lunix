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
//! * A layer is a `BTreeMap<start_lba, Entry>` of non-overlapping intervals.
//!   An entry is `{blocks, journal location | hole, write_seq}` in 32 bytes;
//!   with the key and the B-tree's fill that is about 60 to 85 bytes per
//!   unmergeable entry (the memory gate is 96).
//! * **Versions.** A block may hold several versions (different `write_seq`).
//!   A shard is a stack of layers: layer 0 holds the newest version of every
//!   block it has, layer `k` the `k`-th newest; a block present in layer
//!   `k + 1` is present in layer `k` with a larger `write_seq`. Almost every
//!   shard has one layer. An insert goes into layer 0; whatever it displaces
//!   (an older entry) or whatever a newer entry shadows (the inserted data)
//!   sinks to the next layer, recursively. An equal `write_seq` replaces. Arrival
//!   order never matters.
//! * Neighbouring entries of a layer merge when they have the same `write_seq`
//!   and the same kind and, for journal data, contiguous locations. Records have
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
//! Superseded versions are **kept** until released (ADR-3, DESIGN section 8.1
//! step 3: a commit for cut `c` holds every write `<= c`, also a block that a
//! write `> c` has since overwritten). [`lookup`](Overlay::lookup) reads layer
//! 0 only. [`cut(c)`](Overlay::cut) returns, per block, the newest version with
//! `write_seq <= c`: it walks the layers from the newest and takes each entry
//! `<= c` where no newer layer already supplied the block, under read locks, and
//! copies. The returned [`OverlayCut`] is owned data, so later inserts cannot
//! change it. [`release`](Overlay::release) removes every version with
//! `write_seq <= cut_seq` (the newest one `<= c` of a block and all older ones;
//! newer versions survive). Versions are counted in
//! [`dirty_bytes`](Overlay::dirty_bytes), so the cap bounds them: a block
//! rewritten many times between cuts costs 4 KiB per version until the seal.
//!
//! `release` assumes the cut protocol: no insert with `write_seq <= c` arrives
//! after `cut(c)` (DESIGN section 8.1 step 2 waits for the in-flight writes).
//!
//! # Dirty-memory cap
//!
//! [`dirty_bytes`](Overlay::dirty_bytes) is exact and counts every version held. [`OverlayOptions::dirty_cap_bytes`]
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

/// One layer: non-overlapping intervals.
type Layer = BTreeMap<u64, Entry>;

/// A shard: version stacks as layers. Layer 0 holds, per block, the newest
/// version; layer `k` the `k`-th newest. So a block present in layer `k + 1` is
/// present in layer `k` with a larger `write_seq`. Most shards have one layer.
#[derive(Debug, Default)]
struct Shard {
    layers: Vec<Layer>,
}

fn read(s: &RwLock<Shard>) -> RwLockReadGuard<'_, Shard> {
    s.read().unwrap_or_else(|p| p.into_inner())
}

fn write(s: &RwLock<Shard>) -> RwLockWriteGuard<'_, Shard> {
    s.write().unwrap_or_else(|p| p.into_inner())
}

/// Puts `[a, b)` of the new entry `new` (which starts at `s`) into `map`,
/// merging with its neighbours. Never merges across a stripe boundary: an
/// interval stays inside its stripe.
fn fill(map: &mut Layer, stripe: u64, s: u64, new: &Entry, a: u64, b: u64) {
    if a >= b {
        return;
    }
    let ent = new.part(a - s, b - a);
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

/// Inserts `new` (starting at block `s`, inside one stripe) into one layer.
/// Where the layer holds a newer entry, the new data goes to `cascade` (for
/// the next layer); where it holds an older one, that entry's overlapped part
/// goes to `cascade`; an entry with the same `write_seq` is replaced.
/// Returns the blocks `(removed, added)` in this layer.
fn put(
    map: &mut Layer,
    stripe: u64,
    s: u64,
    new: &Entry,
    cascade: &mut Vec<(u64, Entry)>,
) -> (u64, u64) {
    let e = s + new.len();
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
        match o.seq.cmp(&new.seq) {
            std::cmp::Ordering::Greater => {
                // The older new data fills around the newer entry and goes
                // below it too.
                fill(map, stripe, s, new, cursor, ms);
                added += ms.saturating_sub(cursor);
                map.insert(ms, o.part(ms - k, me - ms));
                added += me - ms;
                cascade.push((ms, new.part(ms - s, me - ms)));
                cursor = me;
            }
            std::cmp::Ordering::Less => cascade.push((ms, o.part(ms - k, me - ms))),
            std::cmp::Ordering::Equal => {}
        }
        from = me;
    }
    fill(map, stripe, s, new, cursor, e);
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

/// The parts of `[a, b)` that `chosen` (disjoint intervals, start to end) does
/// not cover, ascending.
fn uncovered(chosen: &BTreeMap<u64, u64>, a: u64, b: u64, out: &mut Vec<(u64, u64)>) {
    let mut pos = a;
    let start = match chosen.range(..=a).next_back() {
        Some((&cs, &ce)) if ce > a => cs,
        _ => a,
    };
    for (&cs, &ce) in chosen.range(start..b) {
        if cs > pos {
            out.push((pos, cs));
        }
        pos = pos.max(ce);
    }
    if pos < b {
        out.push((pos, b));
    }
}

fn cover(chosen: &mut BTreeMap<u64, u64>, gaps: &[(u64, u64)]) {
    for &(a, b) in gaps {
        chosen.insert(a, b);
    }
}

/// `source` moved `blocks` blocks further.
fn advance(source: PieceSource, blocks: u64) -> PieceSource {
    match source {
        PieceSource::Journal(l) => PieceSource::Journal(JournalLoc {
            offset: l.offset + blocks * BLOCK,
            ..l
        }),
        other => other,
    }
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
                .map(|_| RwLock::new(Shard::default()))
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

    /// Number of intervals held, versions included (for tests and metrics;
    /// takes every read lock in turn).
    pub fn entries(&self) -> usize {
        self.shards
            .iter()
            .map(|s| read(s).layers.iter().map(Layer::len).sum::<usize>())
            .sum()
    }

    /// Checks the structural invariants: intervals are non-empty, ordered,
    /// non-overlapping, inside one stripe of their own shard; a block in layer
    /// `k + 1` is in layer `k` with a larger `write_seq`; the dirty counter
    /// equals the blocks (versions) held. Meant for tests; quiesce writers
    /// first.
    ///
    /// # Errors
    /// A description of the first violation.
    pub fn validate(&self) -> std::result::Result<(), String> {
        let mut total = 0u64;
        for (i, sh) in self.shards.iter().enumerate() {
            let g = read(sh);
            for (li, m) in g.layers.iter().enumerate() {
                if m.is_empty() {
                    return Err(format!("empty layer {li}"));
                }
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
                    if li > 0 {
                        // Every block of this entry must be covered by layer
                        // li - 1 with a larger seq.
                        let above = &g.layers[li - 1];
                        let mut pos = k;
                        let start = match above.range(..=k).next_back() {
                            Some((&ak, ae)) if ak + ae.len() > k => ak,
                            _ => k,
                        };
                        for (&ak, ae) in above.range(start..prev_end) {
                            if ak > pos || ae.seq <= e.seq {
                                return Err(format!("layer order broken at {pos}"));
                            }
                            pos = ak + ae.len();
                        }
                        if pos < prev_end {
                            return Err(format!("layer {li} entry at {k} has no newer version"));
                        }
                    }
                }
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
            let new = Entry::new(src, a - range.lba, b - a, seq.0);
            let (removed, added) = insert_stripe(&mut write(shard), self.stripe_blocks, a, new);
            self.adjust(removed, added);
        });
    }
}

/// Inserts into layer 0 and lets displaced and shadowed parts sink through the
/// layers. Returns the blocks `(removed, added)` over all layers.
fn insert_stripe(g: &mut Shard, stripe: u64, s: u64, new: Entry) -> (u64, u64) {
    let (mut removed, mut added) = (0u64, 0u64);
    let mut cascade = Vec::new();
    let mut pending: Vec<(usize, u64, Entry)> = Vec::new();
    let mut cur = Some((0usize, s, new));
    while let Some((k, at, ent)) = cur.take().or_else(|| pending.pop()) {
        if k == g.layers.len() {
            g.layers.push(Layer::new());
        }
        let (r, a) = put(&mut g.layers[k], stripe, at, &ent, &mut cascade);
        removed += r;
        added += a;
        pending.extend(cascade.drain(..).map(|(at, e)| (k + 1, at, e)));
    }
    (removed, added)
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
            let g = read(shard);
            let Some(m) = g.layers.first() else {
                push_none(out, s, e - s);
                return;
            };
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
        let mut raw: Vec<Piece> = Vec::new();
        for sh in self.shards.iter() {
            let g = read(sh);
            // Blocks already chosen from a newer layer (start -> end); only
            // needed with more than one layer.
            let mut chosen: BTreeMap<u64, u64> = BTreeMap::new();
            let multi = g.layers.len() > 1;
            for layer in &g.layers {
                let mut gaps: Vec<(u64, u64)> = Vec::new();
                for (&k, en) in layer.iter().filter(|(_, en)| en.seq <= c.0) {
                    let end = k + en.len();
                    gaps.clear();
                    uncovered(&chosen, k, end, &mut gaps);
                    for &(a, b) in &gaps {
                        raw.push(Piece {
                            range: BlockRange {
                                lba: a,
                                blocks: b - a,
                            },
                            source: en.source(a - k),
                            write_seq: WriteSeq(en.seq),
                        });
                    }
                    // Collected after the layer is done: a layer's own
                    // entries never overlap.
                    if multi {
                        cover(&mut chosen, &gaps);
                    }
                }
            }
        }
        raw.sort_unstable_by_key(|p| p.range.lba);
        let mut chunks: Vec<CutChunk> = Vec::new();
        for p in raw {
            // Split at chunk boundaries so that a piece stays in its chunk.
            let end = p.range.lba + p.range.blocks;
            let mut a = p.range.lba;
            while a < end {
                let next_chunk = (a / self.opts.chunk_blocks)
                    .saturating_add(1)
                    .saturating_mul(self.opts.chunk_blocks);
                let b = end.min(next_chunk);
                let part = Piece {
                    range: BlockRange {
                        lba: a,
                        blocks: b - a,
                    },
                    source: advance(p.source, a - p.range.lba),
                    write_seq: p.write_seq,
                };
                let chunk = a / self.opts.chunk_blocks;
                match chunks.last_mut() {
                    Some(last) if last.chunk == chunk => last.pieces.push(part),
                    _ => chunks.push(CutChunk {
                        chunk,
                        pieces: vec![part],
                    }),
                }
                a = b;
            }
        }
        OverlayCut { cut_seq: c, chunks }
    }

    fn release(&self, cut: &OverlayCut) {
        for sh in self.shards.iter() {
            let mut removed = 0u64;
            let mut g = write(sh);
            // Per block the versions `<= cut_seq` are the oldest ones, so
            // removing them keeps every remaining version's rank.
            for layer in &mut g.layers {
                layer.retain(|_, en| {
                    let keep = en.seq > cut.cut_seq.0;
                    if !keep {
                        removed += en.len();
                    }
                    keep
                });
            }
            while g.layers.last().is_some_and(Layer::is_empty) {
                g.layers.pop();
            }
            drop(g);
            self.adjust(removed, 0);
        }
    }

    fn dirty_bytes(&self) -> u64 {
        self.dirty_blocks
            .load(Ordering::Relaxed)
            .saturating_mul(BLOCK)
    }
}
