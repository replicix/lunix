//! The cryptographic seams: extents, metadata objects and commits (ADR-11,
//! D2, D9, D11, D16). `lunix-crypto` implements them for real; the testkit's
//! `PlainCodec` implements all three with a fixed test key.
//!
//! # Threads and blocking
//!
//! CPU-bound, no I/O, thread-safe, **blocking-free** but not cheap: destage
//! workers and read helpers call them; queue threads call them only for
//! small, bounded work (an extent open of at most 16 KiB).
//!
//! # Salt rule
//!
//! No method here accepts a salt or a nonce. `seal` draws a fresh random salt
//! (extents) or nonce (metadata) from the operating system every call, so
//! sealing the same chunk twice, and compaction re-sealing it, give different
//! bytes under different keys.

use std::any::Any;
use std::fmt;
use std::sync::Arc;

use lunix_format::commit::Commit;
use lunix_format::{
    ChunkId, Codec, CommitSeq, CryptoMode, ExtentKind, IdPrefix, Level, ObjectKind,
};

use crate::error::{Error, Result};
use crate::store::CommitStore;

/// How a chunk is encoded inside an extent.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct EncodingDesc {
    /// The codec.
    pub codec: Codec,
    /// The codec level as written in the `CCH1` header.
    pub level: Level,
    /// The uncompressed length.
    pub ulen: u32,
    /// The zstd dictionary id for codec 3 (`Invalid` unless `codec` is
    /// `ZstdDict`; always `None` in phase 1). On encrypted volumes the
    /// segment writer cannot read it from the ciphertext, so it is passed
    /// along explicitly (`SegmentWriter::append_extent`).
    pub dict_id: Option<u32>,
}

/// What `open` returns.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OpenOutput {
    /// The stored payload as the codec produced it (compressed), decrypted.
    /// For compaction (re-seal) and dedup confirmation: no decompression.
    Stored,
    /// The decompressed plaintext (`ulen` bytes), for reads and `--deep` fsck.
    Plaintext,
}

/// What the caller expects of an extent; every field is checked before the
/// payload is trusted.
#[derive(Clone, Copy, Debug)]
pub struct OpenExpect<'a> {
    /// The volume's chunk size: a data extent's `ulen` must equal it, and
    /// decompression is capped at it, never at the header's `ulen`. Map nodes
    /// use their own 64 KiB cap.
    pub chunk_size: u32,
    /// The full id the caller is looking for (dedup confirmation, stale-hint
    /// fallback): all 32 bytes must equal the extent's.
    pub chunk_id: Option<&'a ChunkId>,
    /// Or only a prefix (a map entry resolving through its hint).
    pub id_prefix: Option<&'a IdPrefix>,
    /// Recompute the id from the plaintext (the key commitment). Local reads
    /// leave it false because the tag already authenticates the stored bytes
    /// (D11); `verify_reads` and `--deep` set it.
    pub verify_id: bool,
    /// Which form of the payload to return.
    pub output: OpenOutput,
}

/// What `open` reports about a verified extent (the payload goes to the
/// caller's buffer).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct OpenedMeta {
    /// How the chunk is encoded.
    pub desc: EncodingDesc,
    /// The full chunk id from the header.
    pub chunk_id: ChunkId,
}

/// Seals and opens extents (chunks and map nodes).
pub trait ExtentCodec: Send + Sync {
    /// The volume mode (tag and header layout follow from it).
    fn mode(&self) -> CryptoMode;

    /// The chunk id of `data`: BLAKE3 (plain, integrity) or the keyed id
    /// (encrypted). For a map node, the node hash.
    fn chunk_id(&self, kind: ExtentKind, data: &[u8]) -> ChunkId;

    /// Appends the complete extent for `id`: `CCH1` header, id, `clen`,
    /// crypto region when encrypted, payload (`compressed`, encrypted when
    /// the volume is), and the 16-byte tag (D11: present in every extent).
    /// `compressed` is the codec output the caller produced.
    ///
    /// # Errors
    /// `Invalid` for a bad descriptor; `Unsupported` for the FIPS profile.
    fn seal(
        &self,
        kind: ExtentKind,
        id: &ChunkId,
        desc: &EncodingDesc,
        compressed: &[u8],
        out: &mut Vec<u8>,
    ) -> Result<()>;

    /// Verifies and opens a stored extent and **appends** the payload, in the
    /// form `expect.output` asks for, to `out` (no per-call allocation; `out`
    /// is untouched on error). Checks: header sanity and the id comparison, the
    /// plain tag or AEAD tag, `ulen` against `expect.chunk_size`, bounded
    /// decompression, then the id recomputation when asked. Every failure of a
    /// tag or an id is the same `Auth`, so the order is not observable.
    ///
    /// For `ExtentKind::Node` the `chunk_size` field is ignored and the cap is
    /// `lunix_format::node::MAX_NODE_ULEN`. `verify_id = true` with
    /// `OpenOutput::Stored` decompresses internally (bounded) and still
    /// returns the stored form.
    ///
    /// # Errors
    /// `Auth` for any tag, AEAD or id mismatch (one error for all of them),
    /// `Corrupt`/`Format` for structure, `Unsupported` for codec 3 until
    /// phase 4.
    fn open(
        &self,
        kind: ExtentKind,
        bytes: &[u8],
        expect: &OpenExpect<'_>,
        out: &mut Vec<u8>,
    ) -> Result<OpenedMeta>;
}

