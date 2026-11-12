//! Every frozen on-disk and on-bucket type of Lunix, with strict encode and
//! decode (plan M1.A; layouts in `docs/reference/on-disk-formats.md`).
//!
//! Pure Rust: no I/O, no cryptographic operations, no `unsafe`. MACs, AEAD
//! and key derivation are `lunix-crypto`'s; this crate only builds the bytes
//! they are computed over and parses the bytes they are stored in.
//!
//! Rules every decoder follows:
//!
//! * reserved fields decode only when zero;
//! * every count is checked against its stated maximum and the remaining
//!   input before anything is allocated;
//! * arithmetic on sizes, offsets and sequence numbers is checked;
//! * where a structure is hashed or MAC'd, only its canonical encoding is
//!   accepted, so `encode(decode(b)) == b` for every accepted `b`.
//!
//! A change to any golden file under `tests/golden/` is a format change and
//! needs the maintainer (see `FREEZE.md`).

#![forbid(unsafe_code)]

pub mod bytes;
pub mod envelope;
pub mod error;
pub mod ids;
pub mod mode;
pub mod paths;
pub mod purpose;

pub mod commit;
pub mod extent;
pub mod gcfile;
pub mod index_run;
pub mod journal;
pub mod keyring;
pub mod lease;
pub mod local;
pub mod node;
pub mod opaque;
pub mod pins;
pub mod segment;
pub mod volume;

pub use error::{FormatError, Result};
pub use ids::{
    ChunkId, Codec, CommitSeq, DekGen, Epoch, ExtentKind, FamilyId, Hint, IdLen, IdPrefix,
    LineageIdx, Level, ObjId, PackId, RunId, SegId, TimeMs, VolumeUuid, WriteSeq,
};
pub use mode::{CryptoMode, Form, ObjectKind, Profile};
