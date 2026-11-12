//! Proptest strategies for the operation sequences the fakes and the real
//! crates are tested with.
//!
//! * [`io_ops`]: block-device requests (read, write, flush, FUA, discard,
//!   write-zeroes) for the engine and the device oracle.
//! * [`overlay_ops`]: inserts, lookups, cuts and releases for M4.
//! * [`map_ops`]: transactions, seals, reopens and grows for M5.
//!
//! Strategies generate plain data; the tests interpret it. Everything is
//! deterministic from the proptest seed.

use lunix_format::node::DataEntry;
use lunix_format::{ChunkId, IdLen, IdPrefix, LineageIdx, ObjId};
use lunix_iface::block::IoOp;
use proptest::collection::SizeRange;
use proptest::prelude::*;

const BLOCK: u64 = 4096;
/// The largest request the strategies produce: 1 MiB.
pub const MAX_IO_BYTES: u64 = 1 << 20;

/// One block-device request.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct IoSpec {
    /// The operation.
    pub op: IoOp,
    /// FUA (writes only).
    pub fua: bool,
    /// Byte offset.
    pub offset_bytes: u64,
    /// Length in bytes (0 for flush).
    pub len: u32,
    /// Seeds the payload of a write.
    pub seed: u64,
}

impl IoSpec {
    /// True when the engine must accept the request on a device of
    /// `dev_bytes`: 4 KiB aligned offset and length, inside the device.
    /// Anything else must complete with `EINVAL`. The engine's
    /// `max_io_buf_bytes` is not considered: the strategies never produce more
    /// than [`MAX_IO_BYTES`] for a read or write, which every test's buffer
    /// size must cover.
    pub fn is_valid(&self, dev_bytes: u64) -> bool {
        if self.op == IoOp::Flush {
            return true;
        }
        let len = u64::from(self.len);
        self.offset_bytes.is_multiple_of(BLOCK)
            && len.is_multiple_of(BLOCK)
            && len > 0
            && self
                .offset_bytes
                .checked_add(len)
                .is_some_and(|end| end <= dev_bytes)
    }

    /// The bytes a write carries: `len` bytes derived from `seed`.
    pub fn payload(&self) -> Vec<u8> {
        pattern(self.seed, self.len as usize)
    }
}

/// `len` deterministic, non-constant bytes derived from `seed` (splitmix64),
/// so two writes with different seeds never agree by accident.
pub fn pattern(seed: u64, len: usize) -> Vec<u8> {
    fn mix(state: u64) -> [u8; 8] {
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^= z >> 31;
        z.to_le_bytes()
    }
    let mut out = vec![0u8; len];
    let mut state = seed;
    let (words, rest) = out.as_chunks_mut::<8>();
    for w in words {
        state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        *w = mix(state);
    }
    if !rest.is_empty() {
        state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let bytes = mix(state);
        rest.copy_from_slice(&bytes[..rest.len()]);
    }
    out
}

/// Shape of the requests [`io_ops`] generates.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct IoShape {
    /// Device size in bytes (a multiple of 4096).
    pub dev_bytes: u64,
    /// The volume's chunk size in bytes (a multiple of 4096): offsets and
    /// lengths cluster around its multiples.
    pub chunk_bytes: u64,
    /// Also produce, about one request in twenty, a request that is usually
    /// invalid (unaligned, or reaching past the end). Ask
    /// [`IoSpec::is_valid`] which, never assume.
    pub include_invalid: bool,
}

impl IoShape {
    /// Valid requests only.
    pub fn new(dev_bytes: u64, chunk_bytes: u64) -> Self {
        assert!(
            dev_bytes >= BLOCK && dev_bytes.is_multiple_of(BLOCK),
            "dev_bytes"
        );
        assert!(
            chunk_bytes >= BLOCK && chunk_bytes.is_multiple_of(BLOCK),
            "chunk_bytes"
        );
        Self {
            dev_bytes,
            chunk_bytes,
            include_invalid: false,
        }
    }

    /// Also produce invalid requests.
    #[must_use]
    pub fn with_invalid(mut self) -> Self {
        self.include_invalid = true;
        self
    }

