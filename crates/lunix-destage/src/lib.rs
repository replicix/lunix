//! Zero, hash, dedup, compress, seal, pack and map update (M9; DESIGN section
//! 5, ADR-5, 6, 11, 12, 13; D16 to D20).
//!
//! [`destage_cut`] turns the data of an [`OverlayCut`](lunix_iface::overlay::OverlayCut)
//! into stored chunks. It runs on the commit coordinator's thread (C3), never
//! on a queue thread, and uses a pool of OS threads for the CPU-bound stages.
//!
//! # Pipeline, per dirty chunk
//!
//! 1. **Assemble**: a chunk the pieces cover fully is built from
//!    journal data (`Journal::read_at`) and zeros for hole pieces. A partly
//!    covered chunk reads its base chunk through the map and the segment store
//!    (counted as `cold_rmw`) and patches it. A chunk the hole pieces cover
//!    whole becomes a map hole without any read.
//! 2. **Zero scan** ([`scan::is_all_zero`]): an all-zero chunk is a map hole;
//!    no extent is stored.
//! 3. **Chunk id** through `ExtentCodec::chunk_id`: keyed in `encrypted` and
//!    `integrity` volumes (D18), unkeyed in `plain`. This crate holds no key.
//! 4. **Dedup**: `ChunkIndex::get_for_dedup` (which never returns a condemned
//!    entry), then the cut's own earlier extents. A `Hinted` hit from a
//!    persisted run is **confirmed** before the map points at it (D17): the
//!    extent at the hint is read and opened with `OpenExpect::chunk_id` set to
//!    the new chunk's full id, so all 32 bytes and the tag or AEAD must match;
//!    anything else is a miss and the chunk is stored. A `Verified` hit is
//!    taken as is.
//! 5. **Compress** ([`compress`]): `auto` is an LZ4 probe of the first 4 KiB,
//!    raw if it saves under 6 %, else zstd at the policy level; `lz4`,
//!    `zstd:N` and `none` as the descriptor says. Backpressure degrades to LZ4,
//!    then raw.
//! 6. **Seal** through `ExtentCodec::seal`; the salt is drawn inside it.
//! 7. **Pack** into segments in LBA order. A full segment is sealed at once,
//!    and the last one before the map commit, so every map entry names a
//!    sealed, readable file (DESIGN section 5 step 7: seal at a commit cut).
//! 8. **Map update**: one `MapTxn` for the whole cut, applied only if every
//!    chunk succeeded, after a probe transaction showed before any extent was
//!    stored that the map can take the cut. The index insert is the
//!    coordinator's, after the commit that names the segments is durable
//!    ([`DestageOutcome::insert_into_index`]).
//!
//! # Rule R1
//!
//! Neither stored-extent read of this crate is a scan. The base-chunk read
//! passes the leaf's `id_prefix`, the confirmation read the full id, and both
//! the volume's chunk size, so an extent that is valid but not the one named
//! fails with `Auth` instead of being patched into a new chunk or adopted.
//!
//! The confirmation is a defence against damaged or forged **run** entries, not
//! a security control in `plain` mode: there a writer of the bucket can forge a
//! tag-valid extent carrying any id, and D16 accepts that.
//!
//! # Determinism
//!
//! Workers race, the layout does not. Packing happens in chunk order on the
//! calling thread, and a chunk whose data equals an earlier one in the same cut
//! is resolved there (the first in LBA order is stored, the rest point at it),
//! so the same cut over the same state gives the same ids, entries and segment
//! layout; only the salt-dependent ciphertext differs between runs. Under
//! backpressure the codec depends on one [`Pressure`] level per cut (set by the
//! coordinator, or implied by the cut's size), never on timing.
//!
//! # Threads and blocking
//!
//! Every call here blocks and belongs to the coordinator or a worker; nothing
//! calls a queue-thread interface, and there is no tokio.

#![forbid(unsafe_code)]

mod assemble;
mod compress;
mod config;
mod deps;
mod outcome;
mod pack;
mod pipeline;
mod read;
mod worker;

pub mod scan;
pub mod stats;

pub use compress::Pressure;
pub use config::{DestageConfig, DestageParams};
pub use deps::DestageDeps;
pub use outcome::{DestageError, DestageOutcome, DestagedSegment, ErrorClass, Partial};
pub use pipeline::{destage_cut, destage_cut_with_cancel};
pub use stats::DestageStats;