/// Seals and opens metadata objects: footers, lease, pins, `gc/`, `heat/`,
/// `dicts/` (the commit kinds go through [`CommitCodec`]).
pub trait MetaCodec: Send + Sync {
    /// The volume mode (decides envelope form 0 or 1).
    fn mode(&self) -> CryptoMode;

    /// Wraps `body` in its envelope. `name` is the **bucket-relative** object
    /// name (D9), bound into the AAD or MAC. Returns the bytes to store.
    fn seal(&self, kind: ObjectKind, name: &str, body: &[u8]) -> Result<Vec<u8>>;

    /// Verifies the envelope of `bytes` for `kind` and `name` and returns the
    /// body. An object moved to another name, kind or family fails `Auth`.
    fn open(&self, kind: ObjectKind, name: &str, bytes: &[u8]) -> Result<Vec<u8>>;
}

/// A commit object ready to store.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SealedCommit {
    /// The bytes for `CommitStore::put`.
    pub bytes: Vec<u8>,
    /// BLAKE3-256 of `bytes`: the next commit's `parent_hash`.
    pub hash: [u8; 32],
}

/// [`MetaCodec`] plus the commit chain: sealing for encrypted volumes, the
/// HMAC (integrity) or unkeyed checksum (plain) otherwise.
pub trait CommitCodec: MetaCodec {
    /// Seals or MACs `commit` under its bucket-relative name `commits/<seq>`.
    fn seal_commit(&self, commit: &Commit) -> Result<SealedCommit>;

    /// Authenticates and decodes the commit stored for `seq`. Fails `Auth` on a
    /// bad MAC/AEAD and `Corrupt` when the body's `commit_seq` is not `seq`.
    fn open_commit(&self, seq: CommitSeq, bytes: &[u8]) -> Result<Commit>;

    /// BLAKE3-256 of the stored bytes (what a child's `parent_hash` holds).
    fn object_hash(&self, bytes: &[u8]) -> [u8; 32];

    /// Verifies the chain from `tip` down to `floor` (both inclusive): every
    /// commit authenticates, `commit_seq` is contiguous, `parent_hash` equals
    /// the hash of the parent's stored bytes, `epoch` never decreases, and when
    /// `floor` is 0 the genesis commit's `parent_hash` equals `volume_hash`.
    /// The floor commit's parent may be absent. `created_at` is never checked
    /// (D17). Returns the authenticated tip.
    ///
    /// # Errors
    /// `NotFound` for a missing link, `Auth`, or `Format` for a broken rule.
    fn verify_chain(
        &self,
        store: &dyn CommitStore,
        tip: CommitSeq,
        floor: CommitSeq,
        volume_hash: &[u8; 32],
    ) -> Result<Commit> {
        if floor > tip {
            return Err(Error::invalid("chain floor above the tip"));
        }
        let tip_bytes = store.get(tip)?;
        let tip_commit = self.open_commit(tip, &tip_bytes)?;
        let mut child = *tip_commit.common();
        let mut seq = tip;
        while seq > floor {
            let parent_seq = CommitSeq(seq.0 - 1);
            let parent_bytes = store.get(parent_seq)?;
            let parent = self.open_commit(parent_seq, &parent_bytes)?;
            lunix_format::commit::check_child(
                parent.common(),
                &self.object_hash(&parent_bytes),
                &child,
            )?;
            child = *parent.common();
            seq = parent_seq;
        }
        if floor == CommitSeq(0) {
            lunix_format::commit::check_genesis(&child, volume_hash)?;
        }
        Ok(tip_commit)
    }
}

/// What a key handle can say about itself without exposing key bytes.
pub trait KeyMaterial: Send + Sync + 'static {
    /// The mode the keys serve.
    fn mode(&self) -> CryptoMode;

    /// The 16-byte KMK fingerprint (zero in plain mode).
    fn fingerprint(&self) -> [u8; 16];

    /// For `lunix-crypto` to recover its concrete type.
    fn as_any(&self) -> &dyn Any;
}

/// An opaque, cloneable handle to unlocked key material. It has no `Debug`
/// output beyond its mode and never serialises.
#[derive(Clone)]
pub struct Keys(Arc<dyn KeyMaterial>);

impl Keys {
    /// Wraps key material.
    pub fn new(material: Arc<dyn KeyMaterial>) -> Self {
        Self(material)
    }

    /// The mode.
    pub fn mode(&self) -> CryptoMode {
        self.0.mode()
    }

    /// The fingerprint.
    pub fn fingerprint(&self) -> [u8; 16] {
        self.0.fingerprint()
    }

    /// The implementation's concrete material.
    pub fn material(&self) -> &dyn KeyMaterial {
        &*self.0
    }
}

impl fmt::Debug for Keys {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Keys({:?}, <redacted>)", self.0.mode())
    }
}