    fn build(self, raw: RawIo) -> IoSpec {
        let blocks = self.dev_bytes / BLOCK;
        let chunk_blocks = self.chunk_bytes / BLOCK;
        let max_blocks = (MAX_IO_BYTES / BLOCK).min(blocks);
        let op = match raw.kind % 100 {
            0..=29 => IoOp::Read,
            30..=64 => IoOp::Write,
            65..=74 => IoOp::Write, // FUA below
            75..=82 => IoOp::Flush,
            83..=92 => IoOp::Discard,
            _ => IoOp::WriteZeroes,
        };
        let fua = op == IoOp::Write && (65..=74).contains(&(raw.kind % 100));
        if op == IoOp::Flush {
            return IoSpec {
                op,
                fua: false,
                offset_bytes: 0,
                len: 0,
                seed: raw.seed,
            };
        }
        // Length in blocks: small, around the chunk size, or large.
        let want = match raw.size_sel % 10 {
            0 => 1,
            1 => 2,
            2 => 3,
            3 => 16,
            4 => chunk_blocks,
            5 => chunk_blocks.saturating_add(1),
            6 => chunk_blocks.saturating_sub(1).max(1),
            7 => chunk_blocks.saturating_mul(2),
            8 => max_blocks,
            _ => 1 + raw.size_rand % max_blocks,
        };
        let len_blocks = want.clamp(1, max_blocks);
        // Start block: chunk aligned, straddling a chunk boundary, or anywhere.
        let last_start = blocks - len_blocks;
        let start = match raw.off_sel % 3 {
            0 => (raw.off_rand % (last_start / chunk_blocks + 1)) * chunk_blocks,
            1 => {
                let boundary = (1 + raw.off_rand % (blocks / chunk_blocks).max(1)) * chunk_blocks;
                boundary.saturating_sub(1 + raw.size_rand % len_blocks.max(1))
            }
            _ => raw.off_rand % (last_start + 1),
        }
        .min(last_start);
        let mut offset_bytes = start * BLOCK;
        let mut len = len_blocks * BLOCK;
        if self.include_invalid && raw.invalid.is_multiple_of(20) {
            match raw.invalid / 20 % 4 {
                0 => offset_bytes += 512,
                1 => len += 512,
                2 => offset_bytes = self.dev_bytes,
                _ => len = u64::from(i32::MAX.unsigned_abs()) / BLOCK * BLOCK,
            }
        }
        IoSpec {
            op,
            fua,
            offset_bytes,
            len: u32::try_from(len).expect("clamped to u32"),
            seed: raw.seed,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct RawIo {
    kind: u8,
    size_sel: u8,
    size_rand: u64,
    off_sel: u8,
    off_rand: u64,
    invalid: u8,
    seed: u64,
}

fn raw_io() -> impl Strategy<Value = RawIo> {
    (
        any::<u8>(),
        any::<u8>(),
        any::<u64>(),
        any::<u8>(),
        any::<u64>(),
        any::<u8>(),
        any::<u64>(),
    )
        .prop_map(
            |(kind, size_sel, size_rand, off_sel, off_rand, invalid, seed)| RawIo {
                kind,
                size_sel,
                size_rand,
                off_sel,
                off_rand,
                invalid,
                seed,
            },
        )
}

/// One request of the given shape.
pub fn io_op(shape: IoShape) -> impl Strategy<Value = IoSpec> {
    raw_io().prop_map(move |raw| shape.build(raw))
}

/// A sequence of requests; `len` is the number of requests.
pub fn io_ops(shape: IoShape, len: impl Into<SizeRange>) -> impl Strategy<Value = Vec<IoSpec>> {
    proptest::collection::vec(io_op(shape), len)
}

// ---- overlay -------------------------------------------------------------

/// One step of an overlay test.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum OverlayOp {
    /// `insert_write` of `blocks` blocks at `lba`, journal ordinal `ordinal`.
    Write {
        /// First block.
        lba: u64,
        /// Blocks, at least 1.
        blocks: u64,
        /// Distinguishes journal locations.
        ordinal: u32,
        /// Unique per sequence.
        write_seq: u64,
    },
    /// `insert_hole`.
    Hole {
        /// First block.
        lba: u64,
        /// Blocks, at least 1.
        blocks: u64,
        /// Unique per sequence.
        write_seq: u64,
    },
    /// `lookup`.
    Lookup {
        /// First block.
        lba: u64,
        /// Blocks, at least 1.
        blocks: u64,
    },
    /// `cut(WriteSeq(seq))`.
    Cut {
        /// The cut.
        seq: u64,
    },
    /// `release` of the `n`th cut taken so far (modulo the number taken; a
    /// no-op when none was taken).
    Release {
        /// Which cut.
        n: usize,
    },
}

/// Overlay operations over blocks `0..universe_blocks`. Every `write_seq` in a
/// sequence is distinct (a random permutation of `1..=len`) so arrival order
/// and sequence order differ, as with several journal lanes.
pub fn overlay_ops(universe_blocks: u64, len: usize) -> impl Strategy<Value = Vec<OverlayOp>> {
    assert!(universe_blocks >= 2 && len >= 1);
    let seqs = Just((1..=len as u64).collect::<Vec<_>>()).prop_shuffle();
    let raw = proptest::collection::vec(
        (
            0u8..100,
            0..universe_blocks,
            1..=universe_blocks.min(16),
            0u32..4,
            0..=len as u64,
            any::<usize>(),
        ),
        len,
    );
    (seqs, raw).prop_map(move |(seqs, raw)| {
        raw.into_iter()
            .zip(seqs)
            .map(|((k, lba, blocks, ordinal, cut, n), write_seq)| {
                let blocks = blocks.min(universe_blocks - lba);
                match k {
                    0..=39 => OverlayOp::Write {
                        lba,
                        blocks,
                        ordinal,
                        write_seq,
                    },
                    40..=54 => OverlayOp::Hole {
                        lba,
                        blocks,
                        write_seq,
                    },
                    55..=84 => OverlayOp::Lookup { lba, blocks },
                    85..=94 => OverlayOp::Cut { seq: cut },
                    _ => OverlayOp::Release { n },
                }
            })
            .collect()
    })
}

// ---- map -----------------------------------------------------------------

/// A data entry for chunk `ci` as of `version`: distinct per `(ci, version)`,
/// with a hint that changes with the version.
pub fn data_entry(ci: u64, version: u32, id_len: IdLen) -> DataEntry {
    let mut id = [0u8; 32];
    id[..8].copy_from_slice(&ci.to_le_bytes());
    id[8..12].copy_from_slice(&version.to_le_bytes());
    DataEntry {
        lineage: LineageIdx(0),
        seg: ObjId::new(0, 1 + u64::from(version)).expect("segment id"),
        off: u32::try_from(ci % 1000).expect("small") * 64,
        clen: 1000 + version,
        prefix: IdPrefix::of(&ChunkId(id), id_len),
    }
}

/// One update inside a transaction.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TxnOp {
    /// `set(ci, data_entry(ci, version))`.
    Set {
        /// Chunk.
        ci: u64,
        /// Entry version.
        version: u32,
    },
    /// `set_hole(ci)`.
    Hole {
        /// Chunk.
        ci: u64,
    },
    /// `rehint(ci, hint of data_entry(ci, from), hint of data_entry(ci, to))`.
    Rehint {
        /// Chunk.
        ci: u64,
        /// The version the move started from.
        from: u32,
        /// The version it moved to.
        to: u32,
    },
}

/// One step of a map test.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum MapOp {
    /// Open a transaction, apply the ops, commit.
    Commit(Vec<TxnOp>),
    /// Open a transaction, apply the ops, drop it.
    Abort(Vec<TxnOp>),
    /// `get(ci)`.
    Get {
        /// Chunk.
        ci: u64,
    },
    /// Seal into a fresh pack and check the root.
    Seal,
    /// Seal, then reopen at the sealed root.
    Reopen,
    /// `grow` by this many chunks.
    Grow {
        /// Added chunks.
        by: u64,
    },
}

fn txn_op(chunks: u64) -> impl Strategy<Value = TxnOp> {
    // Few versions so that rehints from the current version actually hit.
    (0u8..100, 0..chunks, 0u32..3, 0u32..3).prop_map(|(k, ci, a, b)| match k {
        0..=59 => TxnOp::Set { ci, version: a },
        60..=74 => TxnOp::Hole { ci },
        _ => TxnOp::Rehint { ci, from: a, to: b },
    })
}

/// Map operations over `chunks` initial chunks. Chunk numbers spread over
/// several 512-chunk leaves when `chunks` is large; `Grow` extends the range
/// but later ops keep drawing from the initial range (the tests clamp).
pub fn map_ops(chunks: u64, len: usize) -> impl Strategy<Value = Vec<MapOp>> {
    assert!(chunks >= 1 && len >= 1);
    let txn = proptest::collection::vec(txn_op(chunks), 1..8);
    proptest::collection::vec(
        (0u8..100, txn, 0..chunks, 1u64..2000).prop_map(|(k, ops, ci, by)| match k {
            0..=44 => MapOp::Commit(ops),
            45..=51 => MapOp::Abort(ops),
            52..=71 => MapOp::Get { ci },
            72..=84 => MapOp::Seal,
            85..=93 => MapOp::Reopen,
            _ => MapOp::Grow { by },
        }),
        len,
    )
}
